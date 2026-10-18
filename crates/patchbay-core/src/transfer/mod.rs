//! The transfer queue: copies between any two machines (local or SFTP),
//! a few transfers at a time, files within a transfer one after another.
//!
//! Lifecycle: `Queued` → `Scanning` (walk the sources, count files/bytes) →
//! `Running` (⇄ `WaitingForUser` on conflicts) → `Done` / `Failed` /
//! `Cancelled`; `Paused` in between. Every state change and throttled
//! progress (at most ~8 per second) is published as
//! [`CoreEvent::Transfer`].
//!
//! Semantics:
//! - Files are written to `<name>.patchbaypart` next to the destination and
//!   renamed into place when complete. A `.patchbaypart` that already exists
//!   when a file starts is continued from its length, assuming it holds a
//!   prefix of the source (true for partials Patchbay wrote: data is written
//!   in order).
//! - Pause lets outstanding requests finish and keeps the partial; resume
//!   continues it. Cancel stops at once and deletes the current partial.
//!   Errors (lost connection, I/O) end in `Failed { resumable: true }`;
//!   retry continues with the file that failed, the scanned file list and
//!   finished files are kept.
//! - Conflicts with existing files are asked about with
//!   [`Prompt::FileConflict`](crate::model::Prompt::FileConflict) (host =
//!   destination); "apply to all" sticks for the rest of the transfer.
//!   Existing directories are merged silently.
//! - Directories get their source mtime and permission bits (when both
//!   sides have Unix permissions) after their contents are copied; only
//!   directories the transfer created are touched. Files get mtime and
//!   permission bits (`0o777` mask) the same way.
//! - Symlinks are recreated with the same target text when the destination
//!   supports them (not on Windows); otherwise, and for sockets, FIFOs and
//!   devices, the entry is skipped and counted in `skipped_files`.
//! - [Background](TransferRequest::background) transfers start right away
//!   and do not count against the concurrency limit. With
//!   [`max_bytes`](TransferRequest::max_bytes) a transfer fails (not
//!   resumable) after scanning if the sources are larger; `total_bytes` is
//!   set by then.

mod job;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::fs::FileSystem;
use crate::fs::path::{local as lpath, remote as rpath};
use crate::model::{CoreEvent, PromptSender, TransferInfo, TransferRequest, TransferState};
use crate::{Error, Result};

use job::{Job, Progress};

/// Transfers running at the same time unless changed with
/// [`TransferManager::set_max_concurrent`].
pub const DEFAULT_MAX_CONCURRENT: usize = 3;

/// Owns the transfer queue. Cheap to clone.
#[derive(Clone)]
pub struct TransferManager {
    inner: Arc<Inner>,
}

struct Inner {
    fs: FileSystem,
    events: broadcast::Sender<CoreEvent>,
    prompts: PromptSender,
    state: Mutex<State>,
}

struct State {
    max_concurrent: usize,
    active: usize,
    /// Creation order, for FIFO scheduling and listing.
    order: Vec<Uuid>,
    entries: HashMap<Uuid, Entry>,
}

