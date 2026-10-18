//! Delivery of terminal output to the webview.
//!
//! Every open terminal gets a forwarding task that moves chunks from the
//! core's [`TerminalOutput`] into the `tauri::ipc::Channel` the frontend
//! passed to `terminal_open`, as raw bytes (`InvokeResponseBody::Raw`, an
//! `ArrayBuffer` in JavaScript; Tauri keeps channel messages in order).
//!
//! - **End of output**: after the last chunk an empty chunk is sent as an
//!   end marker, then the output is released, which lets the core emit
//!   `terminalExit`. Tauri delivers chunks of 1 KiB or more through an
//!   asynchronous fetch, so when the last chunk was that large the release
//!   is delayed briefly to keep the exit event from overtaking it; the end
//!   marker is the exact signal.
//! - **Flow control** (opt-in): once the frontend reports processed bytes
//!   via `terminal_ack` (e.g. from xterm.js `write` callbacks), forwarding
//!   pauses while more than [`HIGH_WATER`] bytes are unacknowledged and
//!   resumes below [`LOW_WATER`]; the core then throttles the shell.
//!   Without acks, output is forwarded as fast as it is produced.
//! - **Dead webviews**: Tauri channels do not report a reloaded page, so
//!   streams are tied to their webview's label and cancelled (and their
//!   terminals closed) when that webview starts loading a page or its window
//!   is destroyed. A failing `send` cancels the stream too. A cancelled
//!   stream drops its output, which closes the terminal in the core.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use patchbay_core::terminal::TerminalOutput;
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::sync::Notify;
use uuid::Uuid;

/// Pause forwarding above this many unacknowledged bytes...
pub const HIGH_WATER: u64 = 1024 * 1024;
/// ...until the backlog is down to this.
pub const LOW_WATER: u64 = 256 * 1024;
/// Raw channel payloads from this size on take Tauri's fetch path.
const FETCH_THRESHOLD: usize = 1024;
/// Head start for a fetch-delivered last chunk before the exit event.
const FETCH_GRACE: Duration = Duration::from_millis(50);

/// Forwarding tasks by terminal id. Cheap to clone.
#[derive(Clone, Default)]
pub struct TerminalStreams {
    streams: Arc<Mutex<HashMap<Uuid, Arc<Stream>>>>,
}

struct Stream {
    webview: String,
    cancelled: AtomicBool,
    sent: AtomicU64,
    acked: AtomicU64,
    /// The frontend acknowledges output (flow control is active).
    acking: AtomicBool,
    wake: Notify,
}

impl Stream {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        loop {
            let woken = self.wake.notified();
            if self.is_cancelled() {
                return;
            }
            woken.await;
        }
    }

    fn backlog(&self) -> u64 {
        self.sent
            .load(Ordering::SeqCst)
            .saturating_sub(self.acked.load(Ordering::SeqCst))
    }

    /// Waits while the frontend is too far behind. False if cancelled.
    async fn window(&self) -> bool {
        if !self.acking.load(Ordering::SeqCst) || self.backlog() <= HIGH_WATER {
            return !self.is_cancelled();
        }
        loop {
            let woken = self.wake.notified();
            if self.is_cancelled() {
                return false;
            }
            if self.backlog() <= LOW_WATER {
                return true;
            }
            woken.await;
        }
    }
}

impl TerminalStreams {
    fn streams(&self) -> MutexGuard<'_, HashMap<Uuid, Arc<Stream>>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forward `output` to `channel`, which belongs to the webview `webview`.
    pub fn start(&self, output: TerminalOutput, channel: Channel, webview: &str) {
        let id = output.id();
        let stream = Arc::new(Stream {
            webview: webview.to_owned(),
            cancelled: AtomicBool::new(false),
            sent: AtomicU64::new(0),
            acked: AtomicU64::new(0),
            acking: AtomicBool::new(false),
            wake: Notify::new(),
        });
        self.streams().insert(id, stream.clone());
        let streams = self.clone();
        tauri::async_runtime::spawn(async move {
            forward(output, channel, &stream).await;
            streams.streams().remove(&id);
        });
    }

    /// The frontend has processed `bytes` more bytes of `id`'s output.
    pub fn ack(&self, id: Uuid, bytes: u64) {
        if let Some(stream) = self.streams().get(&id) {
            stream.acked.fetch_add(bytes, Ordering::SeqCst);
            stream.acking.store(true, Ordering::SeqCst);
            stream.wake.notify_waiters();
        }
    }

    /// Stop forwarding `id`'s output.
    pub fn cancel(&self, id: Uuid) {
        if let Some(stream) = self.streams().get(&id) {
            stream.cancel();
        }
    }

    /// Cancel every stream of `webview`; returns their terminal ids.
    pub fn cancel_webview(&self, webview: &str) -> Vec<Uuid> {
        self.streams()
            .iter()
            .filter(|(_, s)| s.webview == webview)
            .map(|(id, s)| {
                s.cancel();
                *id
            })
            .collect()
    }
}

