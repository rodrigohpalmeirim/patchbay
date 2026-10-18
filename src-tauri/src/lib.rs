mod commands;
mod error;
mod state;
mod streams;
mod tray;
mod window;

use std::sync::atomic::Ordering;
use std::time::Duration;

use patchbay_core::model::CoreEvent;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

/// Upper bound for closing terminals and stopping forwards on exit.
const EXIT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often old drag-out copies are deleted.
const DRAGOUT_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How often the tailnet's devices (and their online state) are re-read.
const TAILNET_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,patchbay=debug")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::reveal(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![window::MINIMIZED_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_drag::init())
        .setup(|app| {
            let (state, mut events, mut prompts) = AppState::new()?;
            let mut backend_events = state.events.subscribe();
            app.manage(state);

            tauri::async_runtime::spawn_blocking(|| {
                if let Err(err) = patchbay_core::secrets::init() {
                    tracing::warn!(
                        "system keychain unavailable, secrets won't be remembered: {err}"
                    );
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let _ = handle.emit("core-event", event);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("frontend event bridge lagged by {n} events");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Backend reactions to core events (tray menu, ssh_config forwards).
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match backend_events.recv().await {
                        Ok(event) => {
                            tray::on_core_event(&handle, &event);
                            if let CoreEvent::HostsChanged = event {
                                handle
                                    .state::<AppState>()
                                    .forwards
                                    .sync_config_rules()
                                    .await;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            tray::request_refresh(&handle);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            // Edit sessions of a previous run; drag-out copies are never reused.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                let dragout = state.dragout.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || dragout.clear()).await;
                let recovered = state.edits.recover().await;
                if !recovered.is_empty() {
                    tracing::info!("{} edit sessions with unsynced changes", recovered.len());
                }
                loop {
                    tokio::time::sleep(DRAGOUT_SWEEP_INTERVAL).await;
                    state.dragout.sweep(patchbay_core::dragout::MAX_AGE).await;
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Some(request) = prompts.recv().await {
                    handle.state::<AppState>().present_prompt(&handle, request);
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    handle.state::<AppState>().refresh_tailnet().await;
                    tokio::time::sleep(TAILNET_REFRESH_INTERVAL).await;
                }
            });

            tray::init(app.handle());
            window::init(app.handle());
            Ok(())
        })
        .on_page_load(|webview, payload| {
            // A (re)loading page has none of the old page's terminal channels.
            if payload.event() == PageLoadEvent::Started
                && let Some(state) = webview.try_state::<AppState>()
            {
                state.close_webview_terminals(webview.label());
            }
            if payload.event() == PageLoadEvent::Finished && webview.label() == window::MAIN {
                window::ready(webview.app_handle());
            }
        })
        .on_window_event(|window, event| match event {
            // Pick up edits to ~/.ssh/config made while the app was in the background.
            WindowEvent::Focused(true) => {
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = app.state::<AppState>().refresh_hosts().await {
                        tracing::warn!("failed to reload hosts: {err}");
                    }
                });
            }
            // Close to tray: keep sessions, terminals and forwards running.
            WindowEvent::CloseRequested { api, .. } if window.label() == window::MAIN => {
                if let Some(state) = window.try_state::<AppState>()
                    && !state.quitting.load(Ordering::SeqCst)
                    && state.current_settings().close_to_tray
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            WindowEvent::Destroyed => {
                if let Some(state) = window.try_state::<AppState>() {
                    for webview in window.webviews() {
                        state.close_webview_terminals(webview.label());
                    }
                    state.close_webview_terminals(window.label());
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_hosts,
            commands::host_warnings,
            commands::reload_hosts,
            commands::create_host,
            commands::update_host,
            commands::delete_host,
            commands::host_renames,
            commands::new_host_file,
            commands::organize_host,
            commands::open_host_file,
            commands::list_connections,
            commands::connect,
            commands::disconnect,
            commands::respond_prompt,
            commands::fs_home,
            commands::fs_list,
            commands::fs_stat,
            commands::fs_mkdir,
            commands::fs_create_file,
            commands::fs_rename,
            commands::fs_delete,
            commands::fs_chmod,
            commands::fs_chown,
            commands::fs_read,
            commands::transfer_start,
            commands::transfer_list,
            commands::transfer_pause,
            commands::transfer_resume,
            commands::transfer_cancel,
            commands::transfer_retry,
            commands::transfer_clear_finished,
            commands::edit_open,
            commands::edit_list,
            commands::edit_close,
            commands::edit_retry,
            commands::edit_reveal,
            commands::open_local,
            commands::dragout_prepare,
            commands::dragout_release,
            commands::dragout_start_linux,
            commands::search_start,
            commands::search_cancel,
            commands::terminal_open,
            commands::terminal_write,
            commands::terminal_resize,
            commands::terminal_close,
            commands::terminal_ack,
            commands::snippets_list,
            commands::snippet_create,
            commands::snippet_update,
            commands::snippet_delete,
            commands::forwards_list,
            commands::forward_statuses,
            commands::forward_create,
            commands::forward_update,
            commands::forward_delete,
            commands::forward_start,
            commands::forward_stop,
            commands::settings_get,
            commands::settings_set,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Patchbay")
        .run(|app, event| match event {
            // Dock icon clicked while the window is hidden in the tray.
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => window::reveal(app),
            // Hang up shells, cancel remote forwards and close their SSH
            // channels before going away.
            RunEvent::Exit => {
                if let Some(state) = app.try_state::<AppState>() {
                    state.dragout.clear();
                    let terminals = state.terminals.clone();
                    let forwards = state.forwards.clone();
                    tauri::async_runtime::block_on(async move {
                        let cleanup =
                            futures_util::future::join(terminals.close_all(), forwards.stop_all());
                        if tokio::time::timeout(EXIT_CLEANUP_TIMEOUT, cleanup)
                            .await
                            .is_err()
                        {
                            tracing::warn!("cleanup on exit timed out");
                        }
                    });
                }
            }
            _ => {}
        });
}

/// Really exit (tray "Quit Patchbay"): closing the window must not hide it.
pub(crate) fn quit(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.quitting.store(true, Ordering::SeqCst);
    }
    app.exit(0);
}
