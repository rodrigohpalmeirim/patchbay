//! The russh client handler: host key verification, connection-loss
//! notification and routing of `forwarded-tcpip` channels.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use russh::Channel;
use russh::client::{ChannelOpenHandle, DisconnectReason, Handler, Msg, Session};
use russh::keys::{PublicKey, PublicKeyOrCertificate};
use tokio::sync::{oneshot, watch};

use super::prompt::Prompter;
use super::remote_forward::ForwardRoutes;
use crate::known_hosts::{self, HostKeyStatus};
use crate::model::{Prompt, PromptResponse};
use crate::{Error, Result};

/// Fires once the SSH session task behind a handle has ended, carrying a
/// human-readable reason when one is known.
#[derive(Debug, Default)]
pub(crate) struct CloseSignal {
    reason: Mutex<Option<String>>,
    closed: watch::Sender<bool>,
    /// We hung up on purpose (as opposed to losing the connection).
    local: AtomicBool,
}

impl CloseSignal {
    pub(crate) fn mark_local(&self) {
        self.local.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_local(&self) -> bool {
        self.local.load(Ordering::Relaxed)
    }

    fn set_reason(&self, reason: String) {
        let mut slot = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(reason);
    }

    fn mark_closed(&self) {
        self.closed.send_replace(true);
    }

    pub(crate) fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub(crate) fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Resolves once the session has ended.
    pub(crate) async fn closed(&self) {
        let mut rx = self.closed.subscribe();
        // The sender lives in `self`, so `wait_for` can only fail if we are
        // being dropped, which cannot happen while borrowed.
        let _ = rx.wait_for(|closed| *closed).await;
    }
}

/// What the handler needs to verify the server's host key.
pub(crate) struct HostKeyPolicy {
    pub known_hosts_files: Vec<PathBuf>,
    /// `HostKeyAlias` or hostname.
    pub host: String,
    pub port: u16,
    pub prompter: Prompter,
}

/// russh [`Handler`] used for every hop. Its lifetime equals the lifetime of
/// the session task, which is how connection loss is detected (see
/// [`CloseSignal`]).
pub struct ClientHandler {
    policy: HostKeyPolicy,
    verified: Option<PublicKey>,
    key_check_started: Option<oneshot::Sender<()>>,
    close: Arc<CloseSignal>,
    routes: Arc<ForwardRoutes>,
}

impl ClientHandler {
    pub(crate) fn new(
        policy: HostKeyPolicy,
        key_check_started: oneshot::Sender<()>,
        close: Arc<CloseSignal>,
        routes: Arc<ForwardRoutes>,
    ) -> Self {
        Self {
            policy,
            verified: None,
            key_check_started: Some(key_check_started),
            close,
            routes,
        }
    }

    async fn verify(&mut self, key: &PublicKey) -> Result<bool> {
        if let Some(verified) = &self.verified {
            return Ok(verified.key_data() == key.key_data());
        }
        let HostKeyPolicy {
            known_hosts_files,
            host,
            port,
            prompter,
        } = &self.policy;
        let (host, port) = (host.as_str(), *port);
        let key_type = key.algorithm().as_str().to_owned();
        let fingerprint = known_hosts::fingerprint(key);

        match known_hosts::check(known_hosts_files, host, port, key)? {
            HostKeyStatus::Known => {}
            HostKeyStatus::Unknown => {
                let prompt = Prompt::HostKeyUnknown {
                    host: host.to_owned(),
                    port,
                    key_type,
                    fingerprint,
                };
                match prompter.ask(prompt).await? {
                    PromptResponse::Accept => {}
                    PromptResponse::Reject => {
                        return Err(Error::HostKeyRejected {
                            host: host.to_owned(),
                        });
                    }
                    _ => return Err(Error::Cancelled),
                }
                match known_hosts_files.first() {
                    Some(file) => {
                        if let Err(e) = known_hosts::append(file, host, port, key) {
                            tracing::warn!(file = %file.display(), "could not record host key: {e}");
                        }
                    }
                    None => tracing::warn!(
                        host,
                        "no known_hosts file configured; key accepted for this session only"
                    ),
                }
            }
            HostKeyStatus::Changed { file, line } | HostKeyStatus::Revoked { file, line } => {
                tracing::warn!(host, port, file = %file.display(), line, "host key mismatch");
                let prompt = Prompt::HostKeyChanged {
                    host: host.to_owned(),
                    port,
                    key_type,
                    fingerprint,
                    known_hosts_file: file,
                    line,
                };
                match prompter.ask(prompt).await? {
                    PromptResponse::Accept => {
                        tracing::warn!(host, port, "changed host key accepted for this session");
                    }
                    PromptResponse::Reject => {
                        return Err(Error::HostKeyRejected {
                            host: host.to_owned(),
                        });
                    }
                    _ => return Err(Error::Cancelled),
                }
            }
        }
        self.verified = Some(key.clone());
        Ok(true)
    }
}

impl Handler for ClientHandler {
    type Error = Error;

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool> {
        if let Some(started) = self.key_check_started.take() {
            let _ = started.send(());
        }
        match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => self.verify(key).await,
            // Host certificates are never negotiated (russh's default
            // `Preferred::host_key_certificates` is empty).
            PublicKeyOrCertificate::Certificate(_) => Err(Error::HostKeyRejected {
                host: self.policy.host.clone(),
            }),
        }
    }

    async fn disconnected(&mut self, reason: DisconnectReason<Self::Error>) -> Result<()> {
        let reason = match reason {
            DisconnectReason::ReceivedDisconnect(info) if info.message.is_empty() => {
                format!("server closed the connection ({:?})", info.reason_code)
            }
            DisconnectReason::ReceivedDisconnect(info) => {
                format!("server closed the connection: {}", info.message)
            }
            DisconnectReason::Error(e) => e.to_string(),
        };
        tracing::debug!(host = %self.policy.host, "disconnected: {reason}");
        self.close.set_reason(reason);
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<()> {
        self.routes.dispatch(
            channel,
            reply,
            (connected_address, connected_port),
            (originator_address, originator_port),
        );
        Ok(())
    }
}

impl Drop for ClientHandler {
    fn drop(&mut self) {
        self.close.mark_closed();
    }
}
