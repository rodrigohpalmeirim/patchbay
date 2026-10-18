//! Asking the frontend questions during a connection attempt.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::oneshot;

use crate::model::{HostId, Prompt, PromptRequest, PromptResponse, PromptSender};
use crate::{Error, Result};

/// Sends [`PromptRequest`]s on behalf of one connection attempt.
#[derive(Clone)]
pub(crate) struct Prompter {
    tx: PromptSender,
    host_id: Option<HostId>,
    gate: Option<Arc<PromptGate>>,
}

/// Decides whether an unattended attempt (auto-reconnect) may bother the
/// user, and records what happened.
#[derive(Debug, Default)]
pub(crate) struct PromptGate {
    allow: bool,
    prompted: AtomicBool,
    suppressed: AtomicBool,
}

impl PromptGate {
    pub(crate) fn new(allow: bool) -> Arc<Self> {
        Arc::new(Self {
            allow,
            ..Self::default()
        })
    }

    /// A prompt was shown to the user.
    pub(crate) fn prompted(&self) -> bool {
        self.prompted.load(Ordering::Relaxed)
    }

    /// A prompt was needed but not allowed (answered with "cancel").
    pub(crate) fn suppressed(&self) -> bool {
        self.suppressed.load(Ordering::Relaxed)
    }
}

impl Prompter {
    pub(crate) fn new(
        tx: PromptSender,
        host_id: Option<HostId>,
        gate: Option<Arc<PromptGate>>,
    ) -> Self {
        Self { tx, host_id, gate }
    }

    /// Ask and wait for the answer. A closed prompt channel, a dropped reply
    /// or a prompt the [`PromptGate`] does not allow is reported as
    /// [`Error::Cancelled`]; an explicit [`PromptResponse::Cancel`] is
    /// returned as-is so callers can decide whether it aborts the attempt or
    /// only skips a step.
    pub(crate) async fn ask(&self, prompt: Prompt) -> Result<PromptResponse> {
        if let Some(gate) = &self.gate {
            if !gate.allow {
                gate.suppressed.store(true, Ordering::Relaxed);
                return Err(Error::Cancelled);
            }
            gate.prompted.store(true, Ordering::Relaxed);
        }
        let (reply, rx) = oneshot::channel();
        let request = PromptRequest {
            id: uuid::Uuid::new_v4(),
            host_id: self.host_id.clone(),
            prompt,
            reply,
        };
        self.tx.send(request).await.map_err(|_| Error::Cancelled)?;
        rx.await.map_err(|_| Error::Cancelled)
    }
}
