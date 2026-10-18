//! Port forwarding: saved rules and the engine that runs them.
//!
//! [`ForwardManager`] owns the rules (app rules in a [`ForwardStore`] plus
//! read-only rules derived from ssh_config, see [`rules`]) and runs each
//! started rule in its own task, publishing [`CoreEvent::PortForward`] on
//! state changes and, while traffic flows, counter updates about once per
//! second ([`TRAFFIC_INTERVAL`]; none while idle). Counters start at zero on
//! every start.
//!
//! # Kinds
//!
//! - **Local** (`-L`): listens on this machine (see [`listen`] for bind
//!   addresses) and opens a `direct-tcpip` channel to the destination on the
//!   host for every accepted connection.
//! - **Dynamic** (`-D`): a SOCKS4/4a/5 server on this machine; each
//!   `CONNECT` becomes a `direct-tcpip` channel.
//! - **Remote** (`-R`): asks the host to listen (`tcpip-forward`; default
//!   address `localhost`) and connects each forwarded connection to the
//!   destination from this machine, or, with `via_host_id`, through a
//!   `direct-tcpip` channel on that other host (machine-to-machine).
//!
//! Byte counts are from the point of view of the connecting client:
//! `bytes_out` flows towards the destination, `bytes_in` back.
//!
//! # Lifecycle
//!
//! Starting connects the host(s) if needed (prompts appear as usual) and
//! binds the listener; port conflicts, missing privileges and refused remote
//! forwards end in `Failed`. Cancelling a prompt while starting ends in
//! `Stopped`.
//!
//! If a carrying connection is lost while a forward is active, the forward
//! goes `Reconnecting` and uses [`SessionManager::reconnect`] (exponential
//! backoff, at most one round of prompts per loop; see the
//! [session docs](crate::session#reconnecting)); errors that need the user
//! end in `Failed`. Local listeners stay bound meanwhile: new connections
//! wait in the OS backlog and are served once the tunnel is back. Remote
//! forwards are requested again after reconnecting; a refusal then (the old
//! session may still hold the port) is retried with backoff. Connections
//! that were open when the link dropped are closed. If the user disconnects
//! the host on purpose, its forwards stop.
//!
//! Rules with `auto_start` are started by [`ForwardManager::start_auto`];
//! for those, an unreachable host at startup is retried like a lost
//! connection instead of failing.

mod listen;
mod pump;
pub mod rules;
mod socks;
mod status;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use russh::ChannelOpenFailure;
use tokio::net::TcpStream;
use tokio::sync::{RwLock, broadcast, watch};
use tokio::task::{AbortHandle, JoinSet};

pub use rules::{ForwardStore, config_rules, describe, validate};
pub use status::TRAFFIC_INTERVAL;

use crate::hosts::HostStore;
use crate::model::{
    ConnectTarget, CoreEvent, HostId, PortForward, PortForwardDraft, PortForwardKind,
    PortForwardState, PortForwardStatus,
};
use crate::session::{
    Backoff, Connection, ForwardedTcpip, RemoteListener, SessionManager, is_fatal, join_host_port,
};
use crate::{Error, Result};

use listen::Listeners;
use status::StatusCell;

/// Bound for [`ForwardManager::stop`] before the forward's task is aborted.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound for connecting to a remote forward's destination.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound for a SOCKS client to send its request.
const SOCKS_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for cancelling a remote forward when stopping.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
/// Pause after a failed `accept` (e.g. out of file descriptors).
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);

/// Runs port forwards. Cheap to clone; all methods may be called
/// concurrently.
#[derive(Clone)]
pub struct ForwardManager {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: SessionManager,
    hosts: Arc<RwLock<HostStore>>,
    events: broadcast::Sender<CoreEvent>,
    store: tokio::sync::Mutex<ForwardStore>,
    runs: Mutex<HashMap<String, Run>>,
    /// ssh_config rules as last seen, to detect changes.
    config_rules: Mutex<Vec<PortForward>>,
}

