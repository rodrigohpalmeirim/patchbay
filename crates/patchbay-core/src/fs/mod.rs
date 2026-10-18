//! File operations on the local machine (`host: None`) and on connected
//! hosts over SFTP (`host: Some(id)`), behind one API: [`FileSystem`].
//!
//! Remote operations never connect on their own: they need a live
//! connection from [`SessionManager::get`], else they fail with
//! [`Error::NotConnected`]. Per connection, two SFTP channels are opened
//! lazily and cached: one for browsing and one shared by all transfers, so a
//! bulk copy does not queue directory listings behind megabytes of writes.
//! Channels are reopened when the connection was replaced or a channel hit a
//! transport error.

mod local;
pub mod path;
pub(crate) mod sftp;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use futures_util::stream::{self, StreamExt, TryStreamExt};
use russh_sftp::protocol::FileAttributes;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use self::path::remote as rpath;
use self::sftp::{NameCache, PipelinedReader, PipelinedWriter, Sftp};
use crate::model::{DirListing, FileEntry, FileKind, HostId, RemoteOs};
use crate::session::{Connection, SessionManager};
use crate::{Error, Result};

/// Upper bound for [`FileSystem::read`].
pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;
/// SFTP request timeout for browsing.
const BROWSE_TIMEOUT_SECS: u64 = 30;
/// SFTP request timeout for transfers: with a deep pipeline on a slow link a
/// request can legitimately wait behind megabytes of data.
const BULK_TIMEOUT_SECS: u64 = 120;
/// Concurrent requests for per-entry operations (delete, chmod, chown).
const TREE_CONCURRENCY: usize = 32;
/// Local read size for transfers.
const LOCAL_CHUNK: usize = 256 * 1024;

/// Lstat-style metadata, local or remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Meta {
    pub kind: FileKind,
    pub size: u64,
    pub mtime: Option<i64>,
    pub perms: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// File operations on any machine. Cheap to clone.
#[derive(Clone)]
pub struct FileSystem {
    inner: Arc<Inner>,
}

struct Inner {
    sessions: SessionManager,
    slots: Mutex<HashMap<HostId, Arc<tokio::sync::Mutex<Slot>>>>,
}

/// Cached SFTP channels of one connection.
#[derive(Default)]
struct Slot {
    conn: Weak<Connection>,
    browse: Option<Arc<Sftp>>,
    bulk: Option<Arc<Sftp>>,
    names: Arc<NameCache>,
}

/// An SFTP channel on a live connection.
#[derive(Clone)]
pub(crate) struct Remote {
    pub conn: Arc<Connection>,
    pub sftp: Arc<Sftp>,
    pub names: Arc<NameCache>,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

impl FileSystem {
    pub fn new(sessions: SessionManager) -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions,
                slots: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn sessions(&self) -> &SessionManager {
        &self.inner.sessions
    }

    /// The browsing (`bulk == false`) or transfer SFTP channel for `host`.
    pub(crate) async fn remote(&self, host: &str, bulk: bool) -> Result<Remote> {
        let conn = self
            .inner
            .sessions
            .get(host)
            .ok_or_else(|| Error::NotConnected(host.to_owned()))?;
        let slot = {
            let mut slots = self.inner.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots.retain(|key, slot| {
                key == host || slot.try_lock().map_or(true, |s| s.conn.strong_count() > 0)
            });
            slots.entry(host.to_owned()).or_default().clone()
        };
        let mut slot = slot.lock().await;
        if !std::ptr::eq(slot.conn.as_ptr(), Arc::as_ptr(&conn)) {
            *slot = Slot {
                conn: Arc::downgrade(&conn),
                ..Slot::default()
            };
        }
        let names = slot.names.clone();
        let cell = if bulk {
            &mut slot.bulk
        } else {
            &mut slot.browse
        };
        let sftp = match cell {
            Some(sftp) if !sftp.is_broken() => sftp.clone(),
            _ => {
                let timeout = if bulk {
                    BULK_TIMEOUT_SECS
                } else {
                    BROWSE_TIMEOUT_SECS
                };
                let sftp = Arc::new(Sftp::open(&conn, timeout).await?);
                *cell = Some(sftp.clone());
                sftp
            }
        };
        Ok(Remote { conn, sftp, names })
    }

