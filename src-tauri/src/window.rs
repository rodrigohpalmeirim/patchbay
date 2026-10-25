//! Patchbay's windows. Every window is equal: the full UI with its own tabs.
//! The one in `tauri.conf.json` ("main") exists at launch; more are opened
//! from the tab strip's "+" menu or the tray (`window-<n>`). Labels are only
//! unique within a run; saved windows are matched by order.
//!
//! - **Visibility**: windows are created hidden and shown once their page has
//!   rendered (no white flash), or after a fallback delay. Launched at login
//!   with "start minimized", the restored windows stay in the tray until
//!   [`reveal`]ed.
//! - **Closing**: closing a window while others are open closes it for good
//!   (its tabs and their shells; with shells running the frontend asks first,
//!   `window-close-requested`) and forgets its layout. Closing the last one
//!   hides it to the tray (setting) or quits; quitting keeps every window's
//!   layout.
//! - **Layouts**: geometry is tracked from move/resize events, tabs and
//!   whether the sidebar is hidden are sent by the frontend
//!   (`window_save_session`); all are written to
//!   `windows.json` ([`patchbay_core::layouts`]) shortly after changes and
//!   on exit.
//! - **Roles**: the most recently focused window is what the tray shows and
//!   gets prompts no particular window asked for; the oldest open window is
//!   the "leader" that runs once-per-app work (update checks). Changes are
//!   broadcast as `windows-changed`.
//! - **Shared state**: window-local preferences and the file clipboard are
//!   relayed between windows (`ui-shared`); the latest value of each topic is
//!   kept for windows opened later.
//! - **Moving tabs**: a tab moved to another window (`window_move_tab`) is
//!   kept here until that window's page takes it (`tabs-incoming` tells an
//!   open page to). Its terminals are handed over separately (see
//!   `crate::streams`). Each window describes itself (active tab title, tab
//!   count) so the others can list it as a destination.
//! - **Dragging tabs between windows**: while a tab dragged out of a window
//!   is outside it, the frontend reports the pointer (`window_drag_tab`);
//!   the window under it ([`window_at`]) is told where it is in its viewport
//!   (`tab-drag-over`, null when it leaves), finds the drop target itself
//!   and shows it. Without known window positions (Wayland) there is never
//!   a window under the pointer, and tabs are only torn off.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use patchbay_core::layouts::{Layouts, SavedWindow, WindowGeometry};
use serde::Serialize;
use tauri::{
    AppHandle, CloseRequestApi, Emitter, EventTarget, Manager, PhysicalPosition, PhysicalSize,
    WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window,
};
use tokio::sync::Notify;

use crate::state::AppState;

pub const MAIN: &str = "main";

/// Command-line flag passed by the login item (see `tauri-plugin-autostart`).
pub const MINIMIZED_ARG: &str = "--minimized";

/// Show a window anyway if its page has not finished loading by then.
const SHOW_FALLBACK: Duration = Duration::from_secs(3);
/// Delay between the first page being loaded and auto-starting forwards, so
/// the frontend has subscribed to prompts by the time a connection asks one.
const AUTO_START_DELAY: Duration = Duration::from_secs(2);
/// Debounce for writing `windows.json`.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// Size and minimum size of new windows (as in `tauri.conf.json`).
const DEFAULT_SIZE: (f64, f64) = (1200.0, 720.0);
const MIN_SIZE: (f64, f64) = (720.0, 480.0);
/// How far a new window is moved from the one it was opened from (logical).
const CASCADE: f64 = 32.0;
/// A restored window must overlap a monitor by this much (physical pixels),
/// otherwise it is centered instead (e.g. that monitor is gone).
const MIN_VISIBLE: i64 = 64;

