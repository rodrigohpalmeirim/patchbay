//! Local copies of remote files for dragging them out of Patchbay onto the
//! desktop or into a file manager.
//!
//! An OS drag-and-drop needs real local paths when it starts, so the
//! frontend asks for a copy as soon as an internal drag of remote entries
//! begins ([`DragOutCache::prepare`]). The copy is a
//! [background](crate::model::TransferRequest::background) transfer into a
//! fresh `<cache_dir>/patchbay/dragout/<id>/` directory, capped at
//! [`MAX_BYTES`]. Copies are deleted by [`release`](DragOutCache::release)
//! when the drag ends inside the window, by [`sweep`](DragOutCache::sweep)
//! once they are older than [`MAX_AGE`] (the drop target has long copied
//! them by then), and by [`clear`](DragOutCache::clear) on startup and exit.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::fs::path::{local as lpath, remote as rpath};
use crate::model::{DragOut, FileRef, TransferRequest, TransferState};
use crate::transfer::TransferManager;
use crate::{Error, Result};

/// Largest total size that is copied for a drag-out.
pub const MAX_BYTES: u64 = 200 * 1024 * 1024;
/// Age after which [`DragOutCache::sweep`] deletes a copy.
pub const MAX_AGE: Duration = Duration::from_secs(30 * 60);

/// Owns the drag-out copies. Cheap to clone.
#[derive(Clone)]
pub struct DragOutCache {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    transfers: TransferManager,
    entries: Mutex<HashMap<Uuid, Entry>>,
}

struct Entry {
    dir: PathBuf,
    transfer: Uuid,
    created: Instant,
}

