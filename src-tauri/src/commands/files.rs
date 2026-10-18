//! File browsing on the local machine (`hostId: null`) and connected hosts.

use patchbay_core::model::{DirListing, FileEntry, HostId};
use tauri::State;
use tauri::ipc::Response;

use crate::error::CommandResult;
use crate::state::AppState;

#[tauri::command]
pub async fn fs_home(state: State<'_, AppState>, host_id: Option<HostId>) -> CommandResult<String> {
    Ok(state.fs.home(host_id.as_deref()).await?)
}

#[tauri::command]
pub async fn fs_list(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    path: String,
) -> CommandResult<DirListing> {
    Ok(state.fs.list(host_id.as_deref(), &path).await?)
}

#[tauri::command]
pub async fn fs_stat(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    path: String,
) -> CommandResult<FileEntry> {
    Ok(state.fs.stat(host_id.as_deref(), &path).await?)
}

#[tauri::command]
pub async fn fs_mkdir(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    path: String,
) -> CommandResult<()> {
    Ok(state.fs.mkdir(host_id.as_deref(), &path).await?)
}

#[tauri::command]
pub async fn fs_create_file(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    path: String,
) -> CommandResult<()> {
    Ok(state.fs.create_file(host_id.as_deref(), &path).await?)
}

#[tauri::command]
pub async fn fs_rename(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    from: String,
    to: String,
) -> CommandResult<()> {
    Ok(state.fs.rename(host_id.as_deref(), &from, &to).await?)
}

#[tauri::command]
pub async fn fs_delete(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    paths: Vec<String>,
) -> CommandResult<()> {
    Ok(state.fs.delete(host_id.as_deref(), &paths).await?)
}

#[tauri::command]
pub async fn fs_chmod(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    paths: Vec<String>,
    mode: u32,
    recursive: bool,
) -> CommandResult<()> {
    Ok(state
        .fs
        .chmod(host_id.as_deref(), &paths, mode, recursive)
        .await?)
}

#[tauri::command]
pub async fn fs_chown(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    paths: Vec<String>,
    owner: Option<String>,
    group: Option<String>,
    recursive: bool,
) -> CommandResult<()> {
    Ok(state
        .fs
        .chown(
            host_id.as_deref(),
            &paths,
            owner.as_deref(),
            group.as_deref(),
            recursive,
        )
        .await?)
}

/// The first `maxBytes` of a file as raw bytes (an `ArrayBuffer` in JS).
#[tauri::command]
pub async fn fs_read(
    state: State<'_, AppState>,
    host_id: Option<HostId>,
    path: String,
    max_bytes: u64,
) -> CommandResult<Response> {
    let bytes = state.fs.read(host_id.as_deref(), &path, max_bytes).await?;
    Ok(Response::new(bytes))
}
