//! The server side of SOCKS for dynamic forwards: SOCKS5 (RFC 1928, no
//! authentication, `CONNECT` only, IPv4 / IPv6 / domain names) and SOCKS4 /
//! SOCKS4a (`CONNECT` only, user id ignored).
//!
//! [`handshake`] reads a request; the caller opens the tunnel and then
//! answers with [`reply`].

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Longest SOCKS4 user id / SOCKS4a host name we read.
const MAX_FIELD: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Version {
    V4,
    V5,
}

/// A `CONNECT` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    pub version: Version,
    /// IP literal (IPv6 without brackets) or domain name.
    pub host: String,
    pub port: u16,
}

/// Outcome sent back to the client; SOCKS5 reply codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reply {
    Succeeded = 0x00,
    GeneralFailure = 0x01,
    NotAllowed = 0x02,
    HostUnreachable = 0x04,
    ConnectionRefused = 0x05,
    CommandNotSupported = 0x07,
    AddressTypeNotSupported = 0x08,
}

fn protocol_error(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("SOCKS: {msg}"))
}

/// Read a client's greeting and request. Requests we cannot serve are
/// answered (with the matching error reply) and reported as errors.
pub(crate) async fn handshake<S>(stream: &mut S) -> io::Result<Request>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match stream.read_u8().await? {
        5 => handshake5(stream).await,
        4 => handshake4(stream).await,
        v => Err(protocol_error(&format!("unsupported version {v}"))),
    }
}

async fn handshake5<S>(stream: &mut S) -> io::Result<Request>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let count = stream.read_u8().await?;
    let mut methods = vec![0; count.into()];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[5, 0xff]).await?;
        return Err(protocol_error("client requires authentication"));
    }
    stream.write_all(&[5, 0]).await?;

    let mut head = [0; 4];
    stream.read_exact(&mut head).await?;
    let [version, command, _reserved, atyp] = head;
    if version != 5 {
        return Err(protocol_error("bad request version"));
    }
    let host = match atyp {
        1 => {
            let mut ip = [0; 4];
            stream.read_exact(&mut ip).await?;
            Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let len = stream.read_u8().await?;
            let mut name = vec![0; len.into()];
            stream.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| protocol_error("host name is not UTF-8"))?
        }
        4 => {
            let mut ip = [0; 16];
            stream.read_exact(&mut ip).await?;
            Ipv6Addr::from(ip).to_string()
        }
        _ => {
            reply(stream, Version::V5, Reply::AddressTypeNotSupported).await?;
            return Err(protocol_error("unsupported address type"));
        }
    };
    let port = stream.read_u16().await?;
    if command != 1 {
        reply(stream, Version::V5, Reply::CommandNotSupported).await?;
        return Err(protocol_error("only CONNECT is supported"));
    }
    if host.is_empty() {
        reply(stream, Version::V5, Reply::HostUnreachable).await?;
        return Err(protocol_error("empty host name"));
    }
    Ok(Request {
        version: Version::V5,
        host,
        port,
    })
}

async fn handshake4<S>(stream: &mut S) -> io::Result<Request>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let command = stream.read_u8().await?;
    let port = stream.read_u16().await?;
    let mut ip = [0; 4];
    stream.read_exact(&mut ip).await?;
    read_cstr(stream).await?; // user id
    // SOCKS4a: 0.0.0.x (x != 0) means "a host name follows".
    let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
        let name = read_cstr(stream).await?;
        String::from_utf8(name).map_err(|_| protocol_error("host name is not UTF-8"))?
    } else {
        Ipv4Addr::from(ip).to_string()
    };
    if command != 1 {
        reply(stream, Version::V4, Reply::CommandNotSupported).await?;
        return Err(protocol_error("only CONNECT is supported"));
    }
    Ok(Request {
        version: Version::V4,
        host,
        port,
    })
}

async fn read_cstr<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match stream.read_u8().await? {
            0 => return Ok(out),
            _ if out.len() >= MAX_FIELD => return Err(protocol_error("field too long")),
            b => out.push(b),
        }
    }
}

