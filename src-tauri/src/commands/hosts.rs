//! Hosts, connections and prompts.

use std::collections::HashMap;

use patchbay_core::model::{ConnectionState, CoreEvent, Host, HostDraft, HostId, PromptResponse};
use tauri::State;
use uuid::Uuid;

use crate::error::CommandResult;
use crate::state::AppState;

#[tauri::command]
pub async fn list_hosts(state: State<'_, AppState>) -> CommandResult<Vec<Host>> {
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

#[tauri::command]
pub async fn update_host(
    state: State<'_, AppState>,
    id: HostId,
    draft: HostDraft,
) -> CommandResult<Host> {
    let host = state.hosts.write().await.update(&id, draft)?;
    let _ = state.events.send(CoreEvent::HostsChanged);
    Ok(host)
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
#[tauri::command]
pub async fn connect(state: State<'_, AppState>, id: HostId) -> CommandResult<()> {
    let target = state.hosts.read().await.resolve_target(&id)?;
    state.sessions.connect(target).await?;
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
