//! Integration tests for `terminal`: local PTYs (Unix, always run) and
//! remote shells against the OpenSSH containers in `docker/test`:
//!
//! ```sh
//! docker compose -f docker/test/compose.yml up -d --build --wait
//! PATCHBAY_DOCKER_TESTS=1 cargo test -p patchbay-core --test terminal
//! ```
//!
//! Remote tests are skipped without `PATCHBAY_DOCKER_TESTS=1`. The `bench_*`
//! tests are `#[ignore]`d; run them with `-- --ignored --nocapture`.

#![cfg(unix)]

use std::path::Path;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use patchbay_core::Error;
use patchbay_core::model::{
    AgentSource, ConnectTarget, CoreEvent, Prompt, PromptRequest, PromptResponse, TerminalInfo,
    TerminalSpec,
};
use patchbay_core::session::{Connection, SessionManager};
use patchbay_core::terminal::{TerminalManager, TerminalOutput};
use tempfile::TempDir;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

const TARGET_PORT: u16 = 2201;
const SFTPONLY_PORT: u16 = 2203;
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

macro_rules! require_docker {
    () => {
        if std::env::var("PATCHBAY_DOCKER_TESTS").as_deref() != Ok("1") {
            eprintln!("skipped: set PATCHBAY_DOCKER_TESTS=1 and start docker/test/compose.yml");
            return;
        }
    };
}

/// Local terminals run `$SHELL`; make that a plain, profile-free `/bin/sh`
/// so the tests do not depend on the developer's shell setup.
fn plain_local_shell() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: runs once, before any terminal is spawned by this binary.
        unsafe {
            std::env::set_var("SHELL", "/bin/sh");
            std::env::set_var("ENV", "");
        }
    });
}

async fn timed<T>(fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, fut)
        .await
        .expect("test timed out")
}

struct Harness {
    sessions: SessionManager,
    terminals: TerminalManager,
    events: broadcast::Receiver<CoreEvent>,
    scratch: TempDir,
}

impl Harness {
    fn new() -> Self {
        plain_local_shell();
        let (events_tx, events) = broadcast::channel(1024);
        let (prompt_tx, mut prompt_rx) = mpsc::channel::<PromptRequest>(16);
        tokio::spawn(async move {
            while let Some(req) = prompt_rx.recv().await {
                let answer = match req.prompt {
                    Prompt::HostKeyUnknown { .. } => PromptResponse::Accept,
                    _ => PromptResponse::Cancel,
                };
                let _ = req.reply.send(answer);
            }
        });
        let sessions = SessionManager::new(events_tx.clone(), prompt_tx);
        Self {
            terminals: TerminalManager::new(sessions.clone(), events_tx),
            sessions,
            events,
            scratch: TempDir::new().unwrap(),
        }
    }

    async fn connect(&self, id: &str, port: u16) -> Arc<Connection> {
        let test_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docker/test");
        let known_hosts = self.scratch.path().join("known_hosts");
        if !known_hosts.exists() {
            std::fs::copy(test_dir.join("known_hosts"), &known_hosts).unwrap();
        }
        let target = ConnectTarget {
            host_id: Some(id.to_owned()),
            label: id.trim_start_matches("test:").to_owned(),
            hostname: "127.0.0.1".to_owned(),
            port,
            user: "patchbay".to_owned(),
            identity_files: vec![test_dir.join("keys/id_ed25519")],
            identities_only: true,
            agent: AgentSource::Disabled,
            host_key_alias: None,
            known_hosts_files: vec![known_hosts],
            connect_timeout: Some(Duration::from_secs(10)),
            server_alive_interval: None,
            jumps: Vec::new(),
            auth_methods: Default::default(),
        };
        timed(self.sessions.connect(target)).await.unwrap()
    }

    async fn open(&self, host: Option<&str>, cwd: Option<&str>) -> Term {
        let (info, out) = timed(self.terminals.open(TerminalSpec {
            host_id: host.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
            cols: 80,
            rows: 24,
        }))
        .await
        .unwrap();
        Term {
            info,
            out: Some(out),
            seen: Vec::new(),
        }
    }

