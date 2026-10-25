//! Windows: opening and closing them, their saved tabs, the prompts routed to
//! them and the state they share. See `crate::window`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, Webview, WebviewWindow};
use uuid::Uuid;

use crate::error::{CommandError, CommandResult};
use crate::state::{AppState, PromptPayload};
use crate::window::{WindowSession, Windows, WindowsInfo};

/// Terminals handed to a window that has not taken them over by then are
/// closed (its page failed to load, or never got to them).
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(30);

fn window_error(e: tauri::Error) -> CommandError {
    CommandError {
        kind: "other",
        message: e.to_string(),
    }
}

/// Opens a new, empty window next to the calling one.
#[tauri::command]
pub async fn window_open(app: AppHandle, window: WebviewWindow) -> CommandResult<()> {
    crate::window::open(&app, Some(&window)).map_err(window_error)?;
    Ok(())
}

/// A point on screen, in logical pixels.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ScreenPoint {
    x: f64,
    y: f64,
}

fn move_error(message: &str) -> CommandError {
    CommandError {
        kind: "other",
        message: message.to_owned(),
    }
}

/// Moves a tab of the calling window to window `to`, or to a new window
/// (with its tab strip at `at`, else next to the caller). `tab` is the
/// frontend's description of it, kept for the target to take
/// (`window_take_tabs`); `terminals` are its shells, which the caller paused
/// with `terminal_detach`. They are handed to the target, which takes them
/// over with `terminal_attach` (or they are closed after
/// [`HANDOVER_TIMEOUT`]). On error nothing was handed over: the caller still
/// owns its terminals and resumes them. Returns the target's label.
#[tauri::command]
pub async fn window_move_tab(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    tab: serde_json::Value,
    terminals: Vec<Uuid>,
    to: Option<String>,
    at: Option<ScreenPoint>,
) -> CommandResult<String> {
    let windows = app.state::<Windows>();
    let from = window.label();
    if terminals
        .iter()
        .any(|&id| !state.streams.is_paused(id, from))
    {
        return Err(move_error("a terminal of this tab is no longer running"));
    }
    let (target, created) = match to {
        Some(label) if label != from && windows.is_open(&label) => (label, None),
        Some(_) => return Err(move_error("that window was closed")),
        None => {
            let created = match at {
                Some(p) => crate::window::open_at(&app, &window, (p.x, p.y)),
                None => crate::window::open(&app, Some(&window)),
            }
            .map_err(window_error)?;
            (created.label().to_owned(), Some(created))
        }
    };
    // Undo everything if the target goes away before it has the tab.
    let undo = |handed: &[(Uuid, u64)]| {
        for &(id, _) in handed {
            state.streams.reclaim(id, from, &target);
        }
        if let Some(created) = &created {
            let _ = created.destroy();
        }
    };
    let mut handed = Vec::with_capacity(terminals.len());
    for &id in &terminals {
        match state.streams.hand_over(id, from, &target) {
            Some(handover) => handed.push((id, handover)),
            None => {
                undo(&handed);
                return Err(move_error("a terminal of this tab is no longer running"));
            }
        }
    }
    if !windows.deliver(&target, tab) {
        undo(&handed);
        return Err(move_error("that window was closed"));
    }
    tracing::debug!(from, to = %target, terminals = handed.len(), "moved a tab");
    crate::window::tabs_incoming(&app, &target);
    for (id, handover) in handed {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(HANDOVER_TIMEOUT).await;
            let state = app.state::<AppState>();
            if state.streams.expire(id, handover) {
                tracing::warn!(%id, "a moved terminal was not taken over; closing it");
                state.terminals.close(id).await;
            }
        });
    }
    Ok(target)
}

/// The window under a tab dragged out of the calling one, and the pointer in
/// its viewport (logical pixels).
#[derive(Debug, Clone, Serialize)]
pub struct TabDragOver {
    label: String,
    x: f64,
    y: f64,
}

/// Payload of `tab-drag-over`: the dragged tab as its window described it,
/// and the pointer in the receiving window's viewport.
#[derive(Debug, Clone, Serialize)]
struct TabDragIn {
    tab: serde_json::Value,
    x: f64,
    y: f64,
}

