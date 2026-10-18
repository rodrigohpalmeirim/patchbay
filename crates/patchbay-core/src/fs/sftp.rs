//! A thin layer over [`RawSftpSession`]: error mapping, directory listings
//! with `longname` owner/group parsing, and pipelined reads and writes.
//!
//! `russh_sftp`'s high-level `SftpSession` hides the `longname` of READDIR
//! entries and the raw request API, so Patchbay drives the raw session itself.
//! Every request method on [`RawSftpSession`] takes `&self` and only sends
//! its packet when first polled, so keeping many request futures alive in a
//! `FuturesOrdered`/`FuturesUnordered` gives as many requests in flight.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::{FuturesOrdered, FuturesUnordered, StreamExt};
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};

use super::Meta;
use super::path::remote;
use crate::model::{FileEntry, FileKind};
use crate::session::Connection;
use crate::{Error, Result};

/// Largest READ we ask for; replies must fit the 256 KiB incoming packet limit.
const MAX_READ_LEN: u64 = 255 * 1024;
/// Largest WRITE we send (bounded further by the server's `limits@openssh.com`).
const MAX_WRITE_LEN: u64 = 255 * 1024;
/// Read/write size every SFTP server must support (draft-ietf-secsh-filexfer-02).
const SAFE_IO_LEN: u32 = 32 * 1024;
/// Bytes requested but not yet answered, per file. Bounded by the SSH channel
/// window anyway; this keeps request timeouts meaningful on slow links.
const PIPELINE_BYTES: usize = 4 * 1024 * 1024;
const MIN_WINDOW: usize = 4;
const MAX_WINDOW: usize = 64;
/// Symlinks resolved concurrently while listing a directory.
const LINK_CONCURRENCY: usize = 16;

pub(crate) type SftpResult<T> = std::result::Result<T, SftpError>;
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// One SFTP channel.
pub(crate) struct Sftp {
    raw: RawSftpSession,
    read_len: u32,
    write_len: u32,
    posix_rename: bool,
    /// OpenSSH (and Windows OpenSSH) swap the SSH_FXP_SYMLINK arguments.
    swapped_symlink: bool,
    broken: AtomicBool,
}

/// A READDIR entry before symlink resolution.
pub(crate) struct RawEntry {
    pub name: String,
    pub meta: Meta,
    owner: Option<String>,
    group: Option<String>,
}

impl RawEntry {
    /// Record the owner/group names from the `longname`, if any.
    pub(crate) fn learn_names(&self, names: &NameCache) {
        if let (Some(owner), Some(group)) = (&self.owner, &self.group) {
            names.learn(&self.meta, owner, group);
        }
    }
}

/// uid/gid to name mappings learned from READDIR `longname`s, so `stat`
/// (which has no longname) can show names too.
#[derive(Default)]
pub(crate) struct NameCache {
    users: Mutex<HashMap<u32, String>>,
    groups: Mutex<HashMap<u32, String>>,
}

impl NameCache {
    fn learn(&self, meta: &Meta, owner: &str, group: &str) {
        if let Some(uid) = meta.uid {
            lock(&self.users).insert(uid, owner.to_owned());
        }
        if let Some(gid) = meta.gid {
            lock(&self.groups).insert(gid, group.to_owned());
        }
    }

    fn user(&self, uid: u32) -> String {
        lock(&self.users)
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }

    fn group(&self, gid: u32) -> String {
        lock(&self.groups)
            .get(&gid)
            .cloned()
            .unwrap_or_else(|| gid.to_string())
    }

    pub(crate) fn uid_of(&self, name: &str) -> Option<u32> {
        lock(&self.users)
            .iter()
            .find_map(|(id, n)| (n == name).then_some(*id))
    }

