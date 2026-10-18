//! Integration tests for `fs`, `transfer` and `search` against the OpenSSH
//! containers in `docker/test`:
//!
//! ```sh
//! docker compose -f docker/test/compose.yml up -d --build --wait
//! PATCHBAY_DOCKER_TESTS=1 cargo test -p patchbay-core --test files
//! ```
//!
//! Without `PATCHBAY_DOCKER_TESTS=1` every test is skipped. `bench_throughput`
//! is `#[ignore]`d; run it with `-- --ignored --nocapture`, optionally
//! against a container with simulated latency (`PATCHBAY_BENCH_PORT`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use patchbay_core::Error;
use patchbay_core::fs::FileSystem;
use patchbay_core::model::{
    AgentSource, ConflictResolution, ConnectTarget, CoreEvent, FileEntry, FileKind, FileRef,
    Prompt, PromptRequest, PromptResponse, TransferInfo, TransferRequest, TransferState,
};
use patchbay_core::search::SearchManager;
use patchbay_core::session::{Connection, SessionManager};
use patchbay_core::transfer::TransferManager;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};

const TARGET_PORT: u16 = 2201;
const SFTPONLY_PORT: u16 = 2203;
const TARGET: &str = "test:files-target";
const SFTPONLY: &str = "test:files-sftponly";
const BIG_SHA256: &str = "4ef0e6ddb3d6dd51ea71bab90f6b2e86fafb1dd4477fdd442a3c095dd1a8516f";
const TEST_TIMEOUT: Duration = Duration::from_secs(120);

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

fn connect_target(id: &str, port: u16, known_hosts: PathBuf) -> ConnectTarget {
    ConnectTarget {
        host_id: Some(id.to_owned()),
        label: id.to_owned(),
        hostname: "127.0.0.1".to_owned(),
        port,
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
    }
}

/// Managers wired together; conflict prompts are forwarded to `conflicts`,
/// connection prompts are accepted.
struct Harness {
    sessions: SessionManager,
    fs: FileSystem,
    transfers: TransferManager,
    search: SearchManager,
    events: broadcast::Receiver<CoreEvent>,
    conflicts: mpsc::Receiver<PromptRequest>,
    local: TempDir,
}

