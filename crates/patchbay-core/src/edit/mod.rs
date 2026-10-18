//! Editing remote files in local applications ("open with default app,
//! upload on save").
//!
//! [`EditManager::open`] downloads a file into a directory of its own below
//! the cache (`<cache_dir>/patchbay/edit/<session id>/<file name>`; the name is
//! kept so editors pick the right mode), watches that directory and uploads
//! the file back whenever it is saved:
//!
//! - File system events are debounced ([`DEBOUNCE`]) and changes are
//!   detected by content hash. Watching the directory rather than the file
//!   catches editors that save through a temporary file and a rename (vim,
//!   VS Code, JetBrains IDEs); saves without changes upload nothing.
//! - Before uploading, the remote file is stat'ed. If its mtime or size
//!   differ from what Patchbay last saw, nothing is written: the session goes
//!   to [`EditState::Conflict`] and the user is asked
//!   ([`Prompt::EditConflict`]) whether to overwrite the server's version,
//!   discard the local changes (download again), or keep the local copy and
//!   stop syncing ([`EditState::Stopped`]). Dismissing the question leaves
//!   the conflict until [`EditManager::retry`] asks again.
//! - Uploads write `.<name>.patchbay-<random>.tmp` next to the remote file,
//!   copy the permission bits and rename it over the original (atomically
//!   where the server supports `posix-rename@openssh.com`). The file is
//!   rewritten in place instead when the directory is not writable or the
//!   file belongs to another user or group, so owner and group never change.
//!   Symlinks are resolved when opening: saves go to the link's target.
//! - A sync that fails (e.g. the connection is down) leaves the session
//!   [`EditState::Failed`]; it is tried again on the next save, on
//!   [`EditManager::retry`] and, after connection errors, as soon as the
//!   host is connected again.
//! - Next to each session directory, `<session id>.json` records what was
//!   last synced. On startup, [`EditManager::recover`] deletes sessions of
//!   a previous run whose local copy matches that record and lists the
//!   others again (`recovered`, `Failed`) so their changes can still be
//!   uploaded (with [`EditManager::retry`]) or discarded.
//!
//! Remote mtimes have a resolution of one second over SFTP, so a remote
//! change that keeps the size and lands in the same second as Patchbay's last
//! sync goes unnoticed.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::event::{AccessKind, AccessMode, MetadataKind, ModifyKind};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::AbortHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::fs::{Endpoint, FileSystem, Meta};
use crate::model::{
    ConnectionState, CoreEvent, EditConflictResolution, EditSession, EditState, FileKind, FileStat,
    HostId, Prompt, PromptRequest, PromptResponse, PromptSender,
};
use crate::persist;
use crate::{Error, Result};

/// Largest file that can be edited.
pub const MAX_EDIT_BYTES: u64 = 100 * 1024 * 1024;
/// Quiet time after the last file system event before a save is synced.
pub const DEBOUNCE: Duration = Duration::from_millis(300);
/// Delay before looking again when the local copy could not be read (e.g.
/// locked by the editor on Windows).
const REREAD_DELAY: Duration = Duration::from_secs(1);
/// Chunk size for uploads.
const CHUNK: usize = 256 * 1024;
const RECORD_VERSION: u32 = 1;
const RECOVERED_MESSAGE: &str = "Unsynced changes from a previous session";

/// Owns the edit sessions. Cheap to clone.
#[derive(Clone)]
pub struct EditManager {
    inner: Arc<Inner>,
}

struct Inner {
    fs: FileSystem,
    events: broadcast::Sender<CoreEvent>,
    prompts: PromptSender,
    root: PathBuf,
    /// In the order they were opened.
    sessions: Mutex<Vec<Arc<Session>>>,
    listening: std::sync::Once,
}

struct Session {
    id: Uuid,
    host_id: Option<HostId>,
    dir: PathBuf,
    local_path: PathBuf,
    record_path: PathBuf,
    tx: mpsc::UnboundedSender<Cmd>,
    shared: Mutex<Shared>,
}