/// Event telling a window that closing it needs the user's confirmation.
const CLOSE_REQUESTED_EVENT: &str = "window-close-requested";
/// Event telling a window that tabs were moved to it (`window_take_tabs`).
const TABS_INCOMING_EVENT: &str = "tabs-incoming";
/// Event telling a window that a tab of another one is dragged over it.
const TAB_DRAG_OVER_EVENT: &str = "tab-drag-over";
/// Where a torn-off tab's new window goes relative to the pointer (logical):
/// up and left, so the pointer ends up over its tab strip.
const TEAR_OFF_OFFSET: (f64, f64) = (120.0, 16.0);

/// Managed state: the open windows, their saved layouts and roles.
pub struct Windows {
    /// `windows.json`; `None` if the config directory is unknown.
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
    save: Arc<Notify>,
    /// The first page has loaded (forwards auto-started).
    ready: AtomicBool,
    next: AtomicU32,
    /// Where windows are on screen is known (not on Wayland).
    positions_known: bool,
}

struct Inner {
    restore_tabs: bool,
    /// In the order they were opened; closed ones are kept only while
    /// quitting (forgotten windows are removed).
    windows: Vec<Entry>,
    /// Labels, most recently focused first.
    focus: Vec<String>,
    /// Nothing was saved yet: the main window takes over the tabs saved by
    /// earlier versions (frontend localStorage).
    migrate: bool,
    /// Latest value of each shared topic.
    shared: HashMap<String, serde_json::Value>,
    /// Windows showing a tab dragged over them, by the window it comes from.
    drag_over: HashMap<String, String>,
}

struct Entry {
    label: String,
    saved: SavedWindow,
    open: bool,
    /// Shown at least once; geometry is tracked from then on.
    shown: bool,
    /// Show once the page has loaded (false: launched into the tray).
    show_on_load: bool,
    /// The user agreed to close it although shells are running.
    close_confirmed: bool,
    /// Tabs moved here that its page has not taken yet.
    incoming: Vec<serde_json::Value>,
    /// Title of its active tab, and how many tabs it has.
    title: Option<String>,
    tabs: usize,
}

impl Entry {
    fn new(label: String, saved: SavedWindow, show_on_load: bool) -> Self {
        Self {
            label,
            saved,
            open: true,
            shown: false,
            show_on_load,
            close_confirmed: false,
            incoming: Vec::new(),
            title: None,
            tabs: 0,
        }
    }
}

impl Inner {
    fn entry_mut(&mut self, label: &str) -> Option<&mut Entry> {
        self.windows.iter_mut().find(|e| e.open && e.label == label)
    }

    /// Open windows, most recently focused first, then in opening order.
    fn preferred(&self) -> Vec<String> {
        let open = |l: &String| self.windows.iter().any(|e| e.open && &e.label == l);
        let mut labels: Vec<String> = self.focus.iter().filter(|l| open(l)).cloned().collect();
        for e in self.windows.iter().filter(|e| e.open) {
            if !labels.contains(&e.label) {
                labels.push(e.label.clone());
            }
        }
        labels
    }

    /// See [`Windows::drag_over`].
    fn drag_over(&mut self, source: &str, target: Option<&str>) -> Option<String> {
        let previous = match target {
            Some(target) => self.drag_over.insert(source.to_owned(), target.to_owned()),
            None => self.drag_over.remove(source),
        };
        previous.filter(|p| Some(p.as_str()) != target)
    }

    fn info(&self) -> WindowsInfo {
        let preferred = self.preferred();
        let open: Vec<&Entry> = self.windows.iter().filter(|e| e.open).collect();
        WindowsInfo {
            leader: self
                .windows
                .iter()
                .find(|e| e.open)
                .map(|e| e.label.clone()),
            active: preferred.first().cloned(),
            count: preferred.len(),
            windows: preferred
                .iter()
                .filter_map(|l| open.iter().position(|e| &e.label == l))
                .map(|i| WindowSummary {
                    label: open[i].label.clone(),
                    title: open[i].title.clone(),
                    tabs: open[i].tabs,
                    number: i + 1,
                })
                .collect(),
        }
    }
}