    pub(crate) async fn endpoint(&self, host: Option<&str>, bulk: bool) -> Result<Endpoint> {
        Ok(match host {
            None => Endpoint::Local,
            Some(host) => Endpoint::Remote(self.remote(host, bulk).await?),
        })
    }

    /// Home directory (remote: SFTP `realpath(".")`).
    pub async fn home(&self, host: Option<&str>) -> Result<String> {
        match host {
            None => local::home(),
            Some(host) => self.remote(host, false).await?.sftp.realpath(".").await,
        }
    }

    /// List a directory. Locally on Windows, `""` lists the drives.
    pub async fn list(&self, host: Option<&str>, path: &str) -> Result<DirListing> {
        let Some(host) = host else {
            let path = path.to_owned();
            return blocking(move || local::list(&path)).await;
        };
        let r = self.remote(host, false).await?;
        let path = r
            .sftp
            .realpath(if path.is_empty() { "." } else { path })
            .await?;
        let entries = r.sftp.list(&path, &r.names).await?;
        Ok(DirListing {
            parent: rpath::parent(&path),
            path,
            entries,
        })
    }

    /// Metadata of one path (symlinks are not followed, but their target is
    /// described).
    pub async fn stat(&self, host: Option<&str>, path: &str) -> Result<FileEntry> {
        self.endpoint(host, false).await?.describe(path).await
    }

    pub async fn mkdir(&self, host: Option<&str>, path: &str) -> Result<()> {
        match host {
            None => {
                let path = path.to_owned();
                blocking(move || local::mkdir(&path)).await
            }
            Some(host) => self.remote(host, false).await?.sftp.mkdir(path).await,
        }
    }

    /// Create an empty file; fails with [`Error::AlreadyExists`] if the path exists.
    pub async fn create_file(&self, host: Option<&str>, path: &str) -> Result<()> {
        match host {
            None => {
                let path = path.to_owned();
                blocking(move || local::create_file(&path)).await
            }
            Some(host) => self.remote(host, false).await?.sftp.create_new(path).await,
        }
    }

    /// Rename / move within one machine; never replaces an existing target.
    pub async fn rename(&self, host: Option<&str>, from: &str, to: &str) -> Result<()> {
        match host {
            None => {
                let (from, to) = (from.to_owned(), to.to_owned());
                blocking(move || local::rename(&from, &to)).await
            }
            Some(host) => self.remote(host, false).await?.sftp.rename(from, to).await,
        }
    }

    /// Delete files, symlinks and whole directory trees. Symlinks are
    /// removed, never followed.
    pub async fn delete(&self, host: Option<&str>, paths: &[String]) -> Result<()> {
        let Some(host) = host else {
            let paths = paths.to_vec();
            return blocking(move || paths.iter().try_for_each(|p| local::delete(p))).await;
        };
        let r = self.remote(host, false).await?;
        for path in paths {
            let meta = r
                .sftp
                .lstat(path)
                .await?
                .ok_or_else(|| Error::NotFound(path.clone()))?;
            if meta.kind == FileKind::Dir {
                delete_tree(&r.sftp, path).await?;
            } else {
                r.sftp.remove_file(path).await?;
            }
        }
        Ok(())
    }

    /// Set permission bits (`mode & 0o7777`) on files and directories; with
    /// `recursive`, on everything below directories too (without following
    /// symlinks).
    pub async fn chmod(
        &self,
        host: Option<&str>,
        paths: &[String],
        mode: u32,
        recursive: bool,
    ) -> Result<()> {
        let Some(host) = host else {
            let paths = paths.to_vec();
            return blocking(move || {
                paths
                    .iter()
                    .try_for_each(|p| local::chmod(p, mode, recursive))
            })
            .await;
        };
        let r = self.remote(host, false).await?;
        let op = AttrOp::Mode(mode & 0o7777);
        for path in paths {
            apply_attrs(&r.sftp, path, &op, recursive).await?;
        }
        Ok(())
    }

