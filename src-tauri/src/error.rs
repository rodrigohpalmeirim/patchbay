use serde::Serialize;

/// Error shape sent to the frontend: a machine-readable `kind` plus a
/// human-readable `message`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub kind: &'static str,
    pub message: String,
}

impl From<patchbay_core::Error> for CommandError {
    fn from(err: patchbay_core::Error) -> Self {
        use patchbay_core::Error as E;
        let kind = match &err {
            E::UnknownHost(_) => "unknownHost",
            E::ReadOnlyHost(_) => "readOnlyHost",
            E::HostKeyRejected { .. } => "hostKeyRejected",
            E::AuthFailed { .. } => "authFailed",
            E::UserRequired(_) => "userRequired",
            E::Cancelled => "cancelled",
            E::NotConnected(_) => "notConnected",
            E::Timeout(_) => "timeout",
            E::NotFound(_) => "notFound",
            E::PermissionDenied(_) => "permissionDenied",
            E::AlreadyExists(_) => "alreadyExists",
            E::Unsupported(_) => "unsupported",
            E::Io(io) => match io.kind() {
                std::io::ErrorKind::NotFound => "notFound",
                std::io::ErrorKind::PermissionDenied => "permissionDenied",
                std::io::ErrorKind::AlreadyExists => "alreadyExists",
                _ => "io",
            },
            E::ReadFile { .. } => "io",
            E::Sftp(_) => "sftp",
            E::Ssh(_) | E::Key(_) => "ssh",
            _ => "other",
        };
        Self {
            kind,
            message: err.to_string(),
        }
    }
}

pub type CommandResult<T> = Result<T, CommandError>;