/// Payload of `windows-changed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsInfo {
    /// Runs once-per-app work.
    pub leader: Option<String>,
    /// Most recently focused.
    pub active: Option<String>,
    pub count: usize,
    /// The open windows, most recently focused first.
    pub windows: Vec<WindowSummary>,
}

/// How a window describes itself to the others (`window_describe`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSummary {
    pub label: String,
    /// Title of its active tab.
    pub title: Option<String>,
    pub tabs: usize,
    /// Its place among the open windows in the order they were opened (1:
    /// the oldest), to tell apart windows that look alike.
    pub number: usize,
}

/// What was saved for a window (`window_session`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowSession {
    /// Its tabs (none if they aren't restored).
    pub session: Option<serde_json::Value>,
    /// Its sidebar is hidden (kept even when tabs aren't restored).
    pub sidebar_hidden: bool,
    /// Nothing saved yet: take over the single-window session of earlier
    /// versions.
    pub migrate: bool,
}

impl Windows {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn next_label(&self) -> String {
        format!("window-{}", self.next.fetch_add(1, Ordering::Relaxed))
    }

    pub fn info(&self) -> WindowsInfo {
        self.lock().info()
    }

    /// Labels of open windows, most recently focused first.
    pub fn preferred_labels(&self) -> Vec<String> {
        self.lock().preferred()
    }

    pub fn session(&self, label: &str) -> WindowSession {
        let mut inner = self.lock();
        let migrate = inner.migrate && label == MAIN;
        let entry = inner.entry_mut(label);
        WindowSession {
            session: entry.as_ref().and_then(|e| e.saved.session.clone()),
            sidebar_hidden: entry.is_some_and(|e| e.saved.sidebar_hidden),
            migrate,
        }
    }

    /// Stores a window's tabs and whether its sidebar is hidden (ignored
    /// once the window is closing).
    pub fn save_session(
        &self,
        label: &str,
        session: serde_json::Value,
        sidebar_hidden: bool,
        restore_tabs: bool,
    ) {
        let mut inner = self.lock();
        inner.restore_tabs = restore_tabs;
        if label == MAIN {
            inner.migrate = false;
        }
        if let Some(entry) = inner.entry_mut(label) {
            entry.saved.session = Some(session);
            entry.saved.sidebar_hidden = sidebar_hidden;
        }
        drop(inner);
        self.save.notify_one();
    }

    pub fn share(&self, topic: String, value: serde_json::Value) {
        self.lock().shared.insert(topic, value);
    }

    pub fn shared(&self, topic: &str) -> Option<serde_json::Value> {
        self.lock().shared.get(topic).cloned()
    }

    /// Updates how `label` describes itself; true if that changed.
    pub fn describe(&self, label: &str, title: Option<String>, tabs: usize) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.entry_mut(label) else {
            return false;
        };
        if entry.title == title && entry.tabs == tabs {
            return false;
        }
        entry.title = title;
        entry.tabs = tabs;
        true
    }

    pub fn is_open(&self, label: &str) -> bool {
        self.lock().entry_mut(label).is_some()
    }

    /// Keeps a tab moved to `label` until its page takes it; false if that
    /// window is not open.
    pub fn deliver(&self, label: &str, tab: serde_json::Value) -> bool {
        match self.lock().entry_mut(label) {
            Some(entry) => {
                entry.incoming.push(tab);
                true
            }
            None => false,
        }
    }

    /// The tabs moved to `label` since it last asked.
    pub fn take_incoming(&self, label: &str) -> Vec<serde_json::Value> {
        self.lock()
            .entry_mut(label)
            .map(|e| std::mem::take(&mut e.incoming))
            .unwrap_or_default()
    }

    /// Window `target` now shows the tab dragged out of `source` (`None`:
    /// none does); returns the one that showed it before, if another.
    pub fn drag_over(&self, source: &str, target: Option<&str>) -> Option<String> {
        self.lock().drag_over(source, target)
    }

    /// The next close request of `label` goes through despite running shells.
    pub fn confirm_close(&self, label: &str) {
        if let Some(entry) = self.lock().entry_mut(label) {
            entry.close_confirmed = true;
        }
    }

    /// Writes `windows.json`.
    pub fn flush(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let layouts = {
            let inner = self.lock();
            // The main page never saved: keep the old session for next time.
            if inner.migrate {
                return;
            }
            Layouts {
                restore_tabs: inner.restore_tabs,
                windows: inner.windows.iter().map(|e| e.saved.clone()).collect(),
            }
        };
        if let Err(e) = layouts.save(path) {
            tracing::warn!("could not save the window layouts: {e}");
        }
    }
}

