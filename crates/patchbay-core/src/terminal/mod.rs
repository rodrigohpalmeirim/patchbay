//! Interactive terminals: a shell on a connected host (PTY on an SSH session
//! channel) or on this machine (`portable-pty`), behind one API:
//! [`TerminalManager`].
//!
//! [`TerminalManager::open`] returns the terminal's [`TerminalOutput`], a
//! stream of raw output bytes. Nothing is decoded here: the frontend's
//! terminal emulator deals with UTF-8 and escape sequences, so chunk
//! boundaries may fall anywhere.
//!
//! # Output pipeline
//!
//! The shell's output is read by a backend-specific producer (an SSH channel
//! task, or a blocking reader thread for local PTYs) into a small bounded
//! queue, then coalesced by a per-terminal driver task: a chunk is flushed
//! as soon as it reaches [`MAX_CHUNK`] bytes, or [`FLUSH_INTERVAL`] after the
//! previous flush. Output after a quiet period is flushed immediately, so
//! interactive echo has no added latency while heavy output (`yes`, `cat`)
//! turns into few large chunks.
//!
//! All queues are bounded ([`OUTPUT_QUEUE`] chunks towards the consumer).
//! When the consumer falls behind, the driver stops reading, the producer
//! blocks and the shell is throttled: a local PTY's kernel buffer fills up
//! and the shell blocks on write; for SSH, russh then stops reading the
//! connection (it has no per-channel flow control, so a stalled consumer
//! also stalls other channels of that connection until it catches up).
//!
//! # Exit
//!
//! Every terminal produces exactly one [`CoreEvent::TerminalExit`]. It is
//! sent once the shell has ended *and* the [`TerminalOutput`] has been
//! dropped, so a consumer that reads until `None` and then drops the output
//! has delivered every byte before the event goes out. Dropping the output
//! early closes the terminal.

mod driver;
mod local;
mod remote;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::model::{CoreEvent, TerminalInfo, TerminalSpec};
use crate::session::SessionManager;
use crate::{Error, Result};

use driver::{Command, Finish, Shared};

/// Largest chunk the coalescer aims for (a chunk may exceed it by one read).
pub const MAX_CHUNK: usize = 64 * 1024;
/// Minimum spacing between two output chunks of one terminal.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(5);
/// Chunks queued towards the consumer before the shell is throttled.
pub const OUTPUT_QUEUE: usize = 16;
/// Upper bound for [`TerminalManager::close`] (hangup, then kill).
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Terminal size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Size {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

/// Runs terminals. Cheap to clone; all methods may be called concurrently.
#[derive(Clone)]
pub struct TerminalManager {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: SessionManager,
    events: broadcast::Sender<CoreEvent>,
    terminals: Mutex<HashMap<Uuid, Entry>>,
}

struct Entry {
    info: TerminalInfo,
    commands: mpsc::UnboundedSender<Command>,
    done: watch::Receiver<bool>,
    task: AbortHandle,
}

impl Inner {
    fn terminals(&self) -> MutexGuard<'_, HashMap<Uuid, Entry>> {
        self.terminals.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Output of one terminal, in order. See the [module docs](self) for when
/// the exit event is sent relative to dropping this.
#[derive(Debug)]
pub struct TerminalOutput {
    id: Uuid,
    rx: mpsc::Receiver<Vec<u8>>,
    shared: Arc<Shared>,
}

impl TerminalOutput {
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// The next chunk of output; `None` once the terminal has ended and
    /// everything was received.
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }
}

impl Drop for TerminalOutput {
    fn drop(&mut self) {
        self.rx.close();
        self.shared.output_dropped();
    }
}

impl TerminalManager {
    pub fn new(sessions: SessionManager, events: broadcast::Sender<CoreEvent>) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions,
                events,
                terminals: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Start a shell. Remote terminals need a live connection
    /// ([`Error::NotConnected`] otherwise) with shell access
    /// ([`Error::Unsupported`] on SFTP-only servers). A local `cwd` must be
    /// an existing directory ([`Error::NotFound`]); a remote one that cannot
    /// be entered falls back to the home directory.
    pub async fn open(&self, spec: TerminalSpec) -> Result<(TerminalInfo, TerminalOutput)> {
        let size = Size::new(spec.cols, spec.rows);
        let cwd = spec.cwd.as_deref().filter(|c| !c.is_empty());
        let started = match &spec.host_id {
            Some(host) => remote::start(&self.inner.sessions, host, cwd, size).await?,
            None => local::start(cwd, size).await?,
        };

        let id = Uuid::new_v4();
        let info = TerminalInfo {
            id,
            host_id: spec.host_id,
            shell: started.shell,
        };
        tracing::debug!(%id, host = ?info.host_id, shell = %info.shell, "terminal opened");
        let (out_tx, out_rx) = mpsc::channel(OUTPUT_QUEUE);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (done_tx, done_rx) = watch::channel(false);
        let shared = Arc::new(Shared::new(id, self.inner.events.clone()));
        let finish = Finish::new(
            id,
            shared.clone(),
            done_tx,
            Box::new({
                let inner = Arc::downgrade(&self.inner);
                move || {
                    if let Some(inner) = inner.upgrade() {
                        inner.terminals().remove(&id);
                    }
                }
            }),
        );

        // Hold the lock across spawning, so the task cannot deregister
        // itself before it is registered.
        let mut terminals = self.inner.terminals();
        let task = tokio::spawn(driver::run(
            started.backend,
            started.events,
            cmd_rx,
            out_tx,
            finish,
        ));
        terminals.insert(
            id,
            Entry {
                info: info.clone(),
                commands: cmd_tx,
                done: done_rx,
                task: task.abort_handle(),
            },
        );
        drop(terminals);

        let output = TerminalOutput {
            id,
            rx: out_rx,
            shared,
        };
        Ok((info, output))
    }

    /// Send input (keystrokes, pasted text) to the shell.
    pub fn write(&self, id: Uuid, data: Vec<u8>) -> Result<()> {
        self.command(id, Command::Write(data))
    }

    pub fn resize(&self, id: Uuid, cols: u16, rows: u16) -> Result<()> {
        self.command(id, Command::Resize(Size::new(cols, rows)))
    }

    /// End the shell (hang up, then kill if it does not exit) and wait until
    /// the terminal is gone, at most [`CLOSE_TIMEOUT`]. No-op for unknown or
    /// already finished terminals.
    pub async fn close(&self, id: Uuid) {
        let Some((commands, mut done, task)) = self
            .inner
            .terminals()
            .get(&id)
            .map(|e| (e.commands.clone(), e.done.clone(), e.task.clone()))
        else {
            return;
        };
        let _ = commands.send(Command::Close);
        if tokio::time::timeout(CLOSE_TIMEOUT, done.wait_for(|done| *done))
            .await
            .is_err()
        {
            tracing::warn!(%id, "terminal did not close in time; aborting it");
            task.abort();
        }
    }

    /// Close every terminal (e.g. when the app exits).
    pub async fn close_all(&self) {
        let ids: Vec<Uuid> = self.inner.terminals().keys().copied().collect();
        futures_util::future::join_all(ids.into_iter().map(|id| self.close(id))).await;
    }

    /// Terminals that are still running.
    pub fn list(&self) -> Vec<TerminalInfo> {
        self.inner
            .terminals()
            .values()
            .map(|e| e.info.clone())
            .collect()
    }

    fn command(&self, id: Uuid, command: Command) -> Result<()> {
        self.inner
            .terminals()
            .get(&id)
            .and_then(|e| e.commands.send(command).ok())
            .ok_or_else(|| Error::NotFound(format!("terminal {id}")))
    }
}