    /// Change owner and/or group, given as numeric ids or names. Remote names
    /// are resolved from earlier listings or, when the host has a POSIX shell,
    /// with `id -u` / `getent group`.
    pub async fn chown(
        &self,
        host: Option<&str>,
        paths: &[String],
        owner: Option<&str>,
        group: Option<&str>,
        recursive: bool,
    ) -> Result<()> {
        if owner.is_none() && group.is_none() {
            return Ok(());
        }
        let Some(host) = host else {
            let paths = paths.to_vec();
            let (owner, group) = (owner.map(str::to_owned), group.map(str::to_owned));
            return blocking(move || {
                paths.iter().try_for_each(|p| {
                    local::chown(p, owner.as_deref(), group.as_deref(), recursive)
                })
            })
            .await;
        };
        let r = self.remote(host, false).await?;
        let uid = match owner {
            Some(name) => Some(resolve_id(&r, name, false).await?),
            None => None,
        };
        let gid = match group {
            Some(name) => Some(resolve_id(&r, name, true).await?),
            None => None,
        };
        let op = AttrOp::Owner { uid, gid };
        for path in paths {
            apply_attrs(&r.sftp, path, &op, recursive).await?;
        }
        Ok(())
    }

    /// Up to `max_bytes` (capped at [`MAX_READ_BYTES`]) from the start of a file.
    pub async fn read(&self, host: Option<&str>, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let max = max_bytes.min(MAX_READ_BYTES);
        let Some(host) = host else {
            let path = path.to_owned();
            return blocking(move || local::read_head(&path, max)).await;
        };
        let r = self.remote(host, false).await?;
        let handle = r.sftp.open_read(path).await?;
        let mut reader = PipelinedReader::new(r.sftp.clone(), handle, 0, max, Some(max));
        let mut out = Vec::new();
        let result = async {
            while let Some(chunk) = reader.next().await? {
                out.extend_from_slice(&chunk);
            }
            Ok(())
        }
        .await;
        reader.close().await;
        result.map(|()| out)
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

fn delete_tree<'a>(sftp: &'a Sftp, dir: &'a str) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        let entries = sftp.read_dir(dir).await?;
        let (dirs, others): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|e| e.meta.kind == FileKind::Dir);
        stream::iter(others)
            .map(|e| async move { sftp.remove_file(&rpath::join(dir, &e.name)).await })
            .buffer_unordered(TREE_CONCURRENCY)
            .try_collect::<()>()
            .await?;
        for d in dirs {
            delete_tree(sftp, &rpath::join(dir, &d.name)).await?;
        }
        sftp.remove_dir(dir).await
    })
}

enum AttrOp {
    Mode(u32),
    Owner { uid: Option<u32>, gid: Option<u32> },
}

impl AttrOp {
    /// SETSTAT attributes for an entry currently described by `meta`. SFTP v3
    /// sets uid and gid together, so a missing one is taken from `meta`.
    fn attrs(&self, meta: &Meta) -> Option<FileAttributes> {
        let mut attrs = FileAttributes::empty();
        match *self {
            AttrOp::Mode(mode) => attrs.permissions = Some(mode),
            AttrOp::Owner { uid, gid } => {
                attrs.uid = Some(uid.or(meta.uid)?);
                attrs.gid = Some(gid.or(meta.gid)?);
            }
        }
        Some(attrs)
    }

    fn pre_order(&self) -> bool {
        match *self {
            AttrOp::Mode(mode) => local::mode_keeps_dir_traversable(mode),
            AttrOp::Owner { .. } => true,
        }
    }
}

async fn set_with(sftp: &Sftp, path: &str, op: &AttrOp, meta: &Meta) -> Result<()> {
    let attrs = op
        .attrs(meta)
        .ok_or_else(|| Error::Other(format!("{path}: server did not report owner ids")))?;
    sftp.set_attrs(path, attrs).await
}

async fn apply_attrs(sftp: &Sftp, path: &str, op: &AttrOp, recursive: bool) -> Result<()> {
    let target = sftp
        .stat(path)
        .await?
        .ok_or_else(|| Error::NotFound(path.to_owned()))?;
    let recurse = recursive
        && sftp
            .lstat(path)
            .await?
            .is_some_and(|m| m.kind == FileKind::Dir);
    if !recurse {
        return set_with(sftp, path, op, &target).await;
    }
    if op.pre_order() {
        set_with(sftp, path, op, &target).await?;
        apply_below(sftp, path, op).await
    } else {
        apply_below(sftp, path, op).await?;
        set_with(sftp, path, op, &target).await
    }
}