/// Load the saved windows, place the main window and reopen the others;
/// call from `setup`.
pub fn init(app: &AppHandle) {
    let launched_minimized = std::env::args().any(|a| a == MINIMIZED_ARG);
    let start_hidden =
        launched_minimized && app.state::<AppState>().current_settings().start_minimized;
    let path = Layouts::default_path()
        .inspect_err(|e| tracing::warn!("window layouts won't be saved: {e}"))
        .ok();
    let (layouts, migrate) = match path.as_deref().map(Layouts::load) {
        Some(Ok(Some(layouts))) => (layouts, false),
        Some(Ok(None)) => (Layouts::default(), true),
        Some(Err(e)) => {
            tracing::warn!("ignoring unreadable window layouts: {e}");
            (Layouts::default(), false)
        }
        None => (Layouts::default(), false),
    };
    let mut saved = layouts.windows.into_iter();
    let mut first = saved.next().unwrap_or_default();
    let others: Vec<SavedWindow> = if layouts.restore_tabs {
        saved.collect()
    } else {
        first.session = None;
        Vec::new()
    };
    let geometry = first.geometry;
    app.manage(Windows {
        path,
        inner: Mutex::new(Inner {
            restore_tabs: layouts.restore_tabs,
            windows: vec![Entry::new(MAIN.to_owned(), first, !start_hidden)],
            focus: Vec::new(),
            migrate,
            shared: HashMap::new(),
            drag_over: HashMap::new(),
        }),
        save: Arc::new(Notify::new()),
        ready: AtomicBool::new(false),
        next: AtomicU32::new(2),
        positions_known: positions_known(),
    });
    if let Some(main) = app.get_webview_window(MAIN) {
        place(&main, geometry);
    }
    show_later(app, MAIN);
    for saved in others {
        if let Err(e) = create(app, saved, !start_hidden) {
            tracing::warn!("could not reopen a window: {e}");
        }
    }

    let handle = app.clone();
    let save = app.state::<Windows>().save.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            save.notified().await;
            tokio::time::sleep(SAVE_DELAY).await;
            let handle = handle.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                handle.state::<Windows>().flush();
            })
            .await;
        }
    });
}

/// Whether windows know where they are on screen: not on Wayland. Call on
/// the main thread.
fn positions_known() -> bool {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::ObjectExt;
        gtk::gdk::Display::default().is_some_and(|d| d.type_().name() != "GdkWaylandDisplay")
    }
    #[cfg(not(target_os = "linux"))]
    true
}

