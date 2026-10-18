//! Shells on connected hosts: a session channel with a PTY on the host's
//! existing [`Connection`].
//!
//! Without a start directory the server's default shell is requested
//! (`shell`), exactly like `ssh host`. With one ("open terminal here"):
//!
//! - POSIX hosts get an `exec` of a small `/bin/sh` script that changes
//!   into the directory (silently staying in home if that fails) and then
//!   `exec`s the user's `$SHELL` as a login shell (`-l`; `/bin/sh` if
//!   `$SHELL` is unset). The directory is passed as `printf` octal escapes,
//!   so the command survives being parsed by any login shell (sh, bash, zsh,
//!   fish, csh) whatever characters the path contains.
//! - Windows hosts (OpenSSH for Windows, SFTP paths like `/C:/Users/me`)
//!   start the configured `DefaultShell` in that directory: PowerShell /
//!   pwsh via `-NoExit -EncodedCommand Set-Location ...`, anything else via
//!   `cmd /k cd /d "C:\..."`.
//!
//! SFTP-only servers are refused with [`Error::Unsupported`]. Servers with
//! neither SFTP nor a recognizable shell (network gear) still get a plain
//! shell request.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use russh::client::Msg;
use russh::{Channel, ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Pty, Sig};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use super::Size;
use super::driver::{Backend, EVENT_QUEUE, Event, Exit, Started};
use crate::model::{RemoteCapabilities, RemoteOs};
use crate::session::{Connection, SessionManager};
use crate::{Error, Result};

const TERM: &str = "xterm-256color";
/// Bound for opening the channel and getting the shell started.
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound for the shell name probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// PTY modes matching a typical local terminal (xterm.js sends DEL for
/// Backspace; UTF-8 aware line editing).
const PTY_MODES: &[(Pty, u32)] = &[
    (Pty::VINTR, 3),
    (Pty::VQUIT, 28),
    (Pty::VERASE, 127),
    (Pty::VKILL, 21),
    (Pty::VEOF, 4),
    (Pty::VSTART, 17),
    (Pty::VSTOP, 19),
    (Pty::VSUSP, 26),
    (Pty::VREPRINT, 18),
    (Pty::VWERASE, 23),
    (Pty::VLNEXT, 22),
    (Pty::ICRNL, 1),
    (Pty::IUTF8, 1),
    (Pty::ISIG, 1),
    (Pty::ICANON, 1),
    (Pty::IEXTEN, 1),
    (Pty::ECHO, 1),
    (Pty::ECHOE, 1),
    (Pty::ECHOK, 1),
    (Pty::ECHOCTL, 1),
    (Pty::ECHOKE, 1),
    (Pty::OPOST, 1),
    (Pty::ONLCR, 1),
    (Pty::CS8, 1),
    (Pty::TTY_OP_ISPEED, 38400),
    (Pty::TTY_OP_OSPEED, 38400),
];

/// How the shell is started.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch {
    /// Server default shell (`shell` request).
    Shell,
    /// POSIX host, start in this directory.
    PosixDir(String),
    /// Windows host, start in this directory (native form, `C:\...`).
    WindowsDir(String),
}

/// The remote default shell, as far as we could tell.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellProbe {
    /// For display, e.g. `"bash"`.
    name: String,
    /// Windows: PowerShell executable when that is the OpenSSH `DefaultShell`.
    powershell: Option<String>,
}

