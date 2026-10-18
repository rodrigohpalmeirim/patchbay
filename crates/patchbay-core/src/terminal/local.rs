//! Shells on this machine via `portable-pty` (Unix PTYs, Windows ConPTY).
//!
//! Unix runs the user's shell (`$SHELL`, else the passwd entry, else
//! `/bin/sh`) as a login shell (`argv[0]` prefixed with `-`), which matters
//! on macOS where GUI apps do not inherit the login environment. Windows
//! prefers `pwsh.exe`, then `powershell.exe`, then `%COMSPEC%`.
//!
//! Blocking PTY I/O runs on dedicated threads: a reader feeding the driver
//! (blocking when the driver's queue is full, which throttles the shell), a
//! writer for input, and a waiter for the exit status.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

use super::Size;
use super::driver::{Backend, EVENT_QUEUE, Event, Exit, Started};
use crate::{Error, Result};

const READ_BUF: usize = 64 * 1024;

pub(super) async fn start(cwd: Option<&str>, size: Size) -> Result<Started> {
    let cwd = cwd.map(str::to_owned);
    tokio::task::spawn_blocking(move || spawn(cwd, size))
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

fn pty_error(e: impl std::fmt::Display) -> Error {
    Error::Other(format!("could not start a local terminal: {e:#}"))
}

fn pty_size(size: Size) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn spawn(cwd: Option<String>, size: Size) -> Result<Started> {
    let dir = working_dir(cwd.as_deref())?;
    let (mut cmd, shell) = shell_command();
    cmd.cwd(&dir);
    for (key, value) in env_overrides(std::env::vars_os()) {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        }
    }

    let pair = native_pty_system()
        .openpty(pty_size(size))
        .map_err(pty_error)?;
    #[cfg(windows)]
    enable_ctrl_c();
    let mut child = pair.slave.spawn_command(cmd).map_err(pty_error)?;
    // Only the child may hold the slave side, or reads never see EOF.
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let pid = child.process_id();

    let setup = (|| -> Result<_> {
        let reader = pair.master.try_clone_reader().map_err(pty_error)?;
        let writer = pair.master.take_writer().map_err(pty_error)?;
        let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
        let (input, input_rx) = std::sync::mpsc::channel();
        let exited = Arc::new(AtomicBool::new(false));

        thread("patchbay-pty-read", {
            let events = events_tx.clone();
            move || read_loop(reader, events)
        })?;
        thread("patchbay-pty-write", move || write_loop(writer, input_rx))?;
        thread("patchbay-pty-wait", {
            let exited = exited.clone();
            move || {
                let exit = match child.wait() {
                    Ok(status) => exit_of(&status),
                    Err(e) => Exit {
                        code: None,
                        reason: Some(format!("could not wait for the shell: {e}")),
                    },
                };
                exited.store(true, Ordering::SeqCst);
                let _ = events_tx.blocking_send(Event::Exit(exit));
            }
        })?;
        Ok((events, input, exited))
    })();
    let (events, input, exited) = match setup {
        Ok(parts) => parts,
        Err(e) => {
            let _ = killer.kill();
            return Err(e);
        }
    };

    tracing::debug!(shell = %shell, dir = %dir.display(), ?pid, "local shell started");
    Ok(Started {
        shell,
        backend: Box::new(LocalBackend {
            master: Some(pair.master),
            input,
            killer,
            #[cfg(unix)]
            pid,
            exited,
        }),
        events,
    })
}

fn thread(name: &str, f: impl FnOnce() + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(f)
        .map(drop)
        .map_err(Error::from)
}

fn read_loop(mut reader: Box<dyn Read + Send>, events: mpsc::Sender<Event>) {
    let mut buf = vec![0u8; READ_BUF];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if events
                    .blocking_send(Event::Data(buf[..n].to_vec()))
                    .is_err()
                {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            // Linux reports EIO once the slave side is closed.
            Err(e) => {
                tracing::trace!("pty read ended: {e}");
                break;
            }
        }
    }
}

fn write_loop(mut writer: Box<dyn Write + Send>, input: std::sync::mpsc::Receiver<Vec<u8>>) {
    while let Ok(data) = input.recv() {
        if let Err(e) = writer.write_all(&data).and_then(|()| writer.flush()) {
            tracing::debug!("pty write failed: {e}");
            break;
        }
    }
}

