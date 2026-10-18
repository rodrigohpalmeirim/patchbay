//! SSH connections: establishing them (TCP or ProxyJump chains, host key
//! verification, authentication), sharing one connection per host, and
//! reporting their state.
//!
//! [`SessionManager`] owns the connections and publishes
//! [`CoreEvent::Connection`] events. Interactive questions (unknown host key,
//! passwords, passphrases, keyboard-interactive) are sent to the frontend as
//! [`PromptRequest`](crate::model::PromptRequest)s on the [`PromptSender`].
//!
//! Every connection attempt runs in its own task, so dropping the future
//! returned by [`SessionManager::connect`] does not abort it for other callers
//! waiting on the same host; use [`SessionManager::disconnect`] to abort.
//!
//! Lost connections are removed from the manager and reported as
//! `Disconnected { reason }`; nothing reconnects on its own.
//!
//! # Reconnecting
//!
//! Features that should survive a lost connection (port forwards) call
//! [`SessionManager::reconnect`]. It runs one retry loop per host, shared by
//! all callers, reported as `Reconnecting { attempt }` between attempts:
//!
//! - Attempts are spaced by [`Backoff::DEFAULT`] (1 s, 2 s, 4 s, ... capped at
//!   60 s, with jitter) and continue indefinitely while errors look
//!   transient (network errors, timeouts, refused connections).
//! - Prompts are allowed during at most one attempt per loop: once the user
//!   has been asked something (password, passphrase, host key), later
//!   attempts run unattended and give up with an error instead of asking
//!   again. Stored passwords/passphrases, agent keys and unencrypted keys
//!   work unattended.
//! - The loop ends with an error, reported as `Disconnected { reason }`, when
//!   the user cancels or rejects a prompt, authentication or host key
//!   verification fails, or an unattended attempt would need a prompt.
//! - [`SessionManager::connect`] during a loop retries right away (with
//!   prompts allowed again); [`SessionManager::disconnect`] stops the loop.

mod agent;
mod auth;
mod connection;
mod handler;
mod prompt;
mod remote_forward;

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use russh::Preferred;
use russh::client;
use russh::keys::Algorithm;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, broadcast, oneshot, watch};
use tokio::task::AbortHandle;

pub(crate) use connection::join_host_port;
pub use connection::{Connection, DEFAULT_EXEC_TIMEOUT, ExecOutput, SshHandle};
pub use handler::ClientHandler;
pub use remote_forward::{ForwardedTcpip, RemoteListener};

use auth::Authenticator;
use handler::{Banner, CloseSignal, HostKeyPolicy};
use prompt::{PromptGate, Prompter};
use remote_forward::ForwardRoutes;

use crate::known_hosts;
use crate::model::{ConnectTarget, ConnectionState, CoreEvent, HostId, PromptSender};
use crate::{Error, Result};

/// Used when [`ConnectTarget::connect_timeout`] is unset.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Used when [`ConnectTarget::server_alive_interval`] is unset.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
/// Unanswered keepalives before the connection is considered dead.
const KEEPALIVE_MAX: usize = 3;

/// Owns all SSH connections, one per host. Cheap to clone; all methods may be
/// called concurrently.
#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Inner>,
}

type Outcome = Option<std::result::Result<Arc<Connection>, Arc<Error>>>;

struct Inner {
    events: broadcast::Sender<CoreEvent>,
    prompts: PromptSender,
    slots: Mutex<HashMap<HostId, Slot>>,
    next_id: AtomicU64,
}

struct Slot {
    /// Distinguishes successive attempts for the same host, so stale tasks
    /// never touch a newer slot.
    id: u64,
    state: ConnectionState,
    kind: SlotKind,
}

enum SlotKind {
    Connecting {
        done: watch::Receiver<Outcome>,
        task: AbortHandle,
    },
    Reconnecting {
        done: watch::Receiver<Outcome>,
        progress: watch::Receiver<Option<ReconnectProgress>>,
        /// Retry now, with prompts allowed again.
        wake: Arc<Notify>,
        task: AbortHandle,
    },
    Connected(Arc<Connection>),
}

