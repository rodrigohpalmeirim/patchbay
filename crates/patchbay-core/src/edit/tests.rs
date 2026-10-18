//! Local-to-local edit sessions (`host: None`); the remote paths, conflicts
//! over SFTP and reconnecting are covered by the Docker integration tests.

use std::fs;
use std::path::{Path, PathBuf};

use tokio::sync::{broadcast, mpsc};

use super::*;
use crate::session::SessionManager;

struct Harness {
    edits: EditManager,
    events: broadcast::Receiver<CoreEvent>,
    prompts: mpsc::Receiver<PromptRequest>,
    events_tx: broadcast::Sender<CoreEvent>,
    prompts_tx: PromptSender,
    fs: FileSystem,
    dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (events_tx, events) = broadcast::channel(1024);
        let (prompts_tx, prompts) = mpsc::channel(16);
        let fs = FileSystem::new(SessionManager::new(events_tx.clone(), prompts_tx.clone()));
        let edits = EditManager::new(
            fs.clone(),
            events_tx.clone(),
            prompts_tx.clone(),
            dir.path().join("edit"),
        );
        Self {
            edits,
            events,
            prompts,
            events_tx,
            prompts_tx,
            fs,
            dir,
        }
    }

    /// A second manager on the same cache directory (the next app run).
    fn restart(&self) -> EditManager {
        EditManager::new(
            self.fs.clone(),
            self.events_tx.clone(),
            self.prompts_tx.clone(),
            self.dir.path().join("edit"),
        )
    }

    fn remote(&self, name: &str, data: &[u8]) -> String {
        let path = self.dir.path().join("remote").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path.to_string_lossy().into_owned()
    }

    async fn open(&self, path: &str) -> EditSession {
        self.edits.open(None, path).await.unwrap()
    }

    /// The next edit event of `id` whose session satisfies `pred`.
    async fn wait(&mut self, id: Uuid, pred: impl Fn(&EditSession) -> bool) -> EditSession {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match self.events.recv().await {
                    Ok(CoreEvent::Edit(s)) if s.id == id && pred(&s) => return s,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("events: {e}"),
                }
            }
        })
        .await
        .expect("timed out waiting for an edit event")
    }

    async fn synced_upload(&mut self, id: Uuid) -> EditSession {
        self.wait(id, |s| {
            s.last_upload.is_some() && matches!(s.state, EditState::Synced { .. })
        })
        .await
    }

    async fn prompt(&mut self) -> PromptRequest {
        tokio::time::timeout(Duration::from_secs(10), self.prompts.recv())
            .await
            .expect("timed out waiting for a prompt")
            .unwrap()
    }
}

fn answer(request: PromptRequest, resolution: EditConflictResolution) {
    request
        .reply
        .send(PromptResponse::EditConflict { resolution })
        .unwrap();
}

fn local(session: &EditSession) -> PathBuf {
    PathBuf::from(&session.local_path)
}

