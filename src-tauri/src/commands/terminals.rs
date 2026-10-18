//! Terminals and snippets. Terminal output is streamed to the `onData`
//! channel (see `crate::streams`); `terminalExit` core events follow it.

use patchbay_core::model::{CoreEvent, Snippet, SnippetDraft, TerminalInfo, TerminalSpec};
use tauri::ipc::Channel;
use tauri::{State, Webview};
use uuid::Uuid;

use crate::error::CommandResult;
use crate::state::AppState;

#[tauri::command]
pub async fn terminal_open(
    state: State<'_, AppState>,
    webview: Webview,
    spec: TerminalSpec,
    on_data: Channel,
) -> CommandResult<TerminalInfo> {
    let (info, output) = state.terminals.open(spec).await?;
    state.streams.start(output, on_data, webview.label());
    Ok(info)
}

#[tauri::command]
pub fn terminal_write(state: State<'_, AppState>, id: Uuid, data: String) -> CommandResult<()> {
    Ok(state.terminals.write(id, data.into_bytes())?)
}

#[tauri::command]
pub fn terminal_resize(
    state: State<'_, AppState>,
    id: Uuid,
    cols: u16,
    rows: u16,
) -> CommandResult<()> {
    Ok(state.terminals.resize(id, cols, rows)?)
}

/// Resolves once the shell is gone; output still in flight is dropped.
#[tauri::command]
pub async fn terminal_close(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    state.streams.cancel(id);
    state.terminals.close(id).await;
    Ok(())
}

/// Optional flow control: the frontend has processed `bytes` more bytes of
/// output (e.g. reported from xterm.js `write` callbacks).
#[tauri::command]
pub fn terminal_ack(state: State<'_, AppState>, id: Uuid, bytes: u64) {
    state.streams.ack(id, bytes);
}

#[tauri::command]
pub async fn snippets_list(state: State<'_, AppState>) -> CommandResult<Vec<Snippet>> {
    Ok(state.snippets.read().await.list())
}

#[tauri::command]
pub async fn snippet_create(
    state: State<'_, AppState>,
    draft: SnippetDraft,
) -> CommandResult<Snippet> {
    let snippet = state.snippets.write().await.create(draft)?;
    let _ = state.events.send(CoreEvent::SnippetsChanged);
    Ok(snippet)
}

#[tauri::command]
pub async fn snippet_update(
    state: State<'_, AppState>,
    id: String,
    draft: SnippetDraft,
) -> CommandResult<Snippet> {
    let snippet = state.snippets.write().await.update(&id, draft)?;
    let _ = state.events.send(CoreEvent::SnippetsChanged);
    Ok(snippet)
}

#[tauri::command]
pub async fn snippet_delete(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state.snippets.write().await.delete(&id)?;
    let _ = state.events.send(CoreEvent::SnippetsChanged);
    Ok(())
}