/// The Patchbay window other than `from` under the pointer, which is at
/// screen point `at` (logical pixels of `from`) unless the system says
/// otherwise, with the pointer's position in its viewport (logical pixels).
/// Where windows overlap, the most recently focused one is taken to be on
/// top (`from` included, and their title bars and borders); minimized and
/// hidden windows are not under anything.
pub fn window_at(
    app: &AppHandle,
    from: &WebviewWindow,
    at: (f64, f64),
) -> Option<(WebviewWindow, f64, f64)> {
    let windows = app.state::<Windows>();
    if !windows.positions_known {
        return None;
    }
    let pointer = app.cursor_position().ok().or_else(|| {
        let scale = from.scale_factor().ok()?;
        Some(PhysicalPosition::new(at.0 * scale, at.1 * scale))
    })?;
    // The pointer relative to a rectangle, if inside it.
    let within = |p: PhysicalPosition<i32>, s: PhysicalSize<u32>| {
        let (x, y) = (pointer.x - f64::from(p.x), pointer.y - f64::from(p.y));
        (x >= 0.0 && y >= 0.0 && x < f64::from(s.width) && y < f64::from(s.height))
            .then_some((x, y))
    };
    let top = windows
        .preferred_labels()
        .iter()
        .filter_map(|l| app.get_webview_window(l))
        .find(|w| {
            w.is_visible().unwrap_or(false)
                && !w.is_minimized().unwrap_or(true)
                && w.outer_position()
                    .ok()
                    .zip(w.outer_size().ok())
                    .and_then(|(p, s)| within(p, s))
                    .is_some()
        })?;
    if top.label() == from.label() {
        return None;
    }
    let (x, y) = within(top.inner_position().ok()?, top.inner_size().ok()?)?;
    let scale = top.scale_factor().ok()?;
    Some((top, x / scale, y / scale))
}

/// Shows the tab dragged out of `from` to window `to` at a point of its
/// viewport (`None`: to none), and tells the window that showed it before
/// that it left.
pub fn show_tab_drag(app: &AppHandle, from: &str, to: Option<(&str, serde_json::Value)>) {
    let previous = app
        .state::<Windows>()
        .drag_over(from, to.as_ref().map(|(label, _)| *label));
    if let Some(previous) = previous {
        let _ = app.emit_to(
            EventTarget::webview_window(previous),
            TAB_DRAG_OVER_EVENT,
            serde_json::Value::Null,
        );
    }
    if let Some((label, payload)) = to {
        let _ = app.emit_to(
            EventTarget::webview_window(label),
            TAB_DRAG_OVER_EVENT,
            payload,
        );
    }
}

/// Opens a new, empty window next to `from` (default: the most recently
/// focused window).
pub fn open(app: &AppHandle, from: Option<&WebviewWindow>) -> tauri::Result<WebviewWindow> {
    let from = from.cloned().or_else(|| preferred(app));
    let geometry = from.as_ref().and_then(|w| cascade(app, w));
    create(
        app,
        SavedWindow {
            geometry,
            ..SavedWindow::default()
        },
        true,
    )
}

/// Opens a new, empty window the size of `from` with its tab strip under
/// the screen point `at` (logical pixels; a tab torn off `from`).
pub fn open_at(
    app: &AppHandle,
    from: &WebviewWindow,
    at: (f64, f64),
) -> tauri::Result<WebviewWindow> {
    let geometry = from
        .inner_size()
        .ok()
        .zip(from.scale_factor().ok())
        .map(|(size, scale)| WindowGeometry {
            x: ((at.0 - TEAR_OFF_OFFSET.0) * scale).round() as i32,
            y: ((at.1 - TEAR_OFF_OFFSET.1) * scale).round() as i32,
            width: size.width,
            height: size.height,
            maximized: false,
        });
    create(
        app,
        SavedWindow {
            geometry,
            ..SavedWindow::default()
        },
        true,
    )
}

fn create(app: &AppHandle, saved: SavedWindow, show_on_load: bool) -> tauri::Result<WebviewWindow> {
    let windows = app.state::<Windows>();
    let label = windows.next_label();
    let geometry = saved.geometry;
    windows
        .lock()
        .windows
        .push(Entry::new(label.clone(), saved, show_on_load));
    let built = WebviewWindowBuilder::new(app, &label, WebviewUrl::default())
        .title("Patchbay")
        .inner_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1)
        .min_inner_size(MIN_SIZE.0, MIN_SIZE.1)
        .center()
        .visible(false)
        .build();
    let window = match built {
        Ok(window) => window,
        Err(e) => {
            windows.lock().windows.retain(|e| e.label != label);
            return Err(e);
        }
    };
    place(&window, geometry);
    show_later(app, &label);
    changed(app);
    windows.save.notify_one();
    Ok(window)
}

