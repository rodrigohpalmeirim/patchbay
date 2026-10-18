//! Running one transfer: scanning, conflict handling and the copy loop.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::stream::{self, FuturesUnordered, StreamExt};
use tokio::sync::{oneshot, watch};

use super::{Control, Inner, now_ms};
use crate::fs::path::{PARTIAL_SUFFIX, numbered_name};
use crate::fs::{Endpoint, Meta, Reader, Writer};
use crate::model::{
    ConflictResolution, FileKind, FileStat, Prompt, PromptRequest, PromptResponse, TransferInfo,
    TransferRequest, TransferState,
};
use crate::{Error, Result};

/// Minimum time between two progress events of one transfer.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(125);
/// Span of the moving average behind `bytes_per_sec`.
const RATE_WINDOW: Duration = Duration::from_secs(3);
/// Directories listed concurrently while scanning.
const SCAN_CONCURRENCY: usize = 8;

/// What a transfer has achieved so far; survives pause, failure and retry.
#[derive(Default)]
pub(super) struct Progress {
    plan: Option<Arc<Plan>>,
    /// Index of the item to process next.
    next: usize,
    /// Bytes of the files before `next` (copied or skipped).
    completed_bytes: u64,
    apply_to_all: Option<ConflictResolution>,
    /// Where item `next` goes, once decided (kept if it was interrupted).
    current: Option<Target>,
    /// Partial file of item `next`; deleted on cancel.
    pub(super) partial: Option<String>,
    /// Items (directories) this transfer created.
    created_dirs: Vec<usize>,
}

/// Everything to copy, parents before children.
struct Plan {
    /// Destination directory.
    root: String,
    items: Vec<Item>,
}

struct Item {
    src: String,
    /// Path components below the destination directory.
    rel: Vec<String>,
    meta: Meta,
    link: Option<String>,
}

impl Plan {
    fn dst_path(&self, dst: &Endpoint, item: &Item) -> String {
        item.rel
            .iter()
            .fold(self.root.clone(), |dir, name| dst.join(&dir, name))
    }
}

/// Destination of a file.
#[derive(Debug, Clone)]
struct Target {
    path: String,
    partial: String,
    /// Discard an existing partial (explicit overwrite).
    restart: bool,
}

/// Why a run ended early.
enum Stop {
    Paused,
    Cancelled,
    Failed(Error),
}

impl From<Error> for Stop {
    fn from(e: Error) -> Self {
        match e {
            Error::Cancelled => Stop::Cancelled,
            e => Stop::Failed(e),
        }
    }
}

/// Moving average of throughput over [`RATE_WINDOW`].
#[derive(Default)]
struct Rate {
    samples: VecDeque<(Instant, u64)>,
}

impl Rate {
    fn sample(&mut self, now: Instant, total: u64) -> u64 {
        self.samples.push_back((now, total));
        while self.samples.len() > 2 && now.duration_since(self.samples[1].0) >= RATE_WINDOW {
            self.samples.pop_front();
        }
        let (t0, b0) = self.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt < 0.05 {
            0
        } else {
            ((total - b0) as f64 / dt) as u64
        }
    }

    fn reset(&mut self) {
        self.samples.clear();
    }
}

fn is_newer(source: Option<i64>, existing: Option<i64>) -> bool {
    matches!((source, existing), (Some(s), Some(e)) if s > e)
}

async fn path_is_taken(dst: &Endpoint, path: &str) -> Result<bool> {
    Ok(dst.lstat(path).await?.is_some())
}

fn file_stat(meta: &Meta) -> FileStat {
    FileStat {
        size: meta.size,
        modified: meta.mtime,
    }
}

pub(super) struct Job {
    inner: Arc<Inner>,
    info: TransferInfo,
    request: TransferRequest,
    progress: Progress,
    control: watch::Receiver<Control>,
    last_publish: Option<Instant>,
    /// Bytes actually moved in this run (excludes skipped and resumed data).
    transferred: u64,
    rate: Rate,
}

impl Job {
    pub(super) fn new(
        inner: Arc<Inner>,
        info: TransferInfo,
        request: TransferRequest,
        progress: Progress,
        control: watch::Receiver<Control>,
    ) -> Self {
        Self {
            inner,
            info,
            request,
            progress,
            control,
            last_publish: None,
            transferred: 0,
            rate: Rate::default(),
        }
    }