impl Harness {
    async fn new() -> Self {
        let (events_tx, events) = broadcast::channel(8192);
        let (prompt_tx, mut prompt_rx) = mpsc::channel::<PromptRequest>(16);
        let (conflict_tx, conflicts) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(req) = prompt_rx.recv().await {
                match req.prompt {
                    Prompt::FileConflict { .. } => {
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
        Self {
            transfers: TransferManager::new(fs.clone(), events_tx.clone(), prompt_tx),
            search: SearchManager::new(fs.clone(), events_tx),
            sessions,
            fs,
            events,
            conflicts,
            local: TempDir::new().unwrap(),
        }
    }

    async fn connect(&self, id: &str, port: u16) -> Arc<Connection> {
        // A copy, so accepting the key of an unlisted port (benchmarks)
        // never touches the fixture.
        let known_hosts = self.local.path().join(".known_hosts");
        if !known_hosts.exists() {
            std::fs::copy(test_dir().join("known_hosts"), &known_hosts).unwrap();
        }
        timed(self.sessions.connect(connect_target(id, port, known_hosts)))
            .await
            .unwrap()
    }

    fn local(&self, rel: &str) -> String {
        self.local.path().join(rel).to_string_lossy().into_owned()
    }

    async fn transfer(&self, sources: Vec<FileRef>, destination: FileRef) -> TransferInfo {
        self.transfers
            .start(TransferRequest {
                sources,
                destination,
                ..TransferRequest::default()
            })
            .unwrap()
    }

    async fn wait(&mut self, id: uuid::Uuid, pred: impl Fn(&TransferInfo) -> bool) -> TransferInfo {
        timed(async {
            loop {
                match self.events.recv().await {
                    Ok(CoreEvent::Transfer(info)) if info.id == id && pred(&info) => return info,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("events: {e}"),
                }
            }
        })
        .await
    }

    async fn finished(&mut self, id: uuid::Uuid) -> TransferInfo {
        self.wait(id, |i| {
            matches!(
                i.state,
                TransferState::Done | TransferState::Failed { .. } | TransferState::Cancelled
            )
        })
        .await
    }

    /// Run a transfer to the end and return its final info.
    async fn copy(&mut self, sources: Vec<FileRef>, destination: FileRef) -> TransferInfo {
        let id = self.transfer(sources, destination).await.id;
        self.finished(id).await
    }

    /// Collect all results of a search.
    async fn search(&mut self, host: Option<&str>, root: &str, query: &str) -> Vec<FileEntry> {
        let id = self
            .search
            .start(host.map(str::to_owned), root, query)
            .await
            .unwrap();
        let mut found = Vec::new();
        timed(async {
            loop {
                match self.events.recv().await {
                    Ok(CoreEvent::Search {
                        search_id,
                        entries,
                        done,
                        error,
                    }) if search_id == id => {
                        found.extend(entries);
                        assert_eq!(error, None);
                        if done {
                            return;
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("events: {e}"),
                }
            }
        })
        .await;
        found
    }
}

fn remote(host: &str, path: &str) -> FileRef {
    FileRef {
        host_id: Some(host.to_owned()),
        path: path.to_owned(),
    }
}

fn local(path: &str) -> FileRef {
    FileRef {
        host_id: None,
        path: path.to_owned(),
    }
}

async fn timed<T>(fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("test timed out")
}

fn sha256_file(path: &str) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(std::fs::read(path).unwrap()))
}

async fn remote_sha256(conn: &Connection, path: &str) -> String {
    let out = conn.exec(&format!("sha256sum '{path}'")).await.unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// A fresh directory below `/home/patchbay/upload` on the target, removed again
/// by `cleanup`.
async fn scratch_dir(h: &Harness, name: &str) -> String {
    let dir = format!("/home/patchbay/upload/{name}-{}", uuid::Uuid::new_v4());
    h.fs.mkdir(Some(TARGET), &dir).await.unwrap();
    dir
}

fn by_name<'a>(entries: &'a [FileEntry], name: &str) -> &'a FileEntry {
    entries.iter().find(|e| e.name == name).unwrap_or_else(|| {
        panic!(
            "{name} not in {:?}",
            entries.iter().map(|e| &e.name).collect::<Vec<_>>()
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn list_home_with_symlinks_and_unicode() {
    require_docker!();
    let h = Harness::new().await;
    assert!(matches!(
        h.fs.list(Some(TARGET), "/").await,
        Err(Error::NotConnected(_))
    ));
    h.connect(TARGET, TARGET_PORT).await;

    assert_eq!(h.fs.home(Some(TARGET)).await.unwrap(), "/home/patchbay");
    let listing = h.fs.list(Some(TARGET), "/home/patchbay/").await.unwrap();
    assert_eq!(listing.path, "/home/patchbay");
    assert_eq!(listing.parent.as_deref(), Some("/home"));
    let e = &listing.entries;

    let docs_link = by_name(e, "link-to-docs");
    assert_eq!(docs_link.kind, FileKind::Symlink);
    assert_eq!(docs_link.link_target.as_deref(), Some("docs"));
    assert_eq!(docs_link.target_kind, Some(FileKind::Dir));
    let dangling = by_name(e, "dangling-link");
    assert_eq!(dangling.link_target.as_deref(), Some("does-not-exist"));
    assert_eq!(dangling.target_kind, None);

    let unicode = by_name(e, "ünïcödé — 日本語 ✓.txt");
    assert_eq!(unicode.kind, FileKind::File);
    assert_eq!(unicode.size, 8);
    assert_eq!(unicode.path, "/home/patchbay/ünïcödé — 日本語 ✓.txt");
    assert!(by_name(e, ".hidden-file").hidden);
    assert!(!by_name(e, "docs").hidden);
    assert_eq!(by_name(e, "docs").kind, FileKind::Dir);
    assert_eq!(by_name(e, "readonly.txt").permissions, Some(0o444));
    assert_eq!(by_name(e, "script.sh").permissions, Some(0o755));
    assert_eq!(by_name(e, "big.bin").size, 20 * 1024 * 1024);
    assert!(by_name(e, "big.bin").modified.is_some());
    assert_eq!(by_name(e, "big.bin").owner.as_deref(), Some("patchbay"));
    assert_eq!(by_name(e, "big.bin").group.as_deref(), Some("patchbay"));

    // stat has no longname, but names learned from the listing are reused.
    let stat =
        h.fs.stat(Some(TARGET), "/home/patchbay/readonly.txt")
            .await
            .unwrap();
    assert_eq!(stat.name, "readonly.txt");
    assert_eq!(stat.owner.as_deref(), Some("patchbay"));
    let link =
        h.fs.stat(Some(TARGET), "/home/patchbay/docs/link-to-readme.txt")
            .await
            .unwrap();
    assert_eq!(link.kind, FileKind::Symlink);
    assert_eq!(link.target_kind, Some(FileKind::File));
    assert!(matches!(
        h.fs.stat(Some(TARGET), "/home/patchbay/nope").await,
        Err(Error::NotFound(_))
    ));

    let head =
        h.fs.read(Some(TARGET), "/home/patchbay/docs/readme.txt", 1 << 20)
            .await
            .unwrap();
    assert_eq!(head, b"Hello from the Patchbay test target.\n");
    let head =
        h.fs.read(Some(TARGET), "/home/patchbay/big.bin", 1000)
            .await
            .unwrap();
    assert_eq!(head.len(), 1000);

    // Through a directory symlink, listed path is canonical.
    let via_link =
        h.fs.list(Some(TARGET), "/home/patchbay/link-to-docs")
            .await
            .unwrap();
    assert_eq!(via_link.path, "/home/patchbay/docs");
    assert_eq!(h.fs.list(Some(TARGET), "/").await.unwrap().parent, None);

    // SFTP-only chroot.
    h.connect(SFTPONLY, SFTPONLY_PORT).await;
    assert_eq!(h.fs.home(Some(SFTPONLY)).await.unwrap(), "/");
    let root = h.fs.list(Some(SFTPONLY), "/").await.unwrap();
    assert_eq!(root.parent, None);
    assert_eq!(by_name(&root.entries, "upload").kind, FileKind::Dir);
    let pub_dir = h.fs.list(Some(SFTPONLY), "/pub").await.unwrap();
    // No passwd file inside the chroot: the server reports numeric owners.
    assert_eq!(
        by_name(&pub_dir.entries, "readme.txt").owner.as_deref(),
        Some("0")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_file_operations() {
    require_docker!();
    let h = Harness::new().await;
    let conn = h.connect(TARGET, TARGET_PORT).await;
    let t = Some(TARGET);
    let dir = scratch_dir(&h, "ops").await;
    let sub = format!("{dir}/sub");

    h.fs.mkdir(t, &sub).await.unwrap();
    assert!(matches!(
        h.fs.mkdir(t, &sub).await,
        Err(Error::AlreadyExists(_))
    ));
    let file = format!("{sub}/new file.txt");
    h.fs.create_file(t, &file).await.unwrap();
    assert!(matches!(
        h.fs.create_file(t, &file).await,
        Err(Error::AlreadyExists(_))
    ));
    assert_eq!(h.fs.stat(t, &file).await.unwrap().size, 0);

    let renamed = format!("{sub}/renamed.txt");
    h.fs.rename(t, &file, &renamed).await.unwrap();
    h.fs.create_file(t, &file).await.unwrap();
    assert!(matches!(
        h.fs.rename(t, &file, &renamed).await,
        Err(Error::AlreadyExists(_))
    ));

    h.fs.chmod(t, std::slice::from_ref(&renamed), 0o600, false)
        .await
        .unwrap();
    assert_eq!(
        h.fs.stat(t, &renamed).await.unwrap().permissions,
        Some(0o600)
    );
    h.fs.chmod(t, std::slice::from_ref(&dir), 0o750, true)
        .await
        .unwrap();
    assert_eq!(h.fs.stat(t, &sub).await.unwrap().permissions, Some(0o750));
    assert_eq!(h.fs.stat(t, &file).await.unwrap().permissions, Some(0o750));

    // chown to ourselves by id and by name (resolved remotely with `id -u`).
    h.fs.chown(t, std::slice::from_ref(&renamed), Some("1000"), None, false)
        .await
        .unwrap();
    h.fs.chown(
        t,
        std::slice::from_ref(&dir),
        Some("patchbay"),
        Some("patchbay"),
        true,
    )
    .await
    .unwrap();
    assert!(matches!(
        h.fs.chown(
            t,
            std::slice::from_ref(&dir),
            Some("no-such-user-x"),
            None,
            false
        )
        .await,
        Err(Error::NotFound(_))
    ));
    // chown to root is refused by the server.
    assert!(
        h.fs.chown(t, std::slice::from_ref(&renamed), Some("0"), None, false)
            .await
            .is_err()
    );

    // Delete never follows symlinks.
    let out = conn
        .exec(&format!("ln -s /home/patchbay/docs '{sub}/docs-link'"))
        .await
        .unwrap();
    assert!(out.success());
    h.fs.delete(t, std::slice::from_ref(&dir)).await.unwrap();
    assert!(matches!(h.fs.stat(t, &dir).await, Err(Error::NotFound(_))));
    assert!(h.fs.stat(t, "/home/patchbay/docs/readme.txt").await.is_ok());

    // SFTP-only: owner names cannot be resolved without a shell.
    h.connect(SFTPONLY, SFTPONLY_PORT).await;
    let s = Some(SFTPONLY);
    let upload = format!("/upload/ops-{}", uuid::Uuid::new_v4());
    h.fs.create_file(s, &upload).await.unwrap();
    assert!(matches!(
        h.fs.chown(
            s,
            std::slice::from_ref(&upload),
            Some("somebody"),
            None,
            false
        )
        .await,
        Err(Error::Unsupported(_))
    ));
    h.fs.delete(s, &[upload]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn big_file_round_trip() {
    require_docker!();
    let mut h = Harness::new().await;
    let conn = h.connect(TARGET, TARGET_PORT).await;
    let dir = scratch_dir(&h, "roundtrip").await;

    let started = Instant::now();
    let info = h
        .copy(
            vec![remote(TARGET, "/home/patchbay/big.bin")],
            local(&h.local("")),
        )
        .await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    eprintln!("download 20 MiB: {:?}", started.elapsed());
    assert_eq!(info.done_bytes, 20 * 1024 * 1024);
    assert_eq!(sha256_file(&h.local("big.bin")), BIG_SHA256);

    let started = Instant::now();
    let info = h
        .copy(vec![local(&h.local("big.bin"))], remote(TARGET, &dir))
        .await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    eprintln!("upload 20 MiB: {:?}", started.elapsed());
    assert_eq!(
        remote_sha256(&conn, &format!("{dir}/big.bin")).await,
        BIG_SHA256
    );
    let up =
        h.fs.stat(Some(TARGET), &format!("{dir}/big.bin"))
            .await
            .unwrap();
    let orig =
        h.fs.stat(Some(TARGET), "/home/patchbay/big.bin")
            .await
            .unwrap();
    assert_eq!(up.modified, orig.modified, "mtime preserved");

    // A tree with a symlink and an empty directory goes up and comes back.
    let tree = h.local("tree");
    std::fs::create_dir_all(format!("{tree}/a/b")).unwrap();
    std::fs::create_dir_all(format!("{tree}/empty")).unwrap();
    std::fs::write(format!("{tree}/a/b/c.txt"), "c").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a/b", format!("{tree}/link")).unwrap();
    let info = h.copy(vec![local(&tree)], remote(TARGET, &dir)).await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    let up =
        h.fs.list(Some(TARGET), &format!("{dir}/tree"))
            .await
            .unwrap();
    assert_eq!(by_name(&up.entries, "empty").kind, FileKind::Dir);
    #[cfg(unix)]
    {
        let link = by_name(&up.entries, "link");
        assert_eq!(link.link_target.as_deref(), Some("a/b"));
        assert_eq!(link.target_kind, Some(FileKind::Dir));
    }
    let back = h.local("back");
    std::fs::create_dir(&back).unwrap();
    let info = h
        .copy(vec![remote(TARGET, &format!("{dir}/tree"))], local(&back))
        .await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    assert_eq!(
        std::fs::read_to_string(format!("{back}/tree/a/b/c.txt")).unwrap(),
        "c"
    );

    h.fs.delete(Some(TARGET), &[dir]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_to_remote() {
    require_docker!();
    let mut h = Harness::new().await;
    let conn = h.connect(TARGET, TARGET_PORT).await;
    h.connect(SFTPONLY, SFTPONLY_PORT).await;
    let dest = format!("/upload/r2r-{}", uuid::Uuid::new_v4());
    h.fs.mkdir(Some(SFTPONLY), &dest).await.unwrap();

    let info = h
        .copy(
            vec![
                remote(TARGET, "/home/patchbay/big.bin"),
                remote(TARGET, "/home/patchbay/docs"),
            ],
            remote(SFTPONLY, &dest),
        )
        .await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    assert_eq!(info.source_host.as_deref(), Some(TARGET));
    let copied = h.fs.list(Some(SFTPONLY), &dest).await.unwrap();
    assert_eq!(by_name(&copied.entries, "big.bin").size, 20 * 1024 * 1024);
    let docs =
        h.fs.list(Some(SFTPONLY), &format!("{dest}/docs"))
            .await
            .unwrap();
    assert_eq!(
        by_name(&docs.entries, "link-to-readme.txt")
            .link_target
            .as_deref(),
        Some("readme.txt")
    );

    // Back to the target (same pair, other direction) and check the bytes.
    let back = scratch_dir(&h, "r2r").await;
    let info = h
        .copy(
            vec![remote(SFTPONLY, &format!("{dest}/big.bin"))],
            remote(TARGET, &back),
        )
        .await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    assert_eq!(
        remote_sha256(&conn, &format!("{back}/big.bin")).await,
        BIG_SHA256
    );

    // Same host on both ends.
    let info = h
        .copy(
            vec![remote(TARGET, "/home/patchbay/docs")],
            remote(TARGET, &back),
        )
        .await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    assert_eq!(
        h.fs.read(
            Some(TARGET),
            &format!("{back}/docs/nested/deeper/deep.txt"),
            100
        )
        .await
        .unwrap(),
        b"deep file\n"
    );

    h.fs.delete(Some(SFTPONLY), &[dest]).await.unwrap();
    h.fs.delete(Some(TARGET), &[back]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_after_connection_loss() {
    require_docker!();
    let mut h = Harness::new().await;
    h.connect(TARGET, TARGET_PORT).await;
    let dir = scratch_dir(&h, "resume").await;

    // 96 MiB of pseudo-random data, so the upload takes a moment.
    let src = h.local("payload.bin");
    let mut data = vec![0u8; 96 * 1024 * 1024];
    let mut x = 0x2545_f491_4f6c_dd1du64;
    for chunk in data.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
    }
    std::fs::write(&src, &data).unwrap();
    let expected = data_encoding::HEXLOWER.encode(&Sha256::digest(&data));
    drop(data);

    let id = h.transfer(vec![local(&src)], remote(TARGET, &dir)).await.id;
    h.wait(id, |i| {
        i.state == TransferState::Running && i.done_bytes > 4 * 1024 * 1024
    })
    .await;
    h.sessions.disconnect(TARGET).await;
    let failed = h.finished(id).await;
    assert!(
        matches!(
            failed.state,
            TransferState::Failed {
                resumable: true,
                ..
            }
        ),
        "{failed:?}"
    );

    let conn = h.connect(TARGET, TARGET_PORT).await;
    let partial =
        h.fs.stat(Some(TARGET), &format!("{dir}/payload.bin.patchbaypart"))
            .await
            .unwrap();
    assert!(
        partial.size > 0 && partial.size < 96 * 1024 * 1024,
        "{partial:?}"
    );
    eprintln!(
        "interrupted at {} bytes, partial has {}",
        failed.done_bytes, partial.size
    );

    h.transfers.retry(id).unwrap();
    // The first new progress value of the retry already counts the partial
    // (a restart from scratch would report 0 first).
    let resumed = h
        .wait(id, |i| {
            i.state == TransferState::Running && i.done_bytes != failed.done_bytes
        })
        .await;
    assert!(resumed.done_bytes >= partial.size, "{resumed:?}");
    let info = h.finished(id).await;
    assert_eq!(info.state, TransferState::Done, "{info:?}");
    assert_eq!(info.done_bytes, 96 * 1024 * 1024);
    assert_eq!(
        remote_sha256(&conn, &format!("{dir}/payload.bin")).await,
        expected
    );
    assert!(
        h.fs.stat(Some(TARGET), &format!("{dir}/payload.bin.patchbaypart"))
            .await
            .is_err()
    );

    h.fs.delete(Some(TARGET), &[dir]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_prompt_flow() {
    require_docker!();
    let mut h = Harness::new().await;
    h.connect(TARGET, TARGET_PORT).await;
    let dir = scratch_dir(&h, "conflict").await;
    let src = h.local("report.pdf");
    std::fs::write(&src, "v1").unwrap();
    let info = h.copy(vec![local(&src)], remote(TARGET, &dir)).await;
    assert_eq!(info.state, TransferState::Done);

    std::fs::write(&src, "version 2").unwrap();
    let id = h.transfer(vec![local(&src)], remote(TARGET, &dir)).await.id;
    let prompt = timed(h.conflicts.recv()).await.unwrap();
    assert_eq!(prompt.host_id.as_deref(), Some(TARGET));
    match &prompt.prompt {
        Prompt::FileConflict {
            transfer_id,
            path,
            source,
            existing,
            can_resume,
        } => {
            assert_eq!(*transfer_id, id);
            assert_eq!(path, &format!("{dir}/report.pdf"));
            assert_eq!((source.size, existing.size), (9, 2));
            // The existing file is shorter: it could be continued.
            assert!(can_resume);
        }
        other => panic!("{other:?}"),
    }
    let waiting = h.transfers.get(id).unwrap();
    assert_eq!(waiting.state, TransferState::WaitingForUser);
    prompt
        .reply
        .send(PromptResponse::Conflict {
            resolution: ConflictResolution::Rename,
            apply_to_all: false,
        })
        .unwrap();
    assert_eq!(h.finished(id).await.state, TransferState::Done);
    let t = Some(TARGET);
    assert_eq!(
        h.fs.read(t, &format!("{dir}/report.pdf"), 100)
            .await
            .unwrap(),
        b"v1"
    );
    assert_eq!(
        h.fs.read(t, &format!("{dir}/report (1).pdf"), 100)
            .await
            .unwrap(),
        b"version 2"
    );

    // Overwrite replaces the file atomically (posix-rename).
    let id = h.transfer(vec![local(&src)], remote(TARGET, &dir)).await.id;
    let prompt = timed(h.conflicts.recv()).await.unwrap();
    prompt
        .reply
        .send(PromptResponse::Conflict {
            resolution: ConflictResolution::Overwrite,
            apply_to_all: false,
        })
        .unwrap();
    assert_eq!(h.finished(id).await.state, TransferState::Done);
    assert_eq!(
        h.fs.read(t, &format!("{dir}/report.pdf"), 100)
            .await
            .unwrap(),
        b"version 2"
    );
    h.fs.delete(t, &[dir]).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn search_with_find_and_walk() {
    require_docker!();
    let mut h = Harness::new().await;
    h.connect(TARGET, TARGET_PORT).await;
    h.connect(SFTPONLY, SFTPONLY_PORT).await;

    let mut paths: Vec<String> = h
        .search(Some(TARGET), "/home/patchbay/docs", "README")
        .await
        .into_iter()
        .map(|e| e.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/home/patchbay/docs/link-to-readme.txt",
            "/home/patchbay/docs/readme.txt"
        ]
    );
    // Other tests copy files below upload/; `find` does not follow link-to-docs.
    let deep: Vec<String> = h
        .search(Some(TARGET), "/home/patchbay", "deep.txt")
        .await
        .into_iter()
        .map(|e| e.path)
        .filter(|p| !p.starts_with("/home/patchbay/upload/"))
        .collect();
    assert_eq!(deep, ["/home/patchbay/docs/nested/deeper/deep.txt"]);
    let found = h.search(Some(TARGET), "/home/patchbay", "*.TXT").await;
    assert!(found.iter().any(|e| e.name == "ünïcödé — 日本語 ✓.txt"));
    assert!(
        found
            .iter()
            .any(|e| e.path == "/home/patchbay/dir with spaces/inner file.txt")
    );
    let link = found
        .iter()
        .find(|e| e.name == "link-to-readme.txt")
        .unwrap();
    assert_eq!(link.target_kind, Some(FileKind::File));
    assert!(
        h.search(Some(TARGET), "/home/patchbay", "it's [x]")
            .await
            .is_empty()
    );

    // SFTP-only: directory walk.
    let found = h.search(Some(SFTPONLY), "/pub", "readme").await;
    assert_eq!(
        found.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        ["/pub/readme.txt"]
    );
    assert!(h.search(Some(SFTPONLY), "/pub", "r?a*.md").await.is_empty());
    assert!(!h.search(Some(SFTPONLY), "/", "R?AD*.txt").await.is_empty());

    // Local walk.
    std::fs::create_dir_all(h.local("x/y")).unwrap();
    std::fs::write(h.local("x/y/Needle.md"), "").unwrap();
    let found = h.search(None, &h.local(""), "needle").await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "Needle.md");

    assert!(
        h.search
            .start(Some(TARGET.into()), "/nope", "x")
            .await
            .is_err()
    );
    assert!(matches!(
        h.search.start(Some("test:unknown".into()), "/", "x").await,
        Err(Error::NotConnected(_))
    ));

    // Cancelling sends a final event.
    let id = h.search.start(Some(TARGET.into()), "/", "e").await.unwrap();
    h.search.cancel(id);
    let done = timed(async {
        loop {
            if let Ok(CoreEvent::Search {
                search_id,
                done: true,
                ..
            }) = h.events.recv().await
                && search_id == id
            {
                return true;
            }
        }
    })
    .await;
    assert!(done);
}

/// Throughput of naive (one request in flight) vs pipelined SFTP. Set
/// `PATCHBAY_BENCH_PORT` to use another server (e.g. one behind `tc netem`).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_throughput() {
    require_docker!();
    let port = std::env::var("PATCHBAY_BENCH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(TARGET_PORT);
    let mb = std::env::var("PATCHBAY_BENCH_MIB")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(128u64);
    let mut h = Harness::new().await;
    let conn = h.connect(TARGET, port).await;
    let dir = scratch_dir(&h, "bench").await;
    let large = format!("{dir}/large.bin");
    let out = conn
        .exec_timeout(
            &format!("head -c {} /dev/urandom > '{large}'", mb << 20),
            Duration::from_secs(120),
        )
        .await
        .unwrap();
    assert!(out.success());
    let rate = |bytes: u64, t: Duration| {
        format!(
            "{:.1} MiB/s",
            bytes as f64 / (1 << 20) as f64 / t.as_secs_f64()
        )
    };

    // Naive: one 32 KiB READ at a time.
    let naive_bytes = 20u64 << 20;
    let channel = conn.open_sftp_channel().await.unwrap();
    let raw = russh_sftp::client::RawSftpSession::new(channel.into_stream());
    raw.init().await.unwrap();
    let handle = raw
        .open(
            "/home/patchbay/big.bin",
            russh_sftp::protocol::OpenFlags::READ,
            Default::default(),
        )
        .await
        .unwrap()
        .handle;
    let started = Instant::now();
    let mut offset = 0;
    while offset < naive_bytes {
        let data = raw.read(handle.as_str(), offset, 32 * 1024).await.unwrap();
        offset += data.data.len() as u64;
    }
    let naive = started.elapsed();
    eprintln!(
        "naive download   20 MiB: {naive:>10.2?}  {}",
        rate(naive_bytes, naive)
    );

    let started = Instant::now();
    let info = h
        .copy(
            vec![remote(TARGET, "/home/patchbay/big.bin")],
            local(&h.local("")),
        )
        .await;
    assert_eq!(info.state, TransferState::Done);
    let t = started.elapsed();
    eprintln!(
        "patchbay download   20 MiB: {t:>10.2?}  {}",
        rate(20 << 20, t)
    );

    let started = Instant::now();
    let info = h
        .copy(vec![local(&h.local("big.bin"))], remote(TARGET, &dir))
        .await;
    assert_eq!(info.state, TransferState::Done);
    let t = started.elapsed();
    eprintln!(
        "patchbay upload     20 MiB: {t:>10.2?}  {}",
        rate(20 << 20, t)
    );

    let started = Instant::now();
    let info = h
        .copy(vec![remote(TARGET, &large)], local(&h.local("")))
        .await;
    assert_eq!(info.state, TransferState::Done);
    let t = started.elapsed();
    eprintln!(
        "patchbay download {mb:>4} MiB: {t:>10.2?}  {}",
        rate(mb << 20, t)
    );

    let up = scratch_dir(&h, "bench-up").await;
    let started = Instant::now();
    let info = h
        .copy(vec![local(&h.local("large.bin"))], remote(TARGET, &up))
        .await;
    assert_eq!(info.state, TransferState::Done);
    let t = started.elapsed();
    eprintln!(
        "patchbay upload   {mb:>4} MiB: {t:>10.2?}  {}",
        rate(mb << 20, t)
    );

    h.fs.delete(Some(TARGET), &[dir, up]).await.unwrap();
}
