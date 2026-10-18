//! Integration tests for `session` against the OpenSSH containers in
//! `docker/test` (see `docker/test/compose.yml`):
//!
//! ```sh
//! docker compose -f docker/test/compose.yml up -d --build --wait
//! PATCHBAY_DOCKER_TESTS=1 cargo test -p patchbay-core --test session
//! ```
//!
//! Without `PATCHBAY_DOCKER_TESTS=1` every test is skipped.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use patchbay_core::Error;
use patchbay_core::model::{
    AgentSource, ConnectTarget, ConnectionState, CoreEvent, Prompt, PromptRequest, PromptResponse,
    RemoteOs,
};
use patchbay_core::session::SessionManager;
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};

const TARGET_PORT: u16 = 2201;
const BASTION_PORT: u16 = 2202;
const SFTPONLY_PORT: u16 = 2203;
const TEST_TIMEOUT: Duration = Duration::from_secs(90);

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

fn key(name: &str) -> PathBuf {
    test_dir().join("keys").join(name)
}

/// A scratch directory holding this test's known_hosts file.
struct Scratch {
    _dir: TempDir,
    known_hosts: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let known_hosts = dir.path().join("ssh").join("known_hosts");
        Self {
            _dir: dir,
            known_hosts,
        }
    }

    /// Start from the fixture known_hosts that lists all test containers.
    fn with_fixture_known_hosts() -> Self {
        let s = Self::new();
        std::fs::create_dir_all(s.known_hosts.parent().unwrap()).unwrap();
        std::fs::copy(test_dir().join("known_hosts"), &s.known_hosts).unwrap();
        s
    }

    fn write_known_hosts(&self, contents: &str) {
        std::fs::create_dir_all(self.known_hosts.parent().unwrap()).unwrap();
        std::fs::write(&self.known_hosts, contents).unwrap();
    }

    fn read_known_hosts(&self) -> String {
        std::fs::read_to_string(&self.known_hosts).unwrap_or_default()
    }

    fn target(&self, label: &str, port: u16) -> ConnectTarget {
        ConnectTarget {
            host_id: Some(format!("test:{label}")),
            label: label.to_owned(),
            hostname: "127.0.0.1".to_owned(),
            port,
            user: "patchbay".to_owned(),
            identity_files: Vec::new(),
            identities_only: true,
            agent: AgentSource::Disabled,
            host_key_alias: None,
            known_hosts_files: vec![self.known_hosts.clone()],
            connect_timeout: Some(Duration::from_secs(10)),
            server_alive_interval: None,
            jumps: Vec::new(),
            auth_methods: Default::default(),
        }
    }
}

fn with_key(mut target: ConnectTarget, name: &str) -> ConnectTarget {
    target.identity_files = vec![key(name)];
    target
}

/// A [`SessionManager`] wired to a task that answers prompts with `policy`
/// and records every prompt it saw.
struct Harness {
    manager: SessionManager,
    events: broadcast::Receiver<CoreEvent>,
    prompts: Arc<Mutex<Vec<Prompt>>>,
}

impl Harness {
    fn new(policy: impl Fn(&Prompt) -> PromptResponse + Send + Sync + 'static) -> Self {
        let (events_tx, events) = broadcast::channel(256);
        let (prompt_tx, mut prompt_rx) = mpsc::channel::<PromptRequest>(16);
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let seen = prompts.clone();
        tokio::spawn(async move {
            while let Some(req) = prompt_rx.recv().await {
                seen.lock().unwrap().push(req.prompt.clone());
                let _ = req.reply.send(policy(&req.prompt));
            }
        });
        Self {
            manager: SessionManager::new(events_tx, prompt_tx),
            events,
            prompts,
        }
    }

    /// Accepts host keys and answers every secret correctly.
    fn friendly() -> Self {
        Self::new(friendly)
    }

    fn prompts(&self) -> Vec<Prompt> {
        self.prompts.lock().unwrap().clone()
    }

    fn count(&self, pred: impl Fn(&Prompt) -> bool) -> usize {
        self.prompts().iter().filter(|p| pred(p)).count()
    }

