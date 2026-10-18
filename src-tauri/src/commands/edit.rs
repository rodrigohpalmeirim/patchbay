//! Editing remote files in local applications, and opening local files.
//! Session changes arrive as `edit` core events, conflicts as
//! `editConflict` prompts.

use std::path::Path;

use patchbay_core::model::{EditSession, HostId};
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

fn open_with_default_app(app: &AppHandle, path: &str) -> CommandResult<()> {
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| CommandError {
            kind: "other",
            message: format!("couldn't open {path} with its default app: {e}"),
        })
}

/// Downloads a remote file (or reuses its running session) and opens the
/// local copy with the system's default application.
#[tauri::command]
pub async fn edit_open(
    app: AppHandle,
    state: State<'_, AppState>,
    host_id: HostId,
    path: String,
) -> CommandResult<EditSession> {
    let session = state.edits.open(Some(&host_id), &path).await?;
    open_with_default_app(&app, &session.local_path)?;
    Ok(session)
}

#[tauri::command]
pub fn edit_list(state: State<'_, AppState>) -> Vec<EditSession> {
    state.edits.list()
}

/// Stops syncing and deletes the local copy. Without `force`, a copy with
/// unsynced changes is kept and its path returned.
#[tauri::command]
pub async fn edit_close(
    state: State<'_, AppState>,
    id: Uuid,
    force: bool,
) -> CommandResult<Option<String>> {
    Ok(state.edits.close(id, force).await?)
}

#[tauri::command]
pub fn edit_retry(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    Ok(state.edits.retry(id)?)
}

/// Shows the local copy in the system file manager, or opens its folder
/// where revealing is not available (Linux without the `FileManager1`
/// D-Bus service or the desktop portal).
#[tauri::command]
pub fn edit_reveal(app: AppHandle, state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    let session = state
        .edits
        .get(id)
        .ok_or_else(|| patchbay_core::Error::NotFound(format!("edit session {id}")))?;
    let opener = app.opener();
    opener
        .reveal_item_in_dir(&session.local_path)
        .or_else(|e| match Path::new(&session.local_path).parent() {
            Some(dir) => opener
                .open_path(dir.to_string_lossy(), None::<&str>)
                .map_err(|_| e),
            None => Err(e),
        })
        .map_err(|e| CommandError {
            kind: "other",
            message: format!("couldn't show {}: {e}", session.local_path),
        })
}

/// Opens a file on this machine with the system's default application.
#[tauri::command]
pub fn open_local(app: AppHandle, path: String) -> CommandResult<()> {
    open_with_default_app(&app, &path)
}