    pub(super) async fn run(mut self) {
        let outcome = self.execute().await;
        self.info.bytes_per_sec = 0;
        self.info.state = match outcome {
            Ok(()) => {
                self.info.current_file = None;
                self.info.finished_at = Some(now_ms());
                TransferState::Done
            }
            Err(Stop::Paused) => TransferState::Paused,
            Err(Stop::Cancelled) => {
                self.remove_partial().await;
                self.info.current_file = None;
                self.info.finished_at = Some(now_ms());
                TransferState::Cancelled
            }
            Err(Stop::Failed(e)) => {
                tracing::info!(transfer = %self.info.id, "transfer failed: {e}");
                let resumable = self.progress.plan.is_some()
                    || matches!(
                        e,
                        Error::Ssh(_)
                            | Error::Sftp(_)
                            | Error::Io(_)
                            | Error::NotConnected(_)
                            | Error::Timeout(_)
                    );
                self.info.finished_at = Some(now_ms());
                TransferState::Failed {
                    message: e.to_string(),
                    resumable,
                }
            }
        };
        Inner::finished(&self.inner, self.info, self.progress);
    }

    async fn remove_partial(&mut self) {
        let Some(partial) = self.progress.partial.take() else {
            return;
        };
        let host = self.request.destination.host_id.as_deref();
        match self.inner.fs.endpoint(host, true).await {
            Ok(dst) => {
                if let Err(e) = dst.remove_file(&partial).await {
                    tracing::debug!("removing {partial}: {e}");
                }
            }
            Err(e) => tracing::debug!("cannot remove {partial}: {e}"),
        }
    }

    fn control(&self) -> Control {
        *self.control.borrow()
    }

    fn check_control(&self) -> Result<(), Stop> {
        match self.control() {
            Control::Run => Ok(()),
            Control::Pause => Err(Stop::Paused),
            Control::Cancel => Err(Stop::Cancelled),
        }
    }

    fn publish(&mut self) {
        let now = Instant::now();
        if self.info.state == TransferState::Running {
            self.info.bytes_per_sec = self.rate.sample(now, self.transferred);
        }
        self.last_publish = Some(now);
        self.inner.publish(&self.info);
    }

