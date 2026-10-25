use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use patchbay_core::dragout::DragOutCache;
use patchbay_core::edit::EditManager;
use patchbay_core::forward::{ForwardManager, ForwardStore};
use patchbay_core::fs::FileSystem;
use patchbay_core::hosts::HostStore;
use patchbay_core::model::{AppSettings, CoreEvent, HostId, Prompt, PromptRequest, PromptResponse};
use patchbay_core::search::SearchManager;
use patchbay_core::session::SessionManager;
use patchbay_core::settings::SettingsStore;
use patchbay_core::snippets::SnippetStore;
use patchbay_core::tailscale;
use patchbay_core::terminal::TerminalManager;
use patchbay_core::transfer::TransferManager;
use serde::Serialize;
use tauri::{AppHandle, Emitter, EventTarget, Manager, WebviewWindow};
use tokio::sync::{RwLock, broadcast, mpsc, oneshot};
use uuid::Uuid;

use crate::streams::TerminalStreams;

pub struct AppState {
    pub hosts: Arc<RwLock<HostStore>>,
    pub snippets: RwLock<SnippetStore>,
    pub sessions: SessionManager,
    pub fs: FileSystem,
    pub transfers: TransferManager,
    pub edits: EditManager,
    pub dragout: DragOutCache,
    pub search: SearchManager,
    pub terminals: TerminalManager,
    pub forwards: ForwardManager,
    pub streams: TerminalStreams,
    pub events: broadcast::Sender<CoreEvent>,
    settings: Mutex<SettingsStore>,
    /// Set once the user chose to quit, so closing the window is no longer
    /// turned into hiding it.
    pub quitting: AtomicBool,
    /// The tailnet has been read at least once (see `ensure_tailnet`).
    tailnet_checked: AtomicBool,
    pending_prompts: Mutex<HashMap<Uuid, PendingPrompt>>,
    /// The window that last asked for each connection, transfer and edit
    /// session, so their prompts go there.
    origins: Mutex<HashMap<Origin, String>>,
}

/// What a prompt is about, as far as choosing its window goes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    Host(HostId),
    Transfer(Uuid),
    Edit(Uuid),
}

impl Origin {
    fn of(payload: &PromptPayload) -> Option<Self> {
        match &payload.prompt {
            Prompt::FileConflict { transfer_id, .. } => Some(Self::Transfer(*transfer_id)),
            Prompt::EditConflict { session_id, .. } => Some(Self::Edit(*session_id)),
            _ => payload.host_id.clone().map(Self::Host),
        }
    }
}

struct PendingPrompt {
    answer: oneshot::Sender<PromptResponse>,
    payload: PromptPayload,
    /// The window showing it; `None` while no window is open.
    window: Option<String>,
}

/// A prompt as delivered to the frontend; answered via `respond_prompt`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptPayload {
    pub id: Uuid,
    pub host_id: Option<HostId>,
    pub prompt: Prompt,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptClosedPayload {
    pub id: Uuid,
}