/// A started forward (running or finished; finished ones keep their final
/// status until restarted).
struct Run {
    rule: PortForward,
    cell: Arc<StatusCell>,
    stop: watch::Sender<bool>,
    task: AbortHandle,
}

impl Run {
    fn is_live(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Inner {
    fn runs(&self) -> MutexGuard<'_, HashMap<String, Run>> {
        self.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit_rules_changed(&self) {
        let _ = self.events.send(CoreEvent::PortForwardsChanged);
    }
}

impl ForwardManager {
    pub fn new(
        sessions: SessionManager,
        hosts: Arc<RwLock<HostStore>>,
        events: broadcast::Sender<CoreEvent>,
        store: ForwardStore,
    ) -> Self {
        let config_rules = hosts
            .try_read()
            .map(|h| h.config_forwards().to_vec())
            .unwrap_or_default();
        Self {
            inner: Arc::new(Inner {
                sessions,
                hosts,
                events,
                store: tokio::sync::Mutex::new(store),
                runs: Mutex::new(HashMap::new()),
                config_rules: Mutex::new(config_rules),
            }),
        }
    }

    /// App rules (in creation order), then ssh_config rules.
    pub async fn list(&self) -> Vec<PortForward> {
        let mut rules = self.inner.store.lock().await.list();
        rules.extend_from_slice(self.inner.hosts.read().await.config_forwards());
        rules
    }

    pub async fn get(&self, id: &str) -> Option<PortForward> {
        if id.starts_with("cfg:") {
            let hosts = self.inner.hosts.read().await;
            return hosts.config_forwards().iter().find(|r| r.id == id).cloned();
        }
        self.inner.store.lock().await.get(id)
    }

    pub async fn create(&self, draft: PortForwardDraft) -> Result<PortForward> {
        let rule = self.validate(draft).await?;
        let rule = self.inner.store.lock().await.create(rule)?;
        self.inner.emit_rules_changed();
        Ok(rule)
    }

    /// Replace an app rule. A running forward is restarted with the new
    /// settings (in the background; watch its status).
    pub async fn update(&self, id: &str, draft: PortForwardDraft) -> Result<PortForward> {
        if id.starts_with("cfg:") {
            return Err(rules::read_only(id));
        }
        let rule = self.validate(draft).await?;
        let rule = self.inner.store.lock().await.update(id, rule)?;
        let was_live = self.inner.runs().get(id).is_some_and(Run::is_live);
        self.stop(id).await;
        self.inner.runs().remove(id);
        if was_live {
            self.restart_in_background(id.to_owned());
        }
        self.inner.emit_rules_changed();
        Ok(rule)
    }

    /// Stop (if running) and delete an app rule.
    pub async fn delete(&self, id: &str) -> Result<()> {
        if id.starts_with("cfg:") {
            return Err(rules::read_only(id));
        }
        self.stop(id).await;
        self.inner.store.lock().await.delete(id)?;
        self.inner.runs().remove(id);
        self.inner.emit_rules_changed();
        Ok(())
    }

    /// Points app rules at the new ids of renamed hosts (old id -> new id).
    pub async fn rename_hosts(&self, renamed: &BTreeMap<HostId, HostId>) -> Result<()> {
        if self.inner.store.lock().await.rename_hosts(renamed)? {
            self.inner.emit_rules_changed();
        }
        Ok(())
    }

    /// Status of every rule (`Stopped` for those never started).
    pub async fn statuses(&self) -> Vec<PortForwardStatus> {
        let rules = self.list().await;
        let runs = self.inner.runs();
        rules
            .iter()
            .map(|r| match runs.get(&r.id) {
                Some(run) => run.cell.snapshot(),
                None => stopped(&r.id),
            })
            .collect()
    }

    /// Start a rule and wait until it is active (or, for a rule already
    /// running, until it has settled). Fails with the error that put it in
    /// `Failed`, or [`Error::Cancelled`] if it was stopped meanwhile or the
    /// user cancelled a prompt.
    pub async fn start(&self, id: &str) -> Result<PortForwardStatus> {
        self.start_with(id, false).await
    }

    /// Start every rule marked `auto_start`, without waiting for them.
    pub async fn start_auto(&self) {
        for rule in self.list().await.into_iter().filter(|r| r.auto_start) {
            let this = self.clone();
            tokio::spawn(async move {
                if let Err(e) = this.start_with(&rule.id, true).await {
                    tracing::info!(forward = %rule.id, "auto-start failed: {e}");
                }
            });
        }
    }

    /// Stop a forward and wait (at most [`STOP_TIMEOUT`]) until it has
    /// released its listener. Clears a `Failed` status. No-op for idle or
    /// unknown rules.
    pub async fn stop(&self, id: &str) {
        let (cell, task) = {
            let runs = self.inner.runs();
            let Some(run) = runs.get(id) else {
                return;
            };
            run.stop.send_replace(true);
            (run.cell.clone(), run.task.clone())
        };
        // The runner releases its sockets before reporting a final state.
        let mut state = cell.subscribe();
        let finished = tokio::time::timeout(STOP_TIMEOUT, async {
            let _ = state
                .wait_for(|s| {
                    matches!(
                        s,
                        PortForwardState::Stopped | PortForwardState::Failed { .. }
                    )
                })
                .await;
        })
        .await;
        if finished.is_err() {
            tracing::warn!(forward = %id, "forward did not stop in time; aborting it");
            task.abort();
        }
        cell.set_state(PortForwardState::Stopped);
    }

    /// Stop every running forward (e.g. when the app exits).
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self
            .inner
            .runs()
            .iter()
            .filter(|(_, run)| run.is_live())
            .map(|(id, _)| id.clone())
            .collect();
        futures_util::future::join_all(ids.iter().map(|id| self.stop(id))).await;
    }

    /// Pick up ssh_config changes (call after the host list was reloaded):
    /// emits [`CoreEvent::PortForwardsChanged`] if the derived rules
    /// changed, stops running ones that disappeared and restarts changed
    /// ones.
    pub async fn sync_config_rules(&self) {
        let current = self.inner.hosts.read().await.config_forwards().to_vec();
        let previous = std::mem::replace(
            &mut *self
                .inner
                .config_rules
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            current.clone(),
        );
        if previous == current {
            return;
        }
        let affected: Vec<(String, bool)> = self
            .inner
            .runs()
            .iter()
            .filter(|(id, _)| id.starts_with("cfg:"))
            .filter_map(|(id, run)| match current.iter().find(|r| r.id == *id) {
                Some(rule) if *rule == run.rule => None,
                Some(_) => Some((id.clone(), run.is_live())),
                None => Some((id.clone(), false)),
            })
            .collect();
        for (id, restart) in affected {
            self.stop(&id).await;
            self.inner.runs().remove(&id);
            if restart {
                self.restart_in_background(id);
            }
        }
        self.inner.emit_rules_changed();
    }

    fn restart_in_background(&self, id: String) {
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.start(&id).await {
                tracing::info!(forward = %id, "restart failed: {e}");
            }
        });
    }

    async fn validate(&self, draft: PortForwardDraft) -> Result<PortForward> {
        let hosts = self.inner.hosts.read().await;
        validate(draft, |id| hosts.get(id).is_some())
    }

    async fn start_with(&self, id: &str, retry_initial: bool) -> Result<PortForwardStatus> {
        let rule = self
            .get(id)
            .await
            .ok_or_else(|| Error::NotFound(format!("port forward {id}")))?;
        let cell = {
            let mut runs = self.inner.runs();
            match runs.get(id) {
                Some(run) if run.is_live() => run.cell.clone(),
                _ => {
                    let run = self.spawn(rule, retry_initial);
                    let cell = run.cell.clone();
                    runs.insert(id.to_owned(), run);
                    cell
                }
            }
        };
        let mut state = cell.subscribe();
        let settled = state
            .wait_for(|s| !matches!(s, PortForwardState::Starting))
            .await
            .map(|s| s.clone())
            .unwrap_or(PortForwardState::Stopped);
        match settled {
            PortForwardState::Active { .. } | PortForwardState::Reconnecting { .. } => {
                Ok(cell.snapshot())
            }
            PortForwardState::Failed { message } => {
                Err(cell.failure().unwrap_or(Error::Other(message)))
            }
            PortForwardState::Stopped | PortForwardState::Starting => Err(Error::Cancelled),
        }
    }

    fn spawn(&self, rule: PortForward, retry_initial: bool) -> Run {
        let cell = StatusCell::new(
            rule.id.clone(),
            self.inner.events.clone(),
            PortForwardState::Starting,
        );
        let (stop_tx, stop_rx) = watch::channel(false);
        let runner = Runner {
            sessions: self.inner.sessions.clone(),
            hosts: self.inner.hosts.clone(),
            rule: rule.clone(),
            cell: cell.clone(),
            stop: stop_rx,
            retry_initial,
        };
        let task = tokio::spawn(runner.run());
        Run {
            rule,
            cell,
            stop: stop_tx,
            task: task.abort_handle(),
        }
    }
}