struct Entry {
    info: TransferInfo,
    request: TransferRequest,
    /// Set while a job task runs this transfer.
    control: Option<watch::Sender<Control>>,
    /// Kept between runs (pause/retry); owned by the job while it runs.
    progress: Option<Progress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Run,
    Pause,
    Cancel,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl TransferManager {
    pub fn new(
        fs: FileSystem,
        events: broadcast::Sender<CoreEvent>,
        prompts: PromptSender,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                fs,
                events,
                prompts,
                state: Mutex::new(State {
                    max_concurrent: DEFAULT_MAX_CONCURRENT,
                    active: 0,
                    order: Vec::new(),
                    entries: HashMap::new(),
                }),
            }),
        }
    }

    pub fn max_concurrent(&self) -> usize {
        self.inner.lock().max_concurrent
    }

    /// Change how many transfers may run at once (at least one). Running
    /// transfers beyond a lowered limit are not interrupted.
    pub fn set_max_concurrent(&self, n: usize) {
        self.inner.lock().max_concurrent = n.max(1);
        Inner::pump(&self.inner);
    }

    /// Queue a transfer. All sources must be on the same machine; the
    /// destination is an existing directory.
    pub fn start(&self, request: TransferRequest) -> Result<TransferInfo> {
        let first = request
            .sources
            .first()
            .ok_or_else(|| Error::Other("nothing to transfer".into()))?;
        if request.sources.iter().any(|s| s.host_id != first.host_id) {
            return Err(Error::Other(
                "all sources of a transfer must be on the same machine".into(),
            ));
        }
        let name = match &first.host_id {
            None => lpath::file_name(&first.path),
            Some(_) => rpath::file_name(&first.path).to_owned(),
        };
        let label = match request.sources.len() {
            1 => name,
            n => format!("{name} and {} more", n - 1),
        };
        let info = TransferInfo {
            id: Uuid::new_v4(),
            label,
            background: request.background,
            source_host: first.host_id.clone(),
            destination: request.destination.clone(),
            state: TransferState::Queued,
            total_bytes: None,
            done_bytes: 0,
            total_files: None,
            done_files: 0,
            skipped_files: 0,
            current_file: None,
            bytes_per_sec: 0,
            created_at: now_ms(),
            finished_at: None,
        };
        {
            let mut st = self.inner.lock();
            st.order.push(info.id);
            st.entries.insert(
                info.id,
                Entry {
                    info: info.clone(),
                    request,
                    control: None,
                    progress: Some(Progress::default()),
                },
            );
        }
        self.inner.emit(&info);
        Inner::pump(&self.inner);
        Ok(info)
    }

    /// All transfers, oldest first.
    pub fn list(&self) -> Vec<TransferInfo> {
        let st = self.inner.lock();
        st.order
            .iter()
            .filter_map(|id| st.entries.get(id))
            .map(|e| e.info.clone())
            .collect()
    }

    pub fn get(&self, id: Uuid) -> Option<TransferInfo> {
        self.inner.lock().entries.get(&id).map(|e| e.info.clone())
    }

    /// Pause a queued or running transfer (running ones stop once their
    /// outstanding requests have finished).
    pub fn pause(&self, id: Uuid) -> Result<()> {
        self.inner.update(id, |entry| {
            if let Some(control) = &entry.control {
                control.send_if_modified(|c| {
                    let changed = *c == Control::Run;
                    if changed {
                        *c = Control::Pause;
                    }
                    changed
                });
                false
            } else if entry.info.state == TransferState::Queued {
                entry.info.state = TransferState::Paused;
                true
            } else {
                false
            }
        })
    }

    /// Continue a paused transfer (it queues again for a free slot).
    pub fn resume(&self, id: Uuid) -> Result<()> {
        self.inner.update(id, |entry| {
            if let Some(control) = &entry.control {
                control.send_if_modified(|c| {
                    let changed = *c == Control::Pause;
                    if changed {
                        *c = Control::Run;
                    }
                    changed
                });
                false
            } else if entry.info.state == TransferState::Paused {
                entry.info.state = TransferState::Queued;
                true
            } else {
                false
            }
        })?;
        Inner::pump(&self.inner);
        Ok(())
    }

    /// Stop a transfer and delete the partial copy of its current file.
    pub fn cancel(&self, id: Uuid) -> Result<()> {
        let mut cleanup = None;
        self.inner.update(id, |entry| {
            if let Some(control) = &entry.control {
                control.send_replace(Control::Cancel);
                return false;
            }
            match entry.info.state {
                TransferState::Queued | TransferState::Paused | TransferState::Failed { .. } => {
                    entry.info.state = TransferState::Cancelled;
                    entry.info.finished_at = Some(now_ms());
                    entry.info.bytes_per_sec = 0;
                    if let Some(progress) = &mut entry.progress {
                        cleanup = progress
                            .partial
                            .take()
                            .map(|p| (entry.request.destination.host_id.clone(), p));
                    }
                    true
                }
                _ => false,
            }
        })?;
        if let Some((host, partial)) = cleanup {
            let fs = self.inner.fs.clone();
            tokio::spawn(async move {
                if let Ok(dst) = fs.endpoint(host.as_deref(), true).await {
                    let _ = dst.remove_file(&partial).await;
                }
            });
        }
        Ok(())
    }

    /// Run a failed or cancelled transfer again, continuing where it
    /// stopped (finished files are not copied again, partials are continued).
    pub fn retry(&self, id: Uuid) -> Result<()> {
        self.inner.update(id, |entry| {
            let retryable = matches!(
                entry.info.state,
                TransferState::Failed { .. } | TransferState::Cancelled
            );
            if entry.control.is_none() && retryable {
                entry.info.state = TransferState::Queued;
                entry.info.finished_at = None;
                true
            } else {
                false
            }
        })?;
        Inner::pump(&self.inner);
        Ok(())
    }

    /// Forget finished, failed and cancelled transfers.
    pub fn clear_finished(&self) {
        let mut st = self.inner.lock();
        let State { order, entries, .. } = &mut *st;
        entries.retain(|_, e| {
            e.control.is_some()
                || !matches!(
                    e.info.state,
                    TransferState::Done | TransferState::Failed { .. } | TransferState::Cancelled
                )
        });
        order.retain(|id| entries.contains_key(id));
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn emit(&self, info: &TransferInfo) {
        let _ = self.events.send(CoreEvent::Transfer(info.clone()));
    }

    /// Apply `f` to an entry; emits its info if `f` returns true.
    fn update(&self, id: Uuid, f: impl FnOnce(&mut Entry) -> bool) -> Result<()> {
        let mut st = self.lock();
        let entry = st
            .entries
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("transfer {id}")))?;
        if f(entry) {
            self.emit(&entry.info);
        }
        Ok(())
    }

    /// Record a snapshot published by a running job.
    fn publish(&self, info: &TransferInfo) {
        let mut st = self.lock();
        if let Some(entry) = st.entries.get_mut(&info.id) {
            entry.info = info.clone();
        }
        self.emit(info);
    }

    /// Start queued transfers while slots are free (background ones always).
    fn pump(this: &Arc<Self>) {
        let mut st = this.lock();
        loop {
            let has_slot = st.active < st.max_concurrent;
            let Some(id) = st.order.iter().copied().find(|id| {
                st.entries.get(id).is_some_and(|e| {
                    e.control.is_none()
                        && e.info.state == TransferState::Queued
                        && (has_slot || e.info.background)
                })
            }) else {
                break;
            };
            let Some(entry) = st.entries.get_mut(&id) else {
                break;
            };
            let (tx, rx) = watch::channel(Control::Run);
            entry.control = Some(tx);
            let background = entry.info.background;
            let job = Job::new(
                this.clone(),
                entry.info.clone(),
                entry.request.clone(),
                entry.progress.take().unwrap_or_default(),
                rx,
            );
            if !background {
                st.active += 1;
            }
            tokio::spawn(job.run());
        }
    }

    /// Called by a job when it stops for any reason.
    fn finished(this: &Arc<Self>, info: TransferInfo, progress: Progress) {
        {
            let mut st = this.lock();
            if !info.background {
                st.active = st.active.saturating_sub(1);
            }
            if let Some(entry) = st.entries.get_mut(&info.id) {
                entry.info = info.clone();
                entry.progress = Some(progress);
                entry.control = None;
            }
        }
        this.emit(&info);
        Self::pump(this);
    }
}

#[cfg(test)]
mod tests;
