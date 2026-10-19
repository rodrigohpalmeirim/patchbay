//! The per-terminal task: feeds input to the backend, coalesces output
//! towards the consumer, escalates closing, and reports the exit once.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;
use uuid::Uuid;

use super::{FLUSH_INTERVAL, MAX_CHUNK, Size};
use crate::model::CoreEvent;

/// Producer events queued towards the driver.
pub(super) const EVENT_QUEUE: usize = 16;
/// Time between asking the shell to end and killing it.
const HANGUP_GRACE: Duration = Duration::from_secs(2);
/// Time after a kill before the terminal is given up on.
const KILL_GRACE: Duration = Duration::from_secs(1);
/// After the shell exited, how long to wait for the rest of its output (a
/// background process may keep a local PTY open indefinitely).
const EXIT_DRAIN: Duration = Duration::from_secs(1);

/// What a backend's producers report. Once all producers are done (their
/// senders dropped) the terminal has ended.
#[derive(Debug)]
pub(super) enum Event {
    Data(Vec<u8>),
    Exit(Exit),
}

/// How a shell ended, as reported in [`CoreEvent::TerminalExit`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Exit {
    pub code: Option<i32>,
    pub reason: Option<String>,
}

#[derive(Debug)]
pub(super) enum Command {
    Write(Vec<u8>),
    Resize(Size),
    Close,
}

/// Control side of a running shell. Methods must not block.
pub(super) trait Backend: Send {
    fn write(&mut self, data: Vec<u8>);
    fn resize(&mut self, size: Size);
    /// Ask the shell to end (SIGHUP, channel close).
    fn hangup(&mut self);
    /// The shell did not end after [`Backend::hangup`]; force it.
    fn kill(&mut self);
    /// The shell has exited; release whatever keeps its output open.
    fn exited(&mut self) {}
}

/// A started shell, ready to be driven.
pub(super) struct Started {
    /// For display, e.g. `"zsh"`.
    pub shell: String,
    pub backend: Box<dyn Backend>,
    pub events: mpsc::Receiver<Event>,
}

/// Exit bookkeeping shared by the driver and the [`TerminalOutput`](super::TerminalOutput).
#[derive(Debug)]
pub(super) struct Shared {
    id: Uuid,
    events: broadcast::Sender<CoreEvent>,
    state: Mutex<ExitState>,
}

#[derive(Debug, Default)]
struct ExitState {
    exit: Option<Exit>,
    output_dropped: bool,
    emitted: bool,
}

impl Shared {
    pub(super) fn new(id: Uuid, events: broadcast::Sender<CoreEvent>) -> Self {
        Self {
            id,
            events,
            state: Mutex::default(),
        }
    }

    fn exited(&self, exit: Exit) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.exit.get_or_insert(exit);
        self.maybe_emit(&mut state);
    }

    pub(super) fn output_dropped(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.output_dropped = true;
        self.maybe_emit(&mut state);
    }

    fn maybe_emit(&self, state: &mut ExitState) {
        if state.emitted || !state.output_dropped {
            return;
        }
        let Some(exit) = state.exit.clone() else {
            return;
        };
        state.emitted = true;
        tracing::debug!(id = %self.id, ?exit, "terminal exited");
        let _ = self.events.send(CoreEvent::TerminalExit {
            terminal_id: self.id,
            exit_code: exit.code,
            reason: exit.reason,
        });
    }
}

/// Runs when the driver ends, however it ends (including being aborted):
/// deregisters the terminal, records the exit and wakes `close` waiters.
pub(super) struct Finish {
    id: Uuid,
    shared: Arc<Shared>,
    done: watch::Sender<bool>,
    deregister: Option<Box<dyn FnOnce() + Send>>,
    exit: Option<Exit>,
}

impl Finish {
    pub(super) fn new(
        id: Uuid,
        shared: Arc<Shared>,
        done: watch::Sender<bool>,
        deregister: Box<dyn FnOnce() + Send>,
    ) -> Self {
        Self {
            id,
            shared,
            done,
            deregister: Some(deregister),
            exit: None,
        }
    }
}

impl Drop for Finish {
    fn drop(&mut self) {
        if let Some(deregister) = self.deregister.take() {
            deregister();
        }
        tracing::trace!(id = %self.id, "terminal task finished");
        self.shared.exited(self.exit.take().unwrap_or_default());
        self.done.send_replace(true);
    }
}

/// Output waiting to be sent, flushed per the policy in the module docs.
#[derive(Default)]
struct Coalescer {
    buf: Vec<u8>,
    due: Option<Instant>,
    last_flush: Option<Instant>,
}

impl Coalescer {
    fn push(&mut self, data: Vec<u8>, now: Instant) {
        if self.buf.is_empty() {
            self.due = Some(match self.last_flush {
                Some(last) => (last + FLUSH_INTERVAL).max(now),
                None => now,
            });
            self.buf = data;
        } else {
            self.buf.extend_from_slice(&data);
        }
    }

    fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn is_full(&self) -> bool {
        self.buf.len() >= MAX_CHUNK
    }

    fn is_due(&self, now: Instant, ended: bool) -> bool {
        !self.buf.is_empty() && (ended || self.is_full() || self.due.is_some_and(|d| d <= now))
    }

    fn take(&mut self, now: Instant) -> Vec<u8> {
        self.last_flush = Some(now);
        self.due = None;
        std::mem::take(&mut self.buf)
    }

    fn clear(&mut self) {
        self.buf = Vec::new();
        self.due = None;
    }
}

fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(86400 * 365)
}