    fn states(&mut self, host_id: &str) -> Vec<ConnectionState> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            if let CoreEvent::Connection { host_id: id, state } = event
                && id == host_id
            {
                out.push(state);
            }
        }
        out
    }
}

fn friendly(prompt: &Prompt) -> PromptResponse {
    match prompt {
        Prompt::HostKeyUnknown { .. } | Prompt::HostKeyChanged { .. } => PromptResponse::Accept,
        Prompt::Password { .. } => secret("patchbay"),
        Prompt::KeyPassphrase { .. } => secret("secret"),
        Prompt::KeyboardInteractive { prompts, .. } => PromptResponse::Answers {
            answers: prompts.iter().map(|_| "patchbay".to_owned()).collect(),
        },
        // Not asked while connecting.
        _ => PromptResponse::Cancel,
    }
}

fn secret(value: &str) -> PromptResponse {
    PromptResponse::Secret {
        value: value.to_owned(),
        remember: false,
    }
}

fn is_host_key(p: &Prompt) -> bool {
    matches!(
        p,
        Prompt::HostKeyUnknown { .. } | Prompt::HostKeyChanged { .. }
    )
}

fn is_credential(p: &Prompt) -> bool {
    matches!(
        p,
        Prompt::Password { .. } | Prompt::KeyboardInteractive { .. } | Prompt::KeyPassphrase { .. }
    )
}

async fn timed<T>(fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("test timed out")
}

