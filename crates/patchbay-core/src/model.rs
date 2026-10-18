//! Types shared between the core and its frontends. Everything here is
//! serializable because it crosses the Tauri IPC boundary as-is
//! (camelCase on the wire, mirrored in `ui/lib/types.ts`).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Stable host identifier. `cfg:<alias>` for hosts from `~/.ssh/config`,
/// `app:<uuid>` for hosts managed inside Patchbay.
pub type HostId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostSource {
    SshConfig,
    App,
}

/// A host as shown in the sidebar. For `SshConfig` hosts the fields are the
/// *effective* values after applying all matching `Host` blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub id: HostId,
    /// Display name. For ssh_config hosts this is the `Host` alias.
    pub name: String,
    pub source: HostSource,
    pub hostname: String,
    pub port: u16,
    pub user: Option<String>,
    #[serde(default)]
    pub identity_files: Vec<PathBuf>,
    /// Raw ProxyJump spec, e.g. `"bastion"` or `"user@a:2222,b"`.
    pub proxy_jump: Option<String>,
    pub group: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Fields the user may set when creating or editing an app-managed host.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDraft {
    pub name: String,
    pub hostname: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    #[serde(default)]
    pub identity_files: Vec<PathBuf>,
    pub proxy_jump: Option<String>,
    pub group: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Where to get agent identities from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "path")]
pub enum AgentSource {
    /// `SSH_AUTH_SOCK` on Unix; OpenSSH named pipe, then Pageant, on Windows.
    Default,
    /// Explicit socket / pipe path (`IdentityAgent`).
    Path(PathBuf),
    /// `IdentityAgent none`.
    Disabled,
}