/// Moves a (hidden) window to its saved place, if that is still on screen.
fn place(window: &WebviewWindow, geometry: Option<WindowGeometry>) {
    let Some(g) = geometry.filter(|g| on_screen(window, g)) else {
        return;
    };
    let _ = window.set_size(PhysicalSize::new(g.width, g.height));
    let _ = window.set_position(PhysicalPosition::new(g.x, g.y));
    if g.maximized {
        let _ = window.maximize();
    }
}

fn on_screen(window: &WebviewWindow, g: &WindowGeometry) -> bool {
    let Ok(monitors) = window.available_monitors() else {
        return true;
    };
    monitors.iter().any(|m| {
        let (p, s) = (m.position(), m.size());
        let w = (i64::from(g.x) + i64::from(g.width)).min(i64::from(p.x) + i64::from(s.width))
            - i64::from(g.x).max(i64::from(p.x));
        let h = (i64::from(g.y) + i64::from(g.height)).min(i64::from(p.y) + i64::from(s.height))
            - i64::from(g.y).max(i64::from(p.y));
        w >= MIN_VISIBLE && h >= MIN_VISIBLE
    })
}

/// Where a window opened from `from` goes: same size, a step down and to the
/// right, back to the top-left corner when that leaves the monitor.
fn cascade(app: &AppHandle, from: &WebviewWindow) -> Option<WindowGeometry> {
    let scale = from.scale_factor().ok()?;
    let step = (CASCADE * scale).round() as i32;
    let tracked = app
        .state::<Windows>()
        .lock()
        .entry_mut(from.label())
        .and_then(|e| e.saved.geometry);
    let base = match tracked {
        Some(g) => g,
        None => {
            let (p, s) = (from.outer_position().ok()?, from.inner_size().ok()?);
            WindowGeometry {
                x: p.x,
                y: p.y,
                width: s.width,
                height: s.height,
                maximized: false,
            }
        }
    };
    let mut g = WindowGeometry {
        x: base.x + step,
        y: base.y + step,
        maximized: false,
        ..base
    };
    // Title bar and borders come on top of the inner size.
    let frame = match (from.outer_size(), from.inner_size()) {
        (Ok(outer), Ok(inner)) => (
            outer.width.saturating_sub(inner.width) as i32,
            outer.height.saturating_sub(inner.height) as i32,
        ),
        _ => (0, 0),
    };
    if let Ok(Some(monitor)) = from.current_monitor() {
        let area = monitor.work_area();
        let right = area.position.x + area.size.width as i32;
        let bottom = area.position.y + area.size.height as i32;
        if g.x + g.width as i32 + frame.0 > right || g.y + g.height as i32 + frame.1 > bottom {
            g.x = area.position.x + step;
            g.y = area.position.y + step;
        }
    }
    Some(g)
}

fn show_later(app: &AppHandle, label: &str) {
    let (handle, label) = (app.clone(), label.to_owned());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SHOW_FALLBACK).await;
        page_loaded(&handle, &label);
    });
}

/// A window's page finished loading (or the fallback timer fired): show it
/// unless it starts in the tray. The first time, auto-start forwards.
pub fn page_loaded(app: &AppHandle, label: &str) {
    let windows = app.state::<Windows>();
    let show = windows
        .lock()
        .entry_mut(label)
        .is_some_and(|e| !e.shown && e.show_on_load);
    if show && let Some(window) = app.get_webview_window(label) {
        reveal_window(&window);
    }
    if windows.ready.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(AUTO_START_DELAY).await;
        handle.state::<AppState>().forwards.start_auto().await;
    });
}

/// The most recently focused open window.
pub fn preferred(app: &AppHandle) -> Option<WebviewWindow> {
    let labels = app.state::<Windows>().preferred_labels();
    labels.iter().find_map(|l| app.get_webview_window(l))
}

