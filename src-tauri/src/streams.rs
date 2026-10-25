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
//! - **Handover** (a tab moving to another window): the source webview
//!   [`pause`](TerminalStreams::pause)s the stream, which returns how many
//!   bytes went into its channel. Chunks are sent under the same lock that
//!   pauses, so nothing reaches the old channel afterwards: once the source
//!   has received that many bytes its screen is complete and can be
//!   serialized. The stream is then [`hand_over`](TerminalStreams::hand_over)ed
//!   to the target webview, which restores the screen and
//!   [`attach`](TerminalStreams::attach)es a new channel; forwarding resumes
//!   there with flow control starting afresh. Meanwhile the output waits in
//!   the core (whose bounded queue throttles the shell), so nothing is lost
//!   or sent twice. Acks are ignored while paused (they are about the old
//!   channel). Until it attaches, the stream belongs to the target: it is
//!   cancelled with the target's window, or by
//!   [`expire`](TerminalStreams::expire) if the target never attaches, but
//!   not when the target's page starts loading (a new window's first page
//!   load would cancel it). A move that fails before the target took over
//!   is undone with [`reclaim`](TerminalStreams::reclaim) and
//!   [`resume`](TerminalStreams::resume).

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
    route: Mutex<Route>,
    cancelled: AtomicBool,
    /// Bytes sent into the current channel (changed under the route lock).
    sent: AtomicU64,
    acked: AtomicU64,
    /// The frontend acknowledges output (flow control is active).
    acking: AtomicBool,
    wake: Notify,
}

/// Where the output goes.
struct Route {
    /// The webview owning the stream (while handed over: the target).
    webview: String,
    channel: Channel,
    /// Nothing is sent while paused (see the module docs).
    paused: bool,
    /// Handed over to `webview`, which has not attached yet; `channel` is
    /// still the source's.
    handed_over: bool,
    /// Counts handovers, so an `expire` only affects its own.
    handovers: u64,
}

impl Stream {
    fn route(&self) -> MutexGuard<'_, Route> {
        self.route.lock().unwrap_or_else(|e| e.into_inner())
    }

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

    /// Output may go out: not paused, and no more than `limit` bytes
    /// unacknowledged (if the frontend acknowledges).
    fn open(&self, limit: u64) -> bool {
        !self.route().paused && (!self.acking.load(Ordering::SeqCst) || self.backlog() <= limit)
    }

    /// Waits while paused or the frontend is too far behind. False if
    /// cancelled.
    async fn window(&self) -> bool {
        let mut limit = HIGH_WATER;
        loop {
            let woken = self.wake.notified();
            if self.is_cancelled() {
                return false;
            }
            if self.open(limit) {
                return true;
            }
            limit = LOW_WATER;
            woken.await;
        }
    }

    /// Sends a chunk (empty: the end marker) into the current channel,
    /// waiting while paused. False if cancelled or the channel is gone.
    async fn deliver(&self, id: Uuid, chunk: Vec<u8>) -> bool {
        loop {
            let woken = self.wake.notified();
            if self.is_cancelled() {
                return false;
            }
            {
                let route = self.route();
                if !route.paused {
                    self.sent.fetch_add(chunk.len() as u64, Ordering::SeqCst);
                    return match route.channel.send(InvokeResponseBody::Raw(chunk)) {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::debug!(%id, "terminal channel is gone: {e}");
                            false
                        }
                    };
                }
            }
            woken.await;
        }
    }
}