/// Everything needed to open one SSH connection, fully resolved
/// (ssh_config applied, defaults filled in, jump chain expanded).
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectTarget {
    /// Host this target was resolved from, if any (jump hosts given as
    /// `user@host:port` literals have none).
    pub host_id: Option<HostId>,
    /// Label for UI / logs, e.g. the alias.
    pub label: String,
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub identity_files: Vec<PathBuf>,
    pub identities_only: bool,
    pub agent: AgentSource,
    /// Name to look up / record in known_hosts instead of `hostname`.
    pub host_key_alias: Option<String>,
    pub known_hosts_files: Vec<PathBuf>,
    pub connect_timeout: Option<Duration>,
    pub server_alive_interval: Option<Duration>,
    /// Hops to traverse before reaching this host, first hop first.
    pub jumps: Vec<ConnectTarget>,
    /// Authentication methods the client may use (`PubkeyAuthentication`,
    /// `KbdInteractiveAuthentication`, `PasswordAuthentication`,
    /// `PreferredAuthentications`).
    pub auth_methods: AuthMethods,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthMethods {
    pub publickey: bool,
    pub keyboard_interactive: bool,
    pub password: bool,
}

impl Default for AuthMethods {
    fn default() -> Self {
        Self {
            publickey: true,
            keyboard_interactive: true,
            password: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Interactive prompts: the core asks, the frontend answers.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum Prompt {
    /// Host key not present in known_hosts. Accepting records it.
    HostKeyUnknown {
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
    },
    /// Host key differs from the recorded one. Possible MITM.
    HostKeyChanged {
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        known_hosts_file: PathBuf,
        line: usize,
    },
    Password {
        user: String,
        host: String,
        /// True if a previous attempt (or a stored password) failed.
        retry: bool,
    },
    KeyPassphrase {
        key_path: PathBuf,
        retry: bool,
    },
    KeyboardInteractive {
        user: String,
        host: String,
        name: String,
        instructions: String,
        prompts: Vec<KbdPrompt>,
    },
    /// A transfer is about to write over an existing file.
    FileConflict {
        transfer_id: uuid::Uuid,
        /// Destination path that already exists.
        path: String,
        source: FileStat,
        existing: FileStat,
        /// A partial copy left by an interrupted transfer can be continued.
        can_resume: bool,
    },
    /// A file being edited locally changed on the server since Patchbay last
    /// downloaded or uploaded it, so saving would overwrite someone else's
    /// changes. Answered with [`PromptResponse::EditConflict`]; any other
    /// answer leaves the session in [`EditState::Conflict`] until it is
    /// retried.
    EditConflict {
        session_id: uuid::Uuid,
        remote_path: String,
        /// The server's current version.
        remote: FileStat,
        /// The local copy that was saved.
        local: FileStat,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbdPrompt {
    pub text: String,
    pub echo: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum PromptResponse {
    /// Host key prompts.
    Accept,
    Reject,
    /// Password / passphrase. `remember` stores it in the OS keychain.
    Secret {
        value: String,
        remember: bool,
    },
    /// Keyboard-interactive answers, one per prompt.
    Answers {
        answers: Vec<String>,
    },
    /// Answer to `Prompt::FileConflict`.
    Conflict {
        resolution: ConflictResolution,
        /// Apply to every further conflict of the same transfer.
        apply_to_all: bool,
    },
    /// Answer to `Prompt::EditConflict`.
    EditConflict {
        resolution: EditConflictResolution,
    },
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictResolution {
    Overwrite,
    Skip,
    /// Keep both: write as `name (1).ext`.
    Rename,
    OverwriteIfNewer,
    /// Continue a partial file (only offered when `can_resume`).
    Resume,
}

/// How to settle a [`Prompt::EditConflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditConflictResolution {
    /// Upload the local copy over the server's version.
    Overwrite,
    /// Throw the local changes away and download the server's version.
    DiscardLocal,
    /// Stop syncing; the local copy is kept where it is.
    KeepLocal,
}

/// A prompt in flight. The frontend bridge receives these on an mpsc channel
/// and must eventually send exactly one response (dropping `reply` == Cancel).
#[derive(Debug)]
pub struct PromptRequest {
    pub id: uuid::Uuid,
    /// Host the connection attempt is for (the final target, even when the
    /// prompt concerns a jump hop).
    pub host_id: Option<HostId>,
    pub prompt: Prompt,
    pub reply: tokio::sync::oneshot::Sender<PromptResponse>,
}

pub type PromptSender = tokio::sync::mpsc::Sender<PromptRequest>;

// ---------------------------------------------------------------------------
// Events pushed to the frontend.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "state"
)]
pub enum ConnectionState {
    Connecting { hop: Option<String> },
    Authenticating,
    Connected { capabilities: RemoteCapabilities },
    Reconnecting { attempt: u32 },
    Disconnected { reason: Option<String> },
}

/// What the remote side lets us do beyond SFTP. Detected after connecting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCapabilities {
    pub sftp: bool,
    /// Can run commands (`exec` channel works).
    pub exec: bool,
    pub os: RemoteOs,
    /// Home directory as reported by SFTP `realpath(".")`.
    pub home: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteOs {
    #[default]
    Unknown,
    Linux,
    MacOs,
    Bsd,
    OtherUnix,
    Windows,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum CoreEvent {
    Connection {
        host_id: HostId,
        #[serde(flatten)]
        state: ConnectionState,
    },
    /// Host list changed (ssh_config edited on disk, app host saved/deleted).
    HostsChanged,
    /// A transfer changed state or made progress (progress is throttled).
    Transfer(TransferInfo),
    /// A batch of search results; `done` marks the last batch.
    Search {
        search_id: uuid::Uuid,
        entries: Vec<FileEntry>,
        done: bool,
        /// Set when the search ended with an error.
        error: Option<String>,
    },
    /// A terminal's shell exited or its connection went away. Output for
    /// the terminal has been fully delivered before this event.
    TerminalExit {
        terminal_id: uuid::Uuid,
        /// Exit status if the shell reported one.
        exit_code: Option<i32>,
        /// Human-readable reason when it didn't exit normally (e.g. connection lost).
        reason: Option<String>,
    },
    /// Snippet list changed.
    SnippetsChanged,
    /// A port forward's runtime status changed (state, connection counts and
    /// traffic; traffic updates are throttled).
    PortForward(PortForwardStatus),
    /// The set of port forward rules changed.
    PortForwardsChanged,
    /// App settings changed (possibly from the tray menu).
    SettingsChanged(AppSettings),
    /// An edit session changed state (`Closed` = it is gone from the list).
    Edit(EditSession),
}

// ---------------------------------------------------------------------------
// Files and transfers.
//
// Paths are strings: remote paths are always POSIX-style as spoken by SFTP
// (Windows OpenSSH servers use `/C:/Users/...`); local paths are native.
// `host_id: None` means the local machine.
// ---------------------------------------------------------------------------

/// A path on some machine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRef {
    pub host_id: Option<HostId>,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    /// Full path, usable with any fs command.
    pub path: String,
    pub kind: FileKind,
    /// Symlinks: where the link points and what it points to (`None` if dangling).
    pub link_target: Option<String>,
    pub target_kind: Option<FileKind>,
    pub size: u64,
    /// Unix seconds.
    pub modified: Option<i64>,
    /// Permission bits (`0o7777` mask), if known.
    pub permissions: Option<u32>,
    /// Owner / group names when known, else numeric ids as strings.
    pub owner: Option<String>,
    pub group: Option<String>,
    pub hidden: bool,
}

/// The bits of metadata shown when resolving a conflict.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub size: u64,
    pub modified: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    /// Canonical path of the listed directory.
    pub path: String,
    /// `None` at a root (`/`, a remote Windows drive, or the local drive
    /// list). A local Windows drive root has `""`, the drive list.
    pub parent: Option<String>,
    pub entries: Vec<FileEntry>,
}

/// Copy `sources` (all on the same machine) into the directory `destination`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRequest {
    pub sources: Vec<FileRef>,
    pub destination: FileRef,
    /// Work the user did not ask for explicitly (preparing a drag-out):
    /// starts at once regardless of the concurrency limit, and is flagged so
    /// the UI can keep it out of sight unless it takes a while.
    #[serde(default)]
    pub background: bool,
    /// Fail after scanning if the sources add up to more than this.
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum TransferState {
    Queued,
    /// Walking source directories to count files and bytes.
    Scanning,
    Running,
    Paused,
    /// Waiting for the user to answer a `FileConflict` prompt.
    WaitingForUser,
    Done,
    Failed {
        message: String,
        /// Retrying continues from partial files.
        resumable: bool,
    },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferInfo {
    pub id: uuid::Uuid,
    /// `"big.bin"` or `"photos and 3 more"`.
    pub label: String,
    /// See [`TransferRequest::background`].
    pub background: bool,
    pub source_host: Option<HostId>,
    pub destination: FileRef,
    #[serde(flatten)]
    pub state: TransferState,
    /// Known once scanning finished.
    pub total_bytes: Option<u64>,
    pub done_bytes: u64,
    pub total_files: Option<u64>,
    pub done_files: u64,
    pub skipped_files: u64,
    pub current_file: Option<String>,
    pub bytes_per_sec: u64,
    /// Unix milliseconds.
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

/// A local copy of remote entries being made for dragging them out of the
/// app (see [`crate::dragout`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DragOut {
    pub id: uuid::Uuid,
    /// The background transfer making the copy.
    pub transfer: TransferInfo,
    /// Local paths to hand to the OS drag, one per source; complete once
    /// the transfer is done.
    pub paths: Vec<String>,
}

/// What an edit session is doing. See [`crate::edit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "state"
)]
pub enum EditState {
    /// Downloading the server's version.
    Syncing,
    /// The local copy matches the server as of `at` (Unix milliseconds).
    Synced { at: i64 },
    /// Uploading a save.
    Uploading,
    /// The last sync failed; it is retried on the next save, on request,
    /// and when the host reconnects.
    Failed { message: String },
    /// Waiting for an answer to [`Prompt::EditConflict`].
    Conflict,
    /// No longer syncing, but the local copy (with changes the server does
    /// not have) is kept at `local_path`. Retrying resumes syncing.
    Stopped,
    /// The session is gone (last event for it).
    Closed,
}

/// A remote file opened in a local application, synced back on save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditSession {
    pub id: uuid::Uuid,
    /// `None` edits a local file through a temporary copy (used in tests).
    pub host_id: Option<HostId>,
    pub remote_path: String,
    /// The local copy (native path), named like the remote file.
    pub local_path: String,
    /// The local copy has changes that have not reached the server.
    pub dirty: bool,
    /// Left behind by a previous run of Patchbay with unsynced changes.
    pub recovered: bool,
    /// Last successful upload (Unix milliseconds).
    pub last_upload: Option<i64>,
    #[serde(flatten)]
    pub state: EditState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The wire format is mirrored by hand in `ui/lib/types.ts`.
    #[test]
    fn wire_format_is_camel_case() {
        let event = CoreEvent::Connection {
            host_id: "cfg:a".into(),
            state: ConnectionState::Connecting { hop: None },
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            json!({"type": "connection", "hostId": "cfg:a", "state": "connecting", "hop": null})
        );

        let prompt = Prompt::HostKeyChanged {
            host: "h".into(),
            port: 22,
            key_type: "ssh-ed25519".into(),
            fingerprint: "SHA256:x".into(),
            known_hosts_file: "/k".into(),
            line: 3,
        };
        assert_eq!(
            serde_json::to_value(prompt).unwrap(),
            json!({
                "kind": "hostKeyChanged", "host": "h", "port": 22, "keyType": "ssh-ed25519",
                "fingerprint": "SHA256:x", "knownHostsFile": "/k", "line": 3
            })
        );

        let response: PromptResponse =
            serde_json::from_value(json!({"kind": "secret", "value": "pw", "remember": true}))
                .unwrap();
        assert!(matches!(
            response,
            PromptResponse::Secret { remember: true, .. }
        ));
    }

    #[test]
    fn file_and_transfer_wire_format() {
        let id = uuid::Uuid::nil();
        let info = TransferInfo {
            id,
            label: "big.bin".into(),
            background: false,
            source_host: None,
            destination: FileRef {
                host_id: Some("cfg:a".into()),
                path: "/C:/Users/me".into(),
            },
            state: TransferState::Failed {
                message: "connection lost".into(),
                resumable: true,
            },
            total_bytes: Some(10),
            done_bytes: 4,
            total_files: Some(1),
            done_files: 0,
            skipped_files: 0,
            current_file: Some("big.bin".into()),
            bytes_per_sec: 0,
            created_at: 1,
            finished_at: Some(2),
        };
        assert_eq!(
            serde_json::to_value(CoreEvent::Transfer(info.clone())).unwrap(),
            json!({
                "type": "transfer", "id": id, "label": "big.bin", "background": false,
                "sourceHost": null,
                "destination": {"hostId": "cfg:a", "path": "/C:/Users/me"},
                "state": "failed", "message": "connection lost", "resumable": true,
                "totalBytes": 10, "doneBytes": 4, "totalFiles": 1, "doneFiles": 0,
                "skippedFiles": 0, "currentFile": "big.bin", "bytesPerSec": 0,
                "createdAt": 1, "finishedAt": 2
            })
        );
        let waiting = TransferInfo {
            state: TransferState::WaitingForUser,
            ..info
        };
        let value = serde_json::to_value(CoreEvent::Transfer(waiting)).unwrap();
        assert_eq!(value["type"], "transfer");
        assert_eq!(value["state"], "waitingForUser");
        assert!(value.get("message").is_none());

        let entry = FileEntry {
            name: "l".into(),
            path: "/l".into(),
            kind: FileKind::Symlink,
            link_target: Some("t".into()),
            target_kind: None,
            size: 1,
            modified: None,
            permissions: Some(0o777),
            owner: Some("me".into()),
            group: None,
            hidden: false,
        };
        assert_eq!(
            serde_json::to_value(CoreEvent::Search {
                search_id: id,
                entries: vec![entry],
                done: true,
                error: None,
            })
            .unwrap(),
            json!({
                "type": "search", "searchId": id, "done": true, "error": null,
                "entries": [{
                    "name": "l", "path": "/l", "kind": "symlink", "linkTarget": "t",
                    "targetKind": null, "size": 1, "modified": null, "permissions": 511,
                    "owner": "me", "group": null, "hidden": false
                }]
            })
        );

        let prompt = Prompt::FileConflict {
            transfer_id: id,
            path: "/x".into(),
            source: FileStat {
                size: 2,
                modified: Some(5),
            },
            existing: FileStat {
                size: 1,
                modified: None,
            },
            can_resume: true,
        };
        assert_eq!(
            serde_json::to_value(prompt).unwrap(),
            json!({
                "kind": "fileConflict", "transferId": id, "path": "/x",
                "source": {"size": 2, "modified": 5},
                "existing": {"size": 1, "modified": null},
                "canResume": true
            })
        );

        let response: PromptResponse = serde_json::from_value(
            json!({"kind": "conflict", "resolution": "overwriteIfNewer", "applyToAll": true}),
        )
        .unwrap();
        assert!(matches!(
            response,
            PromptResponse::Conflict {
                resolution: ConflictResolution::OverwriteIfNewer,
                apply_to_all: true
            }
        ));

        let request: TransferRequest = serde_json::from_value(json!({
            "sources": [{"hostId": null, "path": "/a"}],
            "destination": {"hostId": "cfg:b", "path": "/b"}
        }))
        .unwrap();
        assert_eq!(request.sources[0].host_id, None);
        assert_eq!(request.destination.host_id.as_deref(), Some("cfg:b"));
        assert!(!request.background);
        assert_eq!(request.max_bytes, None);
        let request: TransferRequest = serde_json::from_value(json!({
            "sources": [], "destination": {"hostId": null, "path": "/tmp"},
            "background": true, "maxBytes": 1024
        }))
        .unwrap();
        assert!(request.background);
        assert_eq!(request.max_bytes, Some(1024));

        let listing = DirListing {
            path: "/".into(),
            parent: None,
            entries: vec![],
        };
        assert_eq!(
            serde_json::to_value(listing).unwrap(),
            json!({"path": "/", "parent": null, "entries": []})
        );
    }

    #[test]
    fn edit_wire_format() {
        let id = uuid::Uuid::nil();
        let session = EditSession {
            id,
            host_id: Some("cfg:a".into()),
            remote_path: "/etc/app.yml".into(),
            local_path: "/cache/patchbay/edit/x/app.yml".into(),
            dirty: false,
            recovered: false,
            last_upload: Some(7),
            state: EditState::Synced { at: 9 },
        };
        assert_eq!(
            serde_json::to_value(CoreEvent::Edit(session.clone())).unwrap(),
            json!({
                "type": "edit", "id": id, "hostId": "cfg:a", "remotePath": "/etc/app.yml",
                "localPath": "/cache/patchbay/edit/x/app.yml", "dirty": false, "recovered": false,
                "lastUpload": 7, "state": "synced", "at": 9
            })
        );
        let failed = EditSession {
            state: EditState::Failed {
                message: "not connected".into(),
            },
            ..session
        };
        let value = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            (&value["state"], &value["message"]),
            (&json!("failed"), &json!("not connected"))
        );

        let prompt = Prompt::EditConflict {
            session_id: id,
            remote_path: "/etc/app.yml".into(),
            remote: FileStat {
                size: 3,
                modified: Some(5),
            },
            local: FileStat {
                size: 4,
                modified: None,
            },
        };
        assert_eq!(
            serde_json::to_value(prompt).unwrap(),
            json!({
                "kind": "editConflict", "sessionId": id, "remotePath": "/etc/app.yml",
                "remote": {"size": 3, "modified": 5}, "local": {"size": 4, "modified": null}
            })
        );
        for (wire, resolution) in [
            ("overwrite", EditConflictResolution::Overwrite),
            ("discardLocal", EditConflictResolution::DiscardLocal),
            ("keepLocal", EditConflictResolution::KeepLocal),
        ] {
            let response: PromptResponse =
                serde_json::from_value(json!({"kind": "editConflict", "resolution": wire}))
                    .unwrap();
            assert!(
                matches!(response, PromptResponse::EditConflict { resolution: r } if r == resolution)
            );
        }
    }

    #[test]
    fn terminal_and_snippet_wire_format() {
        let id = uuid::Uuid::nil();
        let spec: TerminalSpec = serde_json::from_value(
            json!({"hostId": "cfg:a", "cwd": "/srv/x y", "cols": 120, "rows": 40}),
        )
        .unwrap();
        assert_eq!(
            spec,
            TerminalSpec {
                host_id: Some("cfg:a".into()),
                cwd: Some("/srv/x y".into()),
                cols: 120,
                rows: 40,
            }
        );
        let local: TerminalSpec =
            serde_json::from_value(json!({"hostId": null, "cwd": null, "cols": 80, "rows": 24}))
                .unwrap();
        assert_eq!((local.host_id, local.cwd), (None, None));

        let info = TerminalInfo {
            id,
            host_id: None,
            shell: "zsh".into(),
        };
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            json!({"id": id, "hostId": null, "shell": "zsh"})
        );

        assert_eq!(
            serde_json::to_value(CoreEvent::TerminalExit {
                terminal_id: id,
                exit_code: None,
                reason: Some("Connection to a lost".into()),
            })
            .unwrap(),
            json!({
                "type": "terminalExit", "terminalId": id, "exitCode": null,
                "reason": "Connection to a lost"
            })
        );
        assert_eq!(
            serde_json::to_value(CoreEvent::SnippetsChanged).unwrap(),
            json!({"type": "snippetsChanged"})
        );

        let snippet = Snippet {
            id: "snip:1".into(),
            name: "logs".into(),
            command: "tail -f log".into(),
            host_ids: vec!["cfg:a".into()],
            run: true,
        };
        let value = json!({
            "id": "snip:1", "name": "logs", "command": "tail -f log",
            "hostIds": ["cfg:a"], "run": true
        });
        assert_eq!(serde_json::to_value(&snippet).unwrap(), value);
        assert_eq!(serde_json::from_value::<Snippet>(value).unwrap(), snippet);

        let draft: SnippetDraft = serde_json::from_value(
            json!({"name": "n", "command": "c", "hostIds": ["app:x"], "run": false}),
        )
        .unwrap();
        assert_eq!(draft.host_ids, ["app:x"]);
        let minimal: SnippetDraft =
            serde_json::from_value(json!({"name": "n", "command": "c"})).unwrap();
        assert_eq!(
            minimal,
            SnippetDraft {
                name: "n".into(),
                command: "c".into(),
                ..SnippetDraft::default()
            }
        );
    }

    #[test]
    fn forward_and_settings_wire_format() {
        let rule = PortForward {
            id: "fwd:1".into(),
            name: "db".into(),
            source: PortForwardSource::SshConfig,
            kind: PortForwardKind::Remote,
            host_id: "cfg:a".into(),
            bind_address: None,
            bind_port: 9000,
            target_host: Some("localhost".into()),
            target_port: Some(5432),
            via_host_id: Some("app:b".into()),
            auto_start: true,
        };
        assert_eq!(
            serde_json::to_value(&rule).unwrap(),
            json!({
                "id": "fwd:1", "name": "db", "source": "sshConfig", "kind": "remote",
                "hostId": "cfg:a", "bindAddress": null, "bindPort": 9000,
                "targetHost": "localhost", "targetPort": 5432, "viaHostId": "app:b",
                "autoStart": true
            })
        );

        let draft: PortForwardDraft = serde_json::from_value(json!({
            "name": "", "kind": "dynamic", "hostId": "cfg:a", "bindAddress": "*",
            "targetHost": null, "targetPort": null, "viaHostId": null
        }))
        .unwrap();
        assert_eq!(draft.kind, PortForwardKind::Dynamic);
        assert_eq!((draft.bind_port, draft.auto_start), (0, false));
        assert_eq!(draft.bind_address.as_deref(), Some("*"));

        let status = |state| PortForwardStatus {
            id: "fwd:1".into(),
            state,
            open_connections: 2,
            total_connections: 7,
            bytes_in: 10,
            bytes_out: 20,
        };
        assert_eq!(
            serde_json::to_value(CoreEvent::PortForward(status(
                PortForwardState::Reconnecting {
                    attempt: 3,
                    message: "lost".into()
                }
            )))
            .unwrap(),
            json!({
                "type": "portForward", "id": "fwd:1", "state": "reconnecting", "attempt": 3,
                "message": "lost", "openConnections": 2, "totalConnections": 7,
                "bytesIn": 10, "bytesOut": 20
            })
        );
        for (state, extra) in [
            (PortForwardState::Stopped, json!({"state": "stopped"})),
            (PortForwardState::Starting, json!({"state": "starting"})),
            (
                PortForwardState::Active {
                    address: "127.0.0.1:8080".into(),
                },
                json!({"state": "active", "address": "127.0.0.1:8080"}),
            ),
            (
                PortForwardState::Failed {
                    message: "nope".into(),
                },
                json!({"state": "failed", "message": "nope"}),
            ),
        ] {
            let mut expected = json!({
                "id": "fwd:1", "openConnections": 2, "totalConnections": 7,
                "bytesIn": 10, "bytesOut": 20
            });
            expected
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert_eq!(serde_json::to_value(status(state)).unwrap(), expected);
        }
        assert_eq!(
            serde_json::to_value(CoreEvent::PortForwardsChanged).unwrap(),
            json!({"type": "portForwardsChanged"})
        );

        let settings = AppSettings::default();
        assert_eq!(
            serde_json::to_value(CoreEvent::SettingsChanged(settings.clone())).unwrap(),
            json!({
                "type": "settingsChanged", "closeToTray": true, "launchAtLogin": false,
                "startMinimized": true, "maxConcurrentTransfers": 3
            })
        );
        let partial: AppSettings =
            serde_json::from_value(json!({"closeToTray": false, "maxConcurrentTransfers": 5}))
                .unwrap();
        assert_eq!(
            partial,
            AppSettings {
                close_to_tray: false,
                max_concurrent_transfers: 5,
                ..settings
            }
        );
    }
}