fn show(window: &WebviewWindow) {
    if let Some(entry) = window.state::<Windows>().lock().entry_mut(window.label()) {
        entry.shown = true;
    }
    let _ = window.show();
}

/// Show, unminimize and focus `window`.
pub fn reveal_window(window: &WebviewWindow) {
    let _ = window.unminimize();
    show(window);
    let _ = window.set_focus();
}

/// Bring Patchbay back from the tray: show every window and focus the most
/// recently focused one.
pub fn reveal(app: &AppHandle) {
    let target = preferred(app);
    for window in app.webview_windows().into_values() {
        if !window.is_visible().unwrap_or(true) {
            show(&window);
        }
    }
    if let Some(window) = target {
        reveal_window(&window);
    }
}

/// Tells every window about the current roles and windows (`windows-changed`).
pub fn changed(app: &AppHandle) {
    let info = app.state::<Windows>().info();
    let _ = app.emit("windows-changed", info);
}

pub fn focused(window: &Window) {
    let windows = window.state::<Windows>();
    let label = window.label();
    {
        let mut inner = windows.lock();
        if inner.focus.first().is_some_and(|l| l == label) {
            return;
        }
        inner.focus.retain(|l| l != label);
        inner.focus.insert(0, label.to_owned());
    }
    changed(window.app_handle());
}

/// Remembers where `window` is (moved or resized) while it is shown.
pub fn track(window: &Window) {
    if window.is_minimized().unwrap_or(true) || !window.is_visible().unwrap_or(false) {
        return;
    }
    let maximized = window.is_maximized().unwrap_or(false);
    let rect = match (maximized, window.outer_position(), window.inner_size()) {
        (false, Ok(p), Ok(s)) if s.width > 0 && s.height > 0 => Some((p, s)),
        _ => None,
    };
    let windows = window.state::<Windows>();
    let mut inner = windows.lock();
    let Some(entry) = inner.entry_mut(window.label()).filter(|e| e.shown) else {
        return;
    };
    let geometry = match (rect, entry.saved.geometry) {
        (Some((p, s)), _) => WindowGeometry {
            x: p.x,
            y: p.y,
            width: s.width,
            height: s.height,
            maximized,
        },
        // Maximized: keep the size to return to.
        (None, Some(g)) => WindowGeometry { maximized, ..g },
        (None, None) => return,
    };
    if entry.saved.geometry != Some(geometry) {
        entry.saved.geometry = Some(geometry);
        drop(inner);
        windows.save.notify_one();
    }
}

/// What closing a window means; see the module docs.
pub fn close_requested(window: &Window, api: &CloseRequestApi) {
    let Some(state) = window.try_state::<AppState>() else {
        return;
    };
    if state.quitting.load(Ordering::SeqCst) {
        return;
    }
    let label = window.label();
    let app = window.app_handle();
    let others = app.webview_windows().keys().any(|l| l != label);
    if !others {
        // The last window: close to tray (keeps sessions, terminals and
        // forwards running), or quit.
        if state.current_settings().close_to_tray {
            api.prevent_close();
            let _ = window.hide();
        }
        return;
    }
    let windows = window.state::<Windows>();
    let confirmed = windows
        .lock()
        .entry_mut(label)
        .is_some_and(|e| std::mem::take(&mut e.close_confirmed));
    if !confirmed && state.streams.has_webview(label) {
        api.prevent_close();
        let _ = app.emit_to(
            EventTarget::webview_window(label),
            CLOSE_REQUESTED_EVENT,
            (),
        );
        return;
    }
    // Closed on purpose: don't bring it back next time.
    windows.lock().windows.retain(|e| e.label != label);
    windows.save.notify_one();
}