struct Shared {
    info: EditSession,
    record: Record,
    watcher: Option<RecommendedWatcher>,
    task: Option<AbortHandle>,
    /// The last sync failed in a way reconnecting may fix.
    retry_on_connect: bool,
}

/// What was last synced, persisted as `<root>/<session id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    version: u32,
    host_id: Option<HostId>,
    remote_path: String,
    /// Where saves go (`remote_path` with symlinks resolved).
    target_path: String,
    file_name: String,
    /// The remote file after the last download or upload.
    remote_mtime: Option<i64>,
    remote_size: u64,
    /// SHA-1 (hex) of the content last downloaded or uploaded.
    synced_hash: String,
    synced_at: i64,
    last_upload: Option<i64>,
    /// Syncing was stopped with the local copy kept.
    stopped: bool,
}

impl Record {
    fn matches(&self, meta: &Meta) -> bool {
        self.remote_mtime == meta.mtime && self.remote_size == meta.size
    }
}

enum Cmd {
    /// Something happened to the local copy.
    Changed,
    /// Sync now; re-ask about a conflict; resume a stopped session.
    Retry,
    /// The session's host (re)connected.
    Connected,
    /// See [`EditManager::close`].
    Close {
        force: bool,
        reply: oneshot::Sender<Result<Option<String>>>,
    },
}

/// What the session task should do after a sync attempt.
enum Next {
    Idle,
    Again(Duration),
    Ask { remote: FileStat, local: FileStat },
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hash(data: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha1::digest(data))
}

fn too_large(name: &str, size: u64) -> Error {
    Error::Unsupported(format!(
        "{name} is too large to edit ({} MiB; the limit is {} MiB)",
        size.div_ceil(1024 * 1024),
        MAX_EDIT_BYTES / (1024 * 1024)
    ))
}

/// A file name that is valid on this machine.
fn local_file_name(name: &str) -> String {
    let invalid = |c: char| c.is_control() || (cfg!(windows) && r#"<>:"/\|?*"#.contains(c));
    let name: String = name
        .chars()
        .map(|c| if invalid(c) { '_' } else { c })
        .collect();
    let name = if cfg!(windows) {
        name.trim_end_matches(['.', ' ']).to_owned()
    } else {
        name
    };
    if name.is_empty() || name == "." || name == ".." {
        "file".to_owned()
    } else {
        name
    }
}

/// The local copy's content and mtime; `None` if it does not exist (an
/// editor between deleting and recreating it).
async fn read_local(path: &Path) -> Result<Option<(Vec<u8>, Option<i64>)>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let md = match std::fs::metadata(&path) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if md.len() > MAX_EDIT_BYTES {
            return Err(too_large(&path.to_string_lossy(), md.len()));
        }
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        match std::fs::read(&path) {
            Ok(data) => Ok(Some((data, mtime))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

/// Whether a file system event in the session directory may mean the local
/// copy was saved. Reads (including Patchbay's own) must not count.
fn is_relevant(event: &notify::Event, file_name: &str) -> bool {
    let kind_matters = match event.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(
            MetadataKind::AccessTime
            | MetadataKind::Permissions
            | MetadataKind::Ownership
            | MetadataKind::Extended,
        )) => false,
        _ => true,
    };
    kind_matters
        && (event.paths.is_empty()
            || event
                .paths
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == file_name)))
}

/// Whether reconnecting may fix a failed sync.
fn is_transient(e: &Error) -> bool {
    matches!(
        e,
        Error::NotConnected(_) | Error::Ssh(_) | Error::Sftp(_) | Error::Io(_) | Error::Timeout(_)
    )
}