    pub(crate) fn gid_of(&self, name: &str) -> Option<u32> {
        lock(&self.groups)
            .iter()
            .find_map(|(id, n)| (n == name).then_some(*id))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Sftp {
    /// Open an SFTP channel on `conn`. `timeout_secs` bounds every request.
    pub(crate) async fn open(conn: &Connection, timeout_secs: u64) -> Result<Self> {
        let channel = conn.open_sftp_channel().await?;
        let config = russh_sftp::client::Config {
            request_timeout_secs: timeout_secs,
            ..Default::default()
        };
        let mut raw = RawSftpSession::new_with_config(channel.into_stream(), config);
        let version = raw.init().await?;
        let ext = |name: &str| version.extensions.contains_key(name);
        let swapped_symlink = ext("posix-rename@openssh.com");
        let (mut read_len, mut write_len) = (SAFE_IO_LEN, SAFE_IO_LEN);
        if ext("limits@openssh.com") {
            match raw.limits().await {
                Ok(limits) => {
                    let limits = russh_sftp::client::rawsession::Limits::from(limits);
                    if let Some(len) = limits.read_len {
                        read_len = len.clamp(1024, MAX_READ_LEN) as u32;
                    }
                    if let Some(len) = limits.write_len {
                        write_len = len.clamp(1024, MAX_WRITE_LEN) as u32;
                    }
                    raw.set_limits(limits);
                }
                Err(e) => tracing::debug!("limits@openssh.com failed: {e}"),
            }
        }
        tracing::debug!(host = %conn.key(), read_len, write_len, "SFTP session opened");
        Ok(Self {
            raw,
            read_len,
            write_len,
            posix_rename: swapped_symlink,
            swapped_symlink,
            broken: AtomicBool::new(false),
        })
    }

    /// The session hit a transport-level error and should be replaced.
    pub(crate) fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }

    fn track<T>(&self, result: SftpResult<T>) -> SftpResult<T> {
        if let Err(e) = &result
            && !matches!(e, SftpError::Status(_) | SftpError::Limited(_))
        {
            self.broken.store(true, Ordering::Relaxed);
        }
        result
    }

    pub(crate) async fn realpath(&self, path: &str) -> Result<String> {
        let name = self
            .track(self.raw.realpath(path).await)
            .map_err(|e| map_err(e, path))?;
        name.files
            .into_iter()
            .next()
            .map(|f| f.filename)
            .ok_or_else(|| Error::Other(format!("{path}: empty realpath reply")))
    }

    /// `Ok(None)` if nothing exists at `path`.
    pub(crate) async fn lstat(&self, path: &str) -> Result<Option<Meta>> {
        match self.track(self.raw.lstat(path).await) {
            Ok(attrs) => Ok(Some(meta(&attrs.attrs, None))),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(map_err(e, path)),
        }
    }

    /// Like [`lstat`](Self::lstat) but follows symlinks.
    pub(crate) async fn stat(&self, path: &str) -> Result<Option<Meta>> {
        match self.track(self.raw.stat(path).await) {
            Ok(attrs) => Ok(Some(meta(&attrs.attrs, None))),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(map_err(e, path)),
        }
    }

    pub(crate) async fn read_link(&self, path: &str) -> Result<String> {
        let name = self
            .track(self.raw.readlink(path).await)
            .map_err(|e| map_err(e, path))?;
        name.files
            .into_iter()
            .next()
            .map(|f| f.filename)
            .ok_or_else(|| Error::Other(format!("{path}: empty readlink reply")))
    }

    /// All entries of a directory (without `.`/`..`), lstat-style.
    pub(crate) async fn read_dir(&self, path: &str) -> Result<Vec<RawEntry>> {
        let handle = self
            .track(self.raw.opendir(path).await)
            .map_err(|e| map_err(e, path))?
            .handle;
        let mut entries = Vec::new();
        let result = loop {
            match self.track(self.raw.readdir(handle.as_str()).await) {
                Ok(name) => {
                    for file in name.files {
                        if file.filename == "." || file.filename == ".." {
                            continue;
                        }
                        let longname = parse_longname(&file.longname, file.attrs.size);
                        let meta = meta(&file.attrs, Some(&file.longname));
                        let (owner, group) = longname.unzip();
                        entries.push(RawEntry {
                            name: file.filename,
                            meta,
                            owner,
                            group,
                        });
                    }
                }
                Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => break Ok(()),
                Err(e) => break Err(map_err(e, path)),
            }
        };
        let _ = self.raw.close(handle).await;
        result.map(|()| entries)
    }

    /// [`read_dir`](Self::read_dir) turned into [`FileEntry`]s, with symlink
    /// targets resolved and names recorded in `names`.
    pub(crate) async fn list(&self, dir: &str, names: &NameCache) -> Result<Vec<FileEntry>> {
        let raw = self.read_dir(dir).await?;
        let mut entries = Vec::with_capacity(raw.len());
        for entry in raw {
            entry.learn_names(names);
            let path = remote::join(dir, &entry.name);
            entries.push(self.entry(entry.name, path, entry.meta, names));
        }
        self.resolve_links(&mut entries).await;
        Ok(entries)
    }

    /// Build a [`FileEntry`] without symlink details.
    pub(crate) fn entry(
        &self,
        name: String,
        path: String,
        meta: Meta,
        names: &NameCache,
    ) -> FileEntry {
        FileEntry {
            hidden: name.starts_with('.'),
            name,
            path,
            kind: meta.kind,
            link_target: None,
            target_kind: None,
            size: meta.size,
            modified: meta.mtime,
            permissions: meta.perms,
            owner: meta.uid.map(|u| names.user(u)),
            group: meta.gid.map(|g| names.group(g)),
        }
    }

    /// Fill in `link_target` / `target_kind` of symlink entries.
    pub(crate) async fn resolve_links(&self, entries: &mut [FileEntry]) {
        let links: Vec<(usize, String)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == FileKind::Symlink)
            .map(|(i, e)| (i, e.path.clone()))
            .collect();
        let mut resolved = futures_util::stream::iter(links)
            .map(|(i, path)| async move {
                let (target, meta) = tokio::join!(self.read_link(&path), self.stat(&path));
                (i, target.ok(), meta.ok().flatten().map(|m| m.kind))
            })
            .buffer_unordered(LINK_CONCURRENCY);
        while let Some((i, target, kind)) = resolved.next().await {
            entries[i].link_target = target;
            entries[i].target_kind = kind;
        }
    }