fn exit_of(status: &portable_pty::ExitStatus) -> Exit {
    match status.signal() {
        Some(signal) => Exit {
            code: None,
            reason: Some(format!("Terminated by {signal}")),
        },
        None => Exit {
            code: Some(status.exit_code() as i32),
            reason: None,
        },
    }
}

struct LocalBackend {
    /// `None` once released (dropping it closes the PTY / pseudoconsole).
    master: Option<Box<dyn MasterPty + Send>>,
    input: std::sync::mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    #[cfg(unix)]
    pid: Option<u32>,
    exited: Arc<AtomicBool>,
}

impl LocalBackend {
    /// Closing a ConPTY blocks until its output is drained, so never do it
    /// on an async worker.
    fn release_master(&mut self) {
        if let Some(master) = self.master.take() {
            if cfg!(windows) {
                let _ = std::thread::Builder::new()
                    .name("patchbay-pty-close".into())
                    .spawn(move || drop(master));
            } else {
                drop(master);
            }
        }
    }

    #[cfg(unix)]
    fn signal(&self, signal: libc::c_int) {
        if let Some(pid) = self.pid.and_then(|p| libc::pid_t::try_from(p).ok()) {
            // SAFETY: plain syscall on a pid we spawned.
            unsafe { libc::kill(pid, signal) };
        }
    }
}

impl Backend for LocalBackend {
    fn write(&mut self, data: Vec<u8>) {
        let _ = self.input.send(data);
    }

    fn resize(&mut self, size: Size) {
        if let Some(master) = &self.master
            && let Err(e) = master.resize(pty_size(size))
        {
            tracing::debug!("pty resize failed: {e:#}");
        }
    }

    fn hangup(&mut self) {
        // SIGHUP on Unix (also to the foreground job, e.g. an editor),
        // TerminateProcess on Windows.
        #[cfg(unix)]
        if let Some(pgrp) = self.master.as_ref().and_then(|m| m.process_group_leader())
            && pgrp > 0
            && Some(pgrp as u32) != self.pid
        {
            // SAFETY: plain syscall; a stale process group is harmless.
            unsafe { libc::killpg(pgrp, libc::SIGHUP) };
        }
        if let Err(e) = self.killer.kill() {
            tracing::debug!("hanging up the shell failed: {e}");
        }
    }

    fn kill(&mut self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
        #[cfg(not(unix))]
        let _ = self.killer.kill();
        self.release_master();
    }

    fn exited(&mut self) {
        // A ConPTY keeps its output pipe open until it is closed.
        if cfg!(windows) {
            self.release_master();
        }
    }
}

impl Drop for LocalBackend {
    fn drop(&mut self) {
        if !self.exited.load(Ordering::SeqCst) {
            self.kill();
        }
        self.release_master();
    }
}

/// The directory to start in: `cwd` if given (must exist), else home.
fn working_dir(cwd: Option<&str>) -> Result<PathBuf> {
    match cwd {
        Some(cwd) => {
            let dir = PathBuf::from(cwd);
            if dir.is_dir() {
                Ok(dir)
            } else {
                Err(Error::NotFound(cwd.to_owned()))
            }
        }
        None => dirs::home_dir()
            .filter(|h| h.is_dir())
            .ok_or_else(|| Error::Other("cannot determine the home directory".into())),
    }
}

/// The shell to run and its display name.
#[cfg(unix)]
fn shell_command() -> (CommandBuilder, String) {
    // The default program is the user's shell, started as a login shell.
    let cmd = CommandBuilder::new_default_prog();
    let shell = cmd.get_shell();
    let name = shell.rsplit('/').next().unwrap_or(&shell).to_owned();
    (cmd, name)
}

#[cfg(windows)]
fn shell_command() -> (CommandBuilder, String) {
    for (exe, name) in [("pwsh.exe", "pwsh"), ("powershell.exe", "powershell")] {
        if let Some(path) = find_in_path(exe) {
            let mut cmd = CommandBuilder::new(path);
            cmd.arg("-NoLogo");
            return (cmd, name.to_owned());
        }
    }
    let comspec = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
    (CommandBuilder::new(comspec), "cmd".to_owned())
}