impl DragOutCache {
    /// Copies live below `root` (see [`default_root`](Self::default_root)).
    pub fn new(root: PathBuf, transfers: TransferManager) -> Self {
        Self {
            inner: Arc::new(Inner {
                root,
                transfers,
                entries: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// `<cache_dir>/patchbay/dragout`.
    pub fn default_root() -> Result<PathBuf> {
        let cache = dirs::cache_dir()
            .ok_or_else(|| Error::Other("cannot determine the cache directory".into()))?;
        Ok(cache.join("patchbay").join("dragout"))
    }

    /// Start copying `sources` (all on one machine) into a new directory.
    /// The returned paths exist once the transfer is done.
    pub async fn prepare(&self, sources: Vec<FileRef>) -> Result<DragOut> {
        let id = Uuid::new_v4();
        let dir = self.inner.root.join(id.to_string());
        tokio::fs::create_dir_all(&dir).await?;
        let dir_str = dir.to_string_lossy().into_owned();
        let paths = sources
            .iter()
            .map(|s| {
                let name = match s.host_id {
                    Some(_) => rpath::file_name(&s.path).to_owned(),
                    None => lpath::file_name(&s.path),
                };
                lpath::join(&dir_str, &name)
            })
            .collect();
        let started = self.inner.transfers.start(TransferRequest {
            sources,
            destination: FileRef {
                host_id: None,
                path: dir_str,
            },
            background: true,
            max_bytes: Some(MAX_BYTES),
        });
        let transfer = match started {
            Ok(transfer) => transfer,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&dir).await;
                return Err(e);
            }
        };
        self.inner.lock().insert(
            id,
            Entry {
                dir,
                transfer: transfer.id,
                created: Instant::now(),
            },
        );
        Ok(DragOut {
            id,
            transfer,
            paths,
        })
    }

    /// The copy is not needed (the drag ended inside Patchbay): stop its
    /// transfer and delete it.
    pub async fn release(&self, id: Uuid) {
        let entry = self.inner.lock().remove(&id);
        if let Some(entry) = entry {
            self.inner.discard(entry).await;
        }
    }

    /// Delete copies older than `max_age` whose transfer has finished.
    pub async fn sweep(&self, max_age: Duration) {
        let expired: Vec<Entry> = {
            let mut entries = self.inner.lock();
            let ids: Vec<Uuid> = entries
                .iter()
                .filter(|(_, e)| e.created.elapsed() >= max_age && !self.inner.is_running(e))
                .map(|(id, _)| *id)
                .collect();
            ids.iter().filter_map(|id| entries.remove(id)).collect()
        };
        for entry in expired {
            self.inner.discard(entry).await;
        }
    }

    /// Delete every copy (and leftovers of earlier runs), without waiting
    /// for transfers still writing them: for startup and exit. Blocks.
    pub fn clear(&self) {
        self.inner.lock().clear();
        if let Err(e) = std::fs::remove_dir_all(&self.inner.root)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("cannot delete {}: {e}", self.inner.root.display());
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, HashMap<Uuid, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_running(&self, entry: &Entry) -> bool {
        self.transfers.get(entry.transfer).is_some_and(|t| {
            !matches!(
                t.state,
                TransferState::Done | TransferState::Failed { .. } | TransferState::Cancelled
            )
        })
    }

    /// Cancel the entry's transfer, wait (briefly) until it stopped, then
    /// delete the directory.
    async fn discard(&self, entry: Entry) {
        let _ = self.transfers.cancel(entry.transfer);
        for _ in 0..50 {
            if !self.is_running(&entry) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Err(e) = tokio::fs::remove_dir_all(&entry.dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!("cannot delete {}: {e}", entry.dir.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FileSystem;
    use crate::model::CoreEvent;
    use crate::session::SessionManager;
    use tokio::sync::{broadcast, mpsc};

    fn cache() -> (
        DragOutCache,
        broadcast::Receiver<CoreEvent>,
        tempfile::TempDir,
    ) {
        let (events_tx, events) = broadcast::channel(256);
        let (prompts_tx, _) = mpsc::channel(4);
        let fs = FileSystem::new(SessionManager::new(events_tx.clone(), prompts_tx.clone()));
        let transfers = TransferManager::new(fs, events_tx, prompts_tx);
        let tmp = tempfile::tempdir().unwrap();
        (
            DragOutCache::new(tmp.path().join("dragout"), transfers),
            events,
            tmp,
        )
    }

    async fn finished(events: &mut broadcast::Receiver<CoreEvent>, id: Uuid) -> TransferState {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(CoreEvent::Transfer(t)) = events.recv().await
                    && t.id == id
                    && matches!(
                        t.state,
                        TransferState::Done
                            | TransferState::Failed { .. }
                            | TransferState::Cancelled
                    )
                {
                    return t.state;
                }
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn copies_then_sweeps_and_clears() {
        let (cache, mut events, tmp) = cache();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("folder")).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::write(src.join("folder/b.txt"), b"b").unwrap();
        let local = |p: &str| FileRef {
            host_id: None,
            path: src.join(p).to_string_lossy().into_owned(),
        };

        let out = cache
            .prepare(vec![local("a.txt"), local("folder")])
            .await
            .unwrap();
        assert!(out.transfer.background);
        assert_eq!(
            finished(&mut events, out.transfer.id).await,
            TransferState::Done
        );
        assert_eq!(std::fs::read(&out.paths[0]).unwrap(), b"a");
        assert_eq!(
            std::fs::read(PathBuf::from(&out.paths[1]).join("b.txt")).unwrap(),
            b"b"
        );

        cache.sweep(MAX_AGE).await;
        assert!(PathBuf::from(&out.paths[0]).exists());
        cache.sweep(Duration::ZERO).await;
        assert!(!PathBuf::from(&out.paths[0]).exists());

        let again = cache.prepare(vec![local("a.txt")]).await.unwrap();
        finished(&mut events, again.transfer.id).await;
        cache.release(again.id).await;
        assert!(!PathBuf::from(&again.paths[0]).exists());

        let last = cache.prepare(vec![local("a.txt")]).await.unwrap();
        finished(&mut events, last.transfer.id).await;
        cache.clear();
        assert!(!tmp.path().join("dragout").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn too_large_fails_after_scanning() {
        let (cache, mut events, tmp) = cache();
        let big = tmp.path().join("big.bin");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        let out = cache
            .prepare(vec![FileRef {
                host_id: None,
                path: big.to_string_lossy().into_owned(),
            }])
            .await
            .unwrap();
        assert!(matches!(
            finished(&mut events, out.transfer.id).await,
            TransferState::Failed {
                resumable: false,
                ..
            }
        ));
        assert!(!PathBuf::from(&out.paths[0]).exists());
    }
}
