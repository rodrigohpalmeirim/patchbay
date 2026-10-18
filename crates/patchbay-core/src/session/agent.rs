//! Connecting to the user's SSH agent.
//!
//! Unix: `SSH_AUTH_SOCK` or an explicit socket path. Windows: an explicit
//! pipe path, `SSH_AUTH_SOCK` if it names a pipe, the OpenSSH agent pipe
//! `\\.\pipe\openssh-ssh-agent`, then Pageant. An unreachable agent is not an
//! error, it simply contributes no keys.

use russh::keys::agent::client::{AgentClient, AgentStream};

use crate::model::AgentSource;

pub(crate) type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

pub(crate) async fn connect(source: &AgentSource) -> Option<Agent> {
    let agent = match source {
        AgentSource::Disabled => return None,
        AgentSource::Default => connect_default().await,
        AgentSource::Path(path) => connect_path(path).await,
    };
    if agent.is_none() {
        tracing::debug!(?source, "no SSH agent available");
    }
    agent
}

#[cfg(unix)]
async fn connect_default() -> Option<Agent> {
    match AgentClient::connect_env().await {
        Ok(agent) => Some(agent.dynamic()),
        Err(e) => {
            tracing::debug!("SSH agent from SSH_AUTH_SOCK: {e}");
            None
        }
    }
}

#[cfg(unix)]
async fn connect_path(path: &std::path::Path) -> Option<Agent> {
    match AgentClient::connect_uds(path).await {
        Ok(agent) => Some(agent.dynamic()),
        Err(e) => {
            tracing::debug!(path = %path.display(), "SSH agent: {e}");
            None
        }
    }
}

#[cfg(windows)]
const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

#[cfg(windows)]
async fn connect_default() -> Option<Agent> {
    if let Some(sock) = std::env::var_os("SSH_AUTH_SOCK").filter(|s| !s.is_empty())
        && let Some(agent) = connect_path(std::path::Path::new(&sock)).await
    {
        return Some(agent);
    }
    if let Some(agent) = connect_path(std::path::Path::new(OPENSSH_AGENT_PIPE)).await {
        return Some(agent);
    }
    match AgentClient::connect_pageant().await {
        Ok(agent) => Some(agent.dynamic()),
        Err(e) => {
            tracing::debug!("Pageant: {e}");
            None
        }
    }
}

#[cfg(windows)]
async fn connect_path(path: &std::path::Path) -> Option<Agent> {
    match AgentClient::connect_named_pipe(path).await {
        Ok(agent) => Some(agent.dynamic()),
        Err(e) => {
            tracing::debug!(path = %path.display(), "SSH agent pipe: {e}");
            None
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn connect_default() -> Option<Agent> {
    None
}

#[cfg(not(any(unix, windows)))]
async fn connect_path(_path: &std::path::Path) -> Option<Agent> {
    None
}
