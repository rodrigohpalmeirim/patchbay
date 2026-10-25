//! The transfer queue. Progress arrives as `transfer` core events and
//! conflicts as `fileConflict` prompts (shown in the window that started or
//! last retried the transfer).

use patchbay_core::model::{TransferInfo, TransferRequest};
use tauri::{State, Webview};
use uuid::Uuid;

use crate::error::CommandResult;
use crate::state::{AppState, Origin};

#[tauri::command]
pub async fn transfer_start(
    state: State<'_, AppState>,
    webview: Webview,
    request: TransferRequest,
) -> CommandResult<TransferInfo> {
    let info = state.transfers.start(request)?;
    state.set_origin(Origin::Transfer(info.id), webview.label());
    Ok(info)
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
pub async fn transfer_retry(
    state: State<'_, AppState>,
    webview: Webview,
    id: Uuid,
) -> CommandResult<()> {
    state.set_origin(Origin::Transfer(id), webview.label());
    Ok(state.transfers.retry(id)?)
}

#[tauri::command]
pub fn transfer_clear_finished(state: State<'_, AppState>) {
    state.transfers.clear_finished();
}
