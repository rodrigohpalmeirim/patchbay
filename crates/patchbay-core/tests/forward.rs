//! Integration tests for `forward` against the OpenSSH containers in
//! `docker/test` (see `docker/test/compose.yml`):
//!
//! ```sh
//! docker compose -f docker/test/compose.yml up -d --build --wait
//! PATCHBAY_DOCKER_TESTS=1 cargo test -p patchbay-core --test forward
//! ```
//!
//! Without `PATCHBAY_DOCKER_TESTS=1` only the tests that need no server run.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use patchbay_core::Error;
use patchbay_core::forward::{ForwardManager, ForwardStore};
use patchbay_core::hosts::HostStore;
use patchbay_core::model::{
    ConnectionState, CoreEvent, PortForward, PortForwardDraft, PortForwardKind, PortForwardState,
    PortForwardStatus, Prompt, PromptRequest, PromptResponse,
};
use patchbay_core::session::SessionManager;
use patchbay_core::ssh_config::LocalEnv;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, broadcast, mpsc};

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

/// A port for the test sshd to listen on, distinct per test and run.
fn remote_port() -> u16 {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let base = 20_000 + (std::process::id() % 400) as u16 * 20;
    base + NEXT.fetch_add(1, Ordering::Relaxed)
}

async fn timed<T>(fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("test timed out")
}

/// A [`ForwardManager`] over an ssh_config naming the test containers
/// (`cfg:target`, `cfg:bastion`, `cfg:sftponly`, and `cfg:target-pw`, which
/// has no key and so needs a password), with prompts answered by `policy`.
struct Fx {
    _dir: TempDir,
    sessions: SessionManager,
    forwards: ForwardManager,
    events: Arc<Mutex<Vec<CoreEvent>>>,
    prompts: Arc<Mutex<Vec<Prompt>>>,
}

impl Fx {
    fn new(policy: impl Fn(&Prompt, usize) -> PromptResponse + Send + Sync + 'static) -> Self {
        let dir = TempDir::new().unwrap();
        let known_hosts = dir.path().join("known_hosts");
        std::fs::copy(test_dir().join("known_hosts"), &known_hosts).unwrap();
        let key = test_dir().join("keys/id_ed25519");
        let mut config = String::new();
        for (name, port, key) in [
            ("target", 2201, Some(&key)),
            ("bastion", 2202, Some(&key)),
            ("sftponly", 2203, Some(&key)),
            ("target-pw", 2201, None),
        ] {
            config += &format!(
                "Host {name}\n  HostName 127.0.0.1\n  Port {port}\n  User patchbay\n  \
                 IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile {}\n",
                known_hosts.display()
            );
            match key {
                Some(key) => config += &format!("  IdentityFile {}\n", key.display()),
                None => config += "  IdentityFile /nonexistent/key\n",
            }
        }
        let cfg = dir.path().join("ssh_config");
        std::fs::write(&cfg, config).unwrap();
        let hosts =
            HostStore::load_with_env(cfg, dir.path().join("hosts.json"), LocalEnv::detect())
                .unwrap();

        let (events_tx, mut events_rx) = broadcast::channel(4096);
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        tokio::spawn(async move {
            while let Ok(e) = events_rx.recv().await {
                seen.lock().unwrap().push(e);
            }
        });
        let (prompt_tx, mut prompt_rx) = mpsc::channel::<PromptRequest>(16);
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let asked = prompts.clone();
        tokio::spawn(async move {
            while let Some(req) = prompt_rx.recv().await {
                let n = {
                    let mut asked = asked.lock().unwrap();
                    asked.push(req.prompt.clone());
                    asked.len()
                };
                let _ = req.reply.send(policy(&req.prompt, n));
            }
        });
        let sessions = SessionManager::new(events_tx.clone(), prompt_tx);
        let store = ForwardStore::load(dir.path().join("forwards.json")).unwrap();
        let forwards = ForwardManager::new(
            sessions.clone(),
            Arc::new(RwLock::new(hosts)),
            events_tx,
            store,
        );
        Self {
            _dir: dir,
            sessions,
            forwards,
            events,
            prompts,
        }
    }

    fn friendly() -> Self {
        Self::new(|p, _| friendly(p))
    }

    async fn create(&self, draft: PortForwardDraft) -> PortForward {
        self.forwards.create(draft).await.unwrap()
    }