pub(super) async fn start(
    sessions: &SessionManager,
    host: &str,
    cwd: Option<&str>,
    size: Size,
) -> Result<Started> {
    let conn = sessions
        .get(host)
        .ok_or_else(|| Error::NotConnected(host.to_owned()))?;
    let caps = conn.capabilities();
    let label = conn.target().label.clone();
    let launch = launch(&caps, &label, cwd)?;

    let (probe, channel) = tokio::join!(
        probe_shell(&conn, &caps),
        tokio::time::timeout(OPEN_TIMEOUT, open_pty(&conn, size)),
    );
    let mut channel =
        channel.map_err(|_| Error::Timeout(format!("opening a terminal on {label}")))??;
    let command = match &launch {
        Launch::Shell => None,
        Launch::PosixDir(dir) => Some(posix_cwd_command(dir)),
        Launch::WindowsDir(dir) => Some(windows_cwd_command(dir, probe.powershell.as_deref())),
    };
    let started = tokio::time::timeout(OPEN_TIMEOUT, start_shell(&mut channel, command)).await;
    let initial = match started {
        Ok(Ok(initial)) => initial,
        Ok(Err(e)) => {
            let _ = channel.close().await;
            return Err(e);
        }
        Err(_) => {
            let _ = channel.close().await;
            return Err(Error::Timeout(format!("starting a shell on {label}")));
        }
    };
    tracing::debug!(host, ?launch, shell = %probe.name, "remote shell started");

    let (read, write) = channel.split();
    let write = Arc::new(write);
    let open = Arc::new(AtomicBool::new(true));
    let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
    let (input, input_rx) = mpsc::unbounded_channel();
    let (control, control_rx) = mpsc::unbounded_channel();
    let pump = tokio::spawn(pump(read, events_tx, conn, initial, open.clone()));
    let writer = tokio::spawn(write_loop(write.clone(), input_rx));
    // Ends once the backend (the sender) is gone and queued messages are sent.
    tokio::spawn(control_loop(write.clone(), control_rx));

    Ok(Started {
        shell: probe.name,
        backend: Box::new(RemoteBackend {
            write,
            input,
            control,
            open,
            pump: pump.abort_handle(),
            writer: writer.abort_handle(),
        }),
        events,
    })
}

fn launch(caps: &RemoteCapabilities, label: &str, cwd: Option<&str>) -> Result<Launch> {
    if !caps.exec {
        if caps.sftp {
            return Err(Error::Unsupported(format!(
                "{label} only allows file transfers (SFTP), not terminals"
            )));
        }
        if cwd.is_some() {
            return Err(Error::Unsupported(format!(
                "cannot open a terminal in a folder on {label}: no supported shell"
            )));
        }
        return Ok(Launch::Shell);
    }
    Ok(match cwd {
        None => Launch::Shell,
        Some(dir) if caps.os == RemoteOs::Windows => match windows_path(dir) {
            Some(native) => Launch::WindowsDir(native),
            None => Launch::Shell,
        },
        Some(dir) => Launch::PosixDir(dir.to_owned()),
    })
}

async fn open_pty(conn: &Connection, size: Size) -> Result<Channel<Msg>> {
    let mut channel = conn.handle().channel_open_session().await?;
    let result = async {
        channel
            .request_pty(
                true,
                TERM,
                size.cols.into(),
                size.rows.into(),
                0,
                0,
                PTY_MODES,
            )
            .await?;
        if !wait_reply(&mut channel, &mut Vec::new()).await? {
            return Err(Error::Unsupported(format!(
                "{} refused to allocate a terminal",
                conn.target().label
            )));
        }
        // Most servers only accept LANG and LC_*; refusals are fine.
        for (key, value) in remote_env() {
            channel.set_env(false, key, value).await?;
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(channel),
        Err(e) => {
            let _ = channel.close().await;
            Err(e)
        }
    }
}

/// Request the shell (or run `command`) and wait for the server to accept.
/// Returns output that arrived before the reply.
async fn start_shell(channel: &mut Channel<Msg>, command: Option<String>) -> Result<Vec<u8>> {
    match &command {
        None => channel.request_shell(true).await?,
        Some(command) => channel.exec(true, command.as_bytes()).await?,
    }
    let mut early = Vec::new();
    if wait_reply(channel, &mut early).await? {
        Ok(early)
    } else {
        Err(Error::Unsupported(
            "the server refused to start a shell".into(),
        ))
    }
}

/// Wait for the reply to a request: `true` on success, `false` on refusal.
async fn wait_reply(channel: &mut Channel<Msg>, early: &mut Vec<u8>) -> Result<bool> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(true),
            Some(ChannelMsg::Failure) => return Ok(false),
            Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                early.extend_from_slice(&data);
            }
            Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                return Err(Error::Other(
                    "the server closed the terminal channel".into(),
                ));
            }
            Some(_) => {}
        }
    }
}