/// Spacing of reconnect attempts: exponential, capped, with jitter so many
/// clients do not retry in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Backoff {
    /// 1 s, 2 s, 4 s, ... up to 60 s.
    pub const DEFAULT: Self = Self {
        initial: Duration::from_secs(1),
        max: Duration::from_secs(60),
    };

    /// Delay before attempt `attempt` (1-based), randomized by up to 20 %.
    pub fn delay(&self, attempt: u32) -> Duration {
        let random = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        self.delay_with(attempt, (random % 10_000) as f64 / 10_000.0)
    }

    /// [`delay`](Self::delay) with the random part given (`0.0..1.0`):
    /// the nominal delay scaled into `80 %..100 %`, so the cap holds.
    pub fn delay_with(&self, attempt: u32, random: f64) -> Duration {
        let exp = attempt.saturating_sub(1).min(20);
        let nominal = self.initial.saturating_mul(1 << exp).min(self.max);
        nominal.mul_f64(0.8 + 0.2 * random.clamp(0.0, 1.0))
    }
}

/// State of a reconnect loop, reported to [`SessionManager::reconnect`]
/// callers before each attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconnectProgress {
    /// The attempt about to be made (1-based).
    pub attempt: u32,
    /// Why the previous attempt failed (empty before the first one).
    pub message: String,
    /// How long until the attempt is made.
    pub delay: Duration,
}