async fn forward(mut output: TerminalOutput, channel: Channel, stream: &Stream) {
    let id = output.id();
    let mut last_len = 0;
    loop {
        if !stream.window().await {
            return;
        }
        let chunk = tokio::select! {
            chunk = output.recv() => chunk,
            () = stream.cancelled() => return,
        };
        let Some(chunk) = chunk else {
            break;
        };
        last_len = chunk.len();
        stream.sent.fetch_add(chunk.len() as u64, Ordering::SeqCst);
        if let Err(e) = channel.send(InvokeResponseBody::Raw(chunk)) {
            tracing::debug!(%id, "terminal channel is gone: {e}");
            return;
        }
    }
    let _ = channel.send(InvokeResponseBody::Raw(Vec::new()));
    if last_len >= FETCH_THRESHOLD {
        tokio::time::sleep(FETCH_GRACE).await;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use patchbay_core::model::{CoreEvent, TerminalSpec};
    use patchbay_core::session::SessionManager;
    use patchbay_core::terminal::TerminalManager;
    use tokio::sync::{broadcast, mpsc};

    struct Fixture {
        terminals: TerminalManager,
        streams: TerminalStreams,
        events: broadcast::Receiver<CoreEvent>,
    }

    impl Fixture {
        fn new() -> Self {
            let (events_tx, events) = broadcast::channel(64);
            let (prompts, _) = mpsc::channel(1);
            let sessions = SessionManager::new(events_tx.clone(), prompts);
            Self {
                terminals: TerminalManager::new(sessions, events_tx),
                streams: TerminalStreams::default(),
                events,
            }
        }

        /// Opens a local terminal streaming into the returned receiver.
        async fn open(&self) -> (Uuid, mpsc::UnboundedReceiver<InvokeResponseBody>) {
            let (tx, rx) = mpsc::unbounded_channel();
            let channel = Channel::new(move |body| {
                let _ = tx.send(body);
                Ok(())
            });
            let (info, output) = self
                .terminals
                .open(TerminalSpec {
                    host_id: None,
                    cwd: None,
                    cols: 80,
                    rows: 24,
                })
                .await
                .unwrap();
            self.streams.start(output, channel, "main");
            (info.id, rx)
        }

        fn write(&self, id: Uuid, text: &str) {
            self.terminals.write(id, text.as_bytes().to_vec()).unwrap();
        }

        async fn exit(&mut self, id: Uuid) -> Option<i32> {
            loop {
                if let CoreEvent::TerminalExit {
                    terminal_id,
                    exit_code,
                    ..
                } = self.events.recv().await.unwrap()
                    && terminal_id == id
                {
                    return exit_code;
                }
            }
        }
    }

    fn raw(body: InvokeResponseBody) -> Vec<u8> {
        match body {
            InvokeResponseBody::Raw(bytes) => bytes,
            InvokeResponseBody::Json(json) => panic!("JSON body: {json}"),
        }
    }

    async fn timed<T>(fut: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(30), fut)
            .await
            .expect("timed out")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn raw_chunks_then_end_marker_then_exit() {
        let mut fx = Fixture::new();
        let (id, mut rx) = fx.open().await;
        fx.write(id, "echo hi; exit 3\n");
        let mut output = Vec::new();
        timed(async {
            loop {
                let chunk = raw(rx.recv().await.unwrap());
                if chunk.is_empty() {
                    break;
                }
                output.extend(chunk);
            }
        })
        .await;
        assert!(String::from_utf8_lossy(&output).contains("hi\r\n"));
        assert_eq!(timed(fx.exit(id)).await, Some(3));
        assert!(rx.recv().await.is_none(), "channel released after the end");
        assert!(fx.streams.streams().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn acks_pace_the_output() {
        let mut fx = Fixture::new();
        let (id, mut rx) = fx.open().await;
        fx.write(id, "head -c 6000000 /dev/zero | base64; exit 0\n");
        let first = raw(timed(rx.recv()).await.unwrap()).len() as u64;
        fx.streams.ack(id, first);

        // Without further acks forwarding stops at the high-water mark.
        let mut received = first;
        while let Ok(Some(body)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
        {
            received += raw(body).len() as u64;
        }
        let max_chunk = 2 * patchbay_core::terminal::MAX_CHUNK as u64;
        assert!(received > HIGH_WATER, "{received}");
        assert!(received <= first + HIGH_WATER + max_chunk, "{received}");

        // Acknowledging everything lets the rest through.
        let mut total = received;
        timed(async {
            fx.streams.ack(id, received - first);
            loop {
                let chunk = raw(rx.recv().await.unwrap());
                if chunk.is_empty() {
                    break;
                }
                total += chunk.len() as u64;
                fx.streams.ack(id, chunk.len() as u64);
            }
        })
        .await;
        assert!(total > 8_000_000, "{total}");
        assert_eq!(timed(fx.exit(id)).await, Some(0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_a_webview_closes_its_terminals() {
        let mut fx = Fixture::new();
        let (id, mut rx) = fx.open().await;
        fx.write(id, "echo ready\n");
        timed(async {
            let mut seen = Vec::new();
            while !String::from_utf8_lossy(&seen).contains("ready\r\n") {
                seen.extend(raw(rx.recv().await.unwrap()));
            }
        })
        .await;
        assert_eq!(fx.streams.cancel_webview("other"), Vec::<Uuid>::new());
        assert_eq!(fx.streams.cancel_webview("main"), vec![id]);
        timed(fx.exit(id)).await;
        assert!(fx.terminals.list().is_empty());
        // No end marker for a cancelled stream; the channel is just released.
        assert!(timed(rx.recv()).await.is_none());
    }
}