/// `LANG` / `LC_*` from here (like OpenSSH's default `SendEnv`), plus
/// colour support.
fn remote_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, v)| (k == "LANG" || k.starts_with("LC_")) && !v.is_empty())
        .collect();
    env.push(("COLORTERM".into(), "truecolor".into()));
    env.push(("TERM_PROGRAM".into(), "Patchbay".into()));
    env
}

/// Best-effort name of the remote shell; never fails.
async fn probe_shell(conn: &Connection, caps: &RemoteCapabilities) -> ShellProbe {
    let fallback = |name: &str| ShellProbe {
        name: name.to_owned(),
        powershell: None,
    };
    if !caps.exec {
        return fallback("shell");
    }
    if caps.os == RemoteOs::Windows {
        let query = r"reg query HKLM\SOFTWARE\OpenSSH /v DefaultShell";
        return match conn.exec_timeout(query, PROBE_TIMEOUT).await {
            Ok(out) if out.success() => {
                windows_shell_probe(parse_reg_value(&String::from_utf8_lossy(&out.stdout)))
            }
            _ => fallback("cmd"),
        };
    }
    match conn.exec_timeout("echo $SHELL", PROBE_TIMEOUT).await {
        Ok(out) => {
            let shell = String::from_utf8_lossy(&out.stdout);
            let name = shell.trim().rsplit('/').next().unwrap_or_default();
            fallback(if name.is_empty() { "sh" } else { name })
        }
        Err(e) => {
            tracing::debug!("shell probe failed: {e}");
            fallback("sh")
        }
    }
}

/// The value of a `reg query /v` output line (`  Name  REG_SZ  value`).
fn parse_reg_value(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (_, rest) = line.split_once("REG_")?;
        let (_, value) = rest.split_once(char::is_whitespace)?;
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn windows_shell_probe(default_shell: Option<String>) -> ShellProbe {
    let Some(path) = default_shell else {
        return ShellProbe {
            name: "cmd".into(),
            powershell: None,
        };
    };
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path);
    let name = file
        .strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file)
        .to_lowercase();
    let powershell = matches!(name.as_str(), "pwsh" | "powershell").then(|| {
        // Unexpanded `%SystemRoot%`-style paths only work by name (both are on PATH).
        if path.contains('%') {
            format!("{name}.exe")
        } else {
            path.clone()
        }
    });
    ShellProbe { name, powershell }
}

/// `/C:/Users/me` (SFTP form) -> `C:\Users\me`. `None` if not a drive path.
fn windows_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix('/').unwrap_or(path);
    let b = rest.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    if b.len() > 2 && b[2] != b'/' && b[2] != b'\\' {
        return None;
    }
    let mut native = rest.replace('/', "\\");
    if native.len() == 2 {
        native.push('\\');
    } else if native.len() > 3 {
        native.truncate(native.trim_end_matches('\\').len());
    }
    Some(native)
}

/// See the module docs. The result contains no `'`, `!` or `\\`, so it can
/// be single-quoted for any login shell.
fn posix_cwd_command(dir: &str) -> String {
    let dir = if dir.starts_with('/') {
        dir.to_owned()
    } else {
        format!("./{dir}")
    };
    let mut format = String::with_capacity(dir.len());
    for b in dir.bytes() {
        if b.is_ascii_alphanumeric() || b"/._-+,:@=".contains(&b) {
            format.push(b as char);
        } else {
            let _ = write!(format, "\\{b:03o}");
        }
    }
    // The `x` keeps `$(...)` from stripping trailing newlines of the path.
    format!(
        "exec /bin/sh -c 'd=\"$(printf \"{format}x\")\"; cd \"${{d%x}}\" 2>/dev/null; exec \"${{SHELL:-/bin/sh}}\" -l'"
    )
}

