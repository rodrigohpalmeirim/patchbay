//! The system tray icon and its menu: show the windows, open a new one,
//! toggle port forwards, disconnect hosts, quit.
//!
//! The menu is rebuilt (debounced) when connections, forwards or hosts
//! change, and only if what it shows actually changed, so traffic updates do
//! not churn it. Left-clicking the icon shows the windows where the platform
//! reports clicks (Windows, macOS); on Linux (AppIndicator) only the menu is
//! available. Without a tray host (e.g. a bare X session) the icon simply
//! does not appear.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use patchbay_core::model::{ConnectionState, CoreEvent, PortForwardState};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};
use tokio::sync::Notify;

use crate::state::AppState;
use crate::window;

const TRAY_ID: &str = "main";
const DEBOUNCE: Duration = Duration::from_millis(250);

const SHOW: &str = "show";
const NEW_WINDOW: &str = "new-window";
const QUIT: &str = "quit";
const FORWARD: &str = "forward:";
const DISCONNECT: &str = "disconnect:";

/// What the menu shows; rebuilt only when this changes.
#[derive(Debug, Clone, Default, PartialEq)]
struct Model {
    /// Rule id, label, running.
    forwards: Vec<(String, String, bool)>,
    /// Host id, display name.
    connected: Vec<(String, String)>,
}

/// Managed state: wakes the menu refresher.
struct Tray {
    refresh: Arc<Notify>,
    shown: Mutex<Option<Model>>,
}

/// Create the tray icon. Failure (no tray support) is logged, not fatal.
pub fn init(app: &AppHandle) {
    app.manage(Tray {
        refresh: Arc::new(Notify::new()),
        shown: Mutex::new(None),
    });
    if let Err(e) = build(app) {
        tracing::warn!("system tray unavailable: {e}");
        return;
    }
    let handle = app.clone();
    let refresh = app.state::<Tray>().refresh.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh.notified().await;
            tokio::time::sleep(DEBOUNCE).await;
            if let Err(e) = rebuild(&handle).await {
                tracing::warn!("updating the tray menu failed: {e}");
            }
        }
    });
    request_refresh(app);
}