fn apply_below<'a>(sftp: &'a Sftp, dir: &'a str, op: &'a AttrOp) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        let entries = sftp.read_dir(dir).await?;
        let (dirs, files): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .filter(|e| e.meta.kind != FileKind::Symlink)
            .partition(|e| e.meta.kind == FileKind::Dir);
        stream::iter(files)
            .map(|e| async move { set_with(sftp, &rpath::join(dir, &e.name), op, &e.meta).await })
            .buffer_unordered(TREE_CONCURRENCY)
            .try_collect::<()>()
            .await?;
        for d in dirs {
            let path = rpath::join(dir, &d.name);
            if op.pre_order() {
                set_with(sftp, &path, op, &d.meta).await?;
                apply_below(sftp, &path, op).await?;
            } else {
                apply_below(sftp, &path, op).await?;
                set_with(sftp, &path, op, &d.meta).await?;
            }
        }
        Ok(())
    })
}

/// Numeric id for a user/group name on a remote host.
async fn resolve_id(r: &Remote, name: &str, group: bool) -> Result<u32> {
    if let Ok(id) = name.parse() {
        return Ok(id);
    }
    let cached = if group {
        r.names.gid_of(name)
    } else {
        r.names.uid_of(name)
    };
    if let Some(id) = cached {
        return Ok(id);
    }
    let what = if group { "group" } else { "user" };
    let caps = r.conn.capabilities();
    if !caps.exec || caps.os == RemoteOs::Windows {
        return Err(Error::Unsupported(format!(
            "looking up {what} names needs a shell on the remote host; use a numeric id"
        )));
    }
    let q = shell_quote(name);
    let command = if group {
        format!(
            "g=$(getent group {q} 2>/dev/null | cut -d: -f3); \
             [ -n \"$g\" ] || g=$(dscl . -read /Groups/{q} PrimaryGroupID 2>/dev/null | awk '{{print $2}}'); \
             echo \"$g\""
        )
    } else {
        format!("id -u {q}")
    };
    let out = r.conn.exec(&command).await?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| Error::NotFound(format!("{what} {name}")))
}

/// Quote `s` for a POSIX shell.
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// One side of a transfer: the local disk or an SFTP channel.
pub(crate) enum Endpoint {
    Local,
    Remote(Remote),
}

impl Endpoint {
    pub(crate) fn join(&self, dir: &str, name: &str) -> String {
        match self {
            Endpoint::Local => path::local::join(dir, name),
            Endpoint::Remote(_) => rpath::join(dir, name),
        }
    }

    pub(crate) fn file_name(&self, p: &str) -> String {
        match self {
            Endpoint::Local => path::local::file_name(p),
            Endpoint::Remote(_) => rpath::file_name(p).to_owned(),
        }
    }

    pub(crate) fn parent(&self, p: &str) -> Option<String> {
        match self {
            Endpoint::Local => path::local::parent(p),
            Endpoint::Remote(_) => rpath::parent(p),
        }
    }

    pub(crate) fn is_within(&self, p: &str, ancestor: &str) -> bool {
        match self {
            Endpoint::Local => path::local::is_within(p, ancestor),
            Endpoint::Remote(_) => rpath::is_within(p, ancestor),
        }
    }

    fn remote_os(&self) -> RemoteOs {
        match self {
            Endpoint::Local => RemoteOs::Unknown,
            Endpoint::Remote(r) => r.conn.capabilities().os,
        }
    }

    /// Symlinks can be created here.
    pub(crate) fn supports_symlinks(&self) -> bool {
        match self {
            Endpoint::Local => cfg!(unix),
            Endpoint::Remote(_) => self.remote_os() != RemoteOs::Windows,
        }
    }

    /// Unix permission bits are meaningful here.
    pub(crate) fn has_unix_permissions(&self) -> bool {
        match self {
            Endpoint::Local => cfg!(unix),
            Endpoint::Remote(_) => self.remote_os() != RemoteOs::Windows,
        }
    }

