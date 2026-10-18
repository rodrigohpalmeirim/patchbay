//! Integration tests for `edit` against the OpenSSH containers in
//! `docker/test`:
//!
//! ```sh
//! docker compose -f docker/test/compose.yml up -d --build --wait
//! PATCHBAY_DOCKER_TESTS=1 cargo test -p patchbay-core --test edit
//! ```
//!
//! Without `PATCHBAY_DOCKER_TESTS=1` every test is skipped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use patchbay_core::edit::EditManager;
use patchbay_core::fs::FileSystem;
use patchbay_core::model::{
    AgentSource, ConnectTarget, CoreEvent, EditConflictResolution, EditSession, EditState, Prompt,
    PromptRequest, PromptResponse,
};
use patchbay_core::session::{Connection, SessionManager};
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};

const TARGET_PORT: u16 = 2201;
const TARGET: &str = "test:edit-target";
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

macro_rules! require_docker {
    () => {
        if std::env::var("PATCHBAY_DOCKER_TESTS").as_deref() != Ok("1") {
            eprintln!("skipped: set PATCHBAY_DOCKER_TESTS=1 and start docker/test/compose.yml");
            return;
        }
    };
}

fn test_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/test")
}

/// Managers wired together; edit conflicts are forwarded to `conflicts`,
/// host key prompts are accepted.
struct Harness {
    sessions: SessionManager,
    fs: FileSystem,
    edits: EditManager,
    events: broadcast::Receiver<CoreEvent>,
    conflicts: mpsc::Receiver<PromptRequest>,
    local: TempDir,
}

impl Harness {
    async fn new() -> Self {
        let (events_tx, events) = broadcast::channel(4096);
        let (prompt_tx, mut prompt_rx) = mpsc::channel::<PromptRequest>(16);
        let (conflict_tx, conflicts) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(req) = prompt_rx.recv().await {
                match req.prompt {
                    Prompt::EditConflict { .. } => {
                        let _ = conflict_tx.send(req).await;
                    }
                    Prompt::HostKeyUnknown { .. } => {
                        let _ = req.reply.send(PromptResponse::Accept);
                    }
                    _ => {
                        let _ = req.reply.send(PromptResponse::Cancel);
                    }
                }
            }
        });
        let sessions = SessionManager::new(events_tx.clone(), prompt_tx.clone());
        let fs = FileSystem::new(sessions.clone());
        let local = TempDir::new().unwrap();
        Self {
            edits: EditManager::new(fs.clone(), events_tx, prompt_tx, local.path().join("edit")),
            sessions,
            fs,
            events,
            conflicts,
            local,
        }
    }

    async fn connect(&self) -> Arc<Connection> {
        let known_hosts = self.local.path().join("known_hosts");
        if !known_hosts.exists() {
            std::fs::copy(test_dir().join("known_hosts"), &known_hosts).unwrap();
        }
        let target = ConnectTarget {
            host_id: Some(TARGET.to_owned()),
            label: TARGET.to_owned(),
            hostname: "127.0.0.1".to_owned(),
            port: TARGET_PORT,
            user: "patchbay".to_owned(),
            identity_files: vec![test_dir().join("keys/id_ed25519")],
            identities_only: true,
            agent: AgentSource::Disabled,
            host_key_alias: None,
            known_hosts_files: vec![known_hosts],
            trusted_host_keys: Vec::new(),
            connect_timeout: Some(Duration::from_secs(10)),
            server_alive_interval: None,
            jumps: Vec::new(),
            auth_methods: Default::default(),
        };
        timed(self.sessions.connect(target)).await.unwrap()
    }

    async fn wait(&mut self, id: uuid::Uuid, pred: impl Fn(&EditSession) -> bool) -> EditSession {
        timed(async {
            loop {
                match self.events.recv().await {
                    Ok(CoreEvent::Edit(s)) if s.id == id && pred(&s) => return s,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("events: {e}"),
                }
            }
        })
        .await
    }

    async fn uploaded(&mut self, id: uuid::Uuid) -> EditSession {
        self.wait(id, |s| {
            s.last_upload.is_some() && matches!(s.state, EditState::Synced { .. })
        })
        .await
    }

    async fn conflict(&mut self) -> PromptRequest {
        timed(self.conflicts.recv()).await.unwrap()
    }

    async fn read(&self, path: &str) -> Vec<u8> {
        self.fs.read(Some(TARGET), path, 1 << 20).await.unwrap()
    }

    /// A fresh directory on the target, removed by the caller.
    async fn scratch_dir(&self, name: &str) -> String {
        let dir = format!("/home/patchbay/upload/{name}-{}", uuid::Uuid::new_v4());
        self.fs.mkdir(Some(TARGET), &dir).await.unwrap();
        dir
    }
}

async fn timed<T>(fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("test timed out")
}

fn resolve(request: PromptRequest, resolution: EditConflictResolution) {
    request
        .reply
        .send(PromptResponse::EditConflict { resolution })
        .unwrap();
}