impl EditManager {
    /// Sessions live below `root` (see [`default_root`](Self::default_root)).
    pub fn new(
        fs: FileSystem,
        events: broadcast::Sender<CoreEvent>,
        prompts: PromptSender,
        root: PathBuf,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                fs,
                events,
                prompts,
                root,
                sessions: Mutex::new(Vec::new()),
                listening: std::sync::Once::new(),
            }),
        }
    }

    /// `<cache_dir>/patchbay/edit`.
    pub fn default_root() -> Result<PathBuf> {
        let cache = dirs::cache_dir()
            .ok_or_else(|| Error::Other("cannot determine the cache directory".into()))?;
        Ok(cache.join("patchbay").join("edit"))
    }

    /// Download `path` from `host` (`None` = this machine) and start
    /// syncing it back on save. If the file is already being edited, its
    /// existing session is returned unchanged.
    pub async fn open(&self, host: Option<&str>, path: &str) -> Result<EditSession> {
        self.inner.ensure_listening();
        if let Some(existing) = self.inner.find_by_path(host, path) {
            return Ok(existing.info());
        }
        let ep = self.inner.fs.endpoint(host, true).await?;
        let target = ep.canonicalize(path).await?;
        let meta = ep
            .stat(&target)
            .await?
            .ok_or_else(|| Error::NotFound(path.to_owned()))?;
        if meta.kind != FileKind::File {
            return Err(Error::Unsupported(format!(
                "{path} is not a regular file; only files can be edited"
            )));
        }
        let name = ep.file_name(path);
        if meta.size > MAX_EDIT_BYTES {
            return Err(too_large(&name, meta.size));
        }
        let data = download(&ep, &target, meta.size).await?;

        let id = Uuid::new_v4();
        let dir = self.inner.root.join(id.to_string());
        let file_name = local_file_name(&name);
        let local_path = dir.join(&file_name);
        let now = now_ms();
        let record = Record {
            version: RECORD_VERSION,
            host_id: host.map(str::to_owned),
            remote_path: path.to_owned(),
            target_path: target,
            file_name,
            remote_mtime: meta.mtime,
            remote_size: meta.size,
            synced_hash: hash(&data),
            synced_at: now,
            last_upload: None,
            stopped: false,
        };
        let record_path = self.inner.root.join(format!("{id}.json"));
        {
            let (dir, local_path, record_path) =
                (dir.clone(), local_path.clone(), record_path.clone());
            let body = serde_json::to_vec_pretty(&record)?;
            tokio::task::spawn_blocking(move || -> Result<()> {
                std::fs::create_dir_all(&dir)?;
                std::fs::write(&local_path, &data)?;
                persist::write_atomic(&record_path, &body)
            })
            .await
            .map_err(|e| Error::Other(format!("background task failed: {e}")))??;
        }
        let state = EditState::Synced { at: now };
        let session = self
            .inner
            .add(id, dir, local_path, record_path, record, state, false);
        if let Err(e) = session.watch() {
            self.inner.remove(id);
            session.delete_files().await;
            return Err(e);
        }
        let info = session.info();
        self.inner.emit(info.clone());
        Ok(info)
    }

    /// All sessions, oldest first.
    pub fn list(&self) -> Vec<EditSession> {
        self.inner.sessions().iter().map(|s| s.info()).collect()
    }

    pub fn get(&self, id: Uuid) -> Option<EditSession> {
        self.inner.get(id).map(|s| s.info())
    }

    /// Sync now (after a failure), ask again about an unresolved conflict,
    /// or resume a stopped session.
    pub fn retry(&self, id: Uuid) -> Result<()> {
        self.inner.ensure_listening();
        let session = self
            .inner
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("edit session {id}")))?;
        let _ = session.tx.send(Cmd::Retry);
        Ok(())
    }

    /// Stop syncing and delete the local copy. If it has changes that did
    /// not reach the server and `force` is false, it is kept instead: the
    /// session stays listed as [`EditState::Stopped`] and the path of the
    /// kept copy is returned.
    pub async fn close(&self, id: Uuid, force: bool) -> Result<Option<String>> {
        let session = self
            .inner
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("edit session {id}")))?;
        let (reply, answer) = oneshot::channel();
        session
            .tx
            .send(Cmd::Close { force, reply })
            .map_err(|_| Error::Other("edit session is not running".into()))?;
        answer
            .await
            .map_err(|_| Error::Other("edit session is not running".into()))?
    }

    /// Pick up sessions left behind by a previous run: those without
    /// unsynced changes are deleted, the others are listed (and returned)
    /// as recovered. Call once at startup.
    pub async fn recover(&self) -> Vec<EditSession> {
        self.inner.ensure_listening();
        let root = self.inner.root.clone();
        let found = tokio::task::spawn_blocking(move || scan_root(&root))
            .await
            .unwrap_or_default();
        let mut recovered = Vec::new();
        for (id, record) in found {
            if self.inner.get(id).is_some() {
                continue;
            }
            let dir = self.inner.root.join(id.to_string());
            let local_path = dir.join(&record.file_name);
            let record_path = self.inner.root.join(format!("{id}.json"));
            let stopped = record.stopped;
            let state = EditState::Failed {
                message: RECOVERED_MESSAGE.into(),
            };
            let session = self
                .inner
                .add(id, dir, local_path, record_path, record, state, true);
            session.shared().info.dirty = true;
            if !stopped && let Err(e) = session.watch() {
                tracing::warn!(%id, "cannot watch recovered edit session: {e}");
            }
            let info = session.info();
            self.inner.emit(info.clone());
            recovered.push(info);
        }
        recovered
    }
}