impl TerminalStreams {
    fn streams(&self) -> MutexGuard<'_, HashMap<Uuid, Arc<Stream>>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, id: Uuid) -> Option<Arc<Stream>> {
        self.streams().get(&id).cloned()
    }

    /// Forward `output` to `channel`, which belongs to the webview `webview`.
    pub fn start(&self, output: TerminalOutput, channel: Channel, webview: &str) {
        let id = output.id();
        let stream = Arc::new(Stream {
            route: Mutex::new(Route {
                webview: webview.to_owned(),
                channel,
                paused: false,
                handed_over: false,
                handovers: 0,
            }),
            cancelled: AtomicBool::new(false),
            sent: AtomicU64::new(0),
            acked: AtomicU64::new(0),
            acking: AtomicBool::new(false),
            wake: Notify::new(),
        });
        self.streams().insert(id, stream.clone());
        let streams = self.clone();
        tauri::async_runtime::spawn(async move {
            forward(output, &stream).await;
            streams.streams().remove(&id);
        });
    }

    /// The frontend has processed `bytes` more bytes of `id`'s output.
    pub fn ack(&self, id: Uuid, bytes: u64) {
        if let Some(stream) = self.get(id) {
            if stream.route().paused {
                return;
            }
            stream.acked.fetch_add(bytes, Ordering::SeqCst);
            stream.acking.store(true, Ordering::SeqCst);
            stream.wake.notify_waiters();
        }
    }

    /// Stop forwarding `id`'s output.
    pub fn cancel(&self, id: Uuid) {
        if let Some(stream) = self.get(id) {
            stream.cancel();
        }
    }

    /// Whether `webview` has terminals open (or being handed to it).
    pub fn has_webview(&self, webview: &str) -> bool {
        self.streams()
            .values()
            .any(|s| s.route().webview == webview)
    }

    /// Cancel every stream of `webview`; returns their terminal ids. With
    /// `handovers` false, streams handed to it that it has not attached yet
    /// are left alone (its page is loading: it may still take them).
    pub fn cancel_webview(&self, webview: &str, handovers: bool) -> Vec<Uuid> {
        self.streams()
            .iter()
            .filter(|(_, s)| {
                let route = s.route();
                route.webview == webview && (handovers || !route.handed_over)
            })
            .map(|(id, s)| {
                s.cancel();
                *id
            })
            .collect()
    }

    /// Stops sending `id`'s output to `webview`'s channel; returns how many
    /// bytes were sent into it, or `None` if `webview` does not have it.
    pub fn pause(&self, id: Uuid, webview: &str) -> Option<u64> {
        let stream = self.get(id).filter(|s| !s.is_cancelled())?;
        let mut route = stream.route();
        if route.webview != webview || route.handed_over {
            return None;
        }
        route.paused = true;
        Some(stream.sent.load(Ordering::SeqCst))
    }

    /// Undoes [`pause`](Self::pause): `webview` keeps the stream.
    pub fn resume(&self, id: Uuid, webview: &str) -> bool {
        let Some(stream) = self.get(id) else {
            return false;
        };
        let mut route = stream.route();
        if route.webview != webview || route.handed_over {
            return false;
        }
        route.paused = false;
        drop(route);
        stream.wake.notify_waiters();
        true
    }

    /// Whether `webview` has paused `id` (it may be handed over).
    pub fn is_paused(&self, id: Uuid, webview: &str) -> bool {
        self.get(id).is_some_and(|s| {
            let route = s.route();
            !s.is_cancelled() && route.webview == webview && route.paused && !route.handed_over
        })
    }

    /// Hands a stream `from` paused to `to`, which takes it with
    /// [`attach`](Self::attach). Returns the handover's number for
    /// [`expire`](Self::expire), or `None` if `from` has not paused it.
    pub fn hand_over(&self, id: Uuid, from: &str, to: &str) -> Option<u64> {
        let stream = self.get(id).filter(|s| !s.is_cancelled())?;
        let mut route = stream.route();
        if route.webview != from || !route.paused || route.handed_over {
            return None;
        }
        route.webview = to.to_owned();
        route.handed_over = true;
        route.handovers += 1;
        Some(route.handovers)
    }

    /// Takes back a stream handed `from` to `to` that `to` has not attached:
    /// it is paused for `from` again.
    pub fn reclaim(&self, id: Uuid, from: &str, to: &str) -> bool {
        let Some(stream) = self.get(id) else {
            return false;
        };
        let mut route = stream.route();
        if route.webview != to || !route.handed_over {
            return false;
        }
        route.webview = from.to_owned();
        route.handed_over = false;
        true
    }

    /// `webview` takes over a stream handed to it: output resumes into
    /// `channel`. False if it was not handed to `webview` (or is gone).
    pub fn attach(&self, id: Uuid, webview: &str, channel: Channel) -> bool {
        let Some(stream) = self.get(id).filter(|s| !s.is_cancelled()) else {
            return false;
        };
        let mut route = stream.route();
        if route.webview != webview || !route.handed_over {
            return false;
        }
        route.channel = channel;
        route.handed_over = false;
        route.paused = false;
        stream.sent.store(0, Ordering::SeqCst);
        stream.acked.store(0, Ordering::SeqCst);
        stream.acking.store(false, Ordering::SeqCst);
        drop(route);
        stream.wake.notify_waiters();
        true
    }

    /// Cancels handover number `handover` of `id` if the target still has
    /// not attached. Returns true if it did (the terminal should be closed).
    pub fn expire(&self, id: Uuid, handover: u64) -> bool {
        let Some(stream) = self.get(id) else {
            return false;
        };
        let route = stream.route();
        if !route.handed_over || route.handovers != handover {
            return false;
        }
        drop(route);
        stream.cancel();
        true
    }
}

