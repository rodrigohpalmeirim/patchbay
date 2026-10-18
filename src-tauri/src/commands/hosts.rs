//! Hosts, connections and prompts.

use std::collections::{BTreeMap, HashMap};

use patchbay_core::model::{ConnectionState, CoreEvent, Host, HostDraft, HostId, PromptResponse};
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

#[tauri::command]
pub async fn list_hosts(state: State<'_, AppState>) -> CommandResult<Vec<Host>> {
    state.ensure_tailnet().await;
    Ok(state.hosts.read().await.list())
}

#[tauri::command]
pub async fn host_warnings(state: State<'_, AppState>) -> CommandResult<Vec<String>> {
    Ok(state.hosts.read().await.warnings().to_vec())
}

#[tauri::command]
pub async fn reload_hosts(state: State<'_, AppState>) -> CommandResult<Vec<Host>> {
    state.refresh_hosts().await?;
    Ok(state.hosts.read().await.list())
}

#[tauri::command]
pub async fn create_host(state: State<'_, AppState>, draft: HostDraft) -> CommandResult<Host> {
    let host = state.hosts.write().await.create(draft)?;
    let _ = state.events.send(CoreEvent::HostsChanged);
    Ok(host)
}

/// Saves a Patchbay host. Renaming changes its id; saved port forwards and
/// snippets follow, and the frontend learns the new id from the result (and
/// `host_renames`).
#[tauri::command]
pub async fn update_host(
    state: State<'_, AppState>,
    id: HostId,
    draft: HostDraft,
) -> CommandResult<Host> {
    let renaming = draft.name.trim() != id.strip_prefix("cfg:").unwrap_or(&id);
    if renaming && state.sessions.states().contains_key(&id) {
        return Err(CommandError {
            kind: "other",
            message: "Disconnect from this host before renaming it".into(),
        });
    }
    let (host, renamed) = {
        let mut hosts = state.hosts.write().await;
        let host = hosts.update(&id, draft)?;
        (host, hosts.renamed().clone())
    };
    if host.id != id {
        state.follow_renames(&renamed).await;
    }
    let _ = state.events.send(CoreEvent::HostsChanged);
    Ok(host)
}

/// Old id -> current id of renamed hosts.
#[tauri::command]
pub async fn host_renames(state: State<'_, AppState>) -> CommandResult<BTreeMap<HostId, HostId>> {
    Ok(state.hosts.read().await.renamed().clone())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSharing {
    /// `~/.ssh/config` includes the file Patchbay hosts are saved in.
    pub shared: bool,
    /// That file (`~/.ssh/patchbay.conf`).
    pub path: String,
}

#[tauri::command]
pub async fn ssh_sharing(state: State<'_, AppState>) -> CommandResult<SshSharing> {
    let hosts = state.hosts.read().await;
    Ok(SshSharing {
        shared: hosts.shared_with_ssh(),
        path: hosts.managed_path().display().to_string(),
    })
}

/// Adds or removes the `Include patchbay.conf` line in `~/.ssh/config`.
#[tauri::command]
pub async fn set_ssh_sharing(
    state: State<'_, AppState>,
    shared: bool,
) -> CommandResult<SshSharing> {
    state.hosts.write().await.set_shared_with_ssh(shared)?;
    let _ = state.events.send(CoreEvent::HostsChanged);
    ssh_sharing(state).await
}

#[tauri::command]
pub async fn delete_host(state: State<'_, AppState>, id: HostId) -> CommandResult<()> {
    state.hosts.write().await.delete(&id)?;
    let _ = state.events.send(CoreEvent::HostsChanged);
    Ok(())
}

/// Current state of every known connection (for restoring UI state after a
/// webview reload).
#[tauri::command]
pub fn list_connections(state: State<'_, AppState>) -> HashMap<HostId, ConnectionState> {
    state.sessions.states()
}

/// Connects (or reuses the existing connection) and resolves once connected.
/// Progress and prompts arrive as `core-event` / `prompt` events meanwhile;
/// failures reject with a `CommandError` and also emit `disconnected`.
/// `user` overrides the user to log in as (like `ssh -l`). For Tailscale
/// devices, the user of a successful login is remembered; without one,
/// connecting fails with `userRequired`.
#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    id: HostId,
    user: Option<String>,
) -> CommandResult<()> {
    let user = user.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
    let target = state
        .hosts
        .read()
        .await
        .resolve_target_as(&id, user.as_deref())?;
    let login = target.user.clone();
    state.sessions.connect(target).await?;
    if id.starts_with("ts:") {
        match state
            .hosts
            .write()
            .await
            .remember_tailscale_user(&id, &login)
        {
            Ok(true) => {
                let _ = state.events.send(CoreEvent::HostsChanged);
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("could not remember the user for {id}: {e}"),
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, id: HostId) -> CommandResult<()> {
    state.sessions.disconnect(&id).await;
    Ok(())
}

#[tauri::command]
pub fn respond_prompt(state: State<'_, AppState>, id: Uuid, response: PromptResponse) -> bool {
    state.answer_prompt(id, response)
}