/// Sessions below `root` whose local copy has unsynced changes; everything
/// else there is deleted.
fn scan_root(root: &Path) -> Vec<(Uuid, Record)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut records = HashMap::new();
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = name
            .strip_suffix(".json")
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            records.insert(id, path);
        } else if let Ok(id) = Uuid::parse_str(&name) {
            dirs.push((id, path));
        } else if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    let mut keep = Vec::new();
    for (id, dir) in dirs {
        let record = records
            .remove(&id)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<Record>(&b).ok());
        let local = record
            .as_ref()
            .and_then(|r| std::fs::read(dir.join(&r.file_name)).ok());
        match (record, local) {
            (Some(record), Some(data)) if hash(&data) != record.synced_hash => {
                keep.push((id, record));
            }
            _ => {
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::remove_file(root.join(format!("{id}.json")));
            }
        }
    }
    for (_, path) in records {
        let _ = std::fs::remove_file(path);
    }
    keep.sort_by_key(|(_, r)| r.synced_at);
    keep
}

async fn download(ep: &Endpoint, path: &str, size: u64) -> Result<Vec<u8>> {
    let mut reader = ep.reader(path, 0, size).await?;
    let mut data = Vec::with_capacity(size as usize);
    let result = async {
        while let Some(chunk) = reader.next().await? {
            data.extend_from_slice(&chunk);
            if data.len() as u64 > MAX_EDIT_BYTES {
                return Err(too_large(path, data.len() as u64));
            }
        }
        Ok(())
    }
    .await;
    reader.close().await;
    result.map(|()| data)
}

/// Write `data` to `path` (created or truncated).
async fn write_file(ep: &Endpoint, path: &str, data: &[u8]) -> Result<()> {
    let mut writer = ep.writer(path, 0).await?;
    let mut acked = |_| {};
    for chunk in data.chunks(CHUNK) {
        if let Err(e) = writer.write(chunk.to_vec(), &mut acked).await {
            writer.abort().await;
            return Err(e);
        }
    }
    writer.finish(&mut acked).await
}