impl AppState {
    pub fn new() -> patchbay_core::Result<(
        Self,
        broadcast::Receiver<CoreEvent>,
        mpsc::Receiver<PromptRequest>,
    )> {
        let (events, events_rx) = broadcast::channel(256);
        let (prompts, prompts_rx) = mpsc::channel(16);
        let sessions = SessionManager::new(events.clone(), prompts.clone());
        let fs = FileSystem::new(sessions.clone());
        let hosts = HostStore::load_default()?;
        // References to hosts renamed while they couldn't follow (e.g. hosts
        // moved into ~/.ssh/config on first launch).
        let mut forward_store = ForwardStore::load_default()?;
        let mut snippet_store = SnippetStore::load_default()?;
        if let Err(e) = forward_store.rename_hosts(hosts.renamed()) {
            tracing::warn!("could not update port forwards of renamed hosts: {e}");
        }
        if let Err(e) = snippet_store.rename_hosts(hosts.renamed()) {
            tracing::warn!("could not update snippets of renamed hosts: {e}");
        }
        let hosts = Arc::new(RwLock::new(hosts));
        let settings_path = SettingsStore::default_path()?;
        let settings = SettingsStore::load(settings_path.clone()).unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable settings, using defaults: {e}");
            SettingsStore::with_defaults(settings_path)
        });
        let transfers = TransferManager::new(fs.clone(), events.clone(), prompts.clone());
        transfers.set_max_concurrent(settings.get().max_concurrent_transfers.into());
        let edits = EditManager::new(
            fs.clone(),
            events.clone(),
            prompts,
            EditManager::default_root()?,
        );
        let dragout = DragOutCache::new(DragOutCache::default_root()?, transfers.clone());
        let state = Self {
            forwards: ForwardManager::new(
                sessions.clone(),
                hosts.clone(),
                events.clone(),
                forward_store,
            ),
            hosts,
            snippets: RwLock::new(snippet_store),
            transfers,
            edits,
            dragout,
            search: SearchManager::new(fs.clone(), events.clone()),
            terminals: TerminalManager::new(sessions.clone(), events.clone()),
            streams: TerminalStreams::default(),
            sessions,
            fs,
            events,
            settings: Mutex::new(settings),
            quitting: AtomicBool::new(false),
            tailnet_checked: AtomicBool::new(false),
            pending_prompts: Mutex::new(HashMap::new()),
            origins: Mutex::new(HashMap::new()),
        };
        Ok((state, events_rx, prompts_rx))
    }

    pub fn settings(&self) -> MutexGuard<'_, SettingsStore> {
        self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn current_settings(&self) -> AppSettings {
        self.settings().get()
    }

    /// Prompts about `origin` go to `window` from now on.
    pub fn set_origin(&self, origin: Origin, window: &str) {
        self.origins
            .lock()
            .unwrap()
            .insert(origin, window.to_owned());
    }

    /// The window for a prompt: the one that asked for what it is about if
    /// that is still open, otherwise the most recently focused one.
    fn prompt_window(&self, app: &AppHandle, payload: &PromptPayload) -> Option<WebviewWindow> {
        let origin =
            Origin::of(payload).and_then(|o| self.origins.lock().unwrap().get(&o).cloned());
        let windows = app.state::<crate::window::Windows>();
        let open = windows.preferred_labels();
        origin
            .filter(|l| open.contains(l))
            .and_then(|l| app.get_webview_window(&l))
            .or_else(|| crate::window::preferred(app))
    }

    /// Shows a prompt in `window` (`prompt` event), bringing the window back
    /// from the tray: the answer needs the user.
    fn deliver(app: &AppHandle, window: &WebviewWindow, payload: &PromptPayload) {
        crate::window::reveal_window(window);
        let _ = app.emit_to(
            EventTarget::webview_window(window.label()),
            "prompt",
            payload.clone(),
        );
    }

    /// Hands a core prompt to one window (see `prompt_window`). If the core
    /// abandons it before the user answers (connection aborted, timeout), a
    /// `prompt-closed` event tells every window to dismiss it.
    pub fn present_prompt(&self, app: &AppHandle, request: PromptRequest) {
        let PromptRequest {
            id,
            host_id,
            prompt,
            mut reply,
        } = request;
        let payload = PromptPayload {
            id,
            host_id,
            prompt,
        };
        let (answer_tx, answer_rx) = oneshot::channel();
        let window = self.prompt_window(app, &payload);
        self.pending_prompts.lock().unwrap().insert(
            id,
            PendingPrompt {
                answer: answer_tx,
                payload: payload.clone(),
                window: window.as_ref().map(|w| w.label().to_owned()),
            },
        );

        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::select! {
                answer = answer_rx => {
                    if let Ok(answer) = answer {
                        let _ = reply.send(answer);
                    }
                }
                () = reply.closed() => {
                    handle.state::<AppState>().pending_prompts.lock().unwrap().remove(&id);
                    let _ = handle.emit("prompt-closed", PromptClosedPayload { id });
                }
            }
        });

        tracing::debug!(%id, window = ?window.as_ref().map(|w| w.label()), "presenting prompt");
        if let Some(window) = window {
            Self::deliver(app, &window, &payload);
        }
    }

    /// Returns false if the prompt is unknown (already answered or abandoned).
    pub fn answer_prompt(&self, id: Uuid, response: PromptResponse) -> bool {
        let pending = self.pending_prompts.lock().unwrap().remove(&id);
        match pending {
            Some(pending) => pending.answer.send(response).is_ok(),
            None => false,
        }
    }

    /// Unanswered prompts for `window`, e.g. after it reloaded or before its
    /// page listened. Prompts no window shows yet are given to it.
    pub fn window_prompts(&self, window: &str) -> Vec<PromptPayload> {
        let mut pending = self.pending_prompts.lock().unwrap();
        pending
            .values_mut()
            .filter(|p| p.window.as_deref().is_none_or(|w| w == window))
            .map(|p| {
                p.window = Some(window.to_owned());
                p.payload.clone()
            })
            .collect()
    }

    /// `window` closed: its prompts move to another window, and what it
    /// asked for no longer leads there.
    pub fn window_closed(&self, app: &AppHandle, window: &str) {
        self.origins.lock().unwrap().retain(|_, w| w != window);
        let target = crate::window::preferred(app).filter(|w| w.label() != window);
        let moved: Vec<PromptPayload> = {
            let mut pending = self.pending_prompts.lock().unwrap();
            pending
                .values_mut()
                .filter(|p| p.window.as_deref() == Some(window))
                .map(|p| {
                    p.window = target.as_ref().map(|w| w.label().to_owned());
                    p.payload.clone()
                })
                .collect()
        };
        if let Some(target) = target {
            for payload in &moved {
                tracing::debug!(id = %payload.id, from = window, to = target.label(), "moving prompt");
                Self::deliver(app, &target, payload);
            }
        }
    }

    /// Closes the terminals whose output goes to `webview` (it is reloading
    /// or gone, so nobody can see them anymore). Terminals being handed to
    /// it are only closed when it is `gone` (see `crate::streams`).
    pub fn close_webview_terminals(&self, webview: &str, gone: bool) {
        for id in self.streams.cancel_webview(webview, gone) {
            tracing::debug!(%id, webview, "closing terminal of a reloaded/closed webview");
            let terminals = self.terminals.clone();
            tauri::async_runtime::spawn(async move { terminals.close(id).await });
        }
    }

    /// Re-reads the host sources, emits `HostsChanged` if the host list
    /// differs, and picks up changed ssh_config port forwards.
    pub async fn refresh_hosts(&self) -> patchbay_core::Result<()> {
        {
            let mut hosts = self.hosts.write().await;
            let before = hosts.list();
            hosts.reload()?;
            if hosts.list() != before {
                let _ = self.events.send(CoreEvent::HostsChanged);
            }
        }
        self.forwards.sync_config_rules().await;
        self.refresh_tailnet().await;
        Ok(())
    }

    /// Re-reads the tailnet (when enabled) and emits `HostsChanged` if the
    /// host list differs. Keeps the last known devices if Tailscale doesn't
    /// answer.
    /// Reads the tailnet unless that has happened already, so the first host
    /// list (and the tabs restored from it) includes Tailscale devices.
    pub async fn ensure_tailnet(&self) {
        if !self.tailnet_checked.load(Ordering::Relaxed) {
            self.refresh_tailnet().await;
        }
    }

    pub async fn refresh_tailnet(&self) {
        self.tailnet_checked.store(true, Ordering::Relaxed);
        let tailnet = if self.current_settings().tailscale {
            match tailscale::status().await {
                Ok(status) => status,
                Err(e) => {
                    tracing::debug!("tailscale status: {e}");
                    return;
                }
            }
        } else {
            None
        };
        let gone: Vec<HostId> = {
            let mut hosts = self.hosts.write().await;
            let before = hosts.list();
            hosts.set_tailnet(tailnet);
            if hosts.list() == before {
                return;
            }
            let _ = self.events.send(CoreEvent::HostsChanged);
            before
                .into_iter()
                .filter(|h| h.id.starts_with("ts:") && hosts.get(&h.id).is_none())
                .map(|h| h.id)
                .collect()
        };
        // Their tabs close; don't leave the connections running unseen.
        for id in gone {
            self.sessions.disconnect(&id).await;
        }
    }

    /// Points saved port forwards and snippets at the new ids of renamed
    /// hosts.
    pub async fn follow_renames(&self, renamed: &BTreeMap<HostId, HostId>) {
        if let Err(e) = self.forwards.rename_hosts(renamed).await {
            tracing::warn!("could not update port forwards of renamed hosts: {e}");
        }
        match self.snippets.write().await.rename_hosts(renamed) {
            Ok(true) => {
                let _ = self.events.send(CoreEvent::SnippetsChanged);
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("could not update snippets of renamed hosts: {e}"),
        }
    }
}
