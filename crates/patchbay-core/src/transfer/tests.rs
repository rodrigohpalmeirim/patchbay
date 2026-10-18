//! Local-to-local transfers (the remote paths are covered by the Docker
//! integration tests).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use super::*;
use crate::model::{ConflictResolution, FileRef, Prompt, PromptRequest, PromptResponse};
use crate::session::SessionManager;

struct Harness {
    manager: TransferManager,
    events: broadcast::Receiver<CoreEvent>,
    prompts: mpsc::Receiver<PromptRequest>,
    dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let (events_tx, events) = broadcast::channel(4096);
        let (prompts_tx, prompts) = mpsc::channel(16);
        let fs = FileSystem::new(SessionManager::new(events_tx.clone(), prompts_tx.clone()));
        Self {
            manager: TransferManager::new(fs, events_tx, prompts_tx),
            events,
            prompts,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn write(&self, rel: &str, data: &[u8]) -> PathBuf {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path
    }

    fn request(&self, sources: &[&str], dest: &str) -> TransferRequest {
        TransferRequest {
            sources: sources.iter().map(|s| local_ref(&self.path(s))).collect(),
            destination: local_ref(&self.path(dest)),
            ..TransferRequest::default()
        }
    }

    fn start(&self, sources: &[&str], dest: &str) -> Uuid {
        self.manager.start(self.request(sources, dest)).unwrap().id
    }

    /// Wait for an event of transfer `id` whose state satisfies `pred`.
    async fn wait(&mut self, id: Uuid, pred: impl Fn(&TransferState) -> bool) -> TransferInfo {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match self.events.recv().await {
                    Ok(CoreEvent::Transfer(info)) if info.id == id && pred(&info.state) => {
                        return info;
                    }
                    Ok(_) => {}
                    Err(e) => panic!("event stream: {e}"),
                }
            }
        })
        .await
        .expect("timed out waiting for transfer state")
    }

    async fn finished(&mut self, id: Uuid) -> TransferInfo {
        self.wait(id, |s| {
            matches!(
                s,
                TransferState::Done | TransferState::Failed { .. } | TransferState::Cancelled
            )
        })
        .await
    }

    async fn prompt(&mut self) -> PromptRequest {
        tokio::time::timeout(Duration::from_secs(20), self.prompts.recv())
            .await
            .expect("timed out waiting for a prompt")
            .expect("prompt channel closed")
    }
}

fn local_ref(path: &Path) -> FileRef {
    FileRef {
        host_id: None,
        path: path.to_string_lossy().into_owned(),
    }
}