    async fn status(&self, id: &str) -> PortForwardStatus {
        self.forwards
            .statuses()
            .await
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
    }

    /// Polls until the forward's status satisfies `pred`.
    async fn wait_status(
        &self,
        id: &str,
        pred: impl Fn(&PortForwardStatus) -> bool,
    ) -> PortForwardStatus {
        timed(async {
            loop {
                let s = self.status(id).await;
                if pred(&s) {
                    return s;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
    }

    /// Every state the forward went through, in order, deduplicated.
    fn forward_states(&self, id: &str) -> Vec<PortForwardState> {
        let mut out: Vec<PortForwardState> = Vec::new();
        for e in self.events.lock().unwrap().iter() {
            if let CoreEvent::PortForward(s) = e
                && s.id == id
                && out.last() != Some(&s.state)
            {
                out.push(s.state.clone());
            }
        }
        out
    }

    fn connection_states(&self, host: &str) -> Vec<ConnectionState> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                CoreEvent::Connection { host_id, state } if host_id == host => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    fn prompt_count(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }

    /// Kill the server-side session process of `host`'s connection.
    async fn kill_connection(&self, host: &str) {
        let conn = self.sessions.get(host).expect("connected");
        let _ = conn
            .exec_timeout("kill -9 $PPID", Duration::from_secs(5))
            .await;
        timed(conn.closed()).await;
    }

    async fn exec(&self, host: &str, command: &str) -> String {
        let conn = self.sessions.get(host).expect("connected");
        let out = timed(conn.exec(command)).await.unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn friendly(prompt: &Prompt) -> PromptResponse {
    match prompt {
        Prompt::HostKeyUnknown { .. } => PromptResponse::Accept,
        Prompt::Password { .. } => PromptResponse::Secret {
            value: "patchbay".into(),
            remember: false,
        },
        Prompt::KeyboardInteractive { prompts, .. } => PromptResponse::Answers {
            answers: prompts.iter().map(|_| "patchbay".to_owned()).collect(),
        },
        _ => PromptResponse::Cancel,
    }
}

fn draft(kind: PortForwardKind, host: &str) -> PortForwardDraft {
    PortForwardDraft {
        name: String::new(),
        kind,
        host_id: host.into(),
        bind_address: None,
        bind_port: 0,
        target_host: Some("localhost".into()),
        target_port: Some(22),
        via_host_id: None,
        auto_start: false,
    }
}

fn active_address(s: &PortForwardStatus) -> String {
    match &s.state {
        PortForwardState::Active { address } => address.clone(),
        other => panic!("not active: {other:?}"),
    }
}

fn is_active(s: &PortForwardStatus) -> bool {
    matches!(s.state, PortForwardState::Active { .. })
}

/// Read up to the end of the first line (an SSH banner).
async fn read_line(stream: &mut TcpStream) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8];
    while timed(stream.read(&mut byte)).await.unwrap() == 1 {
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    String::from_utf8_lossy(&line).into_owned()
}

async fn banner_through(address: &str) -> String {
    let mut stream = timed(TcpStream::connect(address)).await.unwrap();
    read_line(&mut stream).await
}

/// A TCP echo server on 127.0.0.1; returns its port.
async fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = socket.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn port_in_use_fails() {
    let fx = Fx::friendly();
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = busy.local_addr().unwrap().port();
    let rule = fx
        .create(PortForwardDraft {
            bind_port: port,
            ..draft(PortForwardKind::Local, "cfg:target")
        })
        .await;
    let err = timed(fx.forwards.start(&rule.id)).await.unwrap_err();
    assert!(err.to_string().contains("already in use"), "{err}");
    let status = fx.status(&rule.id).await;
    assert!(
        matches!(&status.state, PortForwardState::Failed { message } if message.contains(&port.to_string())),
        "{status:?}"
    );
    assert!(fx.sessions.states().is_empty(), "fails before connecting");

    fx.forwards.stop(&rule.id).await;
    assert_eq!(fx.status(&rule.id).await.state, PortForwardState::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn local_forward_reaches_sshd() {
    require_docker!();
    let fx = Fx::friendly();
    let rule = fx
        .create(PortForwardDraft {
            name: "ssh".into(),
            bind_address: Some("localhost".into()),
            ..draft(PortForwardKind::Local, "cfg:target")
        })
        .await;
    assert_eq!(fx.status(&rule.id).await.state, PortForwardState::Stopped);

    let status = timed(fx.forwards.start(&rule.id)).await.unwrap();
    let address = active_address(&status);
    assert!(address.starts_with("127.0.0.1:"), "{address}");
    assert!(
        fx.sessions.get("cfg:target").is_some(),
        "host got connected"
    );

    for _ in 0..3 {
        let banner = banner_through(&address).await;
        assert!(banner.starts_with("SSH-2.0-OpenSSH"), "{banner:?}");
    }
    let port = address.rsplit(':').next().unwrap();
    let banner = banner_through(&format!("[::1]:{port}")).await;
    assert!(banner.starts_with("SSH-2.0-"), "localhost also binds ::1");

    let s = fx
        .wait_status(&rule.id, |s| {
            s.total_connections == 4 && s.open_connections == 0 && s.bytes_in >= 4 * 20
        })
        .await;
    assert_eq!(s.bytes_out, 0);

    // Starting again is a no-op returning the current status.
    let again = timed(fx.forwards.start(&rule.id)).await.unwrap();
    assert_eq!(active_address(&again), address);

    timed(fx.forwards.stop(&rule.id)).await;
    assert_eq!(fx.status(&rule.id).await.state, PortForwardState::Stopped);
    assert!(
        TcpStream::connect(&address).await.is_err(),
        "listener closed"
    );
    assert_eq!(
        fx.forward_states(&rule.id),
        [
            PortForwardState::Starting,
            PortForwardState::Active {
                address: address.clone()
            },
            PortForwardState::Stopped
        ]
    );
    assert_eq!(fx.prompt_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_socks_proxy() {
    require_docker!();
    let fx = Fx::friendly();
    let rule = fx
        .create(draft(PortForwardKind::Dynamic, "cfg:target"))
        .await;
    let address = active_address(&timed(fx.forwards.start(&rule.id)).await.unwrap());

    // SOCKS5, domain name.
    let mut s = timed(TcpStream::connect(&address)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut buf = [0u8; 2];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, [5, 0]);
    let mut req = vec![5, 1, 0, 3, 9];
    req.extend_from_slice(b"localhost");
    req.extend_from_slice(&22u16.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut reply = [0u8; 10];
    timed(s.read_exact(&mut reply)).await.unwrap();
    assert_eq!(reply[..2], [5, 0], "{reply:?}");
    assert!(read_line(&mut s).await.starts_with("SSH-2.0-"));

    // SOCKS5, IPv4 literal, closed port: connection refused.
    let mut s = timed(TcpStream::connect(&address)).await.unwrap();
    s.write_all(&[5, 1, 0, 5, 1, 0, 1, 127, 0, 0, 1, 0, 1])
        .await
        .unwrap();
    let mut reply = [0u8; 12];
    timed(s.read_exact(&mut reply)).await.unwrap();
    assert_eq!(reply[..4], [5, 0, 5, 5], "{reply:?}");

    // SOCKS4a.
    let mut s = timed(TcpStream::connect(&address)).await.unwrap();
    let mut req = vec![4, 1, 0, 22, 0, 0, 0, 1, 0];
    req.extend_from_slice(b"localhost\0");
    s.write_all(&req).await.unwrap();
    let mut reply = [0u8; 8];
    timed(s.read_exact(&mut reply)).await.unwrap();
    assert_eq!(reply[..2], [0, 0x5a]);
    assert!(read_line(&mut s).await.starts_with("SSH-2.0-"));

    fx.wait_status(&rule.id, |s| s.total_connections == 3).await;
    timed(fx.forwards.stop(&rule.id)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_forward_to_local_server() {
    require_docker!();
    let fx = Fx::friendly();
    let echo = echo_server().await;
    let port = remote_port();
    let rule = fx
        .create(PortForwardDraft {
            bind_port: port,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(echo),
            ..draft(PortForwardKind::Remote, "cfg:target")
        })
        .await;
    let status = timed(fx.forwards.start(&rule.id)).await.unwrap();
    assert_eq!(active_address(&status), format!("localhost:{port}"));

    let out = fx
        .exec(
            "cfg:target",
            &format!(
                "bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}; printf patchbay-echo >&3; head -c 13 <&3'"
            ),
        )
        .await;
    assert_eq!(out, "patchbay-echo");
    let s = fx
        .wait_status(&rule.id, |s| {
            s.total_connections == 1 && s.open_connections == 0
        })
        .await;
    assert_eq!((s.bytes_out, s.bytes_in), (13, 13));

    timed(fx.forwards.stop(&rule.id)).await;
    let out = fx
        .exec(
            "cfg:target",
            &format!(
                "bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}' 2>&1 && echo open || echo closed"
            ),
        )
        .await;
    assert!(
        out.ends_with("closed\n"),
        "remote listener cancelled: {out:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn machine_to_machine() {
    require_docker!();
    let fx = Fx::friendly();
    let port = remote_port();
    let rule = fx
        .create(PortForwardDraft {
            bind_port: port,
            via_host_id: Some("cfg:bastion".into()),
            ..draft(PortForwardKind::Remote, "cfg:target")
        })
        .await;
    timed(fx.forwards.start(&rule.id)).await.unwrap();
    assert!(
        fx.sessions.get("cfg:bastion").is_some(),
        "via host connected"
    );

    // From the target, connect to the forwarded port: that reaches
    // localhost:22 as seen from the bastion, i.e. the bastion's sshd.
    let out = fx
        .exec(
            "cfg:target",
            &format!("bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}; head -1 <&3'"),
        )
        .await;
    assert!(out.starts_with("SSH-2.0-OpenSSH"), "{out:?}");
    let bastion_banner = fx
        .exec(
            "cfg:bastion",
            "bash -c 'exec 3<>/dev/tcp/127.0.0.1/22; head -1 <&3'",
        )
        .await;
    assert_eq!(out, bastion_banner);
    let target_hostname = fx.exec("cfg:target", "hostname").await;
    assert_eq!(target_hostname.trim(), "target");

    // Disconnecting the via host on purpose stops the forward too.
    fx.sessions.disconnect("cfg:bastion").await;
    fx.wait_status(&rule.id, |s| s.state == PortForwardState::Stopped)
        .await;
    let out = fx
        .exec(
            "cfg:target",
            &format!(
                "bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}' 2>&1 && echo open || echo closed"
            ),
        )
        .await;
    assert!(
        out.ends_with("closed\n"),
        "remote listener cancelled: {out:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarding_refused_by_server() {
    require_docker!();
    let fx = Fx::friendly();
    let rule = fx
        .create(PortForwardDraft {
            bind_port: remote_port(),
            ..draft(PortForwardKind::Remote, "cfg:sftponly")
        })
        .await;
    let err = timed(fx.forwards.start(&rule.id)).await.unwrap_err();
    assert!(matches!(err, Error::PermissionDenied(_)), "{err}");
    let status = fx.status(&rule.id).await;
    assert!(
        matches!(&status.state, PortForwardState::Failed { message } if message.contains("refused")),
        "{status:?}"
    );

    // Local forwards bind fine; each tunnel is refused and closed.
    let local = fx
        .create(draft(PortForwardKind::Local, "cfg:sftponly"))
        .await;
    let address = active_address(&timed(fx.forwards.start(&local.id)).await.unwrap());
    let mut s = timed(TcpStream::connect(&address)).await.unwrap();
    let mut buf = Vec::new();
    timed(s.read_to_end(&mut buf)).await.unwrap();
    assert!(buf.is_empty());
    timed(fx.forwards.stop(&local.id)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnects_after_connection_loss() {
    require_docker!();
    let fx = Fx::friendly();
    let local = fx.create(draft(PortForwardKind::Local, "cfg:target")).await;
    let echo = echo_server().await;
    let port = remote_port();
    let remote = fx
        .create(PortForwardDraft {
            bind_port: port,
            target_host: Some("127.0.0.1".into()),
            target_port: Some(echo),
            ..draft(PortForwardKind::Remote, "cfg:target")
        })
        .await;
    let address = active_address(&timed(fx.forwards.start(&local.id)).await.unwrap());
    timed(fx.forwards.start(&remote.id)).await.unwrap();
    let before = fx.sessions.get("cfg:target").unwrap();

    fx.kill_connection("cfg:target").await;
    fx.wait_status(&local.id, |s| {
        matches!(s.state, PortForwardState::Reconnecting { .. })
    })
    .await;
    // New connections wait for the tunnel instead of failing.
    let waiting = tokio::spawn({
        let address = address.clone();
        async move { banner_through(&address).await }
    });
    let s = fx.wait_status(&local.id, is_active).await;
    assert_eq!(active_address(&s), address, "same listener");
    fx.wait_status(&remote.id, is_active).await;
    assert!(timed(waiting).await.unwrap().starts_with("SSH-2.0-"));

    let after = fx.sessions.get("cfg:target").unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    assert!(banner_through(&address).await.starts_with("SSH-2.0-"));
    let out = fx
        .exec(
            "cfg:target",
            &format!(
                "bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}; printf again >&3; head -c 5 <&3'"
            ),
        )
        .await;
    assert_eq!(out, "again", "remote forward re-established");

    let states = fx.forward_states(&local.id);
    assert!(
        matches!(
            states.as_slice(),
            [
                PortForwardState::Starting,
                PortForwardState::Active { .. },
                PortForwardState::Reconnecting { attempt: 1, .. },
                ..,
                PortForwardState::Active { .. }
            ]
        ),
        "{states:?}"
    );
    let PortForwardState::Reconnecting { message, .. } = &states[2] else {
        unreachable!()
    };
    assert!(message.contains("lost"), "{message}");
    assert!(
        fx.connection_states("cfg:target")
            .iter()
            .any(|s| matches!(s, ConnectionState::Reconnecting { attempt: 1 })),
    );
    assert_eq!(fx.prompt_count(), 0);

    // Disconnecting on purpose stops the forwards instead.
    fx.sessions.disconnect("cfg:target").await;
    fx.wait_status(&local.id, |s| s.state == PortForwardState::Stopped)
        .await;
    fx.wait_status(&remote.id, |s| s.state == PortForwardState::Stopped)
        .await;
    assert!(TcpStream::connect(&address).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_asks_once_and_cancel_fails() {
    require_docker!();
    // The first password prompt is answered, every later one cancelled.
    let fx = Fx::new(|p, n| match p {
        Prompt::Password { .. } | Prompt::KeyboardInteractive { .. } if n > 1 => {
            PromptResponse::Cancel
        }
        p => friendly(p),
    });
    let rule = fx
        .create(draft(PortForwardKind::Local, "cfg:target-pw"))
        .await;
    timed(fx.forwards.start(&rule.id)).await.unwrap();
    assert_eq!(fx.prompt_count(), 1);

    fx.kill_connection("cfg:target-pw").await;
    let s = fx
        .wait_status(&rule.id, |s| {
            matches!(s.state, PortForwardState::Failed { .. })
        })
        .await;
    assert_eq!(fx.prompt_count(), 2, "asked once while reconnecting");
    assert!(
        matches!(&s.state, PortForwardState::Failed { message } if message.contains("cancelled")),
        "{s:?}"
    );
    assert!(matches!(
        fx.connection_states("cfg:target-pw").last(),
        Some(ConnectionState::Disconnected { reason: Some(_) })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn update_restarts_and_delete_stops() {
    require_docker!();
    let fx = Fx::friendly();
    let rule = fx.create(draft(PortForwardKind::Local, "cfg:target")).await;
    let first = active_address(&timed(fx.forwards.start(&rule.id)).await.unwrap());
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = busy.local_addr().unwrap().port();
    drop(busy);
    fx.forwards
        .update(
            &rule.id,
            PortForwardDraft {
                bind_port: port,
                ..draft(PortForwardKind::Local, "cfg:target")
            },
        )
        .await
        .unwrap();
    let s = fx
        .wait_status(
            &rule.id,
            |s| matches!(&s.state, PortForwardState::Active { address } if *address != first),
        )
        .await;
    assert_eq!(active_address(&s), format!("127.0.0.1:{port}"));
    assert!(
        banner_through(&active_address(&s))
            .await
            .starts_with("SSH-2.0-")
    );

    fx.forwards.delete(&rule.id).await.unwrap();
    assert!(TcpStream::connect(active_address(&s)).await.is_err());
    assert!(fx.forwards.list().await.is_empty());
    assert!(matches!(
        fx.forwards.start(&rule.id).await,
        Err(Error::NotFound(_))
    ));
}