impl SessionManager {
    pub fn new(events: broadcast::Sender<CoreEvent>, prompts: PromptSender) -> Self {
        Self {
            inner: Arc::new(Inner {
                events,
                prompts,
                slots: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// Connect to `target`, or return the existing connection for the same
    /// host (keyed by `host_id`, else `label`). Concurrent calls for the same
    /// host share one attempt. While a [reconnect loop](Self::reconnect)
    /// runs for the host, this makes it retry right away (prompts allowed)
    /// and waits for the loop's outcome.
    pub async fn connect(&self, target: ConnectTarget) -> Result<Arc<Connection>> {
        let key = target_key(&target);
        let mut done = {
            let mut slots = self.inner.slots();
            match slots.get(&key).map(|s| &s.kind) {
                Some(SlotKind::Connected(conn)) if !conn.is_closed() => return Ok(conn.clone()),
                Some(SlotKind::Connecting { done, .. }) => done.clone(),
                Some(SlotKind::Reconnecting { done, wake, .. }) => {
                    wake.notify_one();
                    done.clone()
                }
                _ => Inner::start(&self.inner, &mut slots, key, target),
            }
        };
        wait_outcome(&mut done).await.map_err(|e| match e {
            Error::NotConnected(_) => Error::Cancelled,
            e => e,
        })
    }

    /// Get a working connection to `target`'s host again after it was lost,
    /// retrying as described in the [module docs](self#reconnecting). Returns
    /// the live connection right away if there is one, and joins the loop or
    /// attempt already running for the host otherwise. `on_progress` is
    /// called before every attempt of the loop.
    ///
    /// Fails with the error that ended the loop, or [`Error::NotConnected`]
    /// if the host was disconnected meanwhile. Dropping the future does not
    /// stop the loop (other callers may be waiting); use
    /// [`disconnect`](Self::disconnect).
    pub async fn reconnect(
        &self,
        target: ConnectTarget,
        mut on_progress: impl FnMut(&ReconnectProgress),
    ) -> Result<Arc<Connection>> {
        let key = target_key(&target);
        loop {
            let (mut done, mut progress) = {
                let mut slots = self.inner.slots();
                match slots.get(&key).map(|s| &s.kind) {
                    Some(SlotKind::Connected(conn)) if !conn.is_closed() => {
                        return Ok(conn.clone());
                    }
                    Some(SlotKind::Connecting { done, .. }) => (done.clone(), None),
                    Some(SlotKind::Reconnecting { done, progress, .. }) => {
                        (done.clone(), Some(progress.clone()))
                    }
                    _ => {
                        let (done, progress) =
                            Inner::start_reconnect(&self.inner, &mut slots, key.clone(), &target);
                        (done, Some(progress))
                    }
                }
            };
            let from_loop = progress.is_some();
            if let Some(Some(p)) = progress.as_mut().map(|p| p.borrow_and_update().clone()) {
                on_progress(&p);
            }
            let outcome = loop {
                let changed = async {
                    match progress.as_mut() {
                        Some(p) => p.changed().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    outcome = wait_outcome(&mut done) => break outcome,
                    Ok(()) = changed => {
                        let current = progress.as_mut().and_then(|p| p.borrow_and_update().clone());
                        if let Some(p) = current {
                            on_progress(&p);
                        }
                    }
                }
            };
            match outcome {
                Ok(conn) => return Ok(conn),
                Err(e) if from_loop || is_fatal(&e) => return Err(e),
                // Someone else's single attempt failed; retry in a loop.
                Err(e) => tracing::debug!(host = %key, "joined attempt failed: {e}"),
            }
        }
    }

    /// Close the connection to `host_id`, or abort a connection attempt in
    /// progress (pending `connect` calls then fail with `Cancelled`).
    pub async fn disconnect(&self, host_id: &str) {
        let Some(slot) = self.inner.slots().remove(host_id) else {
            return;
        };
        match slot.kind {
            SlotKind::Connecting { task, .. } | SlotKind::Reconnecting { task, .. } => task.abort(),
            SlotKind::Connected(conn) => conn.close().await,
        }
        self.inner
            .emit(host_id, ConnectionState::Disconnected { reason: None });
    }

    /// The live connection to `host_id`, if any.
    pub fn get(&self, host_id: &str) -> Option<Arc<Connection>> {
        match self.inner.slots().get(host_id).map(|s| &s.kind) {
            Some(SlotKind::Connected(conn)) if !conn.is_closed() => Some(conn.clone()),
            _ => None,
        }
    }

    /// Current state of every host that is connected or connecting.
    pub fn states(&self) -> HashMap<HostId, ConnectionState> {
        self.inner
            .slots()
            .iter()
            .map(|(key, slot)| (key.clone(), slot.state.clone()))
            .collect()
    }
}

impl Inner {
    fn slots(&self) -> MutexGuard<'_, HashMap<HostId, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, key: &str, state: ConnectionState) {
        // No subscribers is fine.
        let _ = self.events.send(CoreEvent::Connection {
            host_id: key.to_owned(),
            state,
        });
    }

    /// Update the state of attempt `id` (if it is still current) and emit it.
    fn set_state(&self, id: u64, key: &str, state: ConnectionState) {
        let mut slots = self.slots();
        if let Some(slot) = slots.get_mut(key).filter(|s| s.id == id) {
            slot.state = state.clone();
            self.emit(key, state);
        }
    }

    fn start(
        this: &Arc<Self>,
        slots: &mut HashMap<HostId, Slot>,
        key: HostId,
        target: ConnectTarget,
    ) -> watch::Receiver<Outcome> {
        let id = this.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = watch::channel(None);
        let state = ConnectionState::Connecting {
            hop: target.jumps.first().map(|j| j.label.clone()),
        };
        let inner = this.clone();
        let task_key = key.clone();
        let task = tokio::spawn(async move {
            let result = inner.establish(id, &task_key, target, None).await;
            inner.finish(id, &task_key, result, tx);
        });
        slots.insert(
            key,
            Slot {
                id,
                state,
                kind: SlotKind::Connecting {
                    done: rx.clone(),
                    task: task.abort_handle(),
                },
            },
        );
        rx
    }

    fn start_reconnect(
        this: &Arc<Self>,
        slots: &mut HashMap<HostId, Slot>,
        key: HostId,
        target: &ConnectTarget,
    ) -> (
        watch::Receiver<Outcome>,
        watch::Receiver<Option<ReconnectProgress>>,
    ) {
        let id = this.next_id.fetch_add(1, Ordering::Relaxed);
        let (done_tx, done_rx) = watch::channel(None);
        let (progress_tx, progress_rx) = watch::channel(None);
        let wake = Arc::new(Notify::new());
        let inner = this.clone();
        let task_key = key.clone();
        let task_target = target.clone();
        let task_wake = wake.clone();
        let task = tokio::spawn(async move {
            let result = inner
                .reconnect_loop(id, &task_key, task_target, progress_tx, task_wake)
                .await;
            inner.finish(id, &task_key, result, done_tx);
        });
        slots.insert(
            key,
            Slot {
                id,
                state: ConnectionState::Reconnecting { attempt: 1 },
                kind: SlotKind::Reconnecting {
                    done: done_rx.clone(),
                    progress: progress_rx.clone(),
                    wake,
                    task: task.abort_handle(),
                },
            },
        );
        (done_rx, progress_rx)
    }

    async fn reconnect_loop(
        &self,
        id: u64,
        key: &str,
        target: ConnectTarget,
        progress: watch::Sender<Option<ReconnectProgress>>,
        wake: Arc<Notify>,
    ) -> Result<Arc<Connection>> {
        let mut may_prompt = true;
        let mut message = String::new();
        for attempt in 1u32.. {
            let delay = Backoff::DEFAULT.delay(attempt);
            self.set_state(id, key, ConnectionState::Reconnecting { attempt });
            progress.send_replace(Some(ReconnectProgress {
                attempt,
                message: message.clone(),
                delay,
            }));
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = wake.notified() => may_prompt = true,
            }
            let gate = PromptGate::new(may_prompt);
            let err = match self
                .establish(id, key, target.clone(), Some(gate.clone()))
                .await
            {
                Ok(conn) => return Ok(conn),
                Err(e) => e,
            };
            tracing::info!(host = %key, attempt, "reconnect attempt failed: {err}");
            if gate.suppressed() {
                return Err(Error::Other(format!(
                    "reconnecting to {} needs your input (password, passphrase or host key); \
                     connect again to continue",
                    target.label
                )));
            }
            if is_fatal(&err) {
                return Err(err);
            }
            may_prompt &= !gate.prompted();
            message = err.to_string();
        }
        unreachable!("the attempt counter does not overflow in practice")
    }

    async fn establish(
        &self,
        id: u64,
        key: &str,
        target: ConnectTarget,
        gate: Option<Arc<PromptGate>>,
    ) -> Result<Arc<Connection>> {
        let prompter = Prompter::new(self.prompts.clone(), target.host_id.clone(), gate);
        let show_banner: Arc<dyn Fn(&str) + Send + Sync> = {
            let events = self.events.clone();
            let host_id = key.to_owned();
            Arc::new(move |message: &str| {
                let _ = events.send(CoreEvent::AuthBanner {
                    host_id: host_id.clone(),
                    message: message.to_owned(),
                });
            })
        };
        let mut hops: Vec<SshHandle> = Vec::with_capacity(target.jumps.len());
        for jump in &target.jumps {
            if !jump.jumps.is_empty() {
                tracing::warn!(hop = %jump.label, "nested jump chain on a hop is ignored");
            }
            self.set_state(
                id,
                key,
                ConnectionState::Connecting {
                    hop: Some(jump.label.clone()),
                },
            );
            let banner = Arc::new(Banner::new(show_banner.clone()));
            let (mut handle, _, _) = open_hop(jump, hops.last(), &prompter, &banner).await?;
            Authenticator::new(&mut handle, jump, &prompter, connect_timeout(jump), banner)
                .run()
                .await?;
            tracing::debug!(hop = %jump.label, "jump host ready");
            hops.push(handle);
        }

        self.set_state(id, key, ConnectionState::Connecting { hop: None });
        let banner = Arc::new(Banner::new(show_banner));
        let (mut handle, close, routes) =
            open_hop(&target, hops.last(), &prompter, &banner).await?;
        self.set_state(id, key, ConnectionState::Authenticating);
        Authenticator::new(
            &mut handle,
            &target,
            &prompter,
            connect_timeout(&target),
            banner,
        )
        .run()
        .await?;
        tracing::info!(host = %key, "authenticated");
        let conn = Connection::new(key.to_owned(), target, handle, hops, close, routes).await;
        Ok(Arc::new(conn))
    }

    fn finish(
        self: &Arc<Self>,
        id: u64,
        key: &str,
        result: Result<Arc<Connection>>,
        done: watch::Sender<Outcome>,
    ) {
        let mut slots = self.slots();
        let current = slots.get_mut(key).filter(|s| s.id == id);
        let outcome = match (result, current) {
            (Ok(conn), Some(slot)) => {
                let state = ConnectionState::Connected {
                    capabilities: conn.capabilities(),
                };
                slot.state = state.clone();
                slot.kind = SlotKind::Connected(conn.clone());
                self.emit(key, state);
                tokio::spawn(Self::monitor(
                    Arc::downgrade(self),
                    id,
                    key.to_owned(),
                    conn.close_signal(),
                ));
                Ok(conn)
            }
            (Ok(conn), None) => {
                // Disconnected while finishing up.
                tokio::spawn(async move { conn.close().await });
                Err(Arc::new(Error::Cancelled))
            }
            (Err(e), current) => {
                tracing::info!(host = %key, "connection failed: {e}");
                if current.is_some() {
                    slots.remove(key);
                    self.emit(
                        key,
                        ConnectionState::Disconnected {
                            reason: Some(e.to_string()),
                        },
                    );
                }
                Err(Arc::new(e))
            }
        };
        done.send_replace(Some(outcome));
    }

    /// Waits for connection `id` to die, then removes it and reports why.
    async fn monitor(inner: Weak<Self>, id: u64, key: HostId, close: Arc<CloseSignal>) {
        close.closed().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let mut slots = inner.slots();
        if slots.get(&key).is_some_and(|s| s.id == id) {
            slots.remove(&key);
            let reason = close
                .reason()
                .unwrap_or_else(|| "connection closed".to_owned());
            tracing::info!(host = %key, "connection lost: {reason}");
            inner.emit(
                &key,
                ConnectionState::Disconnected {
                    reason: Some(reason),
                },
            );
        }
    }
}

/// Wait for an attempt's outcome. A vanished attempt (aborted by
/// `disconnect`) is reported as [`Error::NotConnected`].
async fn wait_outcome(done: &mut watch::Receiver<Outcome>) -> Result<Arc<Connection>> {
    let outcome = done
        .wait_for(Option::is_some)
        .await
        .map(|o| o.clone())
        .unwrap_or(None);
    match outcome {
        Some(Ok(conn)) => Ok(conn),
        Some(Err(e)) => Err(duplicate_error(&e)),
        None => Err(Error::NotConnected("disconnected".into())),
    }
}

/// Errors that retrying cannot fix without the user.
pub(crate) fn is_fatal(e: &Error) -> bool {
    matches!(
        e,
        Error::Cancelled
            | Error::HostKeyRejected { .. }
            | Error::AuthFailed { .. }
            | Error::UnknownHost(_)
            | Error::NotConnected(_)
    )
}

fn target_key(target: &ConnectTarget) -> HostId {
    target
        .host_id
        .clone()
        .unwrap_or_else(|| target.label.clone())
}

fn connect_timeout(target: &ConnectTarget) -> Duration {
    target.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT)
}

/// russh client settings for one hop. `host` is the known_hosts lookup name
/// (`HostKeyAlias` or hostname).
fn client_config(target: &ConnectTarget, host: &str) -> Arc<client::Config> {
    let recorded = known_hosts::recorded_algorithms(&target.known_hosts_files, host, target.port)
        .unwrap_or_else(|e| {
            tracing::debug!("reading known_hosts for algorithm preference: {e}");
            Vec::new()
        });
    let preferred = Preferred {
        key: host_key_preference(&Preferred::DEFAULT.key, &recorded).into(),
        ..Preferred::DEFAULT
    };
    Arc::new(client::Config {
        keepalive_interval: Some(
            target
                .server_alive_interval
                .unwrap_or(DEFAULT_KEEPALIVE_INTERVAL),
        ),
        keepalive_max: KEEPALIVE_MAX,
        nodelay: true,
        // russh's default (2 MiB) caps pipelined SFTP downloads at ~1.5 MiB
        // per round trip; 8 MiB more than doubles throughput at 50 ms RTT.
        window_size: 8 * 1024 * 1024,
        preferred,
        ..Default::default()
    })
}

/// Like OpenSSH: offer host key algorithms already recorded for the host
/// first (keeping the default relative order), so a host known only by,
/// say, its ECDSA key is verified with that key instead of prompting about
/// an "unknown" Ed25519 key. A recorded RSA key covers every RSA signature
/// variant.
fn host_key_preference(defaults: &[Algorithm], recorded: &[Algorithm]) -> Vec<Algorithm> {
    let is_recorded = |alg: &Algorithm| {
        recorded
            .iter()
            .any(|r| r == alg || (r.clone().is_rsa() && alg.clone().is_rsa()))
    };
    let (mut known, unknown): (Vec<_>, Vec<_>) =
        defaults.iter().cloned().partition(|a| is_recorded(a));
    known.extend(unknown);
    known
}

/// Open the transport to `target` (TCP, or a `direct-tcpip` channel through
/// the previous hop) and run the SSH handshake including host key
/// verification. Authentication is left to the caller.
async fn open_hop(
    target: &ConnectTarget,
    via: Option<&SshHandle>,
    prompter: &Prompter,
    banner: &Arc<Banner>,
) -> Result<(SshHandle, Arc<CloseSignal>, Arc<ForwardRoutes>)> {
    let timeout = connect_timeout(target);
    let addr = format!("{}:{}", target.hostname, target.port);
    let close = Arc::new(CloseSignal::default());
    let (started_tx, started_rx) = oneshot::channel();
    let host = target
        .host_key_alias
        .clone()
        .unwrap_or_else(|| target.hostname.clone());
    let config = client_config(target, &host);
    let policy = HostKeyPolicy {
        known_hosts_files: target.known_hosts_files.clone(),
        host,
        port: target.port,
        trusted: target
            .trusted_host_keys
            .iter()
            .filter_map(|k| russh::keys::PublicKey::from_openssh(k).ok())
            .collect(),
        prompter: prompter.clone(),
    };
    let routes = Arc::new(ForwardRoutes::default());
    let handler = ClientHandler::new(
        policy,
        started_tx,
        close.clone(),
        routes.clone(),
        banner.clone(),
    );
    let timed_out = || Error::Timeout(format!("connecting to {addr}"));

    let handle = match via {
        None => {
            tracing::debug!(%addr, "connecting");
            let stream = tokio::time::timeout(
                timeout,
                tokio::net::TcpStream::connect((target.hostname.as_str(), target.port)),
            )
            .await
            .map_err(|_| timed_out())??;
            if let Err(e) = stream.set_nodelay(true) {
                tracing::debug!("set_nodelay: {e}");
            }
            handshake(config, stream, handler, started_rx, timeout).await
        }
        Some(prev) => {
            tracing::debug!(%addr, "connecting through jump host");
            let channel = tokio::time::timeout(
                timeout,
                prev.channel_open_direct_tcpip(
                    target.hostname.clone(),
                    target.port.into(),
                    "127.0.0.1",
                    0,
                ),
            )
            .await
            .map_err(|_| timed_out())??;
            handshake(config, channel.into_stream(), handler, started_rx, timeout).await
        }
    };
    match handle {
        Ok(Some(handle)) => Ok((handle, close, routes)),
        Ok(None) => Err(timed_out()),
        Err(e) => Err(e),
    }
}

/// SSH handshake bounded by `timeout` up to the host key check; the time the
/// user spends answering a host key prompt does not count. `Ok(None)` means
/// timed out.
async fn handshake<S>(
    config: Arc<client::Config>,
    stream: S,
    handler: ClientHandler,
    key_check_started: oneshot::Receiver<()>,
    timeout: Duration,
) -> Result<Option<SshHandle>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let connect = client::connect_stream(config, stream, handler);
    let deadline = async {
        if tokio::time::timeout(timeout, key_check_started)
            .await
            .is_ok()
        {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        res = connect => res.map(Some),
        () = deadline => Ok(None),
    }
}

/// [`Error`] is not `Clone`; reproduce it for every caller sharing an attempt.
pub(crate) fn duplicate_error(e: &Error) -> Error {
    match e {
        Error::HostKeyRejected { host } => Error::HostKeyRejected { host: host.clone() },
        Error::AuthFailed { user, host } => Error::AuthFailed {
            user: user.clone(),
            host: host.clone(),
        },
        Error::Cancelled => Error::Cancelled,
        Error::NotConnected(s) => Error::NotConnected(s.clone()),
        Error::Timeout(s) => Error::Timeout(s.clone()),
        Error::UnknownHost(s) => Error::UnknownHost(s.clone()),
        Error::Keyring(s) => Error::Keyring(s.clone()),
        Error::NotFound(s) => Error::NotFound(s.clone()),
        Error::PermissionDenied(s) => Error::PermissionDenied(s.clone()),
        Error::AlreadyExists(s) => Error::AlreadyExists(s.clone()),
        Error::Unsupported(s) => Error::Unsupported(s.clone()),
        Error::Io(io) => Error::Io(std::io::Error::new(io.kind(), io.to_string())),
        other => Error::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{EcdsaCurve, HashAlg};

    #[test]
    fn backoff_doubles_up_to_the_cap_with_jitter() {
        let b = Backoff::DEFAULT;
        let secs = |attempt, r| b.delay_with(attempt, r).as_secs_f64();
        assert_eq!(secs(1, 1.0), 1.0);
        assert_eq!(secs(2, 1.0), 2.0);
        assert_eq!(secs(3, 1.0), 4.0);
        assert_eq!(secs(6, 1.0), 32.0);
        assert_eq!(secs(7, 1.0), 60.0);
        assert_eq!(secs(1000, 1.0), 60.0);
        assert_eq!(secs(0, 1.0), 1.0);
        assert!((secs(1, 0.0) - 0.8).abs() < 1e-9);
        assert!((secs(1000, 0.0) - 48.0).abs() < 1e-9);
        for attempt in 1..30 {
            let d = b.delay(attempt);
            assert!(
                d <= b.max && d >= b.initial.mul_f64(0.8),
                "{attempt}: {d:?}"
            );
        }
    }

    #[test]
    fn recorded_host_key_algorithms_come_first() {
        let defaults = &Preferred::DEFAULT.key;
        let p256 = Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        };
        let rsa = |hash| Algorithm::Rsa { hash };

        assert_eq!(&host_key_preference(defaults, &[])[..], &defaults[..]);

        let ecdsa_first = host_key_preference(defaults, std::slice::from_ref(&p256));
        assert_eq!(ecdsa_first[0], p256);
        assert_eq!(ecdsa_first[1], Algorithm::Ed25519);
        assert_eq!(ecdsa_first.len(), defaults.len());

        let rsa_first = host_key_preference(defaults, &[rsa(None), Algorithm::Ed25519]);
        assert_eq!(
            rsa_first[..4],
            [
                Algorithm::Ed25519,
                rsa(Some(HashAlg::Sha512)),
                rsa(Some(HashAlg::Sha256)),
                rsa(None)
            ]
        );
        assert_eq!(rsa_first[4], p256);
    }
}