// ---------------------------------------------------------------------------
// Terminals and snippets.
// ---------------------------------------------------------------------------

/// What to open a terminal on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSpec {
    /// `None` = a local shell on this machine.
    pub host_id: Option<HostId>,
    /// Start in this directory ("open terminal here"); `None` = home.
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: uuid::Uuid,
    pub host_id: Option<HostId>,
    /// Shell or command that was started, for display (e.g. `"zsh"`, `"bash -l"`).
    pub shell: String,
}

/// A saved command, global or limited to some hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub id: String,
    pub name: String,
    pub command: String,
    /// Empty = available on every host (and local terminals).
    #[serde(default)]
    pub host_ids: Vec<HostId>,
    /// Press Enter after inserting the command.
    #[serde(default)]
    pub run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetDraft {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub host_ids: Vec<HostId>,
    #[serde(default)]
    pub run: bool,
}

// ---------------------------------------------------------------------------
// Port forwarding.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortForwardKind {
    /// `-L`: listen on this machine, connect to `target` as seen from `host_id`.
    Local,
    /// `-R`: listen on `host_id`, connect to `target` as seen from this machine,
    /// or from `via_host_id` when set (machine-to-machine).
    Remote,
    /// `-D`: SOCKS5 proxy listening on this machine, connecting out from `host_id`.
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PortForwardSource {
    App,
    /// From `LocalForward`/`RemoteForward`/`DynamicForward` in ~/.ssh/config (read-only).
    SshConfig,
}