    fn maybe_publish(&mut self) {
        if self
            .last_publish
            .is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL)
        {
            self.publish();
        }
    }

    fn set_state(&mut self, state: TransferState) {
        if state != TransferState::Running {
            self.info.bytes_per_sec = 0;
            self.rate.reset();
        }
        self.info.state = state;
        self.publish();
    }

    fn add_bytes(&mut self, n: u64) {
        self.info.done_bytes += n;
        self.transferred += n;
        self.maybe_publish();
    }

    fn skip(&mut self, item: &Item) {
        self.info.skipped_files += 1;
        if item.meta.kind == FileKind::File {
            self.progress.completed_bytes += item.meta.size;
            self.info.done_bytes = self.progress.completed_bytes;
        }
        self.maybe_publish();
    }

    async fn execute(&mut self) -> Result<(), Stop> {
        let fs = self.inner.fs.clone();
        let src = fs.endpoint(self.info.source_host.as_deref(), true).await?;
        let dst = fs
            .endpoint(self.request.destination.host_id.as_deref(), true)
            .await?;

        let plan = match self.progress.plan.clone() {
            Some(plan) => plan,
            None => {
                self.set_state(TransferState::Scanning);
                let plan = Arc::new(self.scan(&src, &dst).await?);
                let files = plan.items.iter().filter(|i| i.meta.kind != FileKind::Dir);
                self.info.total_files = Some(files.clone().count() as u64);
                let total: u64 = files
                    .filter(|i| i.meta.kind == FileKind::File)
                    .map(|i| i.meta.size)
                    .sum();
                self.info.total_bytes = Some(total);
                if let Some(max) = self.request.max_bytes.filter(|&max| total > max) {
                    return Err(Stop::Failed(Error::Unsupported(format!(
                        "{} is {total} bytes, more than the limit of {max} bytes",
                        self.info.label
                    ))));
                }
                self.progress = Progress {
                    plan: Some(plan.clone()),
                    ..Progress::default()
                };
                plan
            }
        };

        self.set_state(TransferState::Running);
        while let Some(item) = plan.items.get(self.progress.next) {
            self.check_control()?;
            let path = plan.dst_path(&dst, item);
            self.info.current_file = Some(item.rel.join("/"));
            let result = match item.meta.kind {
                FileKind::Dir => self.make_dir(&dst, &path).await,
                FileKind::File => self.copy_file(&src, &dst, item, path).await,
                FileKind::Symlink => self.copy_symlink(&dst, item, path).await,
                FileKind::Other => {
                    self.skip(item);
                    Ok(())
                }
            };
            match result {
                // Resumed while the pause was taking effect: go on with this file.
                Err(Stop::Paused) if self.control() == Control::Run => continue,
                result => result?,
            }
            self.progress.next += 1;
            self.progress.current = None;
            self.progress.partial = None;
        }
        self.info.current_file = None;

        let keep_perms = src.has_unix_permissions() && dst.has_unix_permissions();
        for &i in self.progress.created_dirs.iter().rev() {
            let item = &plan.items[i];
            let path = plan.dst_path(&dst, item);
            let perms = item.meta.perms.filter(|_| keep_perms).map(|p| p & 0o777);
            if let Err(e) = dst.set_times_and_mode(&path, item.meta.mtime, perms).await {
                tracing::debug!("setting attributes of {path}: {e}");
            }
        }
        Ok(())
    }

    async fn scan(&self, src: &Endpoint, dst: &Endpoint) -> Result<Plan, Stop> {
        let root = self.request.destination.path.clone();
        match dst.stat(&root).await? {
            Some(m) if m.kind == FileKind::Dir => {}
            Some(_) => return Err(Error::Other(format!("{root} is not a folder")).into()),
            None => return Err(Error::NotFound(root).into()),
        }
        let same_machine = self.info.source_host == self.request.destination.host_id;

        let mut items = Vec::new();
        let mut queue = VecDeque::new();
        for source in &self.request.sources {
            let meta = src
                .lstat(&source.path)
                .await?
                .ok_or_else(|| Error::NotFound(source.path.clone()))?;
            let name = src.file_name(&source.path);
            if name.is_empty() || name == "." || name == ".." || src.parent(&source.path).is_none()
            {
                return Err(Error::Other(format!("cannot copy {} as a whole", source.path)).into());
            }
            if same_machine && meta.kind == FileKind::Dir && src.is_within(&root, &source.path) {
                return Err(
                    Error::Other(format!("cannot copy {} into itself", source.path)).into(),
                );
            }
            let rel = vec![name];
            if meta.kind == FileKind::Dir {
                queue.push_back((source.path.clone(), rel.clone()));
            }
            items.push(Item {
                src: source.path.clone(),
                rel,
                meta,
                link: None,
            });
        }

        let mut listing = FuturesUnordered::new();
        loop {
            while listing.len() < SCAN_CONCURRENCY
                && let Some((dir, rel)) = queue.pop_front()
            {
                listing.push(async move {
                    let entries = src.read_dir(&dir).await;
                    (dir, rel, entries)
                });
            }
            let Some((dir, rel, entries)) = listing.next().await else {
                break;
            };
            if self.control() == Control::Cancel {
                return Err(Stop::Cancelled);
            }
            let mut entries = entries?;
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, meta) in entries {
                let path = src.join(&dir, &name);
                let mut child = rel.clone();
                child.push(name);
                if meta.kind == FileKind::Dir {
                    queue.push_back((path.clone(), child.clone()));
                }
                items.push(Item {
                    src: path,
                    rel: child,
                    meta,
                    link: None,
                });
            }
        }

        if dst.supports_symlinks() {
            let links: Vec<(usize, String)> = items
                .iter()
                .enumerate()
                .filter(|(_, i)| i.meta.kind == FileKind::Symlink)
                .map(|(n, i)| (n, i.src.clone()))
                .collect();
            let mut targets = stream::iter(links)
                .map(|(n, path)| async move { (n, src.read_link(&path).await) })
                .buffer_unordered(SCAN_CONCURRENCY);
            while let Some((n, target)) = targets.next().await {
                items[n].link = Some(target?);
            }
        }
        Ok(Plan { root, items })
    }

    async fn make_dir(&mut self, dst: &Endpoint, path: &str) -> Result<(), Stop> {
        match dst.lstat(path).await? {
            None => {
                dst.mkdir(path).await?;
                self.progress.created_dirs.push(self.progress.next);
            }
            Some(m) if m.kind == FileKind::Dir => {}
            Some(m)
                if m.kind == FileKind::Symlink
                    && dst
                        .stat(path)
                        .await?
                        .is_some_and(|t| t.kind == FileKind::Dir) => {}
            Some(_) => {
                return Err(Error::AlreadyExists(format!("{path} (not a folder)")).into());
            }
        }
        Ok(())
    }

    async fn copy_file(
        &mut self,
        src: &Endpoint,
        dst: &Endpoint,
        item: &Item,
        path: String,
    ) -> Result<(), Stop> {
        let target = match self.progress.current.clone() {
            Some(target) => target,
            None => match self.decide(dst, item, path).await? {
                Some(target) => target,
                None => {
                    self.skip(item);
                    return Ok(());
                }
            },
        };
        self.progress.current = Some(target.clone());
        self.progress.partial = Some(target.partial.clone());

        let size = item.meta.size;
        let offset = match dst.lstat(&target.partial).await? {
            Some(m) if !target.restart && m.kind == FileKind::File && m.size <= size => m.size,
            _ => 0,
        };
        self.info.done_bytes = self.progress.completed_bytes + offset;
        if offset > 0 {
            // Resuming a partial is worth showing right away.
            self.publish();
        } else {
            self.maybe_publish();
        }

        let (reader, writer) = tokio::join!(
            src.reader(&item.src, offset, size),
            dst.writer(&target.partial, offset)
        );
        let (reader, writer) = match (reader, writer) {
            (Ok(reader), Ok(writer)) => (reader, writer),
            (Ok(reader), Err(e)) => {
                reader.close().await;
                return Err(e.into());
            }
            (Err(e), Ok(writer)) => {
                writer.abort().await;
                return Err(e.into());
            }
            (Err(e), Err(_)) => return Err(e.into()),
        };
        if let Some(current) = &mut self.progress.current {
            current.restart = false;
        }
        if !self.stream(reader, writer).await? {
            return Err(Stop::Paused);
        }

        let keep_perms = src.has_unix_permissions() && dst.has_unix_permissions();
        let perms = item.meta.perms.filter(|_| keep_perms).map(|p| p & 0o777);
        if let Err(e) = dst
            .set_times_and_mode(&target.partial, item.meta.mtime, perms)
            .await
        {
            tracing::debug!("setting attributes of {}: {e}", target.partial);
        }
        dst.replace(&target.partial, &target.path).await?;

        self.progress.partial = None;
        self.progress.completed_bytes += size;
        self.info.done_bytes = self.progress.completed_bytes;
        self.info.done_files += 1;
        self.maybe_publish();
        Ok(())
    }

    /// Copy until EOF. Returns false if a pause stopped it early (everything
    /// read so far has been written).
    async fn stream(&mut self, mut reader: Reader, mut writer: Writer) -> Result<bool, Stop> {
        let control = self.control.clone();
        let mut pausing = false;
        let mut acked = |n: u64| self.add_bytes(n);
        let copied = async {
            loop {
                match *control.borrow() {
                    Control::Cancel => return Err(Error::Cancelled),
                    Control::Pause if !pausing => {
                        reader.stop();
                        pausing = true;
                    }
                    _ => {}
                }
                let Some(chunk) = reader.next().await? else {
                    return Ok(());
                };
                writer.write(chunk, &mut acked).await?;
            }
        }
        .await;
        let result = match copied {
            Ok(()) => tokio::join!(writer.finish(&mut acked), reader.close()).0,
            Err(e) => {
                tokio::join!(writer.abort(), reader.close());
                Err(e)
            }
        };
        result?;
        Ok(!pausing)
    }

    /// Where a file goes, asking about conflicts. `None` = skip it.
    async fn decide(
        &mut self,
        dst: &Endpoint,
        item: &Item,
        path: String,
    ) -> Result<Option<Target>, Stop> {
        let partial = format!("{path}{PARTIAL_SUFFIX}");
        let Some(existing) = dst.lstat(&path).await? else {
            return Ok(Some(Target {
                path,
                partial,
                restart: false,
            }));
        };
        let size = item.meta.size;
        let has_partial = dst
            .lstat(&partial)
            .await?
            .is_some_and(|m| m.kind == FileKind::File && m.size > 0 && m.size <= size);
        let can_grow = existing.kind == FileKind::File && existing.size < size;
        let resolution = self
            .resolve(&path, item, &existing, has_partial || can_grow)
            .await?;
        let overwrite = |path: String, partial: String| {
            if existing.kind == FileKind::Dir {
                return Err(Stop::Failed(Error::AlreadyExists(format!(
                    "{path} (a folder)"
                ))));
            }
            Ok(Some(Target {
                path,
                partial,
                restart: true,
            }))
        };
        match resolution {
            ConflictResolution::Skip => Ok(None),
            ConflictResolution::Overwrite => overwrite(path, partial),
            ConflictResolution::OverwriteIfNewer => {
                if is_newer(item.meta.mtime, existing.mtime) {
                    overwrite(path, partial)
                } else {
                    Ok(None)
                }
            }
            ConflictResolution::Rename => {
                let path = self.free_name(dst, &path).await?;
                Ok(Some(Target {
                    partial: format!("{path}{PARTIAL_SUFFIX}"),
                    path,
                    restart: false,
                }))
            }
            ConflictResolution::Resume if has_partial => Ok(Some(Target {
                path,
                partial,
                restart: false,
            })),
            ConflictResolution::Resume if can_grow => {
                // Treat the shorter existing file as the partial copy.
                dst.replace(&path, &partial).await?;
                Ok(Some(Target {
                    path,
                    partial,
                    restart: false,
                }))
            }
            ConflictResolution::Resume => Ok(None),
        }
    }

    /// `name (n).ext` next to `path` that does not exist yet.
    async fn free_name(&self, dst: &Endpoint, path: &str) -> Result<String> {
        let parent = dst
            .parent(path)
            .ok_or_else(|| Error::Other(format!("{path} has no parent")))?;
        let name = dst.file_name(path);
        for n in 1..10_000 {
            let candidate = dst.join(&parent, &numbered_name(&name, n));
            if dst.lstat(&candidate).await?.is_none() {
                return Ok(candidate);
            }
        }
        Err(Error::AlreadyExists(path.to_owned()))
    }

    /// The resolution for a conflict at `path`: "apply to all" or the user's answer.
    async fn resolve(
        &mut self,
        path: &str,
        item: &Item,
        existing: &Meta,
        can_resume: bool,
    ) -> Result<ConflictResolution, Stop> {
        if let Some(resolution) = self.progress.apply_to_all {
            return Ok(resolution);
        }
        self.set_state(TransferState::WaitingForUser);
        let (reply, answer) = oneshot::channel();
        let request = PromptRequest {
            id: uuid::Uuid::new_v4(),
            host_id: self.request.destination.host_id.clone(),
            prompt: Prompt::FileConflict {
                transfer_id: self.info.id,
                path: path.to_owned(),
                source: file_stat(&item.meta),
                existing: file_stat(existing),
                can_resume,
            },
            reply,
        };
        let prompts = self.inner.prompts.clone();
        let ask = async move {
            prompts.send(request).await.ok()?;
            answer.await.ok()
        };
        let mut control = self.control.clone();
        let answer = tokio::select! {
            answer = ask => answer.ok_or(Stop::Cancelled)?,
            _ = control.wait_for(|c| *c == Control::Cancel) => return Err(Stop::Cancelled),
        };
        self.set_state(TransferState::Running);
        match answer {
            PromptResponse::Conflict {
                resolution,
                apply_to_all,
            } => {
                if apply_to_all {
                    self.progress.apply_to_all = Some(resolution);
                }
                Ok(resolution)
            }
            _ => Err(Stop::Cancelled),
        }
    }

    async fn copy_symlink(
        &mut self,
        dst: &Endpoint,
        item: &Item,
        path: String,
    ) -> Result<(), Stop> {
        let Some(target) = item.link.as_deref().filter(|_| dst.supports_symlinks()) else {
            self.skip(item);
            return Ok(());
        };
        let mut path = path;
        if let Some(existing) = dst.lstat(&path).await? {
            match self.resolve(&path, item, &existing, false).await? {
                ConflictResolution::Overwrite => {}
                ConflictResolution::OverwriteIfNewer
                    if is_newer(item.meta.mtime, existing.mtime) => {}
                ConflictResolution::Rename => {
                    path = self.free_name(dst, &path).await?;
                }
                _ => {
                    self.skip(item);
                    return Ok(());
                }
            }
            if path_is_taken(dst, &path).await? {
                if existing.kind == FileKind::Dir {
                    return Err(Error::AlreadyExists(format!("{path} (a folder)")).into());
                }
                dst.remove_file(&path).await?;
            }
        }
        dst.symlink(&path, target).await?;
        self.info.done_files += 1;
        self.maybe_publish();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_is_a_moving_average() {
        let mut rate = Rate::default();
        let t0 = Instant::now();
        assert_eq!(rate.sample(t0, 0), 0);
        assert_eq!(rate.sample(t0 + Duration::from_secs(1), 1000), 1000);
        assert_eq!(rate.sample(t0 + Duration::from_secs(2), 3000), 1500);
        // Old samples fall out of the window.
        rate.sample(t0 + Duration::from_secs(5), 6000);
        assert_eq!(rate.sample(t0 + Duration::from_secs(6), 7000), 1000);
    }

    #[test]
    fn newer_needs_both_times() {
        assert!(is_newer(Some(2), Some(1)));
        assert!(!is_newer(Some(1), Some(1)));
        assert!(!is_newer(None, Some(1)));
        assert!(!is_newer(Some(1), None));
    }
}
