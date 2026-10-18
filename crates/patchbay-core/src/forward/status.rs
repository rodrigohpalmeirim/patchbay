//! Runtime status of one forward and its throttled publication as
//! [`CoreEvent::PortForward`].

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, broadcast, watch};

use crate::Error;
use crate::model::{CoreEvent, PortForwardState, PortForwardStatus};
use crate::session::duplicate_error;

/// Minimum spacing of traffic/connection-count updates per forward.
pub const TRAFFIC_INTERVAL: Duration = Duration::from_secs(1);

/// Counters and state of one forward. State changes are published right
/// away; traffic and connection counts at most every [`TRAFFIC_INTERVAL`]
/// (see [`StatusCell::publish_traffic`]).
pub(crate) struct StatusCell {
    id: String,
    events: broadcast::Sender<CoreEvent>,
    state: watch::Sender<PortForwardState>,
    open: AtomicU32,
    total: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    traffic: Notify,
    /// The error behind a `Failed` state.
    failure: Mutex<Option<Error>>,
}

impl StatusCell {
    pub(crate) fn new(
        id: String,
        events: broadcast::Sender<CoreEvent>,
        state: PortForwardState,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            events,
            state: watch::Sender::new(state),
            open: AtomicU32::new(0),
            total: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            traffic: Notify::new(),
            failure: Mutex::new(None),
        })
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn snapshot(&self) -> PortForwardStatus {
        PortForwardStatus {
            id: self.id.clone(),
            state: self.state.borrow().clone(),
            open_connections: self.open.load(Ordering::Relaxed),
            total_connections: self.total.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<PortForwardState> {
        self.state.subscribe()
    }

    /// Change the state and publish it (no-op if unchanged).
    pub(crate) fn set_state(&self, state: PortForwardState) {
        if self.state.send_if_modified(|s| {
            let changed = *s != state;
            *s = state;
            changed
        }) {
            self.publish();
        }
    }

    /// Enter `Failed` with `error`'s message.
    pub(crate) fn fail(&self, error: Error) {
        let message = error.to_string();
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
        self.set_state(PortForwardState::Failed { message });
    }

    /// The error behind the current `Failed` state.
    pub(crate) fn failure(&self) -> Option<Error> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(duplicate_error)
    }

    pub(crate) fn publish(&self) {
        let _ = self.events.send(CoreEvent::PortForward(self.snapshot()));
    }

    /// Counts a tunneled connection until the guard is dropped.
    pub(crate) fn connection(self: &Arc<Self>) -> ConnectionGuard {
        self.open.fetch_add(1, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);
        self.traffic.notify_one();
        ConnectionGuard(self.clone())
    }

    /// Bytes from the connecting client towards the destination.
    pub(crate) fn add_out(&self, n: usize) {
        self.bytes_out.fetch_add(n as u64, Ordering::Relaxed);
        self.traffic.notify_one();
    }

    /// Bytes from the destination back to the client.
    pub(crate) fn add_in(&self, n: usize) {
        self.bytes_in.fetch_add(n as u64, Ordering::Relaxed);
        self.traffic.notify_one();
    }

    /// Publishes counter changes, at most once per [`TRAFFIC_INTERVAL`].
    /// Runs until aborted; idle while nothing changes.
    pub(crate) async fn publish_traffic(self: Arc<Self>) {
        loop {
            self.traffic.notified().await;
            self.publish();
            tokio::time::sleep(TRAFFIC_INTERVAL).await;
        }
    }
}

pub(crate) struct ConnectionGuard(Arc<StatusCell>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::Relaxed);
        self.0.traffic.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn traffic_is_throttled_and_states_are_immediate() {
        let (tx, mut rx) = broadcast::channel(64);
        let cell = StatusCell::new("fwd:x".into(), tx, PortForwardState::Stopped);
        let publisher = tokio::spawn(cell.clone().publish_traffic());

        cell.set_state(PortForwardState::Starting);
        cell.set_state(PortForwardState::Starting);
        let CoreEvent::PortForward(s) = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(s.state, PortForwardState::Starting);
        assert!(rx.try_recv().is_err(), "unchanged state is not republished");

        let guard = cell.connection();
        for _ in 0..100 {
            cell.add_in(10);
            cell.add_out(1);
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let updates: Vec<PortForwardStatus> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|e| match e {
                CoreEvent::PortForward(s) => s,
                _ => panic!(),
            })
            .collect();
        assert!(
            (1..=3).contains(&updates.len()),
            "{} updates",
            updates.len()
        );
        let last = updates.last().unwrap();
        assert_eq!((last.bytes_in, last.bytes_out), (1000, 100));
        assert_eq!((last.open_connections, last.total_connections), (1, 1));

        drop(guard);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let CoreEvent::PortForward(s) = rx.try_recv().unwrap() else {
            panic!()
        };
        assert_eq!(s.open_connections, 0);
        publisher.abort();
    }
}
