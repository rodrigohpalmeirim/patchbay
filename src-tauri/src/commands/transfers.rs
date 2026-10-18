//! The transfer queue. Progress arrives as `transfer` core events and
//! conflicts as `fileConflict` prompts.

use patchbay_core::model::{TransferInfo, TransferRequest};
use tauri::State;
use uuid::Uuid;

use crate::error::CommandResult;
use crate::state::AppState;

#[tauri::command]
pub async fn transfer_start(
    state: State<'_, AppState>,
    request: TransferRequest,
) -> CommandResult<TransferInfo> {
    Ok(state.transfers.start(request)?)
}

#[tauri::command]
pub fn transfer_list(state: State<'_, AppState>) -> Vec<TransferInfo> {
    state.transfers.list()
}

#[tauri::command]
pub async fn transfer_pause(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.transfers.pause(id)?)
}

#[tauri::command]
pub async fn transfer_resume(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.transfers.resume(id)?)
}

#[tauri::command]
pub async fn transfer_cancel(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.transfers.cancel(id)?)
}

#[tauri::command]
pub async fn transfer_retry(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.transfers.retry(id)?)
}

#[tauri::command]
pub fn transfer_clear_finished(state: State<'_, AppState>) {
    state.transfers.clear_finished();
}