    pub(crate) async fn mkdir(&self, path: &str) -> Result<()> {
        match self.track(self.raw.mkdir(path, FileAttributes::empty()).await) {
            Ok(_) => Ok(()),
            Err(e) => Err(self.exists_error(e, path).await),
        }
    }

    /// Create an empty file; fails if anything exists at `path`.
    pub(crate) async fn create_new(&self, path: &str) -> Result<()> {
        let flags = OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE;
        match self.track(self.raw.open(path, flags, FileAttributes::empty()).await) {
            Ok(handle) => {
                self.track(self.raw.close(handle.handle).await)
                    .map_err(|e| map_err(e, path))?;
                Ok(())
            }
            Err(e) => Err(self.exists_error(e, path).await),
        }
    }

    /// Rename without replacing an existing target.
    pub(crate) async fn rename(&self, from: &str, to: &str) -> Result<()> {
        match self.track(self.raw.rename(from, to).await) {
            Ok(_) => Ok(()),
            Err(e) => Err(self.exists_error(e, to).await),
        }
    }

    /// Rename, replacing a file at `to` (atomically with `posix-rename@openssh.com`).
    pub(crate) async fn replace(&self, from: &str, to: &str) -> Result<()> {
        if self.posix_rename {
            let mut data = Vec::with_capacity(from.len() + to.len() + 8);
            for s in [from, to] {
                data.extend_from_slice(&(s.len() as u32).to_be_bytes());
                data.extend_from_slice(s.as_bytes());
            }
            return match self.track(self.raw.extended("posix-rename@openssh.com", data).await) {
                Ok(Packet::Status(s)) if s.status_code == StatusCode::Ok => Ok(()),
                Ok(Packet::Status(s)) => Err(map_err(SftpError::Status(s), to)),
                Ok(_) => Err(Error::Other("unexpected reply to posix-rename".into())),
                Err(e) => Err(map_err(e, to)),
            };
        }
        if self.lstat(to).await?.is_some() {
            self.remove_file(to).await?;
        }
        self.rename(from, to).await
    }

