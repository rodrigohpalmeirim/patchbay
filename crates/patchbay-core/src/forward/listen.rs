//! Local listening sockets for local and dynamic forwards.
//!
//! Bind addresses: `None` = `127.0.0.1`; `localhost` = `127.0.0.1` plus
//! `::1` when IPv6 is available; `*`, `0.0.0.0` (and `""`) = all IPv4
//! interfaces; `::` = all interfaces (dual-stack where the OS allows);
//! IP literals as given; other names are resolved and every address bound.
//! With port 0 the first socket picks a free port and the others reuse it.

use std::future::poll_fn;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::task::Poll;

use tokio::net::{TcpListener, TcpStream};

use crate::{Error, Result};

/// The sockets of one forward; accepts from all of them.
#[derive(Debug)]
pub(crate) struct Listeners {
    sockets: Vec<TcpListener>,
}

impl Listeners {
    /// Bind every address `bind_address` stands for. The first address must
    /// bind; failures on further ones (e.g. no IPv6) are only logged.
    pub(crate) async fn bind(bind_address: Option<&str>, port: u16) -> Result<Self> {
        let addrs = resolve(bind_address, port).await?;
        let mut sockets: Vec<TcpListener> = Vec::with_capacity(addrs.len());
        for mut addr in addrs {
            if let Some(first) = sockets.first() {
                addr.set_port(first.local_addr()?.port());
            }
            match TcpListener::bind(addr).await {
                Ok(socket) => sockets.push(socket),
                Err(e) if sockets.is_empty() => return Err(bind_error(addr, &e)),
                Err(e) => tracing::debug!(%addr, "skipping additional listen address: {e}"),
            }
        }
        Ok(Self { sockets })
    }

    /// `ip:port` of the first socket, for display.
    pub(crate) fn address(&self) -> String {
        self.sockets
            .first()
            .and_then(|s| s.local_addr().ok())
            .map(|a| a.to_string())
            .unwrap_or_default()
    }

    /// The next incoming connection on any socket. Cancel-safe.
    pub(crate) async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        poll_fn(|cx| {
            for socket in &self.sockets {
                if let Poll::Ready(result) = socket.poll_accept(cx) {
                    return Poll::Ready(result);
                }
            }
            Poll::Pending
        })
        .await
    }
}

async fn resolve(bind_address: Option<&str>, port: u16) -> Result<Vec<SocketAddr>> {
    let v4 = |ip: Ipv4Addr| vec![SocketAddr::new(IpAddr::V4(ip), port)];
    let addrs = match bind_address.map(str::trim) {
        None => v4(Ipv4Addr::LOCALHOST),
        Some("" | "*" | "0.0.0.0") => v4(Ipv4Addr::UNSPECIFIED),
        Some(name) if name.eq_ignore_ascii_case("localhost") => vec![
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        ],
        Some(name) => {
            let name = name
                .strip_prefix('[')
                .and_then(|n| n.strip_suffix(']'))
                .unwrap_or(name);
            match name.parse::<IpAddr>() {
                Ok(ip) => vec![SocketAddr::new(ip, port)],
                Err(_) => {
                    let mut found: Vec<SocketAddr> = tokio::net::lookup_host((name, port))
                        .await
                        .map_err(|e| {
                            Error::Other(format!("cannot resolve listen address {name}: {e}"))
                        })?
                        .collect();
                    found.dedup();
                    found
                }
            }
        }
    };
    if addrs.is_empty() {
        return Err(Error::Other(
            "the listen address has no IP addresses".into(),
        ));
    }
    Ok(addrs)
}

/// Explain a failed bind in user terms.
pub(crate) fn bind_error(addr: SocketAddr, e: &io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::AddrInUse => Error::Other(format!(
            "port {} on {} is already in use by another program",
            addr.port(),
            addr.ip()
        )),
        io::ErrorKind::PermissionDenied => {
            let hint = if addr.port() < 1024 {
                " (ports below 1024 need administrator/root privileges)"
            } else {
                ""
            };
            Error::PermissionDenied(format!("not allowed to listen on {addr}{hint}"))
        }
        io::ErrorKind::AddrNotAvailable => Error::Other(format!(
            "cannot listen on {addr}: {} is not an address of this machine",
            addr.ip()
        )),
        _ => Error::Other(format!("cannot listen on {addr}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_bind_addresses() {
        let one = |a: &str| vec![a.parse::<SocketAddr>().unwrap()];
        assert_eq!(resolve(None, 80).await.unwrap(), one("127.0.0.1:80"));
        for any in ["*", "0.0.0.0", " "] {
            assert_eq!(resolve(Some(any), 1).await.unwrap(), one("0.0.0.0:1"));
        }
        assert_eq!(
            resolve(Some("LocalHost"), 2).await.unwrap(),
            [one("127.0.0.1:2"), one("[::1]:2")].concat()
        );
        assert_eq!(resolve(Some("::1"), 3).await.unwrap(), one("[::1]:3"));
        assert_eq!(resolve(Some("[::]"), 3).await.unwrap(), one("[::]:3"));
        assert_eq!(
            resolve(Some("192.168.1.5"), 4).await.unwrap(),
            one("192.168.1.5:4")
        );
    }

    #[tokio::test]
    async fn binds_shares_port_and_reports_conflicts() {
        let listeners = Listeners::bind(Some("localhost"), 0).await.unwrap();
        let ports: Vec<u16> = listeners
            .sockets
            .iter()
            .map(|s| s.local_addr().unwrap().port())
            .collect();
        assert!(ports.iter().all(|p| *p == ports[0] && *p != 0), "{ports:?}");
        let address = listeners.address();
        assert_eq!(address, format!("127.0.0.1:{}", ports[0]));

        let err = Listeners::bind(None, ports[0]).await.unwrap_err();
        assert!(err.to_string().contains("already in use"), "{err}");

        let client = TcpStream::connect(&address).await.unwrap();
        let (_, peer) = listeners.accept().await.unwrap();
        assert_eq!(peer, client.local_addr().unwrap());
    }

    #[test]
    fn bind_error_messages() {
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let e = bind_error(addr, &io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(matches!(e, Error::PermissionDenied(_)));
        assert!(e.to_string().contains("below 1024"), "{e}");
        let e = bind_error(
            "10.9.9.9:8080".parse().unwrap(),
            &io::Error::from(io::ErrorKind::AddrNotAvailable),
        );
        assert!(
            e.to_string().contains("not an address of this machine"),
            "{e}"
        );
    }
}