fn windows_cwd_command(dir: &str, powershell: Option<&str>) -> String {
    match powershell {
        Some(exe) => {
            let script = format!("Set-Location -LiteralPath '{}'", dir.replace('\'', "''"));
            let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            format!(
                "& '{}' -NoLogo -NoExit -EncodedCommand {}",
                exe.replace('\'', "''"),
                data_encoding::BASE64.encode(&utf16)
            )
        }
        None => format!("cmd /k cd /d \"{dir}\""),
    }
}

/// Forwards channel output to the driver, then reports how the shell ended.
async fn pump(
    mut read: ChannelReadHalf,
    events: mpsc::Sender<Event>,
    conn: Arc<Connection>,
    initial: Vec<u8>,
    open: Arc<AtomicBool>,
) {
    if !initial.is_empty() && events.send(Event::Data(initial)).await.is_err() {
        return;
    }
    let mut exit: Option<Exit> = None;
    let mut closed = false;
    while let Some(msg) = read.wait().await {
        match msg {
            ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                if events.send(Event::Data(data.into())).await.is_err() {
                    return;
                }
            }
            ChannelMsg::ExitStatus { exit_status } => {
                exit = Some(Exit {
                    code: Some(exit_status as i32),
                    reason: None,
                });
            }
            ChannelMsg::ExitSignal {
                signal_name,
                error_message,
                ..
            } => {
                let signal = match signal_name {
                    Sig::Custom(name) => name,
                    other => format!("{other:?}"),
                };
                let mut reason = format!("Terminated by SIG{signal}");
                if !error_message.is_empty() {
                    reason = format!("{reason}: {error_message}");
                }
                exit = Some(Exit {
                    code: None,
                    reason: Some(reason),
                });
            }
            ChannelMsg::Close => {
                closed = true;
                break;
            }
            _ => {}
        }
    }
    open.store(false, Ordering::SeqCst);
    let exit = match exit {
        Some(exit) => exit,
        None if !closed => connection_exit(&conn).await,
        None => Exit::default(),
    };
    let _ = events.send(Event::Exit(exit)).await;
}

/// The channel vanished without a close: find out whether the connection
/// went down with it.
async fn connection_exit(conn: &Connection) -> Exit {
    if tokio::time::timeout(Duration::from_secs(1), conn.closed())
        .await
        .is_err()
    {
        return Exit::default();
    }
    let label = &conn.target().label;
    let reason = match conn.close_reason() {
        Some(detail) => format!("Connection to {label} lost: {detail}"),
        None => format!("Connection to {label} lost"),
    };
    Exit {
        code: None,
        reason: Some(reason),
    }
}

async fn write_loop(
    write: Arc<ChannelWriteHalf<Msg>>,
    mut input: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    while let Some(data) = input.recv().await {
        if let Err(e) = write.data_bytes(data).await {
            tracing::debug!("terminal write failed: {e}");
            break;
        }
    }
}

enum Control {
    Resize(Size),
    Hangup,
    Close,
}

/// Window changes, signals and close, in order but independent of
/// (possibly window-blocked) input.
async fn control_loop(
    write: Arc<ChannelWriteHalf<Msg>>,
    mut control: mpsc::UnboundedReceiver<Control>,
) {
    while let Some(c) = control.recv().await {
        let result = match c {
            Control::Resize(size) => {
                write
                    .window_change(size.cols.into(), size.rows.into(), 0, 0)
                    .await
            }
            Control::Hangup => write.signal(Sig::HUP).await,
            Control::Close => write.close().await,
        };
        if let Err(e) = result {
            tracing::debug!("terminal control message failed: {e}");
        }
    }
}