    pub(crate) async fn remove_file(&self, path: &str) -> Result<()> {
        self.track(self.raw.remove(path).await)
            .map(drop)
            .map_err(|e| map_err(e, path))
    }

    pub(crate) async fn remove_dir(&self, path: &str) -> Result<()> {
        self.track(self.raw.rmdir(path).await)
            .map(drop)
            .map_err(|e| map_err(e, path))
    }

    /// Create a symlink at `link` pointing to `target`.
    pub(crate) async fn symlink(&self, link: &str, target: &str) -> Result<()> {
        let result = if self.swapped_symlink {
            self.raw.symlink(target, link).await
        } else {
            self.raw.symlink(link, target).await
        };
        match self.track(result) {
            Ok(_) => Ok(()),
            Err(e) => Err(self.exists_error(e, link).await),
        }
    }

    pub(crate) async fn set_attrs(&self, path: &str, attrs: FileAttributes) -> Result<()> {
        self.track(self.raw.setstat(path, attrs).await)
            .map(drop)
            .map_err(|e| map_err(e, path))
    }

    /// Set modification (and access) time and/or permission bits.
    pub(crate) async fn set_times_and_mode(
        &self,
        path: &str,
        mtime: Option<i64>,
        perms: Option<u32>,
    ) -> Result<()> {
        let mut attrs = FileAttributes::empty();
        if let Some(t) = mtime.and_then(|t| u32::try_from(t).ok()) {
            attrs.mtime = Some(t);
            attrs.atime = Some(t);
        }
        attrs.permissions = perms;
        if attrs.mtime.is_none() && attrs.permissions.is_none() {
            return Ok(());
        }
        self.set_attrs(path, attrs).await
    }

    /// Open a file for reading; returns the handle.
    pub(crate) async fn open_read(&self, path: &str) -> Result<String> {
        self.track(
            self.raw
                .open(path, OpenFlags::READ, FileAttributes::empty())
                .await,
        )
        .map(|h| h.handle)
        .map_err(|e| map_err(e, path))
    }

    /// Open (creating it) a file for writing. `truncate` starts it empty.
    pub(crate) async fn open_write(&self, path: &str, truncate: bool) -> Result<String> {
        let mut flags = OpenFlags::WRITE | OpenFlags::CREATE;
        if truncate {
            flags |= OpenFlags::TRUNCATE;
        }
        self.track(self.raw.open(path, flags, FileAttributes::empty()).await)
            .map(|h| h.handle)
            .map_err(|e| map_err(e, path))
    }

    pub(crate) async fn close(&self, handle: String) -> Result<()> {
        self.track(self.raw.close(handle).await)
            .map(drop)
            .map_err(Error::from)
    }

    /// Distinguish "exists already" from other generic failures.
    async fn exists_error(&self, e: SftpError, path: &str) -> Error {
        if matches!(&e, SftpError::Status(s) if s.status_code == StatusCode::Failure)
            && matches!(self.lstat(path).await, Ok(Some(_)))
        {
            return Error::AlreadyExists(path.to_owned());
        }
        map_err(e, path)
    }
}

fn is_not_found(e: &SftpError) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == StatusCode::NoSuchFile)
}

/// Map an SFTP error for an operation on `path`.
pub(crate) fn map_err(e: SftpError, path: &str) -> Error {
    match &e {
        SftpError::Status(s) => match s.status_code {
            StatusCode::NoSuchFile => Error::NotFound(path.to_owned()),
            StatusCode::PermissionDenied => Error::PermissionDenied(path.to_owned()),
            StatusCode::OpUnsupported => Error::Unsupported(format!("{path}: {}", status_text(s))),
            _ => Error::Other(format!("{path}: {}", status_text(s))),
        },
        _ => Error::Sftp(e),
    }
}