    pub(crate) async fn lstat(&self, p: &str) -> Result<Option<Meta>> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::lstat(&p)).await
            }
            Endpoint::Remote(r) => r.sftp.lstat(p).await,
        }
    }

    pub(crate) async fn stat(&self, p: &str) -> Result<Option<Meta>> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::stat_meta(&p)).await
            }
            Endpoint::Remote(r) => r.sftp.stat(p).await,
        }
    }

    /// Names and lstat metadata of a directory's entries.
    pub(crate) async fn read_dir(&self, p: &str) -> Result<Vec<(String, Meta)>> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::read_dir_meta(&p)).await
            }
            Endpoint::Remote(r) => Ok(r
                .sftp
                .read_dir(p)
                .await?
                .into_iter()
                .map(|e| (e.name, e.meta))
                .collect()),
        }
    }

    /// Describe one path like [`FileSystem::stat`].
    pub(crate) async fn describe(&self, p: &str) -> Result<FileEntry> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::stat(&p)).await
            }
            Endpoint::Remote(r) => {
                let meta = r
                    .sftp
                    .lstat(p)
                    .await?
                    .ok_or_else(|| Error::NotFound(p.to_owned()))?;
                let name = rpath::file_name(p).to_owned();
                let mut entry = [r.sftp.entry(name, p.to_owned(), meta, &r.names)];
                r.sftp.resolve_links(&mut entry).await;
                let [entry] = entry;
                Ok(entry)
            }
        }
    }

    /// One directory of a search walk: the paths of its subdirectories (not
    /// symlinks to directories) and the entries whose name satisfies `want`.
    pub(crate) async fn scan_dir(
        &self,
        dir: &str,
        want: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Result<(Vec<String>, Vec<FileEntry>)> {
        match self {
            Endpoint::Local => {
                let dir = dir.to_owned();
                blocking(move || local::scan_dir(&dir, &*want)).await
            }
            Endpoint::Remote(r) => {
                let mut subdirs = Vec::new();
                let mut matches = Vec::new();
                for entry in r.sftp.read_dir(dir).await? {
                    entry.learn_names(&r.names);
                    let path = rpath::join(dir, &entry.name);
                    if entry.meta.kind == FileKind::Dir {
                        subdirs.push(path.clone());
                    }
                    if want(&entry.name) {
                        matches.push(r.sftp.entry(entry.name, path, entry.meta, &r.names));
                    }
                }
                r.sftp.resolve_links(&mut matches).await;
                Ok((subdirs, matches))
            }
        }
    }

    /// Canonical absolute form of `p`.
    pub(crate) async fn canonicalize(&self, p: &str) -> Result<String> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::canonical(&p)).await
            }
            Endpoint::Remote(r) => r.sftp.realpath(p).await,
        }
    }

    pub(crate) async fn read_link(&self, p: &str) -> Result<String> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::read_link(&p)).await
            }
            Endpoint::Remote(r) => r.sftp.read_link(p).await,
        }
    }

    pub(crate) async fn mkdir(&self, p: &str) -> Result<()> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::mkdir(&p)).await
            }
            Endpoint::Remote(r) => r.sftp.mkdir(p).await,
        }
    }

    pub(crate) async fn remove_file(&self, p: &str) -> Result<()> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::remove_file(&p)).await
            }
            Endpoint::Remote(r) => r.sftp.remove_file(p).await,
        }
    }

    pub(crate) async fn symlink(&self, link: &str, target: &str) -> Result<()> {
        match self {
            Endpoint::Local => {
                let (link, target) = (link.to_owned(), target.to_owned());
                blocking(move || local::symlink(&link, &target)).await
            }
            Endpoint::Remote(r) => r.sftp.symlink(link, target).await,
        }
    }

    pub(crate) async fn set_times_and_mode(
        &self,
        p: &str,
        mtime: Option<i64>,
        perms: Option<u32>,
    ) -> Result<()> {
        match self {
            Endpoint::Local => {
                let p = p.to_owned();
                blocking(move || local::set_times_and_mode(&p, mtime, perms)).await
            }
            Endpoint::Remote(r) => r.sftp.set_times_and_mode(p, mtime, perms).await,
        }
    }

    /// Move `from` to `to`, replacing a file at `to`.
    pub(crate) async fn replace(&self, from: &str, to: &str) -> Result<()> {
        match self {
            Endpoint::Local => {
                let (from, to) = (from.to_owned(), to.to_owned());
                blocking(move || local::replace(&from, &to)).await
            }
            Endpoint::Remote(r) => r.sftp.replace(from, to).await,
        }
    }

    /// Open `p` for reading from `offset`. Remote reads stop at `size` (the
    /// size seen when scanning), saving an EOF probe per file.
    pub(crate) async fn reader(&self, p: &str, offset: u64, size: u64) -> Result<Reader> {
        match self {
            Endpoint::Local => {
                let mut file = tokio::fs::File::open(p)
                    .await
                    .map_err(|e| local::io_err(e, p))?;
                if offset > 0 {
                    file.seek(std::io::SeekFrom::Start(offset))
                        .await
                        .map_err(|e| local::io_err(e, p))?;
                }
                Ok(Reader::Local {
                    file,
                    path: p.to_owned(),
                    stopped: false,
                })
            }
            Endpoint::Remote(r) => {
                let handle = r.sftp.open_read(p).await?;
                Ok(Reader::Remote(PipelinedReader::new(
                    r.sftp.clone(),
                    handle,
                    offset,
                    size,
                    Some(size),
                )))
            }
        }
    }

    /// Open (creating) `p` for writing at `offset`; `offset == 0` truncates.
    pub(crate) async fn writer(&self, p: &str, offset: u64) -> Result<Writer> {
        match self {
            Endpoint::Local => {
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(offset == 0)
                    .open(p)
                    .await
                    .map_err(|e| local::io_err(e, p))?;
                if offset > 0 {
                    file.set_len(offset)
                        .await
                        .map_err(|e| local::io_err(e, p))?;
                    file.seek(std::io::SeekFrom::Start(offset))
                        .await
                        .map_err(|e| local::io_err(e, p))?;
                }
                Ok(Writer::Local {
                    file,
                    path: p.to_owned(),
                })
            }
            Endpoint::Remote(r) => {
                let handle = r.sftp.open_write(p, offset == 0).await?;
                Ok(Writer::Remote(PipelinedWriter::new(
                    r.sftp.clone(),
                    handle,
                    offset,
                )))
            }
        }
    }
}

