//! File name search; results stream in as `search` core events.

use patchbay_core::model::HostId;
use tauri::State;
use uuid::Uuid;

use crate::error::CommandResult;
use crate::state::AppState;

#[tauri::command]
pub async fn search_start(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    root: String,
    query: String,
) -> CommandResult<Uuid> {
    Ok(state.search.start(host_id, &root, &query).await?)
}

#[tauri::command]
pub fn search_cancel(state: State<'_, AppState>, search_id: Uuid) {
    state.search.cancel(search_id);
}