fn answer(request: PromptRequest, resolution: ConflictResolution, apply_to_all: bool) {
    request
        .reply
        .send(PromptResponse::Conflict {
            resolution,
            apply_to_all,
        })
        .unwrap();
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn copies_a_tree_with_metadata() {
    let mut h = Harness::new();
    let big = data(3 * 1024 * 1024 + 17);
    h.write("src/tree/big.bin", &big);
    h.write("src/tree/sub/deeper/small.txt", b"small");
    h.write("src/single.txt", b"single");
    fs::create_dir_all(h.path("src/tree/empty")).unwrap();
    fs::create_dir_all(h.path("dst")).unwrap();
    let mtime = std::time::UNIX_EPOCH + Duration::from_secs(1_500_000_000);
    fs::File::options()
        .write(true)
        .open(h.path("src/tree/sub/deeper/small.txt"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(h.path("src/single.txt"), fs::Permissions::from_mode(0o751)).unwrap();
        std::os::unix::fs::symlink("sub/deeper", h.path("src/tree/link")).unwrap();
    }

    let id = h.start(&["src/tree", "src/single.txt"], "dst");
    let info = h.finished(id).await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    assert_eq!(info.label, "tree and 1 more");
    assert_eq!(info.total_bytes, Some(big.len() as u64 + 5 + 6));
    assert_eq!(info.done_bytes, info.total_bytes.unwrap());
    assert!(info.finished_at.is_some());

    assert_eq!(fs::read(h.path("dst/tree/big.bin")).unwrap(), big);
    assert_eq!(
        fs::read(h.path("dst/tree/sub/deeper/small.txt")).unwrap(),
        b"small"
    );
    assert_eq!(fs::read(h.path("dst/single.txt")).unwrap(), b"single");
    assert!(h.path("dst/tree/empty").is_dir());
    assert_eq!(
        fs::metadata(h.path("dst/tree/sub/deeper/small.txt"))
            .unwrap()
            .modified()
            .unwrap(),
        mtime
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(h.path("dst/single.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o751);
        assert_eq!(
            fs::read_link(h.path("dst/tree/link")).unwrap(),
            Path::new("sub/deeper")
        );
        assert_eq!(info.total_files, Some(4));
        assert_eq!(info.done_files, 4);
    }
    // No partials left behind.
    let leftovers: Vec<_> = walk(&h.path("dst"))
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".patchbaypart"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert_eq!(h.manager.list().len(), 1);
    h.manager.clear_finished();
    assert!(h.manager.list().is_empty());
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() && !path.is_symlink() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn continues_an_existing_partial() {
    let mut h = Harness::new();
    let source = data(1024 * 1024);
    h.write("src/file.bin", &source);
    // A partial whose prefix differs from the source proves it was continued
    // rather than rewritten.
    let mut partial = vec![0xAA; 300_000];
    h.write("dst/file.bin.patchbaypart", &partial);

    let id = h.start(&["src/file.bin"], "dst");
    let info = h.finished(id).await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    partial.extend_from_slice(&source[300_000..]);
    assert_eq!(fs::read(h.path("dst/file.bin")).unwrap(), partial);
    assert!(!h.path("dst/file.bin.patchbaypart").exists());
    assert_eq!(info.done_bytes, source.len() as u64);
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicts_rename_skip_overwrite_and_apply_to_all() {
    let mut h = Harness::new();
    h.write("src/a.txt", b"new a");
    h.write("src/b.txt", b"new b");
    h.write("src/c.txt", b"new c");
    h.write("dst/a.txt", b"old a");
    h.write("dst/b.txt", b"old b");
    h.write("dst/c.txt", b"old c");
    h.write("dst/c (1).txt", b"taken");

    let id = h.start(&["src/a.txt", "src/b.txt"], "dst");
    let prompt = h.prompt().await;
    match &prompt.prompt {
        Prompt::FileConflict {
            transfer_id,
            path,
            source,
            existing,
            can_resume,
        } => {
            assert_eq!(*transfer_id, id);
            assert!(path.ends_with("a.txt"));
            assert_eq!(source.size, 5);
            assert_eq!(existing.size, 5);
            assert!(!can_resume);
        }
        other => panic!("unexpected prompt {other:?}"),
    }
    assert_eq!(prompt.host_id, None);
    assert_eq!(
        h.manager.get(id).unwrap().state,
        TransferState::WaitingForUser
    );
    answer(prompt, ConflictResolution::Skip, false);
    let prompt = h.prompt().await;
    answer(prompt, ConflictResolution::Overwrite, false);
    let info = h.finished(id).await;
    assert_eq!(info.state, TransferState::Done);
    assert_eq!(info.skipped_files, 1);
    assert_eq!(info.done_files, 1);
    assert_eq!(fs::read(h.path("dst/a.txt")).unwrap(), b"old a");
    assert_eq!(fs::read(h.path("dst/b.txt")).unwrap(), b"new b");

    // Rename picks the next free number; apply-to-all answers the rest.
    let id = h.start(&["src/a.txt", "src/c.txt"], "dst");
    let prompt = h.prompt().await;
    answer(prompt, ConflictResolution::Rename, true);
    let info = h.finished(id).await;
    assert_eq!(info.state, TransferState::Done);
    assert_eq!(fs::read(h.path("dst/a (1).txt")).unwrap(), b"new a");
    assert_eq!(fs::read(h.path("dst/c (2).txt")).unwrap(), b"new c");
    assert_eq!(fs::read(h.path("dst/c (1).txt")).unwrap(), b"taken");
    assert!(h.prompts.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn overwrite_if_newer_and_resume_resolutions() {
    let mut h = Harness::new();
    let source = data(100_000);
    h.write("src/grow.bin", &source);
    h.write("src/old.txt", b"older source");
    // The destination holds the first part of grow.bin: "resume" appends.
    h.write("dst/grow.bin", &source[..40_000]);
    h.write("dst/old.txt", b"newer destination");
    let past = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    fs::File::options()
        .write(true)
        .open(h.path("src/old.txt"))
        .unwrap()
        .set_modified(past)
        .unwrap();

    let id = h.start(&["src/grow.bin"], "dst");
    let prompt = h.prompt().await;
    assert!(matches!(
        prompt.prompt,
        Prompt::FileConflict {
            can_resume: true,
            ..
        }
    ));
    answer(prompt, ConflictResolution::Resume, false);
    assert_eq!(h.finished(id).await.state, TransferState::Done);
    assert_eq!(fs::read(h.path("dst/grow.bin")).unwrap(), source);

    let id = h.start(&["src/old.txt"], "dst");
    answer(
        h.prompt().await,
        ConflictResolution::OverwriteIfNewer,
        false,
    );
    let info = h.finished(id).await;
    assert_eq!(info.skipped_files, 1);
    assert_eq!(
        fs::read(h.path("dst/old.txt")).unwrap(),
        b"newer destination"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_prompt_and_cancel_while_waiting_cancel_the_transfer() {
    let mut h = Harness::new();
    h.write("src/a.txt", b"a");
    h.write("dst/a.txt", b"old");

    let id = h.start(&["src/a.txt"], "dst");
    drop(h.prompt().await);
    let info = h.finished(id).await;
    assert_eq!(info.state, TransferState::Cancelled);

    let id = h.start(&["src/a.txt"], "dst");
    let prompt = h.prompt().await;
    h.manager.cancel(id).unwrap();
    let info = h.finished(id).await;
    assert_eq!(info.state, TransferState::Cancelled);
    assert!(prompt.reply.is_closed());
    assert_eq!(fs::read(h.path("dst/a.txt")).unwrap(), b"old");

    // A cancelled transfer can be retried.
    h.manager.retry(id).unwrap();
    answer(h.prompt().await, ConflictResolution::Overwrite, false);
    assert_eq!(h.finished(id).await.state, TransferState::Done);
    assert_eq!(fs::read(h.path("dst/a.txt")).unwrap(), b"a");
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_keeps_the_partial_and_cancel_removes_it() {
    let mut h = Harness::new();
    let source = data(2 * 1024 * 1024);
    h.write("src/f.bin", &source);
    h.write("dst/f.bin", b"old");

    // Pausing while the conflict prompt is open: the copy stops before the
    // first chunk, leaving an (empty) partial.
    let id = h.start(&["src/f.bin"], "dst");
    let prompt = h.prompt().await;
    h.manager.pause(id).unwrap();
    answer(prompt, ConflictResolution::Overwrite, false);
    let info = h.wait(id, |s| *s == TransferState::Paused).await;
    assert_eq!(info.done_files, 0);
    assert!(h.path("dst/f.bin.patchbaypart").exists());

    // Resume continues with the decision already made (no second prompt).
    h.manager.resume(id).unwrap();
    assert_eq!(h.finished(id).await.state, TransferState::Done);
    assert_eq!(fs::read(h.path("dst/f.bin")).unwrap(), source);
    assert!(h.prompts.try_recv().is_err());

    // Same again, but cancel the paused transfer: the partial goes away.
    let id = h.start(&["src/f.bin"], "dst");
    let prompt = h.prompt().await;
    h.manager.pause(id).unwrap();
    answer(prompt, ConflictResolution::Overwrite, false);
    h.wait(id, |s| *s == TransferState::Paused).await;
    assert!(h.path("dst/f.bin.patchbaypart").exists());
    h.manager.cancel(id).unwrap();
    assert_eq!(h.finished(id).await.state, TransferState::Cancelled);
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.path("dst/f.bin.patchbaypart").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("partial not removed");
    assert_eq!(fs::read(h.path("dst/f.bin")).unwrap(), source);
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_respects_the_concurrency_limit() {
    let mut h = Harness::new();
    h.manager.set_max_concurrent(1);
    assert_eq!(h.manager.max_concurrent(), 1);
    h.write("src/a.txt", b"a");
    h.write("src/b.txt", b"b");
    h.write("dst/a.txt", b"old");

    // The first transfer blocks on its prompt; the second must stay queued.
    let first = h.start(&["src/a.txt"], "dst");
    let prompt = h.prompt().await;
    let second = h.start(&["src/b.txt"], "dst");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.manager.get(second).unwrap().state, TransferState::Queued);
    // Pausing a queued transfer and resuming it requeues it.
    h.manager.pause(second).unwrap();
    assert_eq!(h.manager.get(second).unwrap().state, TransferState::Paused);
    h.manager.resume(second).unwrap();
    answer(prompt, ConflictResolution::Skip, false);
    assert_eq!(h.finished(first).await.state, TransferState::Done);
    assert_eq!(h.finished(second).await.state, TransferState::Done);
    assert_eq!(fs::read(h.path("dst/b.txt")).unwrap(), b"b");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_requests() {
    let mut h = Harness::new();
    h.write("src/dir/x", b"x");
    h.write("other", b"o");
    assert!(
        h.manager
            .start(TransferRequest {
                sources: vec![],
                destination: local_ref(&h.path("src")),
                ..TransferRequest::default()
            })
            .is_err()
    );
    let id = h.start(&["src/dir"], "src/dir");
    let info = h.finished(id).await;
    assert!(
        matches!(&info.state, TransferState::Failed { message, .. } if message.contains("into itself")),
        "{info:?}"
    );
    let id = h.start(&["src/dir"], "other");
    assert!(matches!(
        h.finished(id).await.state,
        TransferState::Failed { .. }
    ));
    let id = h.start(&["missing"], "src");
    assert!(matches!(
        h.finished(id).await.state,
        TransferState::Failed { .. }
    ));
    assert!(h.manager.pause(Uuid::new_v4()).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn background_transfers_skip_the_queue_and_respect_max_bytes() {
    let mut h = Harness::new();
    h.write("src/a.txt", b"a");
    h.write("src/big.bin", &data(4096));
    h.write("dst/a.txt", b"old");
    fs::create_dir_all(h.path("bg")).unwrap();
    h.manager.set_max_concurrent(1);

    // Occupy the only slot with a transfer waiting for a conflict answer.
    let blocker = h.start(&["src/a.txt"], "dst");
    let prompt = h.prompt().await;
    let queued = h.start(&["src/a.txt"], "bg");
    let background = h
        .manager
        .start(TransferRequest {
            background: true,
            ..h.request(&["src/big.bin"], "bg")
        })
        .unwrap();
    assert!(background.background);
    let info = h.finished(background.id).await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    assert_eq!(h.manager.get(queued).unwrap().state, TransferState::Queued);

    let capped = h
        .manager
        .start(TransferRequest {
            background: true,
            max_bytes: Some(1000),
            ..h.request(&["src/big.bin"], "dst")
        })
        .unwrap()
        .id;
    let info = h.finished(capped).await;
    assert!(
        matches!(
            &info.state,
            TransferState::Failed {
                resumable: false,
                ..
            }
        ),
        "{info:?}"
    );
    assert_eq!(info.total_bytes, Some(4096));
    assert!(!h.path("dst/big.bin").exists());

    answer(prompt, ConflictResolution::Skip, false);
    assert_eq!(h.finished(blocker).await.state, TransferState::Done);
    assert_eq!(h.finished(queued).await.state, TransferState::Done);
}