struct RemoteBackend {
    write: Arc<ChannelWriteHalf<Msg>>,
    input: mpsc::UnboundedSender<Vec<u8>>,
    control: mpsc::UnboundedSender<Control>,
    /// Cleared once the channel is closed (by either side) or the
    /// connection died.
    open: Arc<AtomicBool>,
    pump: AbortHandle,
    writer: AbortHandle,
}

impl Backend for RemoteBackend {
    fn write(&mut self, data: Vec<u8>) {
        let _ = self.input.send(data);
    }

    fn resize(&mut self, size: Size) {
        let _ = self.control.send(Control::Resize(size));
    }

    /// A `signal` request: sshd sends SIGHUP to the shell's process group,
    /// the shell exits and the server closes the channel as usual, so all
    /// output and the exit status still arrive. (Closing the channel from
    /// our side instead would make russh drop everything the server sends
    /// afterwards, including its close confirmation.)
    fn hangup(&mut self) {
        let _ = self.control.send(Control::Hangup);
    }

    /// The server ignored the signal (unsupported, e.g. on Windows): close
    /// the channel, which makes sshd hang up the PTY, and stop listening.
    fn kill(&mut self) {
        if self.open.swap(false, Ordering::SeqCst) {
            let _ = self.control.send(Control::Close);
        }
        self.pump.abort();
        self.writer.abort();
    }
}