fn status_text(s: &russh_sftp::protocol::Status) -> String {
    if s.error_message.is_empty() {
        s.status_code.to_string()
    } else {
        s.error_message.clone()
    }
}

fn meta(attrs: &FileAttributes, longname: Option<&str>) -> Meta {
    let kind = match attrs.permissions.map(|p| p & 0o170000) {
        Some(0o040000) => FileKind::Dir,
        Some(0o100000) => FileKind::File,
        Some(0o120000) => FileKind::Symlink,
        Some(0) | None => match longname.and_then(|l| l.chars().next()) {
            Some('d') => FileKind::Dir,
            Some('-') => FileKind::File,
            Some('l') => FileKind::Symlink,
            _ => FileKind::Other,
        },
        Some(_) => FileKind::Other,
    };
    Meta {
        kind,
        size: attrs.size.unwrap_or(0),
        mtime: attrs.mtime.map(i64::from),
        perms: attrs.permissions.map(|p| p & 0o7777),
        uid: attrs.uid,
        gid: attrs.gid,
    }
}

/// Owner and group from an `ls -l` style READDIR `longname`
/// (`-rw-r--r--  1 user group 1234 Jan  1 12:00 name`). Only trusted when
/// the size column matches the attributes.
pub(crate) fn parse_longname(longname: &str, size: Option<u64>) -> Option<(String, String)> {
    let mut fields = longname.split_whitespace();
    let mode = fields.next()?;
    if mode.len() < 10 || !mode.starts_with(['-', 'd', 'l', 'c', 'b', 'p', 's']) {
        return None;
    }
    fields.next()?.parse::<u64>().ok()?;
    let owner = fields.next()?;
    let group = fields.next()?;
    let listed: u64 = fields.next()?.parse().ok()?;
    if size.is_some_and(|size| size != listed) {
        return None;
    }
    Some((owner.to_owned(), group.to_owned()))
}

fn window(chunk: u32) -> usize {
    (PIPELINE_BYTES / chunk.max(1) as usize).clamp(MIN_WINDOW, MAX_WINDOW)
}

type ReadReply = (u64, u32, SftpResult<Vec<u8>>);

/// Reads a remote file front to back with many READ requests in flight,
/// yielding chunks in file order.
pub(crate) struct PipelinedReader {
    sftp: Arc<Sftp>,
    handle: String,
    next_offset: u64,
    /// Expected file size; past it only one probing request is in flight.
    size_hint: u64,
    limit: u64,
    chunk: u32,
    window: usize,
    inflight: FuturesOrdered<BoxFuture<ReadReply>>,
    eof: bool,
    stopped: bool,
    closed: bool,
}

impl PipelinedReader {
    /// Read from `offset` until EOF or `limit` (absolute offset).
    pub(crate) fn new(
        sftp: Arc<Sftp>,
        handle: String,
        offset: u64,
        size_hint: u64,
        limit: Option<u64>,
    ) -> Self {
        let chunk = sftp.read_len;
        Self {
            sftp,
            handle,
            next_offset: offset,
            size_hint,
            limit: limit.unwrap_or(u64::MAX),
            chunk,
            window: window(chunk),
            inflight: FuturesOrdered::new(),
            eof: false,
            stopped: false,
            closed: false,
        }
    }

    /// Close the file handle (outstanding reads are abandoned).
    pub(crate) async fn close(mut self) {
        self.inflight = FuturesOrdered::new();
        self.closed = true;
        let _ = self.sftp.close(std::mem::take(&mut self.handle)).await;
    }

    /// Stop issuing requests; [`next`](Self::next) still yields the ones in flight.
    pub(crate) fn stop(&mut self) {
        self.stopped = true;
    }

    fn issue(&mut self, offset: u64, len: u32) {
        let sftp = self.sftp.clone();
        let handle = self.handle.clone();
        self.inflight.push_back(Box::pin(async move {
            let result = sftp.raw.read(handle, offset, len).await.map(|d| d.data);
            (offset, len, sftp.track(result))
        }));
    }