#[tokio::test(flavor = "multi_thread")]
async fn key_auth_and_capabilities() {
    require_docker!();
    let scratch = Scratch::new();
    let mut h = Harness::friendly();
    let target = with_key(scratch.target("key", TARGET_PORT), "id_ed25519");

    let conn = timed(h.manager.connect(target)).await.unwrap();

    let caps = conn.capabilities();
    assert!(caps.sftp);
    assert!(caps.exec);
    assert_eq!(caps.os, RemoteOs::Linux);
    assert_eq!(caps.home.as_deref(), Some("/home/patchbay"));

    let out = timed(conn.exec("echo hello; echo oops >&2; exit 3"))
        .await
        .unwrap();
    assert_eq!(out.stdout, b"hello\n");
    assert_eq!(out.stderr, b"oops\n");
    assert_eq!(out.status, Some(3));

    let sftp = timed(conn.open_sftp()).await.unwrap();
    let readme = timed(sftp.read("docs/readme.txt")).await.unwrap();
    assert_eq!(readme, b"Hello from the Patchbay test target.\n");

    assert_eq!(h.count(is_credential), 0, "{:?}", h.prompts());
    let states = h.states("test:key");
    assert_eq!(
        states.first(),
        Some(&ConnectionState::Connecting { hop: None })
    );
    assert!(states.contains(&ConnectionState::Authenticating));
    assert!(matches!(
        states.last(),
        Some(ConnectionState::Connected { .. })
    ));

    assert!(h.manager.get("test:key").is_some());
    assert!(matches!(
        h.manager.states().get("test:key"),
        Some(ConnectionState::Connected { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn rsa_key_auth() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::friendly();
    let target = with_key(scratch.target("rsa", TARGET_PORT), "id_rsa");
    let conn = timed(h.manager.connect(target)).await.unwrap();
    assert!(conn.capabilities().exec);
    assert_eq!(h.prompts().len(), 0, "{:?}", h.prompts());
}

#[tokio::test(flavor = "multi_thread")]
async fn encrypted_key_asks_for_passphrase_and_retries() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::new(|p| match p {
        Prompt::KeyPassphrase { retry: false, .. } => secret("wrong"),
        other => friendly(other),
    });
    let target = with_key(scratch.target("pass", TARGET_PORT), "id_ed25519_pass");

    timed(h.manager.connect(target)).await.unwrap();

    let retries: Vec<bool> = h
        .prompts()
        .iter()
        .filter_map(|p| match p {
            Prompt::KeyPassphrase { key_path, retry } => {
                assert_eq!(key_path, &key("id_ed25519_pass"));
                Some(*retry)
            }
            _ => None,
        })
        .collect();
    assert_eq!(retries, [false, true]);
    assert_eq!(h.count(|p| matches!(p, Prompt::Password { .. })), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_passphrase_skips_the_key() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::new(|p| match p {
        Prompt::KeyPassphrase { .. } => PromptResponse::Cancel,
        other => friendly(other),
    });
    let mut target = scratch.target("skip", TARGET_PORT);
    target.identity_files = vec![key("does-not-exist"), key("id_ed25519_pass")];

    // Falls through to the PAM password round.
    timed(h.manager.connect(target)).await.unwrap();
    assert_eq!(h.count(|p| matches!(p, Prompt::KeyPassphrase { .. })), 1);
    assert_eq!(h.count(|p| matches!(p, Prompt::Password { .. })), 1);
}

/// PAM's single hidden "Password:" keyboard-interactive round is presented
/// as a password prompt (so it can use the keychain).
#[tokio::test(flavor = "multi_thread")]
async fn keyboard_interactive_password_round() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::friendly();
    let target = scratch.target("kbd", TARGET_PORT);

    timed(h.manager.connect(target)).await.unwrap();

    match h.prompts().as_slice() {
        [Prompt::Password { user, host, retry }] => {
            assert_eq!((user.as_str(), host.as_str()), ("patchbay", "kbd"));
            assert!(!retry);
        }
        other => panic!("unexpected prompts {other:?}"),
    }
}

/// Wrong passwords are retried with `retry: true`: three times through the
/// keyboard-interactive PAM round, then via the `password` method.
#[tokio::test(flavor = "multi_thread")]
async fn password_retries_across_methods() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = asked.clone();
    let h = Harness::new(move |p| match p {
        Prompt::Password { .. } => {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            secret(if n < 3 { "wrong" } else { "patchbay" })
        }
        other => friendly(other),
    });
    let target = scratch.target("password", TARGET_PORT);

    timed(h.manager.connect(target)).await.unwrap();

    let retries: Vec<bool> = h
        .prompts()
        .iter()
        .filter_map(|p| match p {
            Prompt::Password { retry, .. } => Some(*retry),
            _ => None,
        })
        .collect();
    assert_eq!(retries, [false, true, true, true]);
    assert_eq!(h.prompts().len(), 4, "{:?}", h.prompts());
}

#[tokio::test(flavor = "multi_thread")]
async fn all_methods_failing_is_auth_failed() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let mut h = Harness::new(|p| match p {
        Prompt::Password { .. } => secret("wrong"),
        other => friendly(other),
    });
    let target = with_key(
        scratch.target("fail", TARGET_PORT),
        "id_ed25519_unauthorized",
    );

    let err = timed(h.manager.connect(target)).await.unwrap_err();
    assert!(matches!(err, Error::AuthFailed { .. }), "{err:?}");
    // 3 keyboard-interactive PAM rounds + 3 `password` attempts.
    assert_eq!(h.count(|p| matches!(p, Prompt::Password { .. })), 6);
    assert!(h.manager.get("test:fail").is_none());
    assert!(matches!(
        h.states("test:fail").last(),
        Some(ConnectionState::Disconnected { reason: Some(_) })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_password_prompt_aborts() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::new(|p| match p {
        Prompt::Password { .. } => PromptResponse::Cancel,
        other => friendly(other),
    });
    let err = timed(h.manager.connect(scratch.target("cancel", TARGET_PORT)))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_host_key_is_prompted_and_recorded() {
    require_docker!();
    let scratch = Scratch::new();
    let h = Harness::friendly();
    let target = with_key(scratch.target("unknown", TARGET_PORT), "id_ed25519");

    let conn = timed(h.manager.connect(target.clone())).await.unwrap();

    let prompts = h.prompts();
    let host_key_prompts: Vec<_> = prompts.iter().filter(|p| is_host_key(p)).collect();
    assert_eq!(host_key_prompts.len(), 1);
    match host_key_prompts[0] {
        Prompt::HostKeyUnknown {
            host,
            port,
            fingerprint,
            key_type,
        } => {
            assert_eq!((host.as_str(), *port), ("127.0.0.1", TARGET_PORT));
            assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
            assert!(!key_type.is_empty());
        }
        other => panic!("unexpected prompt {other:?}"),
    }
    let recorded = scratch.read_known_hosts();
    assert!(recorded.contains("[127.0.0.1]:2201"), "{recorded}");

    // A fresh manager now trusts the recorded key without asking.
    conn.close().await;
    let h2 = Harness::new(|p| match p {
        Prompt::HostKeyUnknown { .. } | Prompt::HostKeyChanged { .. } => PromptResponse::Reject,
        other => friendly(other),
    });
    timed(h2.manager.connect(target)).await.unwrap();
    assert_eq!(h2.count(is_host_key), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_unknown_host_key_is_not_recorded() {
    require_docker!();
    let scratch = Scratch::new();
    let h = Harness::new(|p| match p {
        Prompt::HostKeyUnknown { .. } => PromptResponse::Reject,
        other => friendly(other),
    });
    let target = with_key(scratch.target("reject", TARGET_PORT), "id_ed25519");
    let err = timed(h.manager.connect(target)).await.unwrap_err();
    assert!(matches!(err, Error::HostKeyRejected { .. }), "{err:?}");
    assert_eq!(scratch.read_known_hosts(), "");
    assert_eq!(h.count(is_credential), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_host_key_is_rejected_or_accepted_once() {
    require_docker!();
    let scratch = Scratch::new();
    // Record some other ed25519 key for the target.
    let other_key = std::fs::read_to_string(key("id_ed25519.pub")).unwrap();
    let other_key: Vec<&str> = other_key.split_whitespace().take(2).collect();
    let contents = format!("[127.0.0.1]:{TARGET_PORT} {}\n", other_key.join(" "));
    scratch.write_known_hosts(&contents);
    let target = with_key(scratch.target("changed", TARGET_PORT), "id_ed25519");

    let h = Harness::new(|p| match p {
        Prompt::HostKeyChanged { .. } => PromptResponse::Reject,
        other => friendly(other),
    });
    let err = timed(h.manager.connect(target.clone())).await.unwrap_err();
    assert!(matches!(err, Error::HostKeyRejected { .. }), "{err:?}");
    match h.prompts().as_slice() {
        [
            Prompt::HostKeyChanged {
                known_hosts_file,
                line,
                host,
                port,
                ..
            },
        ] => {
            assert_eq!(known_hosts_file, &scratch.known_hosts);
            assert_eq!(*line, 1);
            assert_eq!((host.as_str(), *port), ("127.0.0.1", TARGET_PORT));
        }
        other => panic!("unexpected prompts {other:?}"),
    }

    // Accepting proceeds for this session only; known_hosts is untouched.
    let h = Harness::friendly();
    timed(h.manager.connect(target)).await.unwrap();
    assert_eq!(h.count(|p| matches!(p, Prompt::HostKeyChanged { .. })), 1);
    assert_eq!(scratch.read_known_hosts(), contents);
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_jump_through_bastion() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let mut h = Harness::new(|p| match p {
        Prompt::HostKeyUnknown { .. } | Prompt::HostKeyChanged { .. } => PromptResponse::Reject,
        other => friendly(other),
    });
    let bastion = with_key(scratch.target("bastion", BASTION_PORT), "id_ed25519");
    let mut target = with_key(scratch.target("target-via-bastion", 22), "id_ed25519");
    target.hostname = "target".to_owned();
    target.jumps = vec![bastion];

    let conn = timed(h.manager.connect(target)).await.unwrap();
    let out = timed(conn.exec("hostname")).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "target");
    assert!(conn.capabilities().sftp);
    assert_eq!(h.prompts().len(), 0, "{:?}", h.prompts());

    let states = h.states("test:target-via-bastion");
    assert_eq!(
        states.first(),
        Some(&ConnectionState::Connecting {
            hop: Some("bastion".to_owned())
        })
    );
    assert!(states.contains(&ConnectionState::Connecting { hop: None }));
    assert!(matches!(
        states.last(),
        Some(ConnectionState::Connected { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_jump_prompts_for_each_hop_key() {
    require_docker!();
    let scratch = Scratch::new();
    let h = Harness::friendly();
    let bastion = with_key(scratch.target("bastion", BASTION_PORT), "id_ed25519");
    let mut target = with_key(scratch.target("jumped", 22), "id_ed25519");
    target.hostname = "target".to_owned();
    target.jumps = vec![bastion];

    timed(h.manager.connect(target)).await.unwrap();
    let hosts: Vec<(String, u16)> = h
        .prompts()
        .iter()
        .filter_map(|p| match p {
            Prompt::HostKeyUnknown { host, port, .. } => Some((host.clone(), *port)),
            _ => None,
        })
        .collect();
    assert_eq!(
        hosts,
        [
            ("127.0.0.1".to_owned(), BASTION_PORT),
            ("target".to_owned(), 22)
        ]
    );
    let recorded = scratch.read_known_hosts();
    assert!(recorded.contains("[127.0.0.1]:2202"), "{recorded}");
    assert!(
        recorded.lines().any(|l| l.starts_with("target ")),
        "{recorded}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn sftp_only_server_has_no_exec() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::friendly();
    let target = scratch.target("sftponly", SFTPONLY_PORT);

    let conn = timed(h.manager.connect(target)).await.unwrap();
    let caps = conn.capabilities();
    assert!(caps.sftp);
    assert!(!caps.exec);
    assert_eq!(caps.os, RemoteOs::Unknown);
    assert_eq!(caps.home.as_deref(), Some("/"));
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_connects_share_one_attempt() {
    require_docker!();
    let scratch = Scratch::new();
    let mut h = Harness::friendly();
    let target = with_key(scratch.target("dedup", TARGET_PORT), "id_ed25519");

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let manager = h.manager.clone();
            let target = target.clone();
            tokio::spawn(async move { manager.connect(target).await })
        })
        .collect();
    let mut conns = Vec::new();
    for task in tasks {
        conns.push(timed(task).await.unwrap().unwrap());
    }
    assert!(conns.iter().all(|c| Arc::ptr_eq(c, &conns[0])));
    assert_eq!(h.count(is_host_key), 1);
    let connected = h
        .states("test:dedup")
        .iter()
        .filter(|s| matches!(s, ConnectionState::Connected { .. }))
        .count();
    assert_eq!(connected, 1);

    // Later calls reuse the live connection.
    let again = timed(h.manager.connect(target)).await.unwrap();
    assert!(Arc::ptr_eq(&again, &conns[0]));
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_closes_and_reports() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let mut h = Harness::friendly();
    let target = with_key(scratch.target("bye", TARGET_PORT), "id_ed25519");
    let conn = timed(h.manager.connect(target)).await.unwrap();
    h.states("test:bye");

    h.manager.disconnect("test:bye").await;
    assert!(h.manager.get("test:bye").is_none());
    assert!(h.manager.states().is_empty());
    timed(conn.closed()).await;
    assert!(conn.is_closed());
    assert_eq!(
        h.states("test:bye"),
        [ConnectionState::Disconnected { reason: None }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lost_connection_is_removed_and_reported() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let mut h = Harness::friendly();
    let target = with_key(scratch.target("lost", TARGET_PORT), "id_ed25519");
    let conn = timed(h.manager.connect(target)).await.unwrap();
    h.states("test:lost");

    // Kill the server-side session process that owns this connection.
    let _ = conn
        .exec_timeout("kill -9 $PPID", Duration::from_secs(5))
        .await;
    timed(conn.closed()).await;

    let states = timed(async {
        loop {
            let states = h.states("test:lost");
            if !states.is_empty() {
                break states;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        matches!(
            states.as_slice(),
            [ConnectionState::Disconnected { reason: Some(_) }]
        ),
        "{states:?}"
    );
    assert!(h.manager.get("test:lost").is_none());
}

/// An `ssh-agent` on a private socket, killed on drop.
#[cfg(unix)]
struct TestAgent {
    child: std::process::Child,
    socket: PathBuf,
    dir: TempDir,
}

#[cfg(unix)]
impl TestAgent {
    /// `None` if OpenSSH's `ssh-agent` is not installed.
    fn start(keys: &[&str]) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        let dir = TempDir::new().unwrap();
        let socket = dir.path().join("agent.sock");
        let child = Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .stdout(Stdio::null())
            .spawn()
            .ok()?;
        let agent = Self { child, socket, dir };
        for _ in 0..100 {
            if agent.socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        for name in keys {
            // ssh-add insists on private permissions.
            let copy = agent.dir.path().join(name);
            std::fs::copy(key(name), &copy).unwrap();
            std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600)).unwrap();
            let status = Command::new("ssh-add")
                .arg(&copy)
                .env("SSH_AUTH_SOCK", &agent.socket)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "ssh-add {name}");
        }
        Some(agent)
    }
}

#[cfg(unix)]
impl Drop for TestAgent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_auth() {
    require_docker!();
    let Some(agent) = TestAgent::start(&["id_ed25519"]) else {
        eprintln!("skipped: ssh-agent not installed");
        return;
    };
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::friendly();
    let mut target = scratch.target("agent", TARGET_PORT);
    target.identities_only = false;
    target.agent = AgentSource::Path(agent.socket.clone());

    timed(h.manager.connect(target)).await.unwrap();
    assert_eq!(h.prompts().len(), 0, "{:?}", h.prompts());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn identities_only_filters_agent_keys() {
    require_docker!();
    let Some(agent) = TestAgent::start(&["id_ed25519_unauthorized", "id_ed25519"]) else {
        eprintln!("skipped: ssh-agent not installed");
        return;
    };
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::friendly();
    let mut target = with_key(
        scratch.target("agent-only", TARGET_PORT),
        "id_ed25519_unauthorized",
    );
    target.agent = AgentSource::Path(agent.socket.clone());

    // The authorized agent key is not listed in identity_files, so it must
    // not be offered; authentication falls through to the PAM password round.
    timed(h.manager.connect(target)).await.unwrap();
    assert_eq!(h.count(|p| matches!(p, Prompt::Password { .. })), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_agent_is_skipped() {
    require_docker!();
    let scratch = Scratch::with_fixture_known_hosts();
    let h = Harness::friendly();
    let mut target = with_key(scratch.target("no-agent", TARGET_PORT), "id_ed25519");
    target.agent = AgentSource::Path(scratch.known_hosts.with_file_name("no-such-agent.sock"));
    timed(h.manager.connect(target)).await.unwrap();
    assert_eq!(h.prompts().len(), 0, "{:?}", h.prompts());
}

/// Needs no SSH server: a peer that accepts TCP but never speaks SSH.
#[tokio::test]
async fn silent_server_times_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _server = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let scratch = Scratch::new();
    let mut h = Harness::friendly();
    let mut target = scratch.target("silent", port);
    target.connect_timeout = Some(Duration::from_millis(300));

    let err = timed(h.manager.connect(target)).await.unwrap_err();
    assert!(matches!(err, Error::Timeout(_)), "{err:?}");
    assert!(matches!(
        h.states("test:silent").last(),
        Some(ConnectionState::Disconnected { reason: Some(_) })
    ));
    assert!(h.manager.states().is_empty());
}

#[tokio::test]
async fn disconnect_aborts_pending_attempt() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _server = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let scratch = Scratch::new();
    let h = Harness::friendly();
    let target = scratch.target("abort", port);

    let manager = h.manager.clone();
    let pending = tokio::spawn(async move { manager.connect(target).await });
    while h.manager.states().is_empty() {
        tokio::task::yield_now().await;
    }
    h.manager.disconnect("test:abort").await;
    let err = timed(pending).await.unwrap().unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
}

/// `"<type> <base64>"` of the target container's host key of `key_type`,
/// from the fixture known_hosts.
fn target_host_key(key_type: &str) -> String {
    let fixture = std::fs::read_to_string(test_dir().join("known_hosts")).unwrap();
    let line = fixture
        .lines()
        .find(|l| {
            l.starts_with("[127.0.0.1]:2201") && l.split_whitespace().nth(1) == Some(key_type)
        })
        .unwrap_or_else(|| panic!("no {key_type} key for the target in the fixture"));
    line.split_whitespace()
        .skip(1)
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::test(flavor = "multi_thread")]
async fn recorded_host_key_algorithm_is_preferred() {
    require_docker!();
    let cases = [
        ("[127.0.0.1]:2201", None, "ecdsa-sha2-nistp256"),
        ("[127.0.0.1]:2201", None, "ssh-rsa"),
        (
            "[*.alias]:2201",
            Some("target.alias"),
            "ecdsa-sha2-nistp256",
        ),
    ];
    for (hosts, alias, key_type) in cases {
        let scratch = Scratch::new();
        scratch.write_known_hosts(&format!("{hosts} {}\n", target_host_key(key_type)));
        let h = Harness::new(|p| match p {
            Prompt::HostKeyUnknown { .. } | Prompt::HostKeyChanged { .. } => PromptResponse::Reject,
            other => friendly(other),
        });
        let mut target = with_key(scratch.target("prefer", TARGET_PORT), "id_ed25519");
        target.host_key_alias = alias.map(str::to_owned);

        let res = timed(h.manager.connect(target)).await;
        assert!(res.is_ok(), "{key_type} via {hosts}: {res:?}");
        assert_eq!(h.prompts().len(), 0, "{:?}", h.prompts());
    }
}

// ---------------------------------------------------------------------------
// Keyboard-interactive flows the container's PAM stack cannot produce, against
// an in-process russh server. These need no Docker.
// ---------------------------------------------------------------------------

mod kbd_server {
    use std::borrow::Cow;
    use std::sync::Arc;

    use russh::keys::PublicKey;
    use russh::server::{self, Auth, Response};
    use russh::{MethodKind, MethodSet};

    #[derive(Clone, Copy)]
    pub enum Scenario {
        /// One round, two prompts (one echoed), no password.
        TwoPrompts,
        /// PAM-style password round, then an OTP round.
        PasswordThenOtp,
        /// Password via keyboard-interactive is a partial success; the
        /// server then requires a public key.
        PasswordThenKey,
    }

    struct Handler {
        scenario: Scenario,
        round: usize,
        user_key: PublicKey,
    }

    fn partial(prompts: &'static [(Cow<'static, str>, bool)]) -> Auth {
        Auth::Partial {
            name: "Patchbay test".into(),
            instructions: "Answer the questions".into(),
            prompts: Cow::Borrowed(prompts),
        }
    }

    const TWO: &[(Cow<'static, str>, bool)] = &[
        (Cow::Borrowed("Verification code: "), true),
        (Cow::Borrowed("PIN: "), false),
    ];
    const PASSWORD: &[(Cow<'static, str>, bool)] = &[(Cow::Borrowed("Password: "), false)];
    const OTP: &[(Cow<'static, str>, bool)] = &[(Cow::Borrowed("One-time code: "), true)];

    impl server::Handler for Handler {
        type Error = russh::Error;

        async fn auth_keyboard_interactive<'a>(
            &'a mut self,
            _user: &str,
            _submethods: &str,
            response: Option<Response<'a>>,
        ) -> Result<Auth, Self::Error> {
            let answers: Vec<String> = response
                .map(|r| {
                    r.map(|b| String::from_utf8_lossy(&b).into_owned())
                        .collect()
                })
                .unwrap_or_default();
            self.round += 1;
            let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
            Ok(match (self.scenario, self.round, answers.as_slice()) {
                (Scenario::TwoPrompts, 1, []) => partial(TWO),
                (Scenario::TwoPrompts, 2, ["123456", "0000"]) => Auth::Accept,
                (Scenario::PasswordThenOtp, 1, []) => partial(PASSWORD),
                (Scenario::PasswordThenOtp, 2, ["patchbay"]) => partial(OTP),
                (Scenario::PasswordThenOtp, 3, ["42"]) => Auth::Accept,
                (Scenario::PasswordThenKey, 1, []) => partial(PASSWORD),
                (Scenario::PasswordThenKey, 2, ["patchbay"]) => Auth::Reject {
                    proceed_with_methods: Some(MethodSet::from(&[MethodKind::PublicKey][..])),
                    partial_success: true,
                },
                _ => Auth::reject(),
            })
        }

        async fn auth_publickey(
            &mut self,
            _user: &str,
            key: &PublicKey,
        ) -> Result<Auth, Self::Error> {
            let ok = matches!(self.scenario, Scenario::PasswordThenKey)
                && self.round >= 2
                && key.key_data() == self.user_key.key_data();
            Ok(if ok { Auth::Accept } else { Auth::reject() })
        }
    }

    /// Serve `scenario` on an ephemeral port; returns the port and the
    /// known_hosts line for it.
    pub async fn start(scenario: Scenario) -> (u16, String) {
        let dir = super::test_dir();
        let host_key =
            russh::keys::load_secret_key(dir.join("hostkeys/target/ssh_host_ed25519_key"), None)
                .unwrap();
        let user_key = russh::keys::load_public_key(dir.join("keys/id_ed25519.pub")).unwrap();
        let config = Arc::new(server::Config {
            methods: MethodSet::from(&[MethodKind::KeyboardInteractive][..]),
            auth_rejection_time: std::time::Duration::ZERO,
            auth_rejection_time_initial: Some(std::time::Duration::ZERO),
            keys: vec![host_key.clone()],
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let handler = Handler {
                    scenario,
                    round: 0,
                    user_key: user_key.clone(),
                };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(config, socket, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
        let public = host_key.public_key().to_openssh().unwrap();
        (port, format!("[127.0.0.1]:{port} {public}\n"))
    }
}

async fn kbd_scenario(
    scenario: kbd_server::Scenario,
    identity: Option<&str>,
) -> (Harness, Result<(), Error>) {
    let (port, known) = kbd_server::start(scenario).await;
    let scratch = Scratch::new();
    scratch.write_known_hosts(&known);
    let h = Harness::new(|p| match p {
        Prompt::KeyboardInteractive { prompts, .. } => PromptResponse::Answers {
            answers: prompts
                .iter()
                .map(|q| match q.text.as_str() {
                    "Verification code: " => "123456",
                    "PIN: " => "0000",
                    "One-time code: " => "42",
                    _ => "?",
                })
                .map(str::to_owned)
                .collect(),
        },
        other => friendly(other),
    });
    let mut target = scratch.target("kbd-server", port);
    if let Some(name) = identity {
        target.identity_files = vec![key(name)];
    }
    let res = timed(h.manager.connect(target)).await.map(|_| ());
    (h, res)
}

#[tokio::test(flavor = "multi_thread")]
async fn keyboard_interactive_multi_prompt() {
    let (h, res) = kbd_scenario(kbd_server::Scenario::TwoPrompts, None).await;
    res.unwrap();
    match h.prompts().as_slice() {
        [
            Prompt::KeyboardInteractive {
                name,
                instructions,
                prompts,
                ..
            },
        ] => {
            assert_eq!(name, "Patchbay test");
            assert_eq!(instructions, "Answer the questions");
            let shown: Vec<(&str, bool)> =
                prompts.iter().map(|p| (p.text.as_str(), p.echo)).collect();
            assert_eq!(shown, [("Verification code: ", true), ("PIN: ", false)]);
        }
        other => panic!("unexpected prompts {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keyboard_interactive_password_then_otp() {
    let (h, res) = kbd_scenario(kbd_server::Scenario::PasswordThenOtp, None).await;
    res.unwrap();
    let kinds: Vec<&str> = h
        .prompts()
        .iter()
        .map(|p| match p {
            Prompt::Password { .. } => "password",
            Prompt::KeyboardInteractive { .. } => "kbd",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["password", "kbd"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_success_continues_with_next_method() {
    let (h, res) = kbd_scenario(kbd_server::Scenario::PasswordThenKey, Some("id_ed25519")).await;
    res.unwrap();
    assert_eq!(h.count(|p| matches!(p, Prompt::Password { .. })), 1);
}