/// A tab dragged out of the calling window is at screen point `at` (`None`:
/// the drag ended or came back): shows it to the window under the pointer
/// (`tab-drag-over`) and clears it from the one it left. Returns that window
/// and where the pointer is in it.
#[tauri::command]
pub fn window_drag_tab(
    app: AppHandle,
    window: WebviewWindow,
    tab: Option<serde_json::Value>,
    at: Option<ScreenPoint>,
) -> Option<TabDragOver> {
    let Some((tab, (target, x, y))) = tab
        .zip(at)
        .and_then(|(tab, at)| Some((tab, crate::window::window_at(&app, &window, (at.x, at.y))?)))
    else {
        crate::window::show_tab_drag(&app, window.label(), None);
        return None;
    };
    let payload = serde_json::to_value(TabDragIn { tab, x, y }).unwrap_or_default();
    crate::window::show_tab_drag(&app, window.label(), Some((target.label(), payload)));
    Some(TabDragOver {
        label: target.label().to_owned(),
        x,
        y,
    })
}

/// Tabs moved to the calling window since it last asked (`tabs-incoming`).
#[tauri::command]
pub fn window_take_tabs(webview: Webview, windows: State<'_, Windows>) -> Vec<serde_json::Value> {
    windows.take_incoming(webview.label())
}

/// How the calling window describes itself to the others: the title of its
/// active tab and how many tabs it has (`windows-changed`).
#[tauri::command]
pub fn window_describe(
    app: AppHandle,
    webview: Webview,
    windows: State<'_, Windows>,
    title: Option<String>,
    tabs: usize,
) {
    if windows.describe(webview.label(), title, tabs) {
        crate::window::changed(&app);
    }
}

/// Closes the calling window although shells are running in it (the user
/// confirmed; see `window-close-requested`).
#[tauri::command]
pub async fn window_close(window: WebviewWindow, windows: State<'_, Windows>) -> CommandResult<()> {
    windows.confirm_close(window.label());
    window.close().map_err(window_error)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    label: String,
    #[serde(flatten)]
    windows: WindowsInfo,
}

/// The calling window's label and the current roles (`windows-changed`
/// carries later changes).
#[tauri::command]
pub fn window_info(webview: Webview, windows: State<'_, Windows>) -> WindowInfo {
    WindowInfo {
        label: webview.label().to_owned(),
        windows: windows.info(),
    }
}

#[tauri::command]
pub fn window_session(webview: Webview, windows: State<'_, Windows>) -> WindowSession {
    windows.session(webview.label())
}

/// Saves the calling window's tabs and whether its sidebar is hidden, and
/// whether tabs are restored at all.
#[tauri::command]
pub fn window_save_session(
    webview: Webview,
    windows: State<'_, Windows>,
    session: serde_json::Value,
    sidebar_hidden: bool,
    restore_tabs: bool,
) {
    windows.save_session(webview.label(), session, sidebar_hidden, restore_tabs);
}

/// Unanswered prompts routed to the calling window.
#[tauri::command]
pub fn window_prompts(webview: Webview, state: State<'_, AppState>) -> Vec<PromptPayload> {
    state.window_prompts(webview.label())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SharedPayload {
    topic: String,
    value: serde_json::Value,
    /// The window it came from (which ignores it).
    from: String,
}

/// Hands window-local state (display settings, the file clipboard) to the
/// other windows as `ui-shared`, and keeps it for windows opened later.
#[tauri::command]
pub fn ui_share(
    app: AppHandle,
    webview: Webview,
    windows: State<'_, Windows>,
    topic: String,
    value: serde_json::Value,
) {
    windows.share(topic.clone(), value.clone());
    let _ = app.emit(
        "ui-shared",
        SharedPayload {
            topic,
            value,
            from: webview.label().to_owned(),
        },
    );
}

#[tauri::command]
pub fn ui_shared(windows: State<'_, Windows>, topic: String) -> Option<serde_json::Value> {
    windows.shared(&topic)
}