/// A saved port forward rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForward {
    /// `fwd:<uuid>` for app rules, `cfg:<alias>:<n>` for ssh_config rules.
    pub id: String,
    pub name: String,
    pub source: PortForwardSource,
    pub kind: PortForwardKind,
    /// SSH host carrying the tunnel.
    pub host_id: HostId,
    /// Listen address: on this machine for local/dynamic, on `host_id` for
    /// remote. `None` = loopback.
    pub bind_address: Option<String>,
    /// `0` = pick a free port (local/dynamic only).
    pub bind_port: u16,
    /// Destination for local/remote forwards (unused for dynamic).
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    /// Remote forwards only: open the outgoing connections from this other
    /// SSH host instead of this machine.
    pub via_host_id: Option<HostId>,
    /// Start when Patchbay starts (and keep reconnecting while enabled).
    pub auto_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardDraft {
    pub name: String,
    pub kind: PortForwardKind,
    pub host_id: HostId,
    pub bind_address: Option<String>,
    #[serde(default)]
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    pub via_host_id: Option<HostId>,
    #[serde(default)]
    pub auto_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "state"
)]
pub enum PortForwardState {
    Stopped,
    Starting,
    /// Listening. `address` is the actual bound `host:port`.
    Active {
        address: String,
    },
    /// The SSH connection dropped; retrying with backoff.
    Reconnecting {
        attempt: u32,
        message: String,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardStatus {
    pub id: String,
    #[serde(flatten)]
    pub state: PortForwardState,
    /// Currently open tunneled connections.
    pub open_connections: u32,
    /// Total tunneled connections since started.
    pub total_connections: u64,
    /// Bytes from the destination back to the connecting clients.
    pub bytes_in: u64,
    /// Bytes from the connecting clients towards the destination.
    pub bytes_out: u64,
}

// ---------------------------------------------------------------------------
// App settings that the backend acts on (UI-only preferences stay in the UI).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// Closing the window hides Patchbay to the system tray instead of quitting.
    pub close_to_tray: bool,
    /// Start Patchbay when the user logs in.
    pub launch_at_login: bool,
    /// When launched at login, start hidden in the tray.
    pub start_minimized: bool,
    /// Transfers running at the same time.
    pub max_concurrent_transfers: u8,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            launch_at_login: false,
            start_minimized: true,
            max_concurrent_transfers: 3,
        }
    }
}