/// Replace the file at `target` (currently described by `existing`) with
/// `data`, as described in the module docs.
async fn replace_contents(
    ep: &Endpoint,
    target: &str,
    existing: Option<&Meta>,
    data: &[u8],
) -> Result<()> {
    let in_place = async || write_file(ep, target, data).await;
    let Some(dir) = ep.parent(target) else {
        return in_place().await;
    };
    let name = ep.file_name(target);
    let suffix = &Uuid::new_v4().simple().to_string()[..8];
    let tmp = ep.join(&dir, &format!(".{name}.patchbay-{suffix}.tmp"));
    match write_file(ep, &tmp, data).await {
        Ok(()) => {}
        Err(Error::PermissionDenied(_)) => return in_place().await,
        Err(e) => {
            let _ = ep.remove_file(&tmp).await;
            return Err(e);
        }
    }
    let result = async {
        if let Some(existing) = existing {
            let created = ep.lstat(&tmp).await?;
            let same_owner = created.is_none_or(|c| (c.uid, c.gid) == (existing.uid, existing.gid));
            if !same_owner {
                return Ok(false);
            }
            if ep.has_unix_permissions()
                && let Some(perms) = existing.perms
            {
                ep.set_times_and_mode(&tmp, None, Some(perms & 0o7777))
                    .await?;
            }
        }
        match ep.replace(&tmp, target).await {
            Ok(()) => Ok(true),
            Err(Error::PermissionDenied(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }
    .await;
    match result {
        Ok(true) => Ok(()),
        Ok(false) => {
            let _ = ep.remove_file(&tmp).await;
            in_place().await
        }
        Err(e) => {
            let _ = ep.remove_file(&tmp).await;
            Err(e)
        }
    }
}

impl Inner {
    fn sessions(&self) -> MutexGuard<'_, Vec<Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, id: Uuid) -> Option<Arc<Session>> {
        self.sessions().iter().find(|s| s.id == id).cloned()
    }

    fn find_by_path(&self, host: Option<&str>, path: &str) -> Option<Arc<Session>> {
        self.sessions()
            .iter()
            .find(|s| s.host_id.as_deref() == host && s.shared().record.remote_path == path)
            .cloned()
    }

    fn remove(&self, id: Uuid) {
        self.sessions().retain(|s| s.id != id);
    }

    fn emit(&self, info: EditSession) {
        let _ = self.events.send(CoreEvent::Edit(info));
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        self: &Arc<Self>,
        id: Uuid,
        dir: PathBuf,
        local_path: PathBuf,
        record_path: PathBuf,
        record: Record,
        state: EditState,
        recovered: bool,
    ) -> Arc<Session> {
        let (tx, rx) = mpsc::unbounded_channel();
        let info = EditSession {
            id,
            host_id: record.host_id.clone(),
            remote_path: record.remote_path.clone(),
            local_path: local_path.to_string_lossy().into_owned(),
            dirty: false,
            recovered,
            last_upload: record.last_upload,
            state,
        };
        let session = Arc::new(Session {
            id,
            host_id: record.host_id.clone(),
            dir,
            local_path,
            record_path,
            tx,
            shared: Mutex::new(Shared {
                info,
                record,
                watcher: None,
                task: None,
                retry_on_connect: false,
            }),
        });
        let task = tokio::spawn(session.clone().run(Arc::downgrade(self), rx));
        session.shared().task = Some(task.abort_handle());
        self.sessions().push(session.clone());
        session
    }

    /// Retry failed syncs when their host connects again.
    fn ensure_listening(self: &Arc<Self>) {
        self.listening.call_once(|| {
            let mut events = self.events.subscribe();
            let inner = Arc::downgrade(self);
            tokio::spawn(async move {
                loop {
                    let host = match events.recv().await {
                        Ok(CoreEvent::Connection {
                            host_id,
                            state: ConnectionState::Connected { .. },
                        }) => host_id,
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    let Some(inner) = inner.upgrade() else { break };
                    for s in inner.sessions().iter() {
                        if s.host_id.as_deref() == Some(host.as_str()) {
                            let _ = s.tx.send(Cmd::Connected);
                        }
                    }
                }
            });
        });
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        for s in self.sessions().iter() {
            s.stop_task();
            s.unwatch();
        }
    }
}

impl Session {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn info(&self) -> EditSession {
        self.shared().info.clone()
    }

    fn file_name(&self) -> String {
        self.shared().record.file_name.clone()
    }

    /// Apply `f` and publish the result.
    fn update(&self, inner: &Inner, f: impl FnOnce(&mut Shared)) {
        let info = {
            let mut shared = self.shared();
            f(&mut shared);
            shared.info.last_upload = shared.record.last_upload;
            shared.info.clone()
        };
        inner.emit(info);
    }

    fn set_state(&self, inner: &Inner, state: EditState) {
        self.update(inner, |s| s.info.state = state);
    }

    async fn save_record(&self) {
        let record = self.shared().record.clone();
        let path = self.record_path.clone();
        let result = tokio::task::spawn_blocking(move || {
            persist::write_atomic(&path, &serde_json::to_vec_pretty(&record)?)
        })
        .await;
        if let Ok(Err(e)) = result {
            tracing::warn!(session = %self.id, "cannot save edit session record: {e}");
        }
    }

    async fn delete_files(&self) {
        let (dir, record) = (self.dir.clone(), self.record_path.clone());
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(e) = std::fs::remove_dir_all(&dir)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("cannot delete {}: {e}", dir.display());
            }
            let _ = std::fs::remove_file(record);
        })
        .await;
    }

    /// Watch the session directory for saves.
    fn watch(&self) -> Result<()> {
        let tx = self.tx.clone();
        let file_name = self.file_name();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let relevant = match &event {
                    Ok(event) => is_relevant(event, &file_name),
                    Err(_) => true,
                };
                if relevant {
                    let _ = tx.send(Cmd::Changed);
                }
            })
            .map_err(|e| Error::Other(format!("cannot watch {}: {e}", self.dir.display())))?;
        watcher
            .watch(&self.dir, RecursiveMode::NonRecursive)
            .map_err(|e| Error::Other(format!("cannot watch {}: {e}", self.dir.display())))?;
        self.shared().watcher = Some(watcher);
        Ok(())
    }

    fn unwatch(&self) {
        let watcher = self.shared().watcher.take();
        drop(watcher);
    }

    fn stop_task(&self) {
        if let Some(task) = self.shared().task.take() {
            task.abort();
        }
    }

    async fn run(self: Arc<Self>, inner: Weak<Inner>, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        let mut deadline: Option<Instant> = None;
        let mut answer: Option<oneshot::Receiver<PromptResponse>> = None;
        loop {
            let asking = answer.is_some();
            let event = tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => Event::Cmd(cmd),
                    None => break,
                },
                () = sleep_until(deadline) => Event::Due,
                response = wait_answer(answer.as_mut()) => Event::Answer(response),
            };
            let Some(inner) = inner.upgrade() else { break };
            let next = match event {
                Event::Cmd(Cmd::Close { force, reply }) => {
                    answer = None;
                    deadline = None;
                    let result = self.close(&inner, force).await;
                    let removed = matches!(result, Ok(None));
                    let _ = reply.send(result);
                    if removed {
                        break;
                    }
                    continue;
                }
                Event::Cmd(Cmd::Changed) => {
                    let stopped = self.shared().info.state == EditState::Stopped;
                    if !asking && !stopped {
                        deadline = Some(Instant::now() + DEBOUNCE);
                    }
                    continue;
                }
                Event::Cmd(Cmd::Retry) => {
                    answer = None;
                    deadline = None;
                    if let Err(e) = self.resume() {
                        self.fail(&inner, e);
                        continue;
                    }
                    self.sync(&inner, false).await
                }
                Event::Cmd(Cmd::Connected) => {
                    let wanted = {
                        let s = self.shared();
                        s.retry_on_connect && matches!(s.info.state, EditState::Failed { .. })
                    };
                    if !wanted || asking {
                        continue;
                    }
                    deadline = None;
                    self.sync(&inner, false).await
                }
                Event::Due => {
                    deadline = None;
                    self.sync(&inner, false).await
                }
                Event::Answer(response) => {
                    answer = None;
                    match response {
                        Some(PromptResponse::EditConflict { resolution }) => {
                            self.resolve(&inner, resolution).await
                        }
                        // Dismissed: stay in conflict until retried.
                        _ => Next::Idle,
                    }
                }
            };
            match next {
                Next::Idle => {}
                Next::Again(delay) => deadline = Some(Instant::now() + delay),
                Next::Ask { remote, local } => answer = Some(self.ask(&inner, remote, local)),
            }
        }
    }

    /// Watch again if syncing was stopped.
    fn resume(&self) -> Result<()> {
        if self.shared().watcher.is_none() {
            self.watch()?;
        }
        self.shared().record.stopped = false;
        Ok(())
    }

    async fn close(&self, inner: &Inner, force: bool) -> Result<Option<String>> {
        self.unwatch();
        let dirty = match read_local(&self.local_path).await {
            Ok(Some((data, _))) => hash(&data) != self.shared().record.synced_hash,
            Ok(None) => false,
            Err(_) => true,
        };
        if dirty && !force {
            self.update(inner, |s| {
                s.record.stopped = true;
                s.info.dirty = true;
                s.info.state = EditState::Stopped;
            });
            self.save_record().await;
            return Ok(Some(self.local_path.to_string_lossy().into_owned()));
        }
        inner.remove(self.id);
        self.delete_files().await;
        let mut info = self.info();
        info.state = EditState::Closed;
        inner.emit(info);
        Ok(None)
    }

    fn fail(&self, inner: &Inner, e: Error) {
        tracing::info!(session = %self.id, "edit sync failed: {e}");
        let transient = is_transient(&e);
        self.update(inner, |s| {
            s.retry_on_connect = transient;
            s.info.state = EditState::Failed {
                message: e.to_string(),
            };
        });
    }

    /// Upload the local copy if it changed. With `force`, a changed remote
    /// file is overwritten instead of reported as a conflict.
    async fn sync(&self, inner: &Inner, force: bool) -> Next {
        let (data, mtime) = match read_local(&self.local_path).await {
            Ok(Some(local)) => local,
            // Mid-save (deleted, not yet recreated): the next event follows.
            Ok(None) => return Next::Idle,
            Err(e @ Error::Unsupported(_)) => {
                self.fail(inner, e);
                return Next::Idle;
            }
            Err(e) => {
                tracing::debug!(session = %self.id, "cannot read local copy yet: {e}");
                return Next::Again(REREAD_DELAY);
            }
        };
        let data_hash = hash(&data);
        let (unchanged, recovered) = {
            let s = self.shared();
            (s.record.synced_hash == data_hash, s.info.recovered)
        };
        if unchanged {
            self.update(inner, |s| {
                s.info.dirty = false;
                s.info.recovered = false;
                if !matches!(s.info.state, EditState::Synced { .. }) {
                    s.info.state = EditState::Synced {
                        at: s.record.synced_at,
                    };
                }
            });
            if recovered {
                self.save_record().await;
            }
            return Next::Idle;
        }
        self.update(inner, |s| {
            s.info.dirty = true;
            s.info.state = EditState::Uploading;
        });
        match self.upload(inner, &data, &data_hash, force).await {
            Ok(None) => Next::Idle,
            Ok(Some(remote)) => {
                self.set_state(inner, EditState::Conflict);
                let local = FileStat {
                    size: data.len() as u64,
                    modified: mtime,
                };
                Next::Ask { remote, local }
            }
            Err(e) => {
                self.fail(inner, e);
                Next::Idle
            }
        }
    }

    /// Returns the remote file's stat instead of uploading if it changed
    /// since the last sync (unless `force`).
    async fn upload(
        &self,
        inner: &Inner,
        data: &[u8],
        data_hash: &str,
        force: bool,
    ) -> Result<Option<FileStat>> {
        let (target, record) = {
            let s = self.shared();
            (s.record.target_path.clone(), s.record.clone())
        };
        let ep = inner.fs.endpoint(self.host_id.as_deref(), true).await?;
        let existing = ep.stat(&target).await?;
        if let Some(meta) = &existing
            && !force
            && !record.matches(meta)
        {
            return Ok(Some(FileStat {
                size: meta.size,
                modified: meta.mtime,
            }));
        }
        replace_contents(&ep, &target, existing.as_ref(), data).await?;
        let meta = ep.stat(&target).await?;
        let now = now_ms();
        self.update(inner, |s| {
            s.record.synced_hash = data_hash.to_owned();
            s.record.synced_at = now;
            s.record.last_upload = Some(now);
            s.record.remote_mtime = meta.and_then(|m| m.mtime);
            s.record.remote_size = meta.map_or(data.len() as u64, |m| m.size);
            s.retry_on_connect = false;
            s.info.dirty = false;
            s.info.recovered = false;
            s.info.state = EditState::Synced { at: now };
        });
        self.save_record().await;
        Ok(None)
    }

    fn ask(
        &self,
        inner: &Inner,
        remote: FileStat,
        local: FileStat,
    ) -> oneshot::Receiver<PromptResponse> {
        let (reply, answer) = oneshot::channel();
        let request = PromptRequest {
            id: Uuid::new_v4(),
            host_id: self.host_id.clone(),
            prompt: Prompt::EditConflict {
                session_id: self.id,
                remote_path: self.shared().record.remote_path.clone(),
                remote,
                local,
            },
            reply,
        };
        let prompts = inner.prompts.clone();
        tokio::spawn(async move {
            let _ = prompts.send(request).await;
        });
        answer
    }

    async fn resolve(&self, inner: &Inner, resolution: EditConflictResolution) -> Next {
        match resolution {
            EditConflictResolution::Overwrite => self.sync(inner, true).await,
            EditConflictResolution::DiscardLocal => {
                if let Err(e) = self.download_again(inner).await {
                    self.fail(inner, e);
                }
                Next::Idle
            }
            EditConflictResolution::KeepLocal => {
                self.unwatch();
                self.update(inner, |s| {
                    s.record.stopped = true;
                    s.info.state = EditState::Stopped;
                });
                self.save_record().await;
                Next::Idle
            }
        }
    }

    /// Replace the local copy with the server's version.
    async fn download_again(&self, inner: &Inner) -> Result<()> {
        self.set_state(inner, EditState::Syncing);
        let target = self.shared().record.target_path.clone();
        let ep = inner.fs.endpoint(self.host_id.as_deref(), true).await?;
        let meta = ep
            .stat(&target)
            .await?
            .ok_or_else(|| Error::NotFound(target.clone()))?;
        if meta.size > MAX_EDIT_BYTES {
            return Err(too_large(&target, meta.size));
        }
        let data = download(&ep, &target, meta.size).await?;
        let data_hash = hash(&data);
        let local = self.local_path.clone();
        tokio::task::spawn_blocking(move || std::fs::write(local, data))
            .await
            .map_err(|e| Error::Other(format!("background task failed: {e}")))??;
        let now = now_ms();
        self.update(inner, |s| {
            s.record.synced_hash = data_hash;
            s.record.synced_at = now;
            s.record.remote_mtime = meta.mtime;
            s.record.remote_size = meta.size;
            s.retry_on_connect = false;
            s.info.dirty = false;
            s.info.recovered = false;
            s.info.state = EditState::Synced { at: now };
        });
        self.save_record().await;
        Ok(())
    }
}

enum Event {
    Cmd(Cmd),
    Due,
    Answer(Option<PromptResponse>),
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn wait_answer(
    answer: Option<&mut oneshot::Receiver<PromptResponse>>,
) -> Option<PromptResponse> {
    match answer {
        Some(answer) => answer.await.ok(),
        None => std::future::pending().await,
    }
}