/// Sequential reader over a local file or a pipelined SFTP file.
pub(crate) enum Reader {
    Local {
        file: tokio::fs::File,
        path: String,
        stopped: bool,
    },
    Remote(PipelinedReader),
}

impl Reader {
    /// Next chunk, `None` at EOF or once [`stop`](Self::stop)ped and drained.
    pub(crate) async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        match self {
            Reader::Local {
                file,
                path,
                stopped,
            } => {
                if *stopped {
                    return Ok(None);
                }
                let mut buf = Vec::with_capacity(LOCAL_CHUNK);
                let n = (&mut *file)
                    .take(LOCAL_CHUNK as u64)
                    .read_to_end(&mut buf)
                    .await
                    .map_err(|e| local::io_err(e, path))?;
                Ok((n > 0).then_some(buf))
            }
            Reader::Remote(r) => r.next().await,
        }
    }

    /// Stop reading ahead; already requested data is still returned.
    pub(crate) fn stop(&mut self) {
        match self {
            Reader::Local { stopped, .. } => *stopped = true,
            Reader::Remote(r) => r.stop(),
        }
    }

    /// Release the file handle.
    pub(crate) async fn close(self) {
        if let Reader::Remote(r) = self {
            r.close().await;
        }
    }
}

/// Sequential writer to a local file or a pipelined SFTP file.
pub(crate) enum Writer {
    Local { file: tokio::fs::File, path: String },
    Remote(PipelinedWriter),
}

impl Writer {
    /// Write `data` at the current position; `acked` is told how many bytes
    /// reached the destination.
    pub(crate) async fn write(&mut self, data: Vec<u8>, acked: &mut impl FnMut(u64)) -> Result<()> {
        match self {
            Writer::Local { file, path } => {
                file.write_all(&data)
                    .await
                    .map_err(|e| local::io_err(e, path))?;
                acked(data.len() as u64);
                Ok(())
            }
            Writer::Remote(w) => w.write(data, acked).await,
        }
    }

    /// Wait until everything written so far reached the destination, then close.
    pub(crate) async fn finish(self, acked: &mut impl FnMut(u64)) -> Result<()> {
        match self {
            Writer::Local { mut file, path } => {
                file.flush().await.map_err(|e| local::io_err(e, &path))
            }
            Writer::Remote(w) => w.finish(acked).await,
        }
    }

    /// Close without waiting for outstanding remote writes.
    pub(crate) async fn abort(self) {
        match self {
            Writer::Local { mut file, .. } => {
                let _ = file.flush().await;
            }
            Writer::Remote(w) => w.abort().await,
        }
    }
}