fn build(app: &AppHandle) -> tauri::Result<()> {
    let menu = menu(app, &Model::default())?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Patchbay")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                window::reveal(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Refresh the menu if `event` can change what it shows.
pub fn on_core_event(app: &AppHandle, event: &CoreEvent) {
    if matches!(
        event,
        CoreEvent::Connection { .. }
            | CoreEvent::PortForward(_)
            | CoreEvent::PortForwardsChanged
            | CoreEvent::HostsChanged
    ) {
        request_refresh(app);
    }
}

pub fn request_refresh(app: &AppHandle) {
    if let Some(tray) = app.try_state::<Tray>() {
        tray.refresh.notify_one();
    }
}

async fn rebuild(app: &AppHandle) -> tauri::Result<()> {
    let model = model(app).await;
    let tray = app.state::<Tray>();
    {
        let mut shown = tray.shown.lock().unwrap_or_else(|e| e.into_inner());
        if shown.as_ref() == Some(&model) {
            return Ok(());
        }
        *shown = Some(model.clone());
    }
    let menu = menu(app, &model)?;
    if let Some(icon) = app.tray_by_id(TRAY_ID) {
        icon.set_menu(Some(menu))?;
    }
    Ok(())
}

async fn model(app: &AppHandle) -> Model {
    let state = app.state::<AppState>();
    let rules = state.forwards.list().await;
    let statuses = state.forwards.statuses().await;
    let forwards = rules
        .iter()
        .map(|rule| {
            let state = statuses
                .iter()
                .find(|s| s.id == rule.id)
                .map(|s| s.state.clone())
                .unwrap_or(PortForwardState::Stopped);
            let (suffix, running) = match &state {
                PortForwardState::Active { address } => (format!(" — {address}"), true),
                PortForwardState::Starting => (" (starting…)".to_owned(), true),
                PortForwardState::Reconnecting { .. } => (" (reconnecting…)".to_owned(), true),
                PortForwardState::Failed { .. } => (" (failed)".to_owned(), false),
                PortForwardState::Stopped => (String::new(), false),
            };
            (rule.id.clone(), format!("{}{suffix}", rule.name), running)
        })
        .collect();

    let hosts = state.hosts.read().await;
    let mut connected: Vec<(String, String)> = state
        .sessions
        .states()
        .into_iter()
        .filter(|(_, s)| matches!(s, ConnectionState::Connected { .. }))
        .map(|(id, _)| {
            let name = hosts.get(&id).map(|h| h.name).unwrap_or_else(|| id.clone());
            (id, name)
        })
        .collect();
    connected.sort_by_cached_key(|(_, name)| name.to_lowercase());
    Model {
        forwards,
        connected,
    }
}

fn menu(app: &AppHandle, model: &Model) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(
        app,
        SHOW,
        "Show Patchbay",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        NEW_WINDOW,
        "New window",
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    menu.append(&MenuItem::with_id(
        app,
        "forwards",
        "Port forwards",
        false,
        None::<&str>,
    )?)?;
    if model.forwards.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "no-forwards",
            "No port forwards",
            false,
            None::<&str>,
        )?)?;
    }
    for (id, label, running) in &model.forwards {
        menu.append(&CheckMenuItem::with_id(
            app,
            format!("{FORWARD}{id}"),
            label,
            true,
            *running,
            None::<&str>,
        )?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;

    if model.connected.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "hosts",
            "Connected hosts: 0",
            false,
            None::<&str>,
        )?)?;
    } else {
        let hosts = Submenu::with_id(
            app,
            "hosts",
            format!("Connected hosts: {}", model.connected.len()),
            true,
        )?;
        for (id, name) in &model.connected {
            hosts.append(&MenuItem::with_id(
                app,
                format!("{DISCONNECT}{id}"),
                format!("Disconnect {name}"),
                true,
                None::<&str>,
            )?)?;
        }
        menu.append(&hosts)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        QUIT,
        "Quit Patchbay",
        true,
        None::<&str>,
    )?)?;
    Ok(menu)
}

fn on_menu(app: &AppHandle, id: &str) {
    if id == SHOW {
        window::reveal(app);
    } else if id == NEW_WINDOW {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = window::open(&app, None) {
                tracing::warn!("could not open a window: {e}");
            }
        });
    } else if id == QUIT {
        crate::quit(app);
    } else if let Some(forward) = id.strip_prefix(FORWARD) {
        let (handle, forward) = (app.clone(), forward.to_owned());
        tauri::async_runtime::spawn(async move {
            toggle_forward(&handle, &forward).await;
            force_refresh(&handle);
        });
        // The native item toggled its check mark already; resync it.
        force_refresh(app);
    } else if let Some(host) = id.strip_prefix(DISCONNECT) {
        let (app, host) = (app.clone(), host.to_owned());
        tauri::async_runtime::spawn(async move {
            app.state::<AppState>().sessions.disconnect(&host).await;
        });
    }
}

async fn toggle_forward(app: &AppHandle, id: &str) {
    let state = app.state::<AppState>();
    let running = state
        .forwards
        .statuses()
        .await
        .into_iter()
        .find(|s| s.id == id)
        .is_some_and(|s| {
            matches!(
                s.state,
                PortForwardState::Active { .. }
                    | PortForwardState::Starting
                    | PortForwardState::Reconnecting { .. }
            )
        });
    if running {
        state.forwards.stop(id).await;
    } else if let Err(e) = state.forwards.start(id).await {
        tracing::info!(forward = %id, "starting from the tray failed: {e}");
    }
}

/// Rebuild even if the model looks unchanged (native state drifted).
fn force_refresh(app: &AppHandle) {
    if let Some(tray) = app.try_state::<Tray>() {
        *tray.shown.lock().unwrap_or_else(|e| e.into_inner()) = None;
        tray.refresh.notify_one();
    }
}
