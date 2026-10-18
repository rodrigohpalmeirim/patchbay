//! Remote port forwards (`tcpip-forward`): the server listens and opens a
//! `forwarded-tcpip` channel back to us for every connection it accepts.
//!
//! The russh handler receives those channel requests; [`ForwardRoutes`]
//! hands each one to the [`RemoteListener`] that asked for the forward, or
//! rejects it when nobody did.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use russh::ChannelOpenFailure;
use russh::client::{ChannelOpenHandle, Msg};
use russh::{Channel, ChannelId};
use tokio::sync::mpsc;

/// Connections queued per listener before further ones are rejected.
const QUEUE: usize = 64;

/// A connection the server accepted on a remote-forwarded port. Pending
/// (the remote peer waits) until [`accept`](Self::accept)ed or
/// [`reject`](Self::reject)ed; dropping it rejects.
pub struct ForwardedTcpip {
    channel: Channel<Msg>,
    reply: ChannelOpenHandle,
    originator_address: String,
    originator_port: u32,
}

impl ForwardedTcpip {
    /// Where the remote peer connected from, as reported by the server.
    pub fn originator(&self) -> (&str, u32) {
        (&self.originator_address, self.originator_port)
    }

    pub fn id(&self) -> ChannelId {
        self.channel.id()
    }

    /// Confirm the channel and start exchanging data.
    pub async fn accept(self) -> Channel<Msg> {
        self.reply.accept().await;
        self.channel
    }

    pub async fn reject(self, reason: ChannelOpenFailure) {
        self.reply.reject(reason).await;
    }
}

struct Route {
    id: u64,
    address: String,
    /// `0` until the server has told us which port it picked.
    port: u32,
    tx: mpsc::Sender<ForwardedTcpip>,
}

/// The remote forwards requested on one connection.
#[derive(Default)]
pub(crate) struct ForwardRoutes {
    routes: Mutex<Vec<Route>>,
    next_id: AtomicU64,
}

impl ForwardRoutes {
    fn routes(&self) -> std::sync::MutexGuard<'_, Vec<Route>> {
        self.routes.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn add(&self, address: &str, port: u32, tx: mpsc::Sender<ForwardedTcpip>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.routes().push(Route {
            id,
            address: address.to_owned(),
            port,
            tx,
        });
        id
    }

    fn set_port(&self, id: u64, port: u32) {
        if let Some(route) = self.routes().iter_mut().find(|r| r.id == id) {
            route.port = port;
        }
    }

    fn remove(&self, id: u64) {
        self.routes().retain(|r| r.id != id);
    }

    /// The listener for a connection the server reports as arriving on
    /// `address:port`. Servers echo the address as it was requested, but to
    /// be lenient a port that only one listener uses matches regardless of
    /// the address.
    fn find(&self, address: &str, port: u32) -> Option<mpsc::Sender<ForwardedTcpip>> {
        let routes = self.routes();
        if let Some(r) = routes
            .iter()
            .find(|r| r.port == port && r.address.eq_ignore_ascii_case(address))
        {
            return Some(r.tx.clone());
        }
        let mut by_port = routes.iter().filter(|r| r.port == port);
        match (by_port.next(), by_port.next()) {
            (Some(r), None) => Some(r.tx.clone()),
            _ => routes
                .iter()
                .find(|r| r.port == 0 && r.address.eq_ignore_ascii_case(address))
                .map(|r| r.tx.clone()),
        }
    }

    /// Called by the russh handler for every `forwarded-tcpip` request.
    pub(crate) fn dispatch(
        &self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        connected: (&str, u32),
        originator: (&str, u32),
    ) {
        let Some(tx) = self.find(connected.0, connected.1) else {
            tracing::debug!(
                address = connected.0,
                port = connected.1,
                "rejecting forwarded connection for a port we did not ask for"
            );
            // Dropping the reply handle rejects the channel.
            return;
        };
        let conn = ForwardedTcpip {
            channel,
            reply,
            originator_address: originator.0.to_owned(),
            originator_port: originator.1,
        };
        if tx.try_send(conn).is_err() {
            tracing::debug!(
                address = connected.0,
                port = connected.1,
                "rejecting forwarded connection: listener busy or gone"
            );
        }
    }
}

/// A port the server listens on for us. Incoming connections are delivered
/// by [`accept`](Self::accept). Dropping this stops routing (the server-side
/// listener stays until cancelled with
/// [`Connection::cancel_remote`](super::Connection::cancel_remote) or the
/// connection ends).
pub struct RemoteListener {
    routes: Arc<ForwardRoutes>,
    route: u64,
    address: String,
    port: u16,
    rx: mpsc::Receiver<ForwardedTcpip>,
}

impl RemoteListener {
    pub(crate) fn register(routes: Arc<ForwardRoutes>, address: &str, port: u16) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        let route = routes.add(address, port.into(), tx);
        Self {
            routes,
            route,
            address: address.to_owned(),
            port,
            rx,
        }
    }

    pub(crate) fn set_port(&mut self, port: u16) {
        self.port = port;
        self.routes.set_port(self.route, port.into());
    }

    /// The listen address as requested.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The next connection; `None` once the connection is gone.
    pub async fn accept(&mut self) -> Option<ForwardedTcpip> {
        self.rx.recv().await
    }
}

impl Drop for RemoteListener {
    fn drop(&mut self) {
        self.routes.remove(self.route);
    }
}

impl std::fmt::Debug for RemoteListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteListener")
            .field("address", &self.address)
            .field("port", &self.port)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_matching() {
        let routes = ForwardRoutes::default();
        let (a, _ra) = mpsc::channel(1);
        let (b, _rb) = mpsc::channel(1);
        let (c, _rc) = mpsc::channel(1);
        let ia = routes.add("localhost", 9000, a.clone());
        routes.add("0.0.0.0", 9001, b.clone());
        routes.add("localhost", 9001, c.clone());

        let same = |x: Option<mpsc::Sender<ForwardedTcpip>>, y: &mpsc::Sender<ForwardedTcpip>| {
            x.is_some_and(|x| x.same_channel(y))
        };
        assert!(same(routes.find("LOCALHOST", 9000), &a));
        assert!(same(routes.find("127.0.0.1", 9000), &a), "unique port");
        assert!(same(routes.find("0.0.0.0", 9001), &b));
        assert!(same(routes.find("localhost", 9001), &c));
        assert!(routes.find("::1", 9001).is_none(), "ambiguous port");
        assert!(routes.find("localhost", 9002).is_none());

        let (d, _rd) = mpsc::channel(1);
        let id = routes.add("localhost", 0, d.clone());
        assert!(same(routes.find("localhost", 0), &d));
        routes.set_port(id, 41000);
        assert!(same(routes.find("localhost", 41000), &d));

        routes.remove(ia);
        assert!(routes.find("localhost", 9000).is_none());
    }
}
