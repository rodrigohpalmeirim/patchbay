//! Settings the backend acts on. Changes are broadcast as `settingsChanged`.

use patchbay_core::model::{AppSettings, CoreEvent};
use tauri::{AppHandle, Manager, State};
#[cfg(not(windows))]
use tauri_plugin_autostart::ManagerExt;

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

#[tauri::command]
pub fn settings_get(state: State<'_, AppState>) -> AppSettings {
    state.current_settings()
}

/// Saves and applies the settings (transfer parallelism, launch at login)
/// and returns them as stored (numbers clamped into range).
#[tauri::command]
pub fn settings_set(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: AppSettings,
) -> CommandResult<AppSettings> {
    apply_launch_at_login(&app, settings.launch_at_login)?;
    let tailscale_before = state.current_settings().tailscale;
    let saved = state.settings().set(settings)?;
    if saved.tailscale != tailscale_before {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            app.state::<AppState>().refresh_tailnet().await;
        });
    }
    state
        .transfers
        .set_max_concurrent(saved.max_concurrent_transfers.into());
    let _ = state.events.send(CoreEvent::SettingsChanged(saved.clone()));
    Ok(saved)
}

/// Register or remove the login item (started with `--minimized`).
fn apply_launch_at_login(app: &AppHandle, enable: bool) -> CommandResult<()> {
    #[cfg(windows)]
    let autolaunch = windows_autolaunch(app)?;
    #[cfg(not(windows))]
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().ok() == Some(enable) {
        return Ok(());
    }
    let result = if enable {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    result.map_err(launch_at_login_error)
}

fn launch_at_login_error(e: impl std::fmt::Display) -> CommandError {
    CommandError {
        kind: "other",
        message: format!("could not change launch at login: {e}"),
    }
}

/// The autostart plugin's Windows login item is registered machine-wide
/// (HKLM) whenever Patchbay runs elevated, and its path is unquoted (breaks on
/// paths with spaces). Use the current user's `Run` key and a quoted path;
/// disabling still removes an old machine-wide entry when it can.
#[cfg(windows)]
fn windows_autolaunch(app: &AppHandle) -> CommandResult<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().map_err(launch_at_login_error)?;
    Ok(auto_launch::AutoLaunch::new(
        &app.package_info().name,
        &format!("\"{}\"", exe.display()),
        auto_launch::WindowsEnableMode::CurrentUser,
        &[crate::window::MINIMIZED_ARG],
    ))
}