/// Undo an inherited "ignore Ctrl+C" (e.g. Patchbay started by a launcher that
/// used `CREATE_NEW_PROCESS_GROUP`): shells inherit it, and then Ctrl+C in the
/// terminal no longer interrupts console programs.
#[cfg(windows)]
fn enable_ctrl_c() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    // SAFETY: plain Win32 call without a handler; FALSE restores normal
    // Ctrl+C processing for this process and the children it creates.
    if unsafe { SetConsoleCtrlHandler(None, 0) } == 0 {
        tracing::debug!(
            "could not enable Ctrl+C handling: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// `name` in a `PATH` directory. App execution aliases (e.g. the Store's
/// `pwsh.exe`) are reparse points that cannot be stat'ed through, so only
/// the entry itself is checked.
#[cfg(windows)]
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir()))
}

/// Environment changes for the shell, given the app's environment:
/// terminal identification, a UTF-8 locale on macOS (Finder-launched apps
/// have none), and none of an AppImage's private variables.
fn env_overrides(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(String, Option<String>)> {
    let vars: Vec<(String, String)> = vars
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    let get = |key: &str| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let mut out: Vec<(String, Option<String>)> = Vec::new();

    if let Some(appdir) = get("APPDIR").filter(|d| d.len() > 1) {
        let appdir = appdir.trim_end_matches('/');
        for (key, value) in &vars {
            if matches!(key.as_str(), "APPDIR" | "APPIMAGE" | "ARGV0" | "OWD") {
                out.push((key.clone(), None));
            } else if value.contains(appdir) {
                let kept: Vec<&str> = value
                    .split(':')
                    .filter(|part| !part.is_empty() && !part.starts_with(appdir))
                    .collect();
                let kept = (!kept.is_empty()).then(|| kept.join(":"));
                out.push((key.clone(), kept));
            }
        }
    }

    if cfg!(target_os = "macos")
        && ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .all(|k| get(k).is_none_or(str::is_empty))
    {
        out.push(("LANG".into(), Some("en_US.UTF-8".into())));
    }

    for (key, value) in [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "Patchbay"),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
    ] {
        out.push((key.into(), Some(value.into())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides(vars: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
        env_overrides(vars.iter().map(|(k, v)| ((*k).into(), (*v).into())))
    }

    #[test]
    fn terminal_identification() {
        let out = overrides(&[("TERM", "dumb"), ("PATH", "/usr/bin")]);
        assert!(out.contains(&("TERM".into(), Some("xterm-256color".into()))));
        assert!(out.contains(&("COLORTERM".into(), Some("truecolor".into()))));
        assert!(out.contains(&("TERM_PROGRAM".into(), Some("Patchbay".into()))));
        assert!(!out.iter().any(|(k, _)| k == "PATH"));
    }

    #[test]
    fn appimage_variables_are_scrubbed() {
        let out = overrides(&[
            ("APPDIR", "/tmp/.mount_patchbayX/"),
            ("APPIMAGE", "/home/me/Patchbay.AppImage"),
            ("OWD", "/home/me"),
            (
                "XDG_DATA_DIRS",
                "/tmp/.mount_patchbayX/usr/share:/usr/local/share:/usr/share",
            ),
            ("GTK_PATH", "/tmp/.mount_patchbayX/usr/lib/gtk-3.0"),
            ("HOME", "/home/me"),
        ]);
        let get = |key: &str| out.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(get("APPDIR"), Some(None));
        assert_eq!(get("APPIMAGE"), Some(None));
        assert_eq!(get("OWD"), Some(None));
        assert_eq!(
            get("XDG_DATA_DIRS"),
            Some(Some("/usr/local/share:/usr/share".into()))
        );
        assert_eq!(get("GTK_PATH"), Some(None));
        assert_eq!(get("HOME"), None);
    }

    #[test]
    fn working_dir_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        assert_eq!(working_dir(Some(&path)).unwrap(), dir.path());
        let missing = dir.path().join("missing").to_string_lossy().into_owned();
        assert!(matches!(
            working_dir(Some(&missing)),
            Err(Error::NotFound(_))
        ));
        assert!(working_dir(None).unwrap().is_dir());
    }
}
