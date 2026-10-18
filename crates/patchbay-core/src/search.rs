//! File name search below a directory, streamed as [`CoreEvent::Search`]
//! batches (every [`BATCH_SIZE`] results or [`BATCH_INTERVAL`]), ending with
//! a `done: true` event (with `error` set on failure). At most
//! [`MAX_RESULTS`] entries are reported.
//!
//! Matching is case-insensitive on the entry name; `*` and `?` are wildcards
//! matching the whole name, otherwise the query is a substring.
//!
//! On remote hosts with a POSIX shell the search runs `find -iname` there
//! (portable options only, so GNU, BSD/macOS and BusyBox `find` work) and
//! describes the reported paths over SFTP. Elsewhere (local disk, SFTP-only
//! servers, Windows) directories are walked with a few listings in flight,
//! without following symlinked directories and skipping unreadable ones.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesOrdered, FuturesUnordered, StreamExt};
use russh::ChannelMsg;
use tokio::sync::broadcast;
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::fs::{Endpoint, FileSystem, Remote, shell_quote};
use crate::model::{CoreEvent, FileEntry, FileKind, HostId, RemoteOs};
use crate::{Error, Result};

pub const MAX_RESULTS: usize = 5000;
pub const BATCH_SIZE: usize = 100;
pub const BATCH_INTERVAL: Duration = Duration::from_millis(250);
/// Directory listings in flight while walking.
const WALK_CONCURRENCY: usize = 8;
/// SFTP stats in flight for `find` results.
const STAT_CONCURRENCY: usize = 32;
/// `find` exit status when it is not installed.
const COMMAND_NOT_FOUND: u32 = 127;

/// Runs searches. Cheap to clone.
#[derive(Clone)]
pub struct SearchManager {
    inner: Arc<Inner>,
}

struct Inner {
    fs: FileSystem,
    events: broadcast::Sender<CoreEvent>,
    running: Mutex<HashMap<Uuid, AbortHandle>>,
}