impl Drop for RemoteBackend {
    fn drop(&mut self) {
        self.pump.abort();
        self.writer.abort();
        if self.open.swap(false, Ordering::SeqCst)
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            let write = self.write.clone();
            rt.spawn(async move {
                let _ = write.close().await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(sftp: bool, exec: bool, os: RemoteOs) -> RemoteCapabilities {
        RemoteCapabilities {
            sftp,
            exec,
            os,
            home: None,
        }
    }

    #[test]
    fn launch_modes() {
        let linux = caps(true, true, RemoteOs::Linux);
        assert_eq!(launch(&linux, "h", None).unwrap(), Launch::Shell);
        assert_eq!(
            launch(&linux, "h", Some("/srv/x y")).unwrap(),
            Launch::PosixDir("/srv/x y".into())
        );
        let windows = caps(true, true, RemoteOs::Windows);
        assert_eq!(
            launch(&windows, "h", Some("/C:/Users/me")).unwrap(),
            Launch::WindowsDir(r"C:\Users\me".into())
        );
        assert_eq!(launch(&windows, "h", Some("/")).unwrap(), Launch::Shell);

        let sftp_only = caps(true, false, RemoteOs::Unknown);
        let err = launch(&sftp_only, "files", None).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(ref m) if m.contains("files")),
            "{err}"
        );
        assert!(launch(&sftp_only, "files", Some("/")).is_err());

        let router = caps(false, false, RemoteOs::Unknown);
        assert_eq!(launch(&router, "r", None).unwrap(), Launch::Shell);
        assert!(matches!(
            launch(&router, "r", Some("/")),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn windows_paths() {
        assert_eq!(
            windows_path("/C:/Users/me").as_deref(),
            Some(r"C:\Users\me")
        );
        assert_eq!(
            windows_path("/C:/Users/me/").as_deref(),
            Some(r"C:\Users\me")
        );
        assert_eq!(windows_path("/d:").as_deref(), Some(r"d:\"));
        assert_eq!(windows_path("/D:/").as_deref(), Some(r"D:\"));
        assert_eq!(windows_path("C:/x y/ü").as_deref(), Some(r"C:\x y\ü"));
        assert_eq!(windows_path("/"), None);
        assert_eq!(windows_path("/home/me"), None);
        assert_eq!(windows_path("/C:foo"), None);
    }

    #[test]
    fn posix_command_is_safe_for_any_login_shell() {
        let cmd = posix_cwd_command("/home/patchbay/dir with spaces/it's $HOME \"q\" `x` \\ ü%\n");
        let inner = cmd
            .strip_prefix("exec /bin/sh -c '")
            .and_then(|c| c.strip_suffix('\''))
            .unwrap();
        assert!(!inner.contains(['\'', '!', '\n']), "{inner}");
        assert!(!inner.contains("\\\\"), "{inner}");
        assert!(inner.contains(r"/home/patchbay/dir\040with\040spaces/it\047s\040\044HOME"));
        assert!(inner.contains(r"\303\274\045\012x"));
        assert!(inner.ends_with(r#"exec "${SHELL:-/bin/sh}" -l"#));

        assert!(posix_cwd_command("-rf").contains(r#"printf "./-rfx""#));
    }

    #[cfg(unix)]
    #[test]
    fn posix_command_enters_the_directory() {
        let base = tempfile::tempdir().unwrap();
        let name = "dir with spaces/it's $HOME \"q\" `x` \\ ü% \n";
        let dir = base.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.canonicalize().unwrap();
        // Replace the final login shell with `pwd` to observe the result.
        let cmd = posix_cwd_command(&canonical.to_string_lossy())
            .replace(r#"exec "${SHELL:-/bin/sh}" -l"#, "pwd; printf x");
        let shells = [
            "/bin/sh",
            "/bin/bash",
            "/bin/dash",
            "/bin/zsh",
            "/usr/bin/fish",
            "/bin/tcsh",
            "/bin/csh",
            "/bin/mksh",
            "/bin/ksh",
        ];
        let mut tried = 0;
        for shell in shells {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(&cmd)
                .output()
                .unwrap();
            let stdout = String::from_utf8(out.stdout).unwrap();
            assert_eq!(
                stdout.strip_suffix("\nx").unwrap_or(&stdout),
                canonical.to_str().unwrap(),
                "{shell}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            tried += 1;
        }
        eprintln!("checked {tried} shells");
    }

    #[test]
    fn windows_commands() {
        assert_eq!(
            windows_cwd_command(r"C:\Program Files\x", None),
            r#"cmd /k cd /d "C:\Program Files\x""#
        );
        let ps = windows_cwd_command(r"C:\it's", Some(r"C:\Program Files\PowerShell\7\pwsh.exe"));
        let (prefix, encoded) = ps.rsplit_once(' ').unwrap();
        assert_eq!(
            prefix,
            r"& 'C:\Program Files\PowerShell\7\pwsh.exe' -NoLogo -NoExit -EncodedCommand"
        );
        let bytes = data_encoding::BASE64.decode(encoded.as_bytes()).unwrap();
        let utf16: Vec<u16> = bytes
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&utf16).unwrap(),
            r"Set-Location -LiteralPath 'C:\it''s'"
        );
    }

    #[test]
    fn windows_default_shell() {
        let out = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\OpenSSH\r\n    DefaultShell    REG_SZ    C:\\Program Files\\PowerShell\\7\\pwsh.exe\r\n\r\n";
        let value = parse_reg_value(out);
        assert_eq!(
            value.as_deref(),
            Some(r"C:\Program Files\PowerShell\7\pwsh.exe")
        );
        let probe = windows_shell_probe(value);
        assert_eq!(probe.name, "pwsh");
        assert_eq!(
            probe.powershell.as_deref(),
            Some(r"C:\Program Files\PowerShell\7\pwsh.exe")
        );

        let probe = windows_shell_probe(Some(
            r"%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe".into(),
        ));
        assert_eq!(probe.name, "powershell");
        assert_eq!(probe.powershell.as_deref(), Some("powershell.exe"));

        let probe = windows_shell_probe(Some(r"C:\Windows\System32\cmd.exe".into()));
        assert_eq!((probe.name.as_str(), probe.powershell), ("cmd", None));
        assert_eq!(windows_shell_probe(None).name, "cmd");
        assert_eq!(parse_reg_value("ERROR: not found"), None);
    }
}