async fn forward(mut output: TerminalOutput, stream: &Stream) {
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
        if !stream.deliver(id, chunk).await {
            return;
        }
    }
    if !stream.deliver(id, Vec::new()).await {
        return;
    }
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
            let (channel, rx) = channel();
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

    /// A channel delivering into the returned receiver.
    fn channel() -> (Channel, mpsc::UnboundedReceiver<InvokeResponseBody>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let channel = Channel::new(move |body| {
            let _ = tx.send(body);
            Ok(())
        });
        (channel, rx)
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

        // Without further acks forwarding stops at the high-water mark. The
        // first chunk may be just the prompt, so wait longer for the data to
        // start (slow shell startup on CI runners).
        let mut received = first;
        loop {
            let wait = if received > HIGH_WATER { 500 } else { 10_000 };
            match tokio::time::timeout(Duration::from_millis(wait), rx.recv()).await {
                Ok(Some(body)) => received += raw(body).len() as u64,
                _ => break,
            }
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
        assert_eq!(fx.streams.cancel_webview("other", true), Vec::<Uuid>::new());
        assert_eq!(fx.streams.cancel_webview("main", false), vec![id]);
        timed(fx.exit(id)).await;
        assert!(fx.terminals.list().is_empty());
        // No end marker for a cancelled stream; the channel is just released.
        assert!(timed(rx.recv()).await.is_none());
    }

    /// Receives until `pred` holds for everything received so far.
    async fn receive_until(
        rx: &mut mpsc::UnboundedReceiver<InvokeResponseBody>,
        seen: &mut Vec<u8>,
        pred: impl Fn(&[u8]) -> bool,
    ) {
        timed(async {
            while !pred(seen) {
                seen.extend(raw(rx.recv().await.unwrap()));
            }
        })
        .await;
    }

    /// Nothing arrives for a while.
    async fn quiet(rx: &mut mpsc::UnboundedReceiver<InvokeResponseBody>) -> bool {
        tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err()
    }

    /// The numbers printed one per line by `seq`, in order.
    fn numbers(output: &[u8]) -> Vec<u64> {
        String::from_utf8_lossy(output)
            .split("\r\n")
            // (The first may follow terminal modes the shell resets.)
            .filter_map(|l| l.rsplit('\r').next()?.parse().ok())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handover_moves_the_output_to_another_webview() {
        const LINES: u64 = 400_000;
        let mut fx = Fixture::new();
        let (id, mut a) = fx.open().await;
        fx.write(id, &format!("seq 1 {LINES}; exit 0\n"));

        // The source gets some of it, then pauses: what it has received by
        // then is exactly what was sent, and nothing more arrives.
        let mut source = Vec::new();
        receive_until(&mut a, &mut source, |s| numbers(s).len() > 1000).await;
        let sent = fx.streams.pause(id, "main").unwrap();
        while let Ok(body) = a.try_recv() {
            source.extend(raw(body));
        }
        assert_eq!(source.len() as u64, sent);
        assert!(quiet(&mut a).await, "output after pausing");
        assert!(fx.streams.pause(id, "other").is_none());
        assert!(fx.streams.is_paused(id, "main"));

        // Only the webview it was handed to may attach.
        let handover = fx.streams.hand_over(id, "main", "target").unwrap();
        assert!(!fx.streams.is_paused(id, "main"));
        let (wrong, _) = channel();
        assert!(!fx.streams.attach(id, "main", wrong));
        // The target's page loading doesn't cancel it.
        assert!(fx.streams.cancel_webview("target", false).is_empty());
        let (b_channel, mut b) = channel();
        assert!(fx.streams.attach(id, "target", b_channel));
        assert!(!fx.streams.expire(id, handover), "expired after attaching");

        // Flow control starts afresh on the new channel: it stops at the
        // high-water mark until acknowledged.
        let first = raw(timed(b.recv()).await.unwrap());
        fx.streams.ack(id, first.len() as u64);
        let mut target = first;
        while let Ok(Some(body)) = tokio::time::timeout(Duration::from_millis(500), b.recv()).await
        {
            target.extend(raw(body));
        }
        let max_chunk = 2 * patchbay_core::terminal::MAX_CHUNK as u64;
        assert!(target.len() as u64 <= HIGH_WATER + max_chunk + max_chunk);
        fx.streams.ack(id, target.len() as u64);
        timed(async {
            loop {
                let chunk = raw(b.recv().await.unwrap());
                if chunk.is_empty() {
                    break;
                }
                fx.streams.ack(id, chunk.len() as u64);
                target.extend(chunk);
            }
        })
        .await;
        assert_eq!(timed(fx.exit(id)).await, Some(0));

        // Every line exactly once, across both.
        source.extend(target);
        let got = numbers(&source);
        let expected: Vec<u64> = (1..=LINES).collect();
        assert!(
            got == expected,
            "{} lines, first difference at {:?}",
            got.len(),
            got.iter().zip(&expected).position(|(a, b)| a != b)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_handovers_are_undone_or_expire() {
        let mut fx = Fixture::new();
        let (id, mut a) = fx.open().await;
        fx.write(id, "echo one\n");
        let mut seen = Vec::new();
        receive_until(&mut a, &mut seen, |s| {
            String::from_utf8_lossy(s).contains("one\r\n")
        })
        .await;

        // Taken back before the target attached: the source resumes.
        fx.streams.pause(id, "main").unwrap();
        fx.streams.hand_over(id, "main", "target").unwrap();
        assert!(!fx.streams.resume(id, "main"));
        assert!(fx.streams.reclaim(id, "main", "target"));
        assert!(fx.streams.resume(id, "main"));
        fx.write(id, "echo two\n");
        receive_until(&mut a, &mut seen, |s| {
            String::from_utf8_lossy(s).contains("two\r\n")
        })
        .await;

        // The target never attaches: the handover expires and the terminal
        // closes.
        fx.streams.pause(id, "main").unwrap();
        let handover = fx.streams.hand_over(id, "main", "target").unwrap();
        assert!(!fx.streams.expire(id, handover + 1));
        assert!(fx.streams.has_webview("target"));
        assert!(fx.streams.expire(id, handover));
        timed(fx.exit(id)).await;
        assert!(fx.terminals.list().is_empty());

        // Closing the target window cancels what was handed to it.
        let (id, mut a) = fx.open().await;
        fx.write(id, "echo ready\n");
        let mut seen = Vec::new();
        receive_until(&mut a, &mut seen, |s| {
            String::from_utf8_lossy(s).contains("ready\r\n")
        })
        .await;
        fx.streams.pause(id, "main").unwrap();
        fx.streams.hand_over(id, "main", "target").unwrap();
        assert!(fx.streams.cancel_webview("main", true).is_empty());
        assert_eq!(fx.streams.cancel_webview("target", true), vec![id]);
        timed(fx.exit(id)).await;
    }
}
