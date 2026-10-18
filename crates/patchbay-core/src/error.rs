use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("SSH config error at {path}:{line}: {message}")]
    SshConfig {
        path: PathBuf,
        line: usize,
        message: String,
    },

    #[error("unknown host: {0}")]
    UnknownHost(String),

    #[error("host is read-only (defined in ~/.ssh/config): {0}")]
    ReadOnlyHost(String),

    #[error("SSH error: {0}")]
    Ssh(#[from] russh::Error),

    #[error("SSH key error: {0}")]
    Key(#[from] russh::keys::Error),

    #[error("SFTP error: {0}")]
    Sftp(#[from] russh_sftp::client::error::Error),

    #[error("host key verification failed for {host}")]
    HostKeyRejected { host: String },

    #[error("authentication failed for {user}@{host}")]
    AuthFailed { user: String, host: String },

    #[error("cancelled by user")]
    Cancelled,

    #[error("not found: {0}")]
    NotFound(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("already exists: {0}")]
    AlreadyExists(String),

    #[error("not supported: {0}")]
    Unsupported(String),

    #[error("not connected: {0}")]
    NotConnected(String),

    #[error("timed out: {0}")]
    Timeout(String),

    #[error("secret store error: {0}")]
    Keyring(String),

    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}