/// Answer a request. The bound address is always reported as `0.0.0.0:0`
/// (clients do not use it for `CONNECT`).
pub(crate) async fn reply<S>(stream: &mut S, version: Version, reply: Reply) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    match version {
        Version::V5 => {
            stream
                .write_all(&[5, reply as u8, 0, 1, 0, 0, 0, 0, 0, 0])
                .await?
        }
        Version::V4 => {
            let code = if reply == Reply::Succeeded {
                0x5a
            } else {
                0x5b
            };
            stream.write_all(&[0, code, 0, 0, 0, 0, 0, 0]).await?
        }
    }
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// Run a handshake against `client_bytes`; returns the result and what
    /// the server wrote back.
    async fn run(client_bytes: &[u8]) -> (io::Result<Request>, Vec<u8>) {
        let (mut client, mut server) = duplex(1024);
        client.write_all(client_bytes).await.unwrap();
        client.shutdown().await.unwrap();
        let result = handshake(&mut server).await;
        drop(server);
        let mut written = Vec::new();
        client.read_to_end(&mut written).await.unwrap();
        (result, written)
    }

    fn req(version: Version, host: &str, port: u16) -> Request {
        Request {
            version,
            host: host.into(),
            port,
        }
    }

    #[tokio::test]
    async fn socks5_connect() {
        let (r, out) = run(&[5, 2, 2, 0, 5, 1, 0, 1, 10, 0, 0, 1, 0, 80]).await;
        assert_eq!(r.unwrap(), req(Version::V5, "10.0.0.1", 80));
        assert_eq!(out, [5, 0]);

        let mut domain = vec![5, 1, 0, 5, 1, 0, 3, 9];
        domain.extend_from_slice(b"localhost");
        domain.extend_from_slice(&22u16.to_be_bytes());
        let (r, _) = run(&domain).await;
        assert_eq!(r.unwrap(), req(Version::V5, "localhost", 22));

        let mut v6 = vec![5, 1, 0, 5, 1, 0, 4];
        v6.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        v6.extend_from_slice(&443u16.to_be_bytes());
        let (r, _) = run(&v6).await;
        assert_eq!(r.unwrap(), req(Version::V5, "::1", 443));
    }

    #[tokio::test]
    async fn socks5_errors() {
        let (r, out) = run(&[5, 1, 2]).await;
        assert!(r.is_err());
        assert_eq!(out, [5, 0xff], "no acceptable method");

        let (r, out) = run(&[5, 1, 0, 5, 2, 0, 1, 1, 2, 3, 4, 0, 80]).await;
        assert!(r.is_err());
        assert_eq!(out, [5, 0, 5, 7, 0, 1, 0, 0, 0, 0, 0, 0], "BIND refused");

        let (r, out) = run(&[5, 1, 0, 5, 1, 0, 9]).await;
        assert!(r.is_err());
        assert_eq!(out[2..4], [5, 8], "bad address type");

        let (r, _) = run(&[5, 1, 0, 5, 1, 0, 1, 1, 2]).await;
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);

        let (r, _) = run(&[6]).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn socks4_and_4a() {
        let (r, out) = run(&[4, 1, 0, 80, 192, 168, 0, 1, b'm', b'e', 0]).await;
        assert_eq!(r.unwrap(), req(Version::V4, "192.168.0.1", 80));
        assert!(out.is_empty());

        let mut a = vec![4, 1, 0x1f, 0x90, 0, 0, 0, 1, 0];
        a.extend_from_slice(b"example.org\0");
        let (r, _) = run(&a).await;
        assert_eq!(r.unwrap(), req(Version::V4, "example.org", 8080));

        let (r, out) = run(&[4, 2, 0, 80, 1, 2, 3, 4, 0]).await;
        assert!(r.is_err());
        assert_eq!(out, [0, 0x5b, 0, 0, 0, 0, 0, 0]);

        let mut long = vec![4, 1, 0, 80, 1, 2, 3, 4];
        long.extend(std::iter::repeat_n(b'x', 300));
        let (r, _) = run(&long).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn replies() {
        let mut out = Vec::new();
        reply(&mut out, Version::V5, Reply::Succeeded)
            .await
            .unwrap();
        reply(&mut out, Version::V5, Reply::ConnectionRefused)
            .await
            .unwrap();
        reply(&mut out, Version::V4, Reply::Succeeded)
            .await
            .unwrap();
        assert_eq!(
            out,
            [
                5, 0, 0, 1, 0, 0, 0, 0, 0, 0, 5, 5, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0x5a, 0, 0, 0, 0, 0,
                0
            ]
        );
    }
}