    async fn open_err(&self, host: Option<&str>, cwd: Option<&str>) -> Error {
        match timed(self.terminals.open(TerminalSpec {
            host_id: host.map(str::to_owned),
            cwd: cwd.map(str::to_owned),
            cols: 80,
            rows: 24,
        }))
        .await
        {
            Ok((info, _)) => panic!("opened {info:?}"),
            Err(e) => e,
        }
    }

    fn write(&self, term: &Term, text: &str) {
        self.terminals
            .write(term.info.id, text.as_bytes().to_vec())
            .unwrap();
    }

    /// The `TerminalExit` event of `id`.
    async fn exit(&mut self, id: Uuid) -> (Option<i32>, Option<String>) {
        timed(async {
            loop {
                match self.events.recv().await {
                    Ok(CoreEvent::TerminalExit {
                        terminal_id,
                        exit_code,
                        reason,
                    }) if terminal_id == id => return (exit_code, reason),
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => panic!("events: {e}"),
                }
            }
        })
        .await
    }

    fn no_exit_yet(&mut self, id: Uuid) {
        while let Ok(event) = self.events.try_recv() {
            assert!(
                !matches!(event, CoreEvent::TerminalExit { terminal_id, .. } if terminal_id == id),
                "exit event before the output was dropped"
            );
        }
    }
}

/// An open terminal and everything it printed so far.
struct Term {
    info: TerminalInfo,
    out: Option<TerminalOutput>,
    seen: Vec<u8>,
}

impl Term {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// Read until `find` matches the output so far.
    async fn expect_with<T>(&mut self, mut find: impl FnMut(&str) -> Option<T>) -> T {
        let out = self.out.as_mut().expect("output dropped");
        let found = tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                if let Some(found) = find(&String::from_utf8_lossy(&self.seen)) {
                    return Some(found);
                }
                self.seen.extend_from_slice(&out.recv().await?);
            }
        })
        .await;
        match found {
            Ok(Some(found)) => found,
            _ => {
                let tail: String = self.text().chars().rev().take(4000).collect();
                let tail: String = tail.chars().rev().collect();
                panic!("expected output not found in {tail:?}")
            }
        }
    }

    /// Read until the output contains `lines` as consecutive lines (ignoring
    /// escape sequences; the first one may follow a prompt ending in `$ `).
    async fn expect_lines(&mut self, lines: &[&str]) -> String {
        self.expect_with(|text| {
            let text = plain(text);
            let all: Vec<&str> = text.split('\n').collect();
            all.windows(lines.len())
                .any(|w| {
                    (w[0] == lines[0] || w[0].ends_with(&format!("$ {}", lines[0])))
                        && w[1..] == lines[1..]
                })
                .then(|| text.clone())
        })
        .await
    }

    /// Read until the output (since the start) contains `needle`.
    async fn expect(&mut self, needle: &str) -> String {
        self.expect_with(|text| text.contains(needle).then(|| text.to_owned()))
            .await
    }

    /// Read until the terminal ends, then drop the output.
    async fn drain(&mut self) -> String {
        let mut out = self.out.take().expect("output dropped");
        timed(async {
            while let Some(chunk) = out.recv().await {
                self.seen.extend_from_slice(&chunk);
            }
        })
        .await;
        self.text()
    }

    /// `$$` of the shell.
    async fn shell_pid(&mut self, h: &Harness) -> i32 {
        h.write(self, "echo \"pid:$$:\"\n");
        self.expect_with(|text| {
            let (_, rest) = text.rsplit_once("pid:")?;
            rest.split_once(':')?.0.parse().ok()
        })
        .await
    }
}

/// `text` without CSI / OSC escape sequences and carriage returns.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            c => out.push(c),
        }
    }
    out
}

fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Local
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn local_echo_and_exit_code() {
    let mut h = Harness::new();
    let mut t = h.open(None, None).await;
    assert_eq!(t.info.host_id, None);
    assert_eq!(t.info.shell, "sh");
    h.write(&t, "echo hi; exit 3\n");
    let text = plain(&t.drain().await);
    assert!(
        text.lines().any(|l| l == "hi" || l.ends_with("$ hi")),
        "{text:?}"
    );
    assert!(h.terminals.write(t.info.id, b"x".to_vec()).is_err());
    assert!(h.terminals.list().is_empty());
    assert_eq!(h.exit(t.info.id).await, (Some(3), None));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_exit_waits_for_the_output_to_be_dropped() {
    let mut h = Harness::new();
    let mut t = h.open(None, None).await;
    h.write(&t, "exit 0\n");
    let mut out = t.out.take().unwrap();
    while timed(out.recv()).await.is_some() {}
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.no_exit_yet(t.info.id);
    drop(out);
    assert_eq!(h.exit(t.info.id).await, (Some(0), None));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_resize() {
    let h = Harness::new();
    let mut t = h.open(None, None).await;
    h.write(&t, "stty size\n");
    t.expect_lines(&["24 80"]).await;
    h.terminals.resize(t.info.id, 132, 43).unwrap();
    h.write(&t, "stty size\n");
    t.expect_lines(&["43 132"]).await;
    h.terminals.close(t.info.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn local_cwd() {
    let h = Harness::new();
    let dir = h.scratch.path().join("dir with spaces ünïcödé");
    std::fs::create_dir(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let mut t = h.open(None, Some(dir.to_str().unwrap())).await;
    h.write(&t, "pwd\n");
    t.expect_lines(&[dir.to_str().unwrap()]).await;
    h.write(&t, "echo \"$TERM $COLORTERM $TERM_PROGRAM\"\n");
    t.expect_lines(&["xterm-256color truecolor Patchbay"]).await;
    h.terminals.close(t.info.id).await;

    let missing = dir.join("missing");
    assert!(matches!(
        h.open_err(None, Some(missing.to_str().unwrap())).await,
        Error::NotFound(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_close_hangs_up_the_shell() {
    let mut h = Harness::new();
    let mut t = h.open(None, None).await;
    let pid = t.shell_pid(&h).await;
    assert!(process_alive(pid));
    // A foreground job must not keep the terminal alive.
    h.write(&t, "sleep 1000\n");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    timed(h.terminals.close(t.info.id)).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "hangup was enough"
    );
    assert!(!process_alive(pid));
    assert!(h.terminals.list().is_empty());
    t.drain().await;
    let (code, reason) = h.exit(t.info.id).await;
    assert_eq!(code, None);
    assert!(reason.unwrap().contains("Hangup"));
    // Closing again is a no-op.
    h.terminals.close(t.info.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn local_dropping_the_output_closes_the_terminal() {
    let mut h = Harness::new();
    let mut t = h.open(None, None).await;
    let pid = t.shell_pid(&h).await;
    drop(t.out.take());
    h.exit(t.info.id).await;
    assert!(!process_alive(pid));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_close_all() {
    let mut h = Harness::new();
    let mut a = h.open(None, None).await;
    let mut b = h.open(None, None).await;
    let (pa, pb) = (a.shell_pid(&h).await, b.shell_pid(&h).await);
    assert_eq!(h.terminals.list().len(), 2);
    timed(h.terminals.close_all()).await;
    assert!(h.terminals.list().is_empty());
    assert!(!process_alive(pa) && !process_alive(pb));
    drop((a.out.take(), b.out.take()));
    h.exit(a.info.id).await;
    h.exit(b.info.id).await;
}

/// Pipes `bytes` of random data through base64 in the shell and checks
/// that all of it arrives.
async fn heavy_output(h: &mut Harness, t: &mut Term, bytes: u64) -> (u64, Duration) {
    h.write(
        t,
        &format!("head -c {bytes} /dev/urandom | base64; exit 0\n"),
    );
    let mut out = t.out.take().unwrap();
    let started = Instant::now();
    let mut total = 0u64;
    let mut chunks = 0u64;
    timed(async {
        while let Some(chunk) = out.recv().await {
            total += chunk.len() as u64;
            chunks += 1;
        }
    })
    .await;
    let elapsed = started.elapsed();
    drop(out);
    // base64: 4 chars per 3 bytes plus a CR LF per 76 chars.
    let expected = bytes.div_ceil(3) * 4;
    let expected = expected + expected.div_ceil(76) * 2;
    assert!(
        total >= expected,
        "got {total} bytes, expected at least {expected}"
    );
    assert_eq!(h.exit(t.info.id).await, (Some(0), None));
    eprintln!(
        "{} MB of output in {chunks} chunks ({:.0} KiB avg) in {elapsed:.2?}: {:.1} MB/s",
        total / 1_000_000,
        total as f64 / chunks as f64 / 1024.0,
        total as f64 / elapsed.as_secs_f64() / 1e6
    );
    (total, elapsed)
}

#[tokio::test(flavor = "multi_thread")]
async fn local_heavy_output_arrives_complete() {
    let mut h = Harness::new();
    let mut t = h.open(None, None).await;
    heavy_output(&mut h, &mut t, 20_000_000).await;
}

/// A consumer that stops reading must not make memory grow: the shell is
/// throttled instead.
async fn stalled_consumer(h: &Harness, host: Option<&str>) {
    let mut t = h.open(host, None).await;
    h.write(&t, "cat /dev/urandom | base64\n");
    t.expect_with(|text| (text.len() > 1 << 20).then_some(()))
        .await;
    let before = rss_kib();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let stalled = rss_kib();
    let mut out = t.out.take().unwrap();
    let started = Instant::now();
    let mut total = 0u64;
    while started.elapsed() < Duration::from_secs(3) {
        total += timed(out.recv()).await.unwrap().len() as u64;
    }
    let after = rss_kib();
    eprintln!(
        "{}: RSS {before} KiB -> {stalled} KiB after a 3 s stall -> {after} KiB after reading \
         {:.1} MB/s for 3 s",
        host.unwrap_or("local"),
        total as f64 / 3e6
    );
    assert!(stalled < before + 16 * 1024, "memory grew while stalled");
    h.terminals.close(t.info.id).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn bench_local_stalled_consumer() {
    stalled_consumer(&Harness::new(), None).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn bench_local_throughput() {
    let mut h = Harness::new();
    let mut t = h.open(None, None).await;
    heavy_output(&mut h, &mut t, 200_000_000).await;
}

// ---------------------------------------------------------------------------
// Remote
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn remote_requires_a_connection() {
    let h = Harness::new();
    assert!(matches!(
        h.open_err(Some("test:not-connected"), None).await,
        Error::NotConnected(_)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_echo_and_exit_code() {
    require_docker!();
    let mut h = Harness::new();
    h.connect("test:term-echo", TARGET_PORT).await;
    let mut t = h.open(Some("test:term-echo"), None).await;
    assert_eq!(t.info.host_id.as_deref(), Some("test:term-echo"));
    assert_eq!(t.info.shell, "bash");
    h.write(&t, "echo \"$TERM\"; shopt -q login_shell && echo login\n");
    t.expect_lines(&["xterm-256color", "login"]).await;
    h.write(&t, "echo hi; exit 3\n");
    let text = plain(&t.drain().await);
    assert!(
        text.lines().any(|l| l == "hi" || l.ends_with("$ hi")),
        "{text:?}"
    );
    assert_eq!(h.exit(t.info.id).await, (Some(3), None));
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_cwd() {
    require_docker!();
    let h = Harness::new();
    let conn = h.connect("test:term-cwd", TARGET_PORT).await;
    let mut t = h
        .open(
            Some("test:term-cwd"),
            Some("/home/patchbay/dir with spaces"),
        )
        .await;
    assert_eq!(t.info.shell, "bash");
    h.write(&t, "pwd; shopt -q login_shell && echo login; ls\n");
    t.expect_lines(&["/home/patchbay/dir with spaces", "login"])
        .await;
    t.expect("inner file.txt").await;
    h.terminals.close(t.info.id).await;

    let odd = format!(
        "/home/patchbay/upload/ünï cødé ✓ $HOME 'q' \"d\" \\ %s {}",
        Uuid::new_v4()
    );
    let quoted = format!("'{}'", odd.replace('\'', r"'\''"));
    assert!(
        conn.exec(&format!("mkdir -p {quoted}"))
            .await
            .unwrap()
            .success()
    );
    let mut t = h.open(Some("test:term-cwd"), Some(&odd)).await;
    h.write(&t, "pwd\n");
    t.expect_lines(&[&odd]).await;
    h.terminals.close(t.info.id).await;
    conn.exec(&format!("rmdir {quoted}")).await.unwrap();

    // A folder that cannot be entered falls back to home.
    let mut t = h.open(Some("test:term-cwd"), Some("/does/not/exist")).await;
    h.write(&t, "pwd\n");
    t.expect_lines(&["/home/patchbay"]).await;
    h.terminals.close(t.info.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_resize() {
    require_docker!();
    let h = Harness::new();
    h.connect("test:term-resize", TARGET_PORT).await;
    let mut t = h.open(Some("test:term-resize"), None).await;
    h.write(&t, "stty size\n");
    t.expect_lines(&["24 80"]).await;
    h.terminals.resize(t.info.id, 132, 43).unwrap();
    h.write(&t, "stty size\n");
    t.expect_lines(&["43 132"]).await;
    h.terminals.close(t.info.id).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_sftp_only_is_unsupported() {
    require_docker!();
    let h = Harness::new();
    h.connect("test:term-sftponly", SFTPONLY_PORT).await;
    for cwd in [None, Some("/pub")] {
        let err = h.open_err(Some("test:term-sftponly"), cwd).await;
        assert!(matches!(err, Error::Unsupported(_)), "{err}");
        assert!(err.to_string().contains("SFTP"), "{err}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_connection_loss() {
    require_docker!();
    let mut h = Harness::new();
    h.connect("test:term-loss", TARGET_PORT).await;
    let mut t = h.open(Some("test:term-loss"), None).await;
    h.write(&t, "echo before; sleep 0.5; kill -9 $PPID\n");
    t.drain().await;
    assert!(t.text().contains("before\r\n"), "{:?}", t.text());
    let (code, reason) = h.exit(t.info.id).await;
    assert_eq!(code, None);
    let reason = reason.unwrap();
    assert!(
        reason.starts_with("Connection to term-loss lost"),
        "{reason}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_close_leaves_nothing_behind() {
    require_docker!();
    let mut h = Harness::new();
    let conn = h.connect("test:term-close", TARGET_PORT).await;
    let mut t = h.open(Some("test:term-close"), None).await;
    let pid = t.shell_pid(&h).await;
    h.write(&t, "sleep 1000\n");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    timed(h.terminals.close(t.info.id)).await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "closed without escalation"
    );
    assert!(h.terminals.list().is_empty());
    t.drain().await;
    let (code, reason) = h.exit(t.info.id).await;
    assert_eq!(code, None);
    assert_eq!(reason.as_deref(), Some("Terminated by SIGHUP"));

    let check = format!(
        "kill -0 {pid} 2>/dev/null && echo alive || echo gone; pgrep -x sleep -u patchbay -c || true"
    );
    let out = conn.exec(&check).await.unwrap();
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.starts_with("gone\n"), "{out}");

    // Dropping the output closes the terminal too.
    let mut t = h.open(Some("test:term-close"), None).await;
    let pid = t.shell_pid(&h).await;
    drop(t.out.take());
    h.exit(t.info.id).await;
    let out = conn
        .exec(&format!(
            "kill -0 {pid} 2>/dev/null && echo alive || echo gone"
        ))
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "gone\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_heavy_output_arrives_complete() {
    require_docker!();
    let mut h = Harness::new();
    h.connect("test:term-heavy", TARGET_PORT).await;
    let mut t = h.open(Some("test:term-heavy"), None).await;
    heavy_output(&mut h, &mut t, 20_000_000).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn bench_remote_throughput() {
    require_docker!();
    let mut h = Harness::new();
    h.connect("test:term-bench", TARGET_PORT).await;
    let mut t = h.open(Some("test:term-bench"), None).await;
    heavy_output(&mut h, &mut t, 100_000_000).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark"]
async fn bench_remote_stalled_consumer() {
    require_docker!();
    let h = Harness::new();
    h.connect("test:term-stall", TARGET_PORT).await;
    stalled_consumer(&h, Some("test:term-stall")).await;
}

/// Against a Windows OpenSSH server, e.g.
/// `PATCHBAY_WINDOWS_SSH=agent@127.0.0.1:2222 PATCHBAY_WINDOWS_KEY=~/.ssh/key`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a Windows OpenSSH server"]
async fn remote_windows() {
    let (Ok(spec), Ok(key)) = (
        std::env::var("PATCHBAY_WINDOWS_SSH"),
        std::env::var("PATCHBAY_WINDOWS_KEY"),
    ) else {
        eprintln!("skipped: set PATCHBAY_WINDOWS_SSH and PATCHBAY_WINDOWS_KEY");
        return;
    };
    let (user, addr) = spec.split_once('@').unwrap();
    let (host, port) = addr.split_once(':').unwrap();
    let mut h = Harness::new();
    let target = ConnectTarget {
        host_id: Some("test:win".into()),
        label: "win".into(),
        hostname: host.into(),
        port: port.parse().unwrap(),
        user: user.into(),
        identity_files: vec![key.into()],
        identities_only: true,
        agent: AgentSource::Disabled,
        host_key_alias: None,
        known_hosts_files: vec![h.scratch.path().join("known_hosts")],
        connect_timeout: Some(Duration::from_secs(10)),
        server_alive_interval: None,
        jumps: Vec::new(),
        auth_methods: Default::default(),
    };
    let conn = timed(h.sessions.connect(target)).await.unwrap();
    assert_eq!(
        conn.capabilities().os,
        patchbay_core::model::RemoteOs::Windows
    );

    let mut t = h.open(Some("test:win"), None).await;
    eprintln!("default shell: {}", t.info.shell);
    h.write(&t, "echo \"hi-$(1+2)\"\r");
    t.expect("hi-3").await;
    h.terminals.resize(t.info.id, 132, 43).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    h.write(
        &t,
        "echo \"size-$($Host.UI.RawUI.WindowSize.Width)x$($Host.UI.RawUI.WindowSize.Height)\"\r",
    );
    t.expect("size-132x43").await;
    h.write(&t, "exit 3\r");
    t.drain().await;
    // Win32-OpenSSH reports 0 for PTY sessions whatever the shell returned
    // (so does OpenSSH's own client).
    assert!(h.exit(t.info.id).await.0.is_some());

    let dir = format!(
        r"C:\Users\{user}\Downloads\patchbay it's ü {}",
        Uuid::new_v4()
    );
    let quoted = format!("'{}'", dir.replace('\'', "''"));
    let out = conn
        .exec(&format!(
            "New-Item -ItemType Directory -Path {quoted} | Out-Null; echo done"
        ))
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("done"),
        "{out:?}"
    );
    let sftp_path = format!("/{}", dir.replace('\\', "/"));
    let mut t = h.open(Some("test:win"), Some(&sftp_path)).await;
    h.write(&t, "echo \"cwd=$((Get-Location).Path)=\"\r");
    t.expect(&format!("cwd={dir}=")).await;
    let started = Instant::now();
    timed(h.terminals.close(t.info.id)).await;
    eprintln!("close took {:?}", started.elapsed());
    t.drain().await;
    h.exit(t.info.id).await;
    conn.exec(&format!("Remove-Item -Path {quoted}"))
        .await
        .unwrap();
}
