//! The main window's visibility: showing it once the page has rendered
//! (it is created hidden, see `tauri.conf.json`, so there is no white
//! flash), starting hidden in the tray when launched at login, and bringing
//! it back from the tray.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewWindow};

use crate::state::AppState;

pub const MAIN: &str = "main";

/// Command-line flag passed by the login item (see `tauri-plugin-autostart`).
pub const MINIMIZED_ARG: &str = "--minimized";

/// Show the window anyway if the page has not finished loading by then.
const SHOW_FALLBACK: Duration = Duration::from_secs(3);
/// Delay between the page being loaded and auto-starting forwards, so the
/// frontend has subscribed to prompts by the time a connection asks one.
const AUTO_START_DELAY: Duration = Duration::from_secs(2);

/// Startup bookkeeping, managed as Tauri state.
pub struct Startup {
    /// Launched at login with "start minimized" on: stay in the tray.
    start_hidden: bool,
    ready: AtomicBool,
}

pub fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(MAIN)
}

/// Show, unminimize and focus the main window.
pub fn reveal(app: &AppHandle) {
    if let Some(window) = main_window(app) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Decide the startup visibility; call from `setup`.
pub fn init(app: &AppHandle) {
    let launched_minimized = std::env::args().any(|a| a == MINIMIZED_ARG);
    let start_hidden =
        launched_minimized && app.state::<AppState>().current_settings().start_minimized;
    app.manage(Startup {
        start_hidden,
        ready: AtomicBool::new(false),
    });
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SHOW_FALLBACK).await;
        ready(&handle);
    });
}

/// The main page finished loading (or the fallback timer fired): show the
/// window unless starting hidden, then auto-start forwards. Runs once.
pub fn ready(app: &AppHandle) {
    let startup = app.state::<Startup>();
    if startup.ready.swap(true, Ordering::SeqCst) {
        return;
    }
    if !startup.start_hidden {
        reveal(app);
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(AUTO_START_DELAY).await;
        handle.state::<AppState>().forwards.start_auto().await;
    });
}