/// Like vim/VS Code atomic saves: write a temporary file, rename it over.
fn atomic_save(path: &Path, data: &[u8]) {
    let tmp = path.with_file_name(".save.tmp");
    fs::write(&tmp, data).unwrap();
    fs::rename(&tmp, path).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_are_uploaded() {
    let mut h = Harness::new();
    let remote = h.remote("config.yml", b"a: 1\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&remote, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let s = h.open(&remote).await;
    assert_eq!(local(&s).file_name().unwrap(), "config.yml");
    assert!(local(&s).starts_with(h.dir.path().join("edit").join(s.id.to_string())));
    assert_eq!(fs::read(local(&s)).unwrap(), b"a: 1\n");
    assert!(matches!(s.state, EditState::Synced { .. }));
    assert_eq!((s.dirty, s.last_upload), (false, None));
    assert_eq!(h.edits.list(), vec![s.clone()]);

    fs::write(local(&s), b"a: 2\n").unwrap();
    let synced = h.synced_upload(s.id).await;
    assert!(!synced.dirty);
    assert_eq!(fs::read(&remote).unwrap(), b"a: 2\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&remote).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }
    // No temporary files left next to the remote file.
    assert_eq!(
        fs::read_dir(h.dir.path().join("remote")).unwrap().count(),
        1
    );

    // Editors that save through a temporary file and a rename.
    atomic_save(&local(&s), b"a: 3\n");
    h.synced_upload(s.id).await;
    assert_eq!(fs::read(&remote).unwrap(), b"a: 3\n");

    // Opening the same file again returns the running session.
    assert_eq!(h.open(&remote).await.id, s.id);

    assert_eq!(h.edits.close(s.id, false).await.unwrap(), None);
    h.wait(s.id, |s| s.state == EditState::Closed).await;
    assert!(!local(&s).exists());
    assert!(
        !h.dir
            .path()
            .join("edit")
            .join(format!("{}.json", s.id))
            .exists()
    );
    assert!(h.edits.list().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_without_changes_upload_nothing() {
    let h = Harness::new();
    let remote = h.remote("same.txt", b"same");
    let s = h.open(&remote).await;
    let before = fs::metadata(&remote).unwrap().modified().unwrap();
    fs::write(local(&s), b"same").unwrap();
    fs::write(local(&s).with_file_name(".same.txt.swp"), b"swap").unwrap();
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(fs::metadata(&remote).unwrap().modified().unwrap(), before);
    assert_eq!(h.edits.get(s.id).unwrap().last_upload, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_changes_raise_a_conflict() {
    let mut h = Harness::new();
    let remote = h.remote("notes.md", b"v1");
    let s = h.open(&remote).await;

    // Overwrite.
    fs::write(&remote, b"theirs-1").unwrap();
    fs::write(local(&s), b"mine-1").unwrap();
    let prompt = h.prompt().await;
    match &prompt.prompt {
        Prompt::EditConflict {
            session_id,
            remote_path,
            remote: theirs,
            local: mine,
        } => {
            assert_eq!(*session_id, s.id);
            assert_eq!(remote_path, &remote);
            assert_eq!((theirs.size, mine.size), (8, 6));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(h.edits.get(s.id).unwrap().state, EditState::Conflict);
    assert_eq!(fs::read(&remote).unwrap(), b"theirs-1");
    answer(prompt, EditConflictResolution::Overwrite);
    h.synced_upload(s.id).await;
    assert_eq!(fs::read(&remote).unwrap(), b"mine-1");

    // Dismissed: stays in conflict; retry asks again. Discard local.
    fs::write(&remote, b"theirs-two").unwrap();
    fs::write(local(&s), b"mine-2").unwrap();
    drop(h.prompt().await);
    h.wait(s.id, |s| s.state == EditState::Conflict).await;
    h.edits.retry(s.id).unwrap();
    answer(h.prompt().await, EditConflictResolution::DiscardLocal);
    let synced = h
        .wait(s.id, |s| matches!(s.state, EditState::Synced { .. }))
        .await;
    assert!(!synced.dirty);
    assert_eq!(fs::read(local(&s)).unwrap(), b"theirs-two");
    assert_eq!(fs::read(&remote).unwrap(), b"theirs-two");

    // The re-downloaded version is the new baseline: saving uploads.
    fs::write(local(&s), b"mine-3").unwrap();
    h.synced_upload(s.id).await;
    assert_eq!(fs::read(&remote).unwrap(), b"mine-3");

    // Keep local: stop syncing, keep the copy; retry resumes.
    fs::write(&remote, b"theirs-three").unwrap();
    fs::write(local(&s), b"mine-4").unwrap();
    answer(h.prompt().await, EditConflictResolution::KeepLocal);
    let stopped = h.wait(s.id, |s| s.state == EditState::Stopped).await;
    assert!(stopped.dirty);
    fs::write(local(&s), b"mine-5").unwrap();
    tokio::time::sleep(DEBOUNCE * 3).await;
    assert_eq!(fs::read(&remote).unwrap(), b"theirs-three");
    assert_eq!(h.edits.get(s.id).unwrap().state, EditState::Stopped);
    h.edits.retry(s.id).unwrap();
    answer(h.prompt().await, EditConflictResolution::Overwrite);
    h.synced_upload(s.id).await;
    assert_eq!(fs::read(&remote).unwrap(), b"mine-5");
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_with_unsynced_changes_keeps_the_copy() {
    let h = Harness::new();
    let remote = h.remote("draft.txt", b"v1");
    let s = h.open(&remote).await;
    // Saved, then closed within the debounce window: not uploaded yet.
    fs::write(local(&s), b"v2").unwrap();
    let kept = h.edits.close(s.id, false).await.unwrap();
    assert_eq!(kept.as_deref(), Some(s.local_path.as_str()));
    let info = h.edits.get(s.id).unwrap();
    assert_eq!((info.state, info.dirty), (EditState::Stopped, true));
    assert_eq!(fs::read(local(&s)).unwrap(), b"v2");
    assert_eq!(fs::read(&remote).unwrap(), b"v1");

    assert_eq!(h.edits.close(s.id, true).await.unwrap(), None);
    assert!(!local(&s).exists());
    assert!(h.edits.get(s.id).is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn recovers_unsynced_sessions_of_a_previous_run() {
    let mut h = Harness::new();
    let clean = h.open(&h.remote("clean.txt", b"c")).await;
    let remote = h.remote("dirty.txt", b"d1");
    let dirty = h.open(&remote).await;
    let kept = h.open(&h.remote("kept.txt", b"k1")).await;
    fs::write(local(&kept), b"k2").unwrap();
    h.edits.close(kept.id, false).await.unwrap();

    // The app goes away before the save is synced.
    let edit_root = h.dir.path().join("edit");
    fs::create_dir_all(edit_root.join("stray")).unwrap();
    let next_run = h.restart();
    drop(std::mem::replace(&mut h.edits, next_run));
    fs::write(local(&dirty), b"d2").unwrap();

    let recovered = h.edits.recover().await;
    let mut ids: Vec<Uuid> = recovered.iter().map(|s| s.id).collect();
    let mut expected = vec![dirty.id, kept.id];
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected);
    for s in &recovered {
        assert!(s.recovered && s.dirty, "{s:?}");
        assert!(matches!(&s.state, EditState::Failed { message } if message == RECOVERED_MESSAGE));
    }
    assert!(!local(&clean).exists());
    assert!(!edit_root.join(format!("{}.json", clean.id)).exists());
    assert!(!edit_root.join("stray").exists());
    assert_eq!(fs::read(&remote).unwrap(), b"d1");

    // Retrying uploads the recovered changes.
    h.edits.retry(dirty.id).unwrap();
    let synced = h.synced_upload(dirty.id).await;
    assert!(!synced.recovered);
    assert_eq!(fs::read(&remote).unwrap(), b"d2");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_regular_files_up_to_the_limit() {
    let h = Harness::new();
    let dir = h.dir.path().join("remote/folder");
    fs::create_dir_all(&dir).unwrap();
    assert!(matches!(
        h.edits.open(None, &dir.to_string_lossy()).await,
        Err(Error::Unsupported(_))
    ));
    let big = h.remote("big.bin", b"");
    fs::File::options()
        .write(true)
        .open(&big)
        .unwrap()
        .set_len(MAX_EDIT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        h.edits.open(None, &big).await,
        Err(Error::Unsupported(m)) if m.contains("too large")
    ));
    assert!(matches!(
        h.edits
            .open(None, &h.dir.path().join("missing").to_string_lossy())
            .await,
        Err(Error::NotFound(_))
    ));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn symlinks_are_edited_through() {
    let mut h = Harness::new();
    let target = h.remote("real.conf", b"x=1");
    let link = h.dir.path().join("remote/link.conf");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let s = h.open(&link.to_string_lossy()).await;
    assert_eq!(local(&s).file_name().unwrap(), "link.conf");
    fs::write(local(&s), b"x=2").unwrap();
    h.synced_upload(s.id).await;
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(&target).unwrap(), b"x=2");
}

#[test]
fn file_system_events_that_matter() {
    use notify::event::{CreateKind, RenameMode};
    let event = |kind, path: &str| notify::Event::new(kind).add_path(PathBuf::from(path));
    let relevant = |e: &notify::Event| is_relevant(e, "a.txt");
    assert!(relevant(&event(
        EventKind::Access(AccessKind::Close(AccessMode::Write)),
        "/d/a.txt"
    )));
    assert!(relevant(&event(
        EventKind::Modify(ModifyKind::Name(RenameMode::To)),
        "/d/a.txt"
    )));
    assert!(relevant(&event(
        EventKind::Create(CreateKind::File),
        "/d/a.txt"
    )));
    assert!(!relevant(&event(
        EventKind::Access(AccessKind::Open(AccessMode::Read)),
        "/d/a.txt"
    )));
    assert!(!relevant(&event(
        EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any)),
        "/d/.a.txt.swp"
    )));
    assert!(relevant(&notify::Event::new(EventKind::Any)));
}

#[test]
fn local_names_are_valid() {
    assert_eq!(local_file_name("app.yml"), "app.yml");
    assert_eq!(local_file_name(""), "file");
    assert_eq!(local_file_name("a\nb"), "a_b");
    if cfg!(windows) {
        assert_eq!(local_file_name("what?.txt"), "what_.txt");
        assert_eq!(local_file_name("trailing. "), "trailing");
    }
}
