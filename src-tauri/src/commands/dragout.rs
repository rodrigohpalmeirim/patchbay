//! Local copies of remote entries for dragging them out to the OS. The
//! native drag itself is started from JS with `tauri-plugin-drag`.

use patchbay_core::model::{DragOut, FileRef};
use tauri::State;
use uuid::Uuid;

use crate::error::CommandResult;
use crate::state::AppState;

/// Starts copying `sources` into a temporary directory (a background
/// transfer; progress arrives as `transfer` core events).
#[tauri::command]
pub async fn dragout_prepare(
    state: State<'_, AppState>,
    sources: Vec<FileRef>,
) -> CommandResult<DragOut> {
    Ok(state.dragout.prepare(sources).await?)
}

/// The drag ended inside the window: stop the copy and delete it.
#[tauri::command]
pub async fn dragout_release(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    state.dragout.release(id).await;
    Ok(())
}

/// Starts a native drag of local `paths` on Linux (GTK); `onEnd` gets
/// whether a drop target accepted them. `icon` is a PNG.
///
/// macOS and Windows use `tauri-plugin-drag` from JS. Its GTK backend
/// removes the data handler when the button is released (`drop-performed`),
/// before the drop target asks for the data, so on Linux nothing ever
/// arrives; here the handler stays until the drag has ended. File URIs are
/// percent-encoded properly, too.
#[tauri::command]
pub async fn dragout_start_linux(
    app: tauri::AppHandle,
    window: tauri::Window,
    webview: tauri::Webview,
    paths: Vec<String>,
    icon: Vec<u8>,
    on_end: tauri::ipc::Channel<bool>,
) -> CommandResult<()> {
    #[cfg(target_os = "linux")]
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let _ = tx.send(gtk_drag::start(&window, webview, &paths, &icon, on_end));
        })
        .map_err(|e| crate::error::CommandError {
            kind: "other",
            message: format!("couldn't start dragging: {e}"),
        })?;
        rx.await
            .map_err(|_| "the drag was not started".to_owned())
            .and_then(|r| r)
            .map_err(|message| crate::error::CommandError {
                kind: "other",
                message,
            })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, window, webview, paths, icon, on_end);
        Err(
            patchbay_core::Error::Unsupported("use tauri-plugin-drag on this platform".into())
                .into(),
        )
    }
}

#[cfg(target_os = "linux")]
mod gtk_drag {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gtk::gdk::{self, DragAction};
    use gtk::gdk_pixbuf::PixbufLoader;
    use gtk::glib::translate::{ToGlibPtr, ToGlibPtrMut};
    use gtk::glib::{Propagation, SignalHandlerId};
    use gtk::prelude::*;

    pub fn start(
        window: &tauri::Window,
        webview: tauri::Webview,
        paths: &[String],
        icon: &[u8],
        on_end: tauri::ipc::Channel<bool>,
    ) -> Result<(), String> {
        let widget = window.gtk_window().map_err(|e| e.to_string())?;
        let uris = paths
            .iter()
            .map(|p| {
                gtk::glib::filename_to_uri(p, None)
                    .map(|u| u.to_string())
                    .map_err(|e| format!("{p}: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let handlers: Rc<RefCell<Vec<SignalHandlerId>>> = Rc::default();
        let failed = Rc::new(Cell::new(false));
        let data = widget.connect_drag_data_get(move |_, _, selection, _, _| {
            let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
            selection.set_uris(&uris);
        });
        let failed_handler = {
            let failed = failed.clone();
            widget.connect_drag_failed(move |_, _, _| {
                failed.set(true);
                Propagation::Proceed
            })
        };
        let end = {
            let (handlers, failed) = (handlers.clone(), failed.clone());
            widget.connect_drag_end(move |widget, _| {
                for id in handlers.borrow_mut().drain(..) {
                    widget.disconnect(id);
                }
                release_button(&webview);
                let _ = on_end.send(!failed.get());
            })
        };
        handlers.borrow_mut().extend([data, failed_handler, end]);

        let targets = gtk::TargetList::new(&[]);
        targets.add_uri_targets(0);
        let Some(context) =
            widget.drag_begin_with_coordinates(&targets, DragAction::COPY, 1, None, -1, -1)
        else {
            for id in handlers.borrow_mut().drain(..) {
                widget.disconnect(id);
            }
            return Err("couldn't start dragging (is the mouse button still held?)".into());
        };
        let loader = PixbufLoader::new();
        if loader.write(icon).and_then(|()| loader.close()).is_ok()
            && let Some(pixbuf) = loader.pixbuf()
        {
            context.drag_set_icon_pixbuf(&pixbuf, -12, -10);
        }
        Ok(())
    }

    /// The drag's pointer grab swallowed the button release, so WebKit still
    /// thinks the button is down and would ignore the next press: tell it
    /// the button is up.
    fn release_button(webview: &tauri::Webview) {
        let _ = webview.with_webview(|platform| {
            let widget = platform.inner().upcast::<gtk::Widget>();
            let device = widget.display().default_seat().and_then(|s| s.pointer());
            let (Some(window), Some(device)) = (widget.window(), device) else {
                return;
            };
            let mut event = gdk::Event::new(gdk::EventType::ButtonRelease);
            event.set_device(Some(&device));
            // SAFETY: `event` is a button event owned by us; the window
            // reference handed over is released when the event is freed.
            unsafe {
                let raw: *mut gdk::ffi::GdkEvent = event.to_glib_none_mut().0;
                let button = &mut (*raw).button;
                button.window = window.to_glib_full();
                button.send_event = 1;
                button.time = gdk::ffi::GDK_CURRENT_TIME as u32;
                button.x = -1.0;
                button.y = -1.0;
                button.state = gdk::ffi::GDK_BUTTON1_MASK;
                button.button = 1;
            }
            gtk::main_do_event(&mut event);
        });
    }
}