/// Change a remote file behind Patchbay's back.
async fn remote_write(conn: &Connection, path: &str, text: &str) {
    let out = conn
        .exec(&format!("printf '%s' '{text}' > '{path}'"))
        .await
        .unwrap();
    assert!(out.success(), "{out:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_are_uploaded_atomically_with_permissions() {
    require_docker!();
    let mut h = Harness::new().await;
    let conn = h.connect().await;
    let dir = h.scratch_dir("edit").await;
    let path = format!("{dir}/app.yml");
    remote_write(&conn, &path, "port: 1\n").await;
    h.fs.chmod(Some(TARGET), std::slice::from_ref(&path), 0o640, false)
        .await
        .unwrap();

    let s = h.edits.open(Some(TARGET), &path).await.unwrap();
    let local = PathBuf::from(&s.local_path);
    assert_eq!(local.file_name().unwrap(), "app.yml");
    assert_eq!(std::fs::read(&local).unwrap(), b"port: 1\n");

    std::fs::write(&local, "port: 2\n").unwrap();
    h.uploaded(s.id).await;
    assert_eq!(h.read(&path).await, b"port: 2\n");
    let entry = h.fs.stat(Some(TARGET), &path).await.unwrap();
    assert_eq!(entry.permissions, Some(0o640));

    // Editors that write a temporary file and rename it over the original.
    let tmp = local.with_file_name("app.yml.swp~");
    std::fs::write(&tmp, "port: 3\n").unwrap();
    std::fs::rename(&tmp, &local).unwrap();
    h.uploaded(s.id).await;
    assert_eq!(h.read(&path).await, b"port: 3\n");

    let names: Vec<String> =
        h.fs.list(Some(TARGET), &dir)
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
    assert_eq!(names, ["app.yml"], "no temporary files left behind");

    assert_eq!(h.edits.close(s.id, false).await.unwrap(), None);
    assert!(!local.exists());
    h.fs.delete(Some(TARGET), &[dir]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_changes_raise_a_conflict() {
    require_docker!();
    let mut h = Harness::new().await;
    let conn = h.connect().await;
    let dir = h.scratch_dir("edit-conflict").await;
    let path = format!("{dir}/notes.txt");
    remote_write(&conn, &path, "v1").await;
    let s = h.edits.open(Some(TARGET), &path).await.unwrap();
    let local = PathBuf::from(&s.local_path);

    // Overwrite.
    remote_write(&conn, &path, "theirs").await;
    std::fs::write(&local, "mine-1").unwrap();
    let prompt = h.conflict().await;
    assert_eq!(prompt.host_id.as_deref(), Some(TARGET));
    match &prompt.prompt {
        Prompt::EditConflict {
            session_id,
            remote_path,
            remote,
            local,
        } => {
            assert_eq!((*session_id, remote_path.as_str()), (s.id, path.as_str()));
            assert_eq!((remote.size, local.size), (6, 6));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(h.edits.get(s.id).unwrap().state, EditState::Conflict);
    assert_eq!(h.read(&path).await, b"theirs");
    resolve(prompt, EditConflictResolution::Overwrite);
    h.uploaded(s.id).await;
    assert_eq!(h.read(&path).await, b"mine-1");

    // Discard local changes: download the server's version.
    remote_write(&conn, &path, "theirs-2").await;
    std::fs::write(&local, "mine-2").unwrap();
    resolve(h.conflict().await, EditConflictResolution::DiscardLocal);
    h.wait(s.id, |s| {
        matches!(s.state, EditState::Synced { .. }) && !s.dirty
    })
    .await;
    assert_eq!(std::fs::read(&local).unwrap(), b"theirs-2");
    assert_eq!(h.read(&path).await, b"theirs-2");

    // Keep local only: stop syncing, keep the copy.
    remote_write(&conn, &path, "theirs-three").await;
    std::fs::write(&local, "mine-3").unwrap();
    resolve(h.conflict().await, EditConflictResolution::KeepLocal);
    let stopped = h.wait(s.id, |s| s.state == EditState::Stopped).await;
    assert!(stopped.dirty);
    assert_eq!(h.read(&path).await, b"theirs-three");
    let kept = h.edits.close(s.id, false).await.unwrap();
    assert_eq!(kept.as_deref(), Some(s.local_path.as_str()));
    assert_eq!(std::fs::read(&local).unwrap(), b"mine-3");
    assert_eq!(h.edits.close(s.id, true).await.unwrap(), None);
    assert!(!local.exists());

    h.fs.delete(Some(TARGET), &[dir]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_saves_are_retried_after_reconnecting() {
    require_docker!();
    let mut h = Harness::new().await;
    let conn = h.connect().await;
    let dir = h.scratch_dir("edit-offline").await;
    let path = format!("{dir}/offline.txt");
    remote_write(&conn, &path, "v1").await;
    drop(conn);
    let s = h.edits.open(Some(TARGET), &path).await.unwrap();
    let local = PathBuf::from(&s.local_path);

    h.sessions.disconnect(TARGET).await;
    std::fs::write(&local, "saved while offline").unwrap();
    let failed = h
        .wait(s.id, |s| matches!(s.state, EditState::Failed { .. }))
        .await;
    assert!(failed.dirty);

    h.connect().await;
    h.uploaded(s.id).await;
    assert_eq!(h.read(&path).await, b"saved while offline");

    // A manual retry while disconnected fails again and does not lose the change.
    h.sessions.disconnect(TARGET).await;
    std::fs::write(&local, "second offline save").unwrap();
    h.wait(s.id, |s| matches!(s.state, EditState::Failed { .. }))
        .await;
    h.edits.retry(s.id).unwrap();
    h.wait(s.id, |s| matches!(s.state, EditState::Failed { .. }))
        .await;
    h.connect().await;
    h.uploaded(s.id).await;
    assert_eq!(h.read(&path).await, b"second offline save");

    h.edits.close(s.id, false).await.unwrap();
    h.fs.delete(Some(TARGET), &[dir]).await.unwrap();
}