fn stopped(id: &str) -> PortForwardStatus {
    PortForwardStatus {
        id: id.to_owned(),
        state: PortForwardState::Stopped,
        open_connections: 0,
        total_connections: 0,
        bytes_in: 0,
        bytes_out: 0,
    }
}

/// Why setting up (or keeping up) a forward did not work out.
enum Halt {
    /// Asked to stop, the user cancelled while starting, or the host was
    /// disconnected on purpose.
    Stop,
    Fail(Error),
    /// Transient trouble after a reconnect; try again after a pause.
    Retry(String),
}

impl From<Error> for Halt {
    fn from(e: Error) -> Self {
        Halt::Fail(e)
    }
}

/// Which connection went away while serving.
enum Lost {
    Main,
    Via,
}

/// The connections a forward currently runs on.
#[derive(Default)]
struct Links {
    main: Option<Arc<Connection>>,
    via: Option<Arc<Connection>>,
    remote: Option<RemoteListener>,
}

struct Runner {
    sessions: SessionManager,
    hosts: Arc<RwLock<HostStore>>,
    rule: PortForward,
    cell: Arc<StatusCell>,
    stop: watch::Receiver<bool>,
    retry_initial: bool,
}

impl Runner {
    async fn run(mut self) {
        self.cell.publish();
        let publisher = tokio::spawn(self.cell.clone().publish_traffic());
        let mut links = Links::default();
        let outcome = self.run_inner(&mut links).await;
        publisher.abort();
        if let (Some(listener), Some(main)) = (links.remote.take(), links.main.as_ref())
            && !main.is_closed()
        {
            match tokio::time::timeout(CANCEL_TIMEOUT, main.cancel_remote(listener)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::debug!(forward = %self.rule.id, "cancel remote forward: {e}")
                }
                Err(_) => {
                    tracing::debug!(forward = %self.rule.id, "cancel remote forward timed out")
                }
            }
        }
        match outcome {
            Halt::Fail(e) => {
                tracing::info!(forward = %self.rule.id, "forward failed: {e}");
                self.cell.fail(e);
            }
            Halt::Stop | Halt::Retry(_) => self.cell.set_state(PortForwardState::Stopped),
        }
        self.cell.publish();
    }

    /// Runs until stopped or failed; the result says which.
    async fn run_inner(&mut self, links: &mut Links) -> Halt {
        let listeners = match self.rule.kind {
            PortForwardKind::Local | PortForwardKind::Dynamic => {
                match Listeners::bind(self.rule.bind_address.as_deref(), self.rule.bind_port).await
                {
                    Ok(l) => Some(l),
                    Err(e) => return Halt::Fail(e),
                }
            }
            PortForwardKind::Remote => None,
        };
        // `None` while starting; afterwards the reason of the last loss.
        let mut lost_reason: Option<String> = None;
        let mut retries = 0u32;
        loop {
            let address = match self
                .establish(links, listeners.as_ref(), &mut lost_reason)
                .await
            {
                Ok(address) => address,
                Err(Halt::Retry(message)) => {
                    retries += 1;
                    self.cell.set_state(PortForwardState::Reconnecting {
                        attempt: retries,
                        message,
                    });
                    tokio::select! {
                        () = tokio::time::sleep(Backoff::DEFAULT.delay(retries)) => continue,
                        () = stop_requested(&mut self.stop) => return Halt::Stop,
                    }
                }
                Err(halt) => return halt,
            };
            retries = 0;
            tracing::info!(forward = %self.rule.id, %address, "forward active");
            self.cell.set_state(PortForwardState::Active { address });

            let lost = match self.serve(links, listeners.as_ref()).await {
                Some(lost) => lost,
                None => return Halt::Stop,
            };
            let gone = match lost {
                Lost::Main => {
                    links.remote = None;
                    links.main.take()
                }
                Lost::Via => links.via.take(),
            };
            let Some(gone) = gone else {
                continue;
            };
            if gone.closed_locally() {
                tracing::info!(forward = %self.rule.id, host = %gone.key(), "host disconnected; stopping forward");
                return Halt::Stop;
            }
            let reason = format!(
                "connection to {} lost: {}",
                gone.target().label,
                gone.close_reason()
                    .unwrap_or_else(|| "connection closed".into())
            );
            tracing::info!(forward = %self.rule.id, "{reason}; reconnecting");
            self.cell.set_state(PortForwardState::Reconnecting {
                attempt: 1,
                message: reason.clone(),
            });
            lost_reason = Some(reason);
        }
    }

    /// Make sure the connection(s) and the remote listener are up. Returns
    /// the address to show while active.
    async fn establish(
        &mut self,
        links: &mut Links,
        listeners: Option<&Listeners>,
        lost_reason: &mut Option<String>,
    ) -> std::result::Result<String, Halt> {
        let host = self.rule.host_id.clone();
        let main = self.ensure(&host, links.main.take(), lost_reason).await?;
        links.main = Some(main.clone());

        if self.rule.kind == PortForwardKind::Remote && links.remote.is_none() {
            let address = self.rule.bind_address.as_deref().unwrap_or("localhost");
            match main.forward_remote(address, self.rule.bind_port).await {
                Ok(listener) => links.remote = Some(listener),
                Err(e) if lost_reason.is_some() => return Err(Halt::Retry(e.to_string())),
                Err(e) => return Err(Halt::Fail(e)),
            }
        }

        if let Some(via) = self.rule.via_host_id.clone() {
            let conn = self.ensure(&via, links.via.take(), lost_reason).await?;
            links.via = Some(conn);
        }

        Ok(match (&links.remote, listeners) {
            (Some(remote), _) => join_host_port(remote.address(), remote.port()),
            (None, Some(listeners)) => listeners.address(),
            (None, None) => String::new(),
        })
    }

    /// A live connection to `host_id`: `current` if still alive, a new one
    /// while starting, or a reconnected one after a loss.
    async fn ensure(
        &mut self,
        host_id: &HostId,
        current: Option<Arc<Connection>>,
        lost_reason: &mut Option<String>,
    ) -> std::result::Result<Arc<Connection>, Halt> {
        if let Some(conn) = current.as_ref().filter(|c| !c.is_closed()) {
            return Ok(conn.clone());
        }
        let resolved = self.hosts.read().await.resolve_target(host_id);
        let target: ConnectTarget = match (resolved, &current) {
            (Ok(target), _) => target,
            (Err(_), Some(old)) => old.target().clone(),
            (Err(e), None) => return Err(Halt::Fail(e)),
        };

        if lost_reason.is_none() {
            let result = tokio::select! {
                r = self.sessions.connect(target.clone()) => r,
                () = stop_requested(&mut self.stop) => return Err(Halt::Stop),
            };
            match result {
                Ok(conn) => return Ok(conn),
                Err(Error::Cancelled) => return Err(Halt::Stop),
                Err(e) if self.retry_initial && !is_fatal(&e) => {
                    let reason = e.to_string();
                    tracing::info!(forward = %self.rule.id, "cannot connect yet ({reason}); retrying");
                    self.cell.set_state(PortForwardState::Reconnecting {
                        attempt: 1,
                        message: reason.clone(),
                    });
                    *lost_reason = Some(reason);
                }
                Err(e) => return Err(Halt::Fail(e)),
            }
        }

        let fallback = lost_reason.clone().unwrap_or_default();
        let cell = self.cell.clone();
        let result = tokio::select! {
            r = self.sessions.reconnect(target, |p| {
                let message = if p.message.is_empty() { fallback.clone() } else { p.message.clone() };
                cell.set_state(PortForwardState::Reconnecting { attempt: p.attempt, message });
            }) => r,
            () = stop_requested(&mut self.stop) => return Err(Halt::Stop),
        };
        match result {
            Ok(conn) => Ok(conn),
            Err(Error::NotConnected(_)) => Err(Halt::Stop),
            Err(e) => Err(Halt::Fail(e)),
        }
    }

    /// Serve connections until a link is lost (`Some`) or the forward is
    /// stopped (`None`). Tunneled connections end with this call.
    async fn serve(&mut self, links: &mut Links, listeners: Option<&Listeners>) -> Option<Lost> {
        let main = links.main.clone()?;
        let via = links.via.clone();
        let target = self.target();
        let mut tunnels = JoinSet::new();
        loop {
            tokio::select! {
                () = stop_requested(&mut self.stop) => return None,
                () = main.closed() => return Some(Lost::Main),
                () = closed(via.as_deref()) => return Some(Lost::Via),
                accepted = accept(listeners) => match accepted {
                    Ok((socket, peer)) => {
                        let _ = socket.set_nodelay(true);
                        let cell = self.cell.clone();
                        let conn = main.clone();
                        match self.rule.kind {
                            PortForwardKind::Dynamic => {
                                tunnels.spawn(socks_connection(conn, socket, peer, cell));
                            }
                            _ => {
                                tunnels.spawn(local_connection(conn, socket, peer, target.clone(), cell));
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(forward = %self.rule.id, "accept failed: {e}");
                        tokio::time::sleep(ACCEPT_ERROR_PAUSE).await;
                    }
                },
                Some(forwarded) = next_forwarded(links.remote.as_mut()) => {
                    tunnels.spawn(remote_connection(forwarded, via.clone(), target.clone(), self.cell.clone()));
                }
                Some(_) = tunnels.join_next(), if !tunnels.is_empty() => {}
            }
        }
    }

    fn target(&self) -> (String, u16) {
        (
            self.rule.target_host.clone().unwrap_or_default(),
            self.rule.target_port.unwrap_or_default(),
        )
    }
}

/// Resolves once the forward should stop (also if its manager is gone).
async fn stop_requested(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

async fn closed(conn: Option<&Connection>) {
    match conn {
        Some(conn) => conn.closed().await,
        None => std::future::pending().await,
    }
}

async fn accept(listeners: Option<&Listeners>) -> std::io::Result<(TcpStream, SocketAddr)> {
    match listeners {
        Some(l) => l.accept().await,
        None => std::future::pending().await,
    }
}

async fn next_forwarded(listener: Option<&mut RemoteListener>) -> Option<ForwardedTcpip> {
    match listener {
        Some(l) => l.accept().await,
        None => std::future::pending().await,
    }
}

async fn local_connection(
    conn: Arc<Connection>,
    socket: TcpStream,
    peer: SocketAddr,
    (host, port): (String, u16),
    cell: Arc<StatusCell>,
) {
    let _guard = cell.connection();
    match conn.open_direct_tcpip(&host, port, peer).await {
        Ok(channel) => pump::pump(socket, channel.into_stream(), &cell).await,
        Err(e) => tracing::info!(
            forward = %cell.id(),
            "cannot open a tunnel to {}: {e}",
            join_host_port(&host, port)
        ),
    }
}

async fn socks_connection(
    conn: Arc<Connection>,
    mut socket: TcpStream,
    peer: SocketAddr,
    cell: Arc<StatusCell>,
) {
    let request = match tokio::time::timeout(SOCKS_TIMEOUT, socks::handshake(&mut socket)).await {
        Ok(Ok(request)) => request,
        Ok(Err(e)) => {
            tracing::debug!(forward = %cell.id(), %peer, "SOCKS handshake failed: {e}");
            return;
        }
        Err(_) => {
            tracing::debug!(forward = %cell.id(), %peer, "SOCKS client sent no request");
            return;
        }
    };
    let _guard = cell.connection();
    match conn
        .open_direct_tcpip(&request.host, request.port, peer)
        .await
    {
        Ok(channel) => {
            if socks::reply(&mut socket, request.version, socks::Reply::Succeeded)
                .await
                .is_ok()
            {
                pump::pump(socket, channel.into_stream(), &cell).await;
            }
        }
        Err(e) => {
            tracing::info!(
                forward = %cell.id(),
                "SOCKS: cannot open a tunnel to {}: {e}",
                join_host_port(&request.host, request.port)
            );
            let _ = socks::reply(&mut socket, request.version, socks_failure(&e)).await;
        }
    }
}

fn socks_failure(e: &Error) -> socks::Reply {
    match e {
        Error::Ssh(russh::Error::ChannelOpenFailure(f)) => match f {
            ChannelOpenFailure::AdministrativelyProhibited => socks::Reply::NotAllowed,
            ChannelOpenFailure::ConnectFailed => socks::Reply::ConnectionRefused,
            _ => socks::Reply::GeneralFailure,
        },
        _ => socks::Reply::GeneralFailure,
    }
}

async fn remote_connection(
    forwarded: ForwardedTcpip,
    via: Option<Arc<Connection>>,
    (host, port): (String, u16),
    cell: Arc<StatusCell>,
) {
    let _guard = cell.connection();
    let destination = join_host_port(&host, port);
    match via {
        Some(via) => {
            let (address, origin_port) = forwarded.originator();
            let originator = format!("{address}:{origin_port}")
                .parse::<SocketAddr>()
                .or_else(|_| join_host_port(address, origin_port as u16).parse())
                .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 0)));
            match via.open_direct_tcpip(&host, port, originator).await {
                Ok(outgoing) => {
                    let incoming = forwarded.accept().await;
                    pump::pump(incoming.into_stream(), outgoing.into_stream(), &cell).await;
                }
                Err(e) => {
                    tracing::info!(
                        forward = %cell.id(),
                        "cannot reach {destination} from {}: {e}",
                        via.target().label
                    );
                    forwarded.reject(ChannelOpenFailure::ConnectFailed).await;
                }
            }
        }
        None => {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port)))
                .await
            {
                Ok(Ok(socket)) => {
                    let _ = socket.set_nodelay(true);
                    let incoming = forwarded.accept().await;
                    pump::pump(incoming.into_stream(), socket, &cell).await;
                }
                Ok(Err(e)) => {
                    tracing::info!(forward = %cell.id(), "cannot connect to {destination}: {e}");
                    forwarded.reject(ChannelOpenFailure::ConnectFailed).await;
                }
                Err(_) => {
                    tracing::info!(forward = %cell.id(), "connecting to {destination} timed out");
                    forwarded.reject(ChannelOpenFailure::ConnectFailed).await;
                }
            }
        }
    }
}