impl SearchManager {
    pub fn new(fs: FileSystem, events: broadcast::Sender<CoreEvent>) -> Self {
        Self {
            inner: Arc::new(Inner {
                fs,
                events,
                running: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Start searching `root` on `host` for `query`. Fails right away if the
    /// host is not connected or `root` does not exist; results follow as
    /// events carrying the returned id.
    pub async fn start(&self, host: Option<HostId>, root: &str, query: &str) -> Result<Uuid> {
        let query = query.trim();
        if query.is_empty() {
            return Err(Error::Other("empty search query".into()));
        }
        let endpoint = self.inner.fs.endpoint(host.as_deref(), false).await?;
        let root = endpoint.canonicalize(root).await?;
        match endpoint.stat(&root).await? {
            Some(meta) if meta.kind == FileKind::Dir => {}
            Some(_) => return Err(Error::Other(format!("{root} is not a folder"))),
            None => return Err(Error::NotFound(root)),
        }
        let id = Uuid::new_v4();
        let inner = self.inner.clone();
        let query = query.to_owned();
        let mut running = self.inner.running();
        let task = tokio::spawn(async move {
            let mut out = Batcher::new(&inner, id);
            let result = search(&endpoint, &root, &query, &mut out).await;
            out.finish(result.err().map(|e| e.to_string()));
        });
        running.insert(id, task.abort_handle());
        Ok(id)
    }

    /// Stop a search; a final `done` event is sent unless it already ended.
    pub fn cancel(&self, id: Uuid) {
        let mut running = self.inner.running();
        if let Some(task) = running.remove(&id) {
            task.abort();
            let _ = self.inner.events.send(CoreEvent::Search {
                search_id: id,
                entries: Vec::new(),
                done: true,
                error: None,
            });
        }
    }
}

impl Inner {
    fn running(&self) -> MutexGuard<'_, HashMap<Uuid, AbortHandle>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Send a batch unless the search was cancelled. Returns false if it was.
    fn send(&self, id: Uuid, entries: Vec<FileEntry>, done: bool, error: Option<String>) -> bool {
        let mut running = self.running();
        let live = if done {
            running.remove(&id).is_some()
        } else {
            running.contains_key(&id)
        };
        if live {
            let _ = self.events.send(CoreEvent::Search {
                search_id: id,
                entries,
                done,
                error,
            });
        }
        live
    }
}

/// Collects results into batches.
struct Batcher<'a> {
    inner: &'a Inner,
    id: Uuid,
    pending: Vec<FileEntry>,
    last_flush: Instant,
    total: usize,
}

impl<'a> Batcher<'a> {
    fn new(inner: &'a Inner, id: Uuid) -> Self {
        Self {
            inner,
            id,
            pending: Vec::new(),
            last_flush: Instant::now(),
            total: 0,
        }
    }

    /// Add a result; false once the search should stop (limit reached or cancelled).
    fn push(&mut self, entry: FileEntry) -> bool {
        self.pending.push(entry);
        self.total += 1;
        let live = if self.pending.len() >= BATCH_SIZE {
            self.flush()
        } else {
            self.tick()
        };
        live && self.total < MAX_RESULTS
    }

    /// Flush if results have waited long enough.
    fn tick(&mut self) -> bool {
        if self.last_flush.elapsed() >= BATCH_INTERVAL {
            self.flush()
        } else {
            true
        }
    }

    fn flush(&mut self) -> bool {
        self.last_flush = Instant::now();
        if self.pending.is_empty() {
            return true;
        }
        let entries = std::mem::take(&mut self.pending);
        self.inner.send(self.id, entries, false, None)
    }

    fn finish(mut self, error: Option<String>) {
        let entries = std::mem::take(&mut self.pending);
        self.inner.send(self.id, entries, true, error);
    }
}

/// Case-insensitive name matcher: `*`/`?` globs match the whole name,
/// anything else is a substring.
#[derive(Debug, Clone)]
pub(crate) struct Matcher {
    pattern: String,
    glob: bool,
}

impl Matcher {
    pub(crate) fn new(query: &str) -> Self {
        Self {
            pattern: query.to_lowercase(),
            glob: query.contains(['*', '?']),
        }
    }

    pub(crate) fn matches(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        if self.glob {
            let pattern: Vec<char> = self.pattern.chars().collect();
            let name: Vec<char> = name.chars().collect();
            glob_match(&pattern, &name)
        } else {
            name.contains(&self.pattern)
        }
    }
}

fn glob_match(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut backtrack = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                backtrack = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match backtrack {
                Some((star, start)) => {
                    p = star + 1;
                    n = start + 1;
                    backtrack = Some((star, start + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// The `find -iname` pattern for a query: wildcards kept, `[`, `]` and `\`
/// escaped, plain text wrapped in `*...*`.
fn find_pattern(query: &str) -> String {
    let glob = query.contains(['*', '?']);
    let mut pattern = String::with_capacity(query.len() + 2);
    if !glob {
        pattern.push('*');
    }
    for c in query.chars() {
        if matches!(c, '[' | ']' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    if !glob {
        pattern.push('*');
    }
    pattern
}

fn find_command(root: &str, query: &str) -> String {
    let root = if root.starts_with('-') {
        format!("./{root}")
    } else {
        root.to_owned()
    };
    format!(
        "find {} -mindepth 1 -iname {} -print0 2>/dev/null",
        shell_quote(&root),
        shell_quote(&find_pattern(query))
    )
}

async fn search(endpoint: &Endpoint, root: &str, query: &str, out: &mut Batcher<'_>) -> Result<()> {
    if let Endpoint::Remote(remote) = endpoint {
        let caps = remote.conn.capabilities();
        let posix = caps.exec && !matches!(caps.os, RemoteOs::Windows | RemoteOs::Unknown);
        if posix {
            match find(remote, endpoint, root, query, out).await {
                Ok(true) => return Ok(()),
                Ok(false) => tracing::debug!("find unavailable, walking instead"),
                Err(e) if out.total == 0 => {
                    tracing::debug!("find failed ({e}), walking instead");
                }
                Err(e) => return Err(e),
            }
        }
    }
    walk(endpoint, root, &Matcher::new(query), out).await
}

/// Walk directories breadth-first with a few listings in flight.
async fn walk(
    endpoint: &Endpoint,
    root: &str,
    matcher: &Matcher,
    out: &mut Batcher<'_>,
) -> Result<()> {
    let matcher = matcher.clone();
    let want: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(move |name| matcher.matches(name));
    let mut queue = VecDeque::from([root.to_owned()]);
    let mut listing = FuturesUnordered::new();
    let mut tick = tokio::time::interval(BATCH_INTERVAL);
    loop {
        while listing.len() < WALK_CONCURRENCY
            && let Some(dir) = queue.pop_front()
        {
            let want = want.clone();
            listing.push(async move {
                let result = endpoint.scan_dir(&dir, want).await;
                (dir, result)
            });
        }
        tokio::select! {
            next = listing.next() => {
                let Some((dir, result)) = next else { return Ok(()) };
                let (subdirs, matches) = match result {
                    Ok(found) => found,
                    Err(e) if dir == root => return Err(e),
                    Err(e) => {
                        tracing::debug!("search: skipping {dir}: {e}");
                        continue;
                    }
                };
                queue.extend(subdirs);
                for entry in matches {
                    if !out.push(entry) {
                        return Ok(());
                    }
                }
            }
            _ = tick.tick() => {
                if !out.tick() {
                    return Ok(());
                }
            }
        }
    }
}

/// Run `find` remotely and describe its results over SFTP. `Ok(false)` if
/// `find` is not available.
async fn find(
    remote: &Remote,
    endpoint: &Endpoint,
    root: &str,
    query: &str,
    out: &mut Batcher<'_>,
) -> Result<bool> {
    let mut channel = remote.conn.handle().channel_open_session().await?;
    channel.exec(true, find_command(root, query)).await?;
    channel.eof().await?;

    let mut buf: Vec<u8> = Vec::new();
    let mut paths: VecDeque<String> = VecDeque::new();
    let mut accepted = 0usize;
    let mut stats = FuturesOrdered::new();
    let mut tick = tokio::time::interval(BATCH_INTERVAL);
    let mut output_done = false;
    let mut status = None;
    loop {
        while stats.len() < STAT_CONCURRENCY
            && let Some(path) = paths.pop_front()
        {
            stats.push_back(async move { endpoint.describe(&path).await });
        }
        if output_done && stats.is_empty() && paths.is_empty() {
            break;
        }
        tokio::select! {
            msg = channel.wait(), if !output_done => match msg {
                Some(ChannelMsg::Data { data }) => {
                    buf.extend_from_slice(&data);
                    let mut start = 0;
                    while let Some(len) = buf[start..].iter().position(|&b| b == 0) {
                        if accepted < MAX_RESULTS {
                            paths.push_back(String::from_utf8_lossy(&buf[start..start + len]).into_owned());
                            accepted += 1;
                        }
                        start += len + 1;
                    }
                    buf.drain(..start);
                    if accepted >= MAX_RESULTS {
                        let _ = channel.close().await;
                        output_done = true;
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(_) => {}
                None => output_done = true,
            },
            Some(described) = stats.next(), if !stats.is_empty() => {
                // Entries that vanished since `find` saw them are dropped.
                if let Ok(entry) = described
                    && !out.push(entry)
                {
                    let _ = channel.close().await;
                    return Ok(true);
                }
            }
            _ = tick.tick() => {
                if !out.tick() {
                    let _ = channel.close().await;
                    return Ok(true);
                }
            }
        }
    }
    Ok(!(status == Some(COMMAND_NOT_FOUND) && out.total == 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        let m = |q: &str, name: &str| Matcher::new(q).matches(name);
        assert!(m("readme", "README.md"));
        assert!(m("ME.M", "readme.md"));
        assert!(!m("readme", "read.me"));
        assert!(m("*.txt", "Notes.TXT"));
        assert!(!m("*.txt", "notes.txt.bak"));
        assert!(m("a?c", "ABC"));
        assert!(!m("a?c", "abbc"));
        assert!(m("*a*b*c*", "xxaxxbxxcxx"));
        assert!(!m("*a*b*c", "xxaxxbxxcxx"));
        assert!(m("*", ""));
        assert!(m("ü*", "Über.txt"));
        assert!(m("日本", "ünïcödé — 日本語 ✓.txt"));
        assert!(m("**x", "abx"));
    }

    #[test]
    fn find_patterns_and_quoting() {
        assert_eq!(find_pattern("read"), "*read*");
        assert_eq!(find_pattern("*.txt"), "*.txt");
        assert_eq!(find_pattern("a[1]"), "*a\\[1\\]*");
        assert_eq!(
            find_command("/home/o'neil", "it's"),
            r"find '/home/o'\''neil' -mindepth 1 -iname '*it'\''s*' -print0 2>/dev/null"
        );
        assert_eq!(
            find_command("-x", "a"),
            "find './-x' -mindepth 1 -iname '*a*' -print0 2>/dev/null"
        );
    }
}
