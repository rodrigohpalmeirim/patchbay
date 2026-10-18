//! Local ConPTY terminals on Windows (the default shell there is pwsh or
//! Windows PowerShell; `cmd` only without either).

#![cfg(windows)]

use std::time::{Duration, Instant};

use patchbay_core::model::{CoreEvent, TerminalSpec};
use patchbay_core::session::SessionManager;
use patchbay_core::terminal::{TerminalManager, TerminalOutput};
use tokio::sync::{broadcast, mpsc};

async fn expect(out: &mut TerminalOutput, seen: &mut Vec<u8>, needle: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !String::from_utf8_lossy(seen).contains(needle) {
            seen.extend(out.recv().await.expect("output ended"));
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{needle:?} not in {:?}", String::from_utf8_lossy(seen)));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_conpty() {
    let (events_tx, mut events) = broadcast::channel(64);
    let (prompts, _) = mpsc::channel(1);
    let terminals =
        TerminalManager::new(SessionManager::new(events_tx.clone(), prompts), events_tx);
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("dir with spaces ü");
    std::fs::create_dir(&cwd).unwrap();

    let (info, mut out) = terminals
        .open(TerminalSpec {
            host_id: None,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    eprintln!("shell: {}", info.shell);
    assert!(
        info.shell == "pwsh" || info.shell == "powershell",
        "{}",
        info.shell
    );
    let mut seen = Vec::new();
    let write = |s: &str| terminals.write(info.id, s.as_bytes().to_vec()).unwrap();
    // ConPTY asks for the cursor position before starting (xterm.js answers
    // this by itself).
    expect(&mut out, &mut seen, "\x1b[6n").await;
    write("\x1b[1;1R");

    write("echo \"hi-$(1+2) $env:TERM_PROGRAM\"\r");
    expect(&mut out, &mut seen, "hi-3 Patchbay").await;
    write("echo \"cwd=$((Get-Location).Path)=\"\r");
    // PowerShell prints long names; %TEMP% may be an 8.3 path (RUNNER~1).
    let long = std::fs::canonicalize(&cwd).unwrap();
    let long = long.to_string_lossy();
    let long = long.strip_prefix(r"\\?\").unwrap_or(&long);
    expect(&mut out, &mut seen, &format!("cwd={long}=")).await;
    terminals.resize(info.id, 132, 43).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    write("echo \"size-$($Host.UI.RawUI.WindowSize.Width)x$($Host.UI.RawUI.WindowSize.Height)\"\r");
    expect(&mut out, &mut seen, "size-132x43").await;

    // Output written right before the shell exits must not be lost when
    // the pseudoconsole is closed. (Formatted objects followed by `exit` on
    // the same line are never printed by PowerShell itself, so use strings.)
    let before_exit = seen.len();
    write("1..30 | % { \"bye-$_\" }; exit 7\r");
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(chunk) = out.recv().await {
            seen.extend(chunk);
        }
    })
    .await
    .unwrap();
    eprintln!("output ended {:?} after exit", started.elapsed());
    let tail = String::from_utf8_lossy(&seen[before_exit..]);
    assert!(tail.contains("bye-30"), "last output lost: {tail:?}");
    drop(out);
    let exit = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(CoreEvent::TerminalExit {
                terminal_id,
                exit_code,
                ..
            }) = events.recv().await
                && terminal_id == info.id
            {
                return exit_code;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(exit, Some(7));

    // Closing a running shell.
    let (info, out) = terminals
        .open(TerminalSpec {
            host_id: None,
            cwd: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    let mut out = out;
    expect(&mut out, &mut Vec::new(), "\x1b[6n").await;
    terminals.write(info.id, b"\x1b[1;1R".to_vec()).unwrap();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let started = Instant::now();
    terminals.close(info.id).await;
    eprintln!("close took {:?}", started.elapsed());
    assert!(terminals.list().is_empty());
    drop(out);
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

/// Ctrl+C reaches console programs in the shell even if Patchbay itself was
/// started with Ctrl+C ignored (inherited, e.g. from a launcher that used
/// `CREATE_NEW_PROCESS_GROUP`).
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_interrupts_console_programs() {
    // SAFETY: plain Win32 call; makes this process (and children) ignore Ctrl+C.
    assert_ne!(unsafe { SetConsoleCtrlHandler(None, 1) }, 0);
    let (events_tx, _events) = broadcast::channel(64);
    let (prompts, _) = mpsc::channel(1);
    let terminals =
        TerminalManager::new(SessionManager::new(events_tx.clone(), prompts), events_tx);
    let (info, mut out) = terminals
        .open(TerminalSpec {
            host_id: None,
            cwd: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    let write = |s: &str| terminals.write(info.id, s.as_bytes().to_vec()).unwrap();
    expect(&mut out, &mut seen, "\x1b[6n").await;
    write("\x1b[1;1R");
    write("ping -n 60 127.0.0.1\r");
    expect(&mut out, &mut seen, "Reply from 127.0.0.1").await;
    write("\x03");
    // ping prints its statistics when interrupted.
    expect(&mut out, &mut seen, "Control-C").await;
    terminals.close(info.id).await;
}