/// Tells window `label` that tabs were moved to it, and brings it to the
/// front.
pub fn tabs_incoming(app: &AppHandle, label: &str) {
    let _ = app.emit_to(EventTarget::webview_window(label), TABS_INCOMING_EVENT, ());
    if let Some(window) = app.get_webview_window(label)
        && app
            .state::<Windows>()
            .lock()
            .entry_mut(label)
            .is_some_and(|e| e.shown)
    {
        reveal_window(&window);
    }
}

/// A window is gone: hand its roles and prompts to the others.
pub fn destroyed(window: &Window) {
    let Some(windows) = window.try_state::<Windows>() else {
        return;
    };
    {
        let mut inner = windows.lock();
        let label = window.label();
        if let Some(entry) = inner.entry_mut(label) {
            entry.open = false;
        }
        inner.focus.retain(|l| l != label);
    }
    let app = window.app_handle();
    if let Some(state) = app.try_state::<AppState>() {
        state.window_closed(app, window.label());
    }
    // A tab was being dragged out of it.
    show_tab_drag(app, window.label(), None);
    changed(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner(labels: &[&str]) -> Inner {
        Inner {
            restore_tabs: true,
            windows: labels
                .iter()
                .map(|l| Entry::new((*l).to_owned(), SavedWindow::default(), true))
                .collect(),
            focus: Vec::new(),
            migrate: false,
            shared: HashMap::new(),
            drag_over: HashMap::new(),
        }
    }

    #[test]
    fn roles_follow_focus_and_closing() {
        let mut inner = inner(&[MAIN, "window-2", "window-3"]);
        let info = inner.info();
        assert_eq!(info.leader.as_deref(), Some(MAIN));
        assert_eq!(info.active.as_deref(), Some(MAIN));
        assert_eq!(info.count, 3);

        inner.focus = vec!["window-3".into(), MAIN.into()];
        assert_eq!(inner.preferred(), ["window-3", MAIN, "window-2"]);
        assert_eq!(inner.info().active.as_deref(), Some("window-3"));
        // Windows are listed in the same order, as they describe themselves.
        let entry = inner.entry_mut("window-3").unwrap();
        entry.title = Some("htop".into());
        entry.tabs = 2;
        let listed = inner.info().windows;
        assert_eq!(
            listed.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(),
            ["window-3", MAIN, "window-2"]
        );
        assert_eq!(listed[0].title.as_deref(), Some("htop"));
        assert_eq!(listed[0].tabs, 2);
        assert_eq!(
            listed.iter().map(|w| w.number).collect::<Vec<_>>(),
            [3, 1, 2]
        );

        // The leader closes: the next oldest takes over; closed windows are
        // never preferred.
        inner.entry_mut(MAIN).unwrap().open = false;
        assert_eq!(inner.preferred(), ["window-3", "window-2"]);
        let info = inner.info();
        assert_eq!(info.leader.as_deref(), Some("window-2"));
        assert_eq!(info.count, 2);
        assert!(inner.entry_mut(MAIN).is_none());

        inner.windows.iter_mut().for_each(|e| e.open = false);
        assert_eq!(
            inner.info(),
            WindowsInfo {
                leader: None,
                active: None,
                count: 0,
                windows: Vec::new(),
            }
        );
    }

    #[test]
    fn dragged_tab_shown_in_one_window_at_a_time() {
        let mut inner = inner(&[MAIN, "window-2", "window-3"]);
        assert_eq!(inner.drag_over(MAIN, Some("window-2")), None);
        assert_eq!(inner.drag_over(MAIN, Some("window-2")), None);
        // Over another window: the first one is told it left.
        assert_eq!(
            inner.drag_over(MAIN, Some("window-3")).as_deref(),
            Some("window-2")
        );
        // A drag out of another window doesn't interfere.
        assert_eq!(inner.drag_over("window-2", Some("window-3")), None);
        assert_eq!(inner.drag_over(MAIN, None).as_deref(), Some("window-3"));
        assert_eq!(inner.drag_over(MAIN, None), None);
        assert_eq!(
            inner.drag_over("window-2", None).as_deref(),
            Some("window-3")
        );
    }
}