pub(super) async fn run(
    mut backend: Box<dyn Backend>,
    mut events: mpsc::Receiver<Event>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    out: mpsc::Sender<Vec<u8>>,
    mut finish: Finish,
) {
    let mut pending = Coalescer::default();
    // All producers are done (or given up on).
    let mut ended = false;
    let mut output_gone = false;
    let mut commands_open = true;
    // Escalation after a close request: hangup -> kill -> give up.
    let mut escalate_at: Option<Instant> = None;
    let mut killed = false;
    let mut drain_until: Option<Instant> = None;
    let mut give_up = false;

    loop {
        let now = Instant::now();
        if give_up || (ended && (pending.is_empty() || output_gone)) {
            break;
        }
        let mut stop = false;
        tokio::select! {
            biased;

            command = commands.recv(), if commands_open => match command {
                Some(Command::Write(data)) => backend.write(data),
                Some(Command::Resize(size)) => backend.resize(size),
                Some(Command::Close) => stop = true,
                None => {
                    commands_open = false;
                    stop = true;
                }
            },

            permit = out.reserve(), if !output_gone && pending.is_due(now, ended) => match permit {
                Ok(permit) => permit.send(pending.take(now)),
                Err(_) => {
                    output_gone = true;
                    stop = true;
                }
            },

            () = out.closed(), if !output_gone => {
                output_gone = true;
                stop = true;
            }

            mut event = events.recv(), if !ended && !pending.is_full() => loop {
                match event {
                    Some(Event::Data(data)) if !output_gone => pending.push(data, now),
                    Some(Event::Data(_)) => {}
                    Some(Event::Exit(exit)) => {
                        finish.exit.get_or_insert(exit);
                        backend.exited();
                        drain_until = Some(now + EXIT_DRAIN);
                    }
                    None => {
                        ended = true;
                        break;
                    }
                }
                // Take what is already queued without another round through
                // `select!` (PTY reads are often tiny).
                if pending.is_full() {
                    break;
                }
                event = match events.try_recv() {
                    Ok(event) => Some(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => None,
                };
            },

            () = tokio::time::sleep_until(pending.due.unwrap_or_else(far_future)),
                if !pending.is_empty() && !pending.is_due(now, ended) => {}

            () = tokio::time::sleep_until(escalate_at.unwrap_or_else(far_future)),
                if escalate_at.is_some() => {
                if killed {
                    tracing::warn!(id = %finish.id, "giving up on a terminal that does not end");
                    give_up = true;
                } else {
                    tracing::debug!(id = %finish.id, "shell ignored hangup; killing it");
                    backend.kill();
                    killed = true;
                    escalate_at = Some(Instant::now() + KILL_GRACE);
                }
            }

            () = tokio::time::sleep_until(drain_until.unwrap_or_else(far_future)),
                if drain_until.is_some() && !ended => {
                tracing::debug!(id = %finish.id, "output still open after the shell exited");
                ended = true;
            }
        }
        if output_gone {
            pending.clear();
        }
        if stop && escalate_at.is_none() {
            backend.hangup();
            escalate_at = Some(Instant::now() + HANGUP_GRACE);
        }
    }
    drop(backend);
    // Deregister before ending the output, so whoever sees the output end finds the
    // terminal gone (writes fail, it is no longer listed).
    drop(finish);
    drop(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescer_flushes_leading_edge_then_throttles() {
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        assert!(!c.is_due(t0, false));

        c.push(b"a".to_vec(), t0);
        assert!(c.is_due(t0, false), "first output goes out immediately");
        assert_eq!(c.take(t0), b"a");

        let t1 = t0 + Duration::from_millis(1);
        c.push(b"b".to_vec(), t1);
        c.push(b"c".to_vec(), t1);
        assert!(!c.is_due(t1, false), "throttled right after a flush");
        assert!(c.is_due(t1, true), "flushed at the end regardless");
        assert_eq!(c.due, Some(t0 + FLUSH_INTERVAL));
        assert!(c.is_due(t0 + FLUSH_INTERVAL, false));
        assert_eq!(c.take(t0 + FLUSH_INTERVAL), b"bc");

        let t2 = t0 + FLUSH_INTERVAL + Duration::from_millis(1);
        c.push(vec![0; MAX_CHUNK], t2);
        assert!(c.is_full());
        assert!(c.is_due(t2, false), "full chunks are not held back");
        assert_eq!(c.take(t2).len(), MAX_CHUNK);

        let later = t2 + Duration::from_secs(1);
        c.push(b"d".to_vec(), later);
        assert!(c.is_due(later, false), "idle again: immediate");
    }

    #[test]
    fn exit_is_emitted_once_after_both_exit_and_output_drop() {
        let (tx, mut rx) = broadcast::channel(4);
        let id = Uuid::new_v4();

        let shared = Shared::new(id, tx.clone());
        shared.exited(Exit {
            code: Some(3),
            reason: None,
        });
        assert!(rx.try_recv().is_err(), "output still held");
        shared.output_dropped();
        shared.output_dropped();
        shared.exited(Exit::default());
        match rx.try_recv().unwrap() {
            CoreEvent::TerminalExit {
                terminal_id,
                exit_code,
                reason,
            } => {
                assert_eq!(terminal_id, id);
                assert_eq!(exit_code, Some(3));
                assert_eq!(reason, None);
            }
            other => panic!("{other:?}"),
        }
        assert!(rx.try_recv().is_err());

        let shared = Shared::new(id, tx);
        shared.output_dropped();
        assert!(rx.try_recv().is_err(), "still running");
        shared.exited(Exit {
            code: None,
            reason: Some("gone".into()),
        });
        assert!(matches!(
            rx.try_recv().unwrap(),
            CoreEvent::TerminalExit { reason: Some(r), .. } if r == "gone"
        ));
    }
}
