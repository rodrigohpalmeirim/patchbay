//! An established, authenticated SSH connection and what can be done on it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{Handle, Msg};
use russh::{Channel, ChannelMsg, Disconnect};
use russh_sftp::client::SftpSession;

use super::handler::{ClientHandler, CloseSignal};
use super::remote_forward::{ForwardRoutes, RemoteListener};
use crate::model::{ConnectTarget, RemoteCapabilities, RemoteOs};
use crate::{Error, Result};

/// Handle type of the underlying russh session.
pub type SshHandle = Handle<ClientHandler>;

/// Default bound for [`Connection::exec`].
pub const DEFAULT_EXEC_TIMEOUT: Duration = Duration::from_secs(30);

const DETECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound for global requests (`tcpip-forward` and its cancellation).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of running a command with [`Connection::exec`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    /// Exit status, if the server reported one (it may report a signal instead).
    pub status: Option<u32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// One SSH connection to a host, shared by all of its channels (SFTP,
/// shells, forwards). For ProxyJump targets the jump hosts' sessions are
/// owned here too and live exactly as long as this connection.
pub struct Connection {
    key: String,
    target: ConnectTarget,
    handle: SshHandle,
    /// Jump hop sessions, first hop first.
    hops: Vec<SshHandle>,
    close: Arc<CloseSignal>,
    routes: Arc<ForwardRoutes>,
    capabilities: RemoteCapabilities,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("key", &self.key)
            .field("label", &self.target.label)
            .field("hops", &self.hops.len())
            .field("capabilities", &self.capabilities)
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl Connection {
    pub(crate) async fn new(
        key: String,
        target: ConnectTarget,
        handle: SshHandle,
        hops: Vec<SshHandle>,
        close: Arc<CloseSignal>,
        routes: Arc<ForwardRoutes>,
    ) -> Self {
        let mut conn = Self {
            key,
            target,
            handle,
            hops,
            close,
            routes,
            capabilities: RemoteCapabilities::default(),
        };
        conn.capabilities = conn.detect_capabilities().await;
        conn
    }

    /// The manager key (host id, or label for ad-hoc targets).
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn target(&self) -> &ConnectTarget {
        &self.target
    }

    pub fn capabilities(&self) -> RemoteCapabilities {
        self.capabilities.clone()
    }

    /// The underlying russh handle, for PTYs, forwarding and other channel
    /// work not wrapped here.
    pub fn handle(&self) -> &SshHandle {
        &self.handle
    }

    pub fn is_closed(&self) -> bool {
        self.close.is_closed() || self.handle.is_closed()
    }

    /// Resolves once the connection has gone away (for any reason).
    pub async fn closed(&self) {
        self.close.closed().await
    }

    /// Why the connection ended, if it has and the reason is known.
    pub fn close_reason(&self) -> Option<String> {
        self.close.reason()
    }

    /// True if the connection was ended on purpose with [`close`](Self::close)
    /// (e.g. the user disconnected), rather than lost.
    pub fn closed_locally(&self) -> bool {
        self.close.is_local()
    }

    pub(crate) fn close_signal(&self) -> Arc<CloseSignal> {
        self.close.clone()
    }

    /// Open a new SFTP session on its own channel.
    pub async fn open_sftp(&self) -> Result<SftpSession> {
        let channel = self.open_sftp_channel().await?;
        Ok(SftpSession::new(channel.into_stream()).await?)
    }

    /// Open a channel running the SFTP subsystem, for callers that drive the
    /// protocol themselves (e.g. with `russh_sftp::client::RawSftpSession`).
    pub async fn open_sftp_channel(&self) -> Result<Channel<Msg>> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.request_subsystem(true, "sftp").await?;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => break,
                Some(ChannelMsg::Failure) => {
                    return Err(Error::Other("server refused the SFTP subsystem".into()));
                }
                Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                    return Err(Error::Other("SFTP channel closed by the server".into()));
                }
                Some(_) => {}
            }
        }
        Ok(channel)
    }

    /// Run a command (no PTY, empty stdin) and collect its output, bounded by
    /// [`DEFAULT_EXEC_TIMEOUT`].
    pub async fn exec(&self, command: &str) -> Result<ExecOutput> {
        self.exec_timeout(command, DEFAULT_EXEC_TIMEOUT).await
    }

    pub async fn exec_timeout(&self, command: &str, timeout: Duration) -> Result<ExecOutput> {
        tokio::time::timeout(timeout, self.exec_inner(command))
            .await
            .map_err(|_| Error::Timeout(format!("running `{command}`")))?
    }

    async fn exec_inner(&self, command: &str) -> Result<ExecOutput> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, command).await?;
        channel.eof().await?;
        let mut out = ExecOutput::default();
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => out.stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => out.stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => out.status = Some(exit_status),
                ChannelMsg::Failure => {
                    return Err(Error::Other("server refused to run the command".into()));
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Open a `direct-tcpip` channel to `host:port` as seen from the server.
    /// `originator` is the address of the local peer the traffic comes from.
    pub async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
        originator: SocketAddr,
    ) -> Result<Channel<Msg>> {
        Ok(self
            .handle
            .channel_open_direct_tcpip(
                host,
                port.into(),
                originator.ip().to_string(),
                originator.port().into(),
            )
            .await?)
    }

    /// Ask the server to listen on `address:port` and forward connections to
    /// us (`ssh -R`). `port` 0 lets the server pick one (see
    /// [`RemoteListener::port`]). A refusal (port busy, forwarding disabled)
    /// is reported as [`Error::PermissionDenied`].
    pub async fn forward_remote(&self, address: &str, port: u16) -> Result<RemoteListener> {
        // Route first: the server may send connections before its reply.
        let mut listener = RemoteListener::register(self.routes.clone(), address, port);
        let reply = tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.handle.tcpip_forward(address, port.into()),
        )
        .await
        .map_err(|_| Error::Timeout(format!("asking the server to listen on {address}:{port}")))?;
        match reply {
            Ok(assigned) => {
                if port == 0 {
                    let assigned = u16::try_from(assigned).map_err(|_| {
                        Error::Other(format!("server picked an invalid port {assigned}"))
                    })?;
                    listener.set_port(assigned);
                }
                Ok(listener)
            }
            Err(russh::Error::RequestDenied) => Err(Error::PermissionDenied(format!(
                "the server refused to listen on {}: the port may be in use, or remote \
                 forwarding is disabled (AllowTcpForwarding / GatewayPorts)",
                join_host_port(address, port)
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// Stop a remote forward on the server. Connections already forwarded
    /// are not affected.
    pub async fn cancel_remote(&self, listener: RemoteListener) -> Result<()> {
        let (address, port) = (listener.address().to_owned(), listener.port());
        drop(listener);
        tokio::time::timeout(
            REQUEST_TIMEOUT,
            self.handle.cancel_tcpip_forward(address, port.into()),
        )
        .await
        .map_err(|_| Error::Timeout("cancelling a remote forward".into()))??;
        Ok(())
    }

    /// Politely close the session and all jump hops.
    pub async fn close(&self) {
        self.close.mark_local();
        for handle in std::iter::once(&self.handle).chain(self.hops.iter().rev()) {
            if let Err(e) = handle.disconnect(Disconnect::ByApplication, "", "en").await {
                tracing::debug!("disconnect: {e}");
            }
        }
    }

    /// Probe what the server lets us do. Never fails: anything that does not
    /// work is simply reported as unavailable.
    async fn detect_capabilities(&self) -> RemoteCapabilities {
        let mut caps = RemoteCapabilities::default();

        match tokio::time::timeout(DETECT_TIMEOUT, self.sftp_home()).await {
            Ok(Ok(home)) => {
                caps.sftp = true;
                caps.home = home;
            }
            Ok(Err(e)) => tracing::debug!(host = %self.key, "SFTP unavailable: {e}"),
            Err(_) => tracing::debug!(host = %self.key, "SFTP probe timed out"),
        }

        match self.exec_timeout("uname -s", DETECT_TIMEOUT).await {
            Ok(out) if out.success() => {
                if let Some(os) = parse_uname(&String::from_utf8_lossy(&out.stdout)) {
                    caps.exec = true;
                    caps.os = os;
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(host = %self.key, "exec probe failed: {e}"),
        }

        if !caps.exec {
            match self.exec_timeout("cmd /c ver", DETECT_TIMEOUT).await {
                Ok(out) if String::from_utf8_lossy(&out.stdout).contains("Windows") => {
                    caps.exec = true;
                    caps.os = RemoteOs::Windows;
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(host = %self.key, "Windows exec probe failed: {e}"),
            }
        }

        if caps.home.as_deref().is_some_and(is_windows_sftp_path) {
            caps.os = RemoteOs::Windows;
        }
        tracing::debug!(host = %self.key, ?caps, "detected capabilities");
        caps
    }

    async fn sftp_home(&self) -> Result<Option<String>> {
        let sftp = self.open_sftp().await?;
        let home = sftp.canonicalize(".").await.ok();
        let _ = sftp.close().await;
        Ok(home)
    }
}

/// `host:port`, bracketing IPv6 addresses.
pub(crate) fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Map `uname -s` output to an OS. Empty output (e.g. a forced
/// `internal-sftp` command swallowing our exec) means "no usable shell".
fn parse_uname(output: &str) -> Option<RemoteOs> {
    let name = output.lines().next()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(match name {
        "Linux" => RemoteOs::Linux,
        "Darwin" => RemoteOs::MacOs,
        // A Git-for-Windows / MSYS `uname` on the PATH of a Windows host.
        n if n.starts_with("MINGW") || n.starts_with("MSYS") => RemoteOs::Windows,
        n if n.ends_with("BSD") || n == "DragonFly" => RemoteOs::Bsd,
        _ => RemoteOs::OtherUnix,
    })
}

/// Windows OpenSSH reports SFTP paths like `/C:/Users/me`.
fn is_windows_sftp_path(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uname() {
        assert_eq!(parse_uname("Linux\n"), Some(RemoteOs::Linux));
        assert_eq!(parse_uname("Darwin\n"), Some(RemoteOs::MacOs));
        assert_eq!(parse_uname("FreeBSD\n"), Some(RemoteOs::Bsd));
        assert_eq!(parse_uname("OpenBSD"), Some(RemoteOs::Bsd));
        assert_eq!(parse_uname("SunOS\n"), Some(RemoteOs::OtherUnix));
        assert_eq!(
            parse_uname("MINGW64_NT-10.0-19045\n"),
            Some(RemoteOs::Windows)
        );
        assert_eq!(parse_uname(""), None);
        assert_eq!(parse_uname("\n"), None);
    }

    #[test]
    fn windows_paths() {
        assert!(is_windows_sftp_path("/C:/Users/me"));
        assert!(is_windows_sftp_path("/d:"));
        assert!(!is_windows_sftp_path("/home/me"));
        assert!(!is_windows_sftp_path("/"));
    }
}
