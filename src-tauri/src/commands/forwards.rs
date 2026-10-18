//! Port forward rules and their runtime. Status changes arrive as
//! `portForward` core events, rule list changes as `portForwardsChanged`.

use patchbay_core::model::{PortForward, PortForwardDraft, PortForwardStatus};
use tauri::State;

use crate::error::CommandResult;
use crate::state::AppState;

#[tauri::command]
pub async fn forwards_list(state: State<'_, AppState>) -> CommandResult<Vec<PortForward>> {
    Ok(state.forwards.list().await)
}

#[tauri::command]
pub async fn forward_statuses(state: State<'_, AppState>) -> CommandResult<Vec<PortForwardStatus>> {
    Ok(state.forwards.statuses().await)
}

#[tauri::command]
pub async fn forward_create(
    state: State<'_, AppState>,
    draft: PortForwardDraft,
) -> CommandResult<PortForward> {
    Ok(state.forwards.create(draft).await?)
}

/// Restarts the forward (in the background) if it was running.
#[tauri::command]
pub async fn forward_update(
    state: State<'_, AppState>,
    id: String,
    draft: PortForwardDraft,
) -> CommandResult<PortForward> {
    Ok(state.forwards.update(&id, draft).await?)
}

#[tauri::command]
pub async fn forward_delete(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    Ok(state.forwards.delete(&id).await?)
}

/// Resolves once the forward is active (or reconnecting); rejects with the
/// error that made it fail (its `failed` status is emitted as well).
#[tauri::command]
pub async fn forward_start(
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<PortForwardStatus> {
    Ok(state.forwards.start(&id).await?)
}

#[tauri::command]
pub async fn forward_stop(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state.forwards.stop(&id).await;
    Ok(())
}