    fn fill(&mut self) {
        while !self.eof
            && !self.stopped
            && self.inflight.len() < self.window
            && self.next_offset < self.limit
            && (self.next_offset < self.size_hint || self.inflight.is_empty())
        {
            let len = (self.limit - self.next_offset).min(self.chunk as u64) as u32;
            self.issue(self.next_offset, len);
            self.next_offset += len as u64;
        }
    }

    /// The next chunk in file order, `None` at EOF / limit / after `stop`.
    pub(crate) async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        self.fill();
        let Some((offset, len, result)) = self.inflight.next().await else {
            return Ok(None);
        };
        let mut data = match result {
            Ok(data) if data.is_empty() => return Ok(self.at_eof()),
            Ok(data) => data,
            Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => {
                return Ok(self.at_eof());
            }
            Err(e) => return Err(e.into()),
        };
        if data.len() < len as usize {
            // Short read: the server caps READ sizes below ours, or EOF.
            let got = data.len() as u32;
            if offset + (got as u64) < self.size_hint && got >= 4096 {
                self.chunk = got & !4095;
            }
            self.fill_gap(&mut data, offset + got as u64, len - got)
                .await?;
        }
        Ok(Some(data))
    }

    fn at_eof(&mut self) -> Option<Vec<u8>> {
        self.eof = true;
        self.inflight = FuturesOrdered::new();
        None
    }

    async fn fill_gap(&mut self, data: &mut Vec<u8>, mut offset: u64, mut len: u32) -> Result<()> {
        while len > 0 {
            match self
                .sftp
                .track(self.sftp.raw.read(self.handle.as_str(), offset, len).await)
            {
                Ok(d) if d.data.is_empty() => break,
                Ok(d) => {
                    let n = d.data.len() as u32;
                    data.extend_from_slice(&d.data);
                    offset += n as u64;
                    len = len.saturating_sub(n);
                }
                Err(SftpError::Status(s)) if s.status_code == StatusCode::Eof => {
                    self.at_eof();
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

impl Drop for PipelinedReader {
    fn drop(&mut self) {
        if !self.closed {
            close_in_background(&self.sftp, std::mem::take(&mut self.handle));
        }
    }
}

/// Close a handle from a destructor (best effort, needs a tokio runtime).
fn close_in_background(sftp: &Arc<Sftp>, handle: String) {
    if sftp.is_broken() {
        return;
    }
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        let sftp = sftp.clone();
        rt.spawn(async move {
            let _ = sftp.raw.close(handle).await;
        });
    }
}

/// Writes a remote file sequentially with many WRITE requests in flight.
/// WRITEs go out in file order, so after an interruption the file on the
/// server is a prefix of the data (OpenSSH handles requests in order).
pub(crate) struct PipelinedWriter {
    sftp: Arc<Sftp>,
    handle: String,
    offset: u64,
    window: usize,
    inflight: FuturesUnordered<BoxFuture<(u64, SftpResult<()>)>>,
    /// Data not sent yet (less than one WRITE).
    pending: Vec<u8>,
    closed: bool,
}

impl PipelinedWriter {
    pub(crate) fn new(sftp: Arc<Sftp>, handle: String, offset: u64) -> Self {
        let window = window(sftp.write_len);
        Self {
            sftp,
            handle,
            offset,
            window,
            inflight: FuturesUnordered::new(),
            pending: Vec::new(),
            closed: false,
        }
    }

    /// Queue `data` after what was written before; `acked` is called with
    /// the size of every acknowledged write. Data is sent in full-size WRITEs
    /// (the remainder waits for more data or [`finish`](Self::finish)).
    pub(crate) async fn write(&mut self, data: Vec<u8>, acked: &mut impl FnMut(u64)) -> Result<()> {
        let max = self.sftp.write_len as usize;
        if self.pending.is_empty() && data.len() == max {
            return self.issue(data, acked).await;
        }
        self.pending.extend_from_slice(&data);
        while self.pending.len() >= max {
            let rest = self.pending.split_off(max);
            let piece = std::mem::replace(&mut self.pending, rest);
            self.issue(piece, acked).await?;
        }
        Ok(())
    }

    async fn issue(&mut self, data: Vec<u8>, acked: &mut impl FnMut(u64)) -> Result<()> {
        while self.inflight.len() >= self.window {
            self.reap(acked).await?;
        }
        let sftp = self.sftp.clone();
        let handle = self.handle.clone();
        let offset = self.offset;
        let len = data.len() as u64;
        self.offset += len;
        self.inflight.push(Box::pin(async move {
            let result = sftp.raw.write(handle, offset, data).await.map(drop);
            (len, sftp.track(result))
        }));
        // Poll once so the request goes out right away.
        if let Some(Some((n, r))) = futures_util::FutureExt::now_or_never(self.inflight.next()) {
            r?;
            acked(n);
        }
        Ok(())
    }

    async fn reap(&mut self, acked: &mut impl FnMut(u64)) -> Result<()> {
        if let Some((n, result)) = self.inflight.next().await {
            result?;
            acked(n);
        }
        Ok(())
    }

    /// Wait for every write to be acknowledged, then close the handle.
    pub(crate) async fn finish(mut self, acked: &mut impl FnMut(u64)) -> Result<()> {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.issue(rest, acked).await?;
        }
        while !self.inflight.is_empty() {
            self.reap(acked).await?;
        }
        self.closed = true;
        self.sftp.close(std::mem::take(&mut self.handle)).await
    }

    /// Close without waiting for outstanding writes (the server still
    /// handles them before the CLOSE).
    pub(crate) async fn abort(mut self) {
        self.inflight = FuturesUnordered::new();
        self.closed = true;
        if !self.sftp.is_broken() {
            let _ = self.sftp.close(std::mem::take(&mut self.handle)).await;
        }
    }
}

impl Drop for PipelinedWriter {
    fn drop(&mut self) {
        if !self.closed {
            close_in_background(&self.sftp, std::mem::take(&mut self.handle));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longname_owner_group() {
        assert_eq!(
            parse_longname(
                "-rw-r--r--    1 patchbay    staff        1234 Oct 17 13:24 a b.txt",
                Some(1234)
            ),
            Some(("patchbay".into(), "staff".into()))
        );
        assert_eq!(
            parse_longname("drwxr-xr-x 2 1000 1000 4096 Oct 17 13:24 docs", None),
            Some(("1000".into(), "1000".into()))
        );
        // Size mismatch: columns are not what we think they are.
        assert_eq!(
            parse_longname("-rw-r--r-- 1 a b 5 Oct 17 13:24 x", Some(6)),
            None
        );
        assert_eq!(parse_longname("", None), None);
        assert_eq!(parse_longname("plainname", None), None);
    }

    #[test]
    fn kind_from_mode_or_longname() {
        let attrs = |perms| FileAttributes {
            permissions: perms,
            ..FileAttributes::empty()
        };
        assert_eq!(meta(&attrs(Some(0o40755)), None).kind, FileKind::Dir);
        assert_eq!(meta(&attrs(Some(0o100644)), None).kind, FileKind::File);
        assert_eq!(meta(&attrs(Some(0o120777)), None).kind, FileKind::Symlink);
        assert_eq!(meta(&attrs(Some(0o100644)), None).perms, Some(0o644));
        assert_eq!(
            meta(&attrs(None), Some("drwx------ 1 a b 0 x")).kind,
            FileKind::Dir
        );
        assert_eq!(meta(&attrs(None), None).kind, FileKind::Other);
    }

    #[test]
    fn pipeline_window() {
        assert_eq!(window(32 * 1024), 64);
        assert_eq!(window(255 * 1024), 16);
        assert_eq!(window(4 * 1024 * 1024), MIN_WINDOW);
    }
}
