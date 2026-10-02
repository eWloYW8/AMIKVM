//! Native cursor movement is serialized with the session writer, on the UI thread.
use amikvm_core::{Result, input};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, Manager, PhysicalPosition};

static SUPPORTED: OnceLock<bool> = OnceLock::new();
pub fn install() {
    #[cfg(target_os = "linux")]
    let supported = {
        use gtk::{gdk, glib::ObjectExt};
        gdk::Display::default().is_some_and(|d| d.type_().name().contains("X11"))
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let supported = true;
    let _ = SUPPORTED.set(supported);
}
pub fn supported() -> bool {
    *SUPPORTED.get().unwrap_or(&false)
}
pub fn state() -> input::cursor::State {
    let mut state = input::cursor::State::default();
    state.supported = supported();
    state.message =
        (!supported()).then(|| "当前显示系统不支持移动本机光标，可使用相对鼠标捕获。".into());
    state
}
pub fn position(
    app: &AppHandle,
    snapshot: &Arc<Mutex<crate::session::Snapshot>>,
) -> Option<(f64, f64)> {
    let viewport = snapshot.lock().ok().and_then(|s| {
        (s.local_cursor.supported && s.mouse_mode == Some(1) && !s.mouse_capture.requested())
            .then_some(s.local_cursor.viewport)
            .flatten()
    })?;
    let window = app.get_webview_window("main")?;
    let (x, y) = current(&window, viewport.scale).ok()?;
    Some((x - viewport.bounds.left, y - viewport.bounds.top))
}

pub async fn follow(
    app: &AppHandle,
    snapshot: &Arc<Mutex<crate::session::Snapshot>>,
    expected: Option<(f64, f64)>,
    buttons: u8,
) -> Result<()> {
    let Some((point, viewport, revision)) = snapshot.lock().ok().and_then(|s| {
        (s.local_cursor.supported
            && s.mouse_mode == Some(1)
            && !s.mouse_capture.requested()
            && !(s.mouse.active() && s.mouse.paused))
            .then(|| {
                Some((
                    s.mouse.reference?,
                    s.local_cursor.viewport?,
                    s.local_cursor.revision,
                ))
            })?
    }) else {
        return Ok(());
    };
    let handle = app.clone();
    let state = snapshot.clone();
    let (reply, completed) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let moved = (|| {
            let window = handle.get_webview_window("main")?;
            if !window.is_focused().unwrap_or(false) {
                return None;
            }
            let id = state.lock().ok()?.server_id;
            if !crate::pointer_capture::selected(&handle, id) {
                return None;
            }
            let (x, y) = viewport.target(point)?;
            let physical =
                PhysicalPosition::new((x * viewport.scale).round(), (y * viewport.scale).round());
            let size = window.inner_size().ok()?;
            if physical.x < 0.
                || physical.y < 0.
                || physical.x >= f64::from(size.width)
                || physical.y >= f64::from(size.height)
            {
                return None;
            }
            if let Some((event_x, event_y)) = expected {
                let (current_x, current_y) = current(&window, viewport.scale).ok()?;
                // A queued motion may have been superseded by the user's next
                // motion or by leaving the canvas. It must not pull them back.
                if (current_x - viewport.bounds.left - event_x).abs() > 2. / viewport.scale
                    || (current_y - viewport.bounds.top - event_y).abs() > 2. / viewport.scale
                {
                    return None;
                }
            }
            {
                let mut s = state.lock().ok()?;
                if s.local_cursor.revision != revision
                    || s.mouse.reference != Some(point)
                    || s.mouse_mode != Some(1)
                    || !s.video_connected
                    || !s.video_signal
                    || !s.can_control
                    || s.mouse_capture.requested()
                    || (s.mouse.active() && s.mouse.paused)
                {
                    return None;
                }
                let local_x = physical.x / viewport.scale - viewport.bounds.left;
                let local_y = physical.y / viewport.scale - viewport.bounds.top;
                s.local_cursor.warped(local_x, local_y, buttons);
                s.mouse_capture.baseline(
                    local_x,
                    local_y,
                    viewport.bounds.width.round() as u32,
                    viewport.bounds.height.round() as u32,
                );
                if s.mouse.active() && !s.input_focused {
                    s.local_cursor.focus = Some(uuid::Uuid::new_v4());
                }
            }
            // No snapshot/UI mutex is retained across a native window call.
            if let Err(error) = move_cursor(&window, physical) {
                if let Ok(mut s) = state.lock() {
                    s.local_cursor.cancel();
                    s.local_cursor.message = Some("本机光标移动失败，校准已暂停。".into());
                    s.mouse.suspend();
                }
                crate::diagnostics::record(
                    &handle,
                    amikvm_core::diagnostics::Level::Warning,
                    amikvm_core::diagnostics::Category::Input,
                    Some(id),
                    "本机光标移动失败，校准已暂停。",
                    error.to_string(),
                );
            }
            Some(())
        })();
        let _ = reply.send(moved.is_some());
    })
    .map_err(|e| amikvm_core::Error::Invalid(e.to_string()))?;
    if completed.await.unwrap_or(false) {
        crate::mouse::notify(app, snapshot);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn current(window: &tauri::WebviewWindow, scale: f64) -> std::result::Result<(f64, f64), String> {
    let p = window.cursor_position().map_err(|e| e.to_string())?;
    let origin = window.inner_position().map_err(|e| e.to_string())?;
    Ok((
        (p.x - f64::from(origin.x)) / scale,
        (p.y - f64::from(origin.y)) / scale,
    ))
}

#[cfg(target_os = "macos")]
fn current(window: &tauri::WebviewWindow, scale: f64) -> std::result::Result<(f64, f64), String> {
    #[repr(C)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreate(source: *const std::ffi::c_void) -> *const std::ffi::c_void;
        fn CGEventGetLocation(event: *const std::ffi::c_void) -> Point;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const std::ffi::c_void);
    }
    let event = unsafe { CGEventCreate(std::ptr::null()) };
    if event.is_null() {
        return Err("Could not read cursor position".into());
    }
    let p = unsafe { CGEventGetLocation(event) };
    unsafe {
        CFRelease(event);
    }
    let origin = window.inner_position().map_err(|e| e.to_string())?;
    let factor = window.scale_factor().map_err(|e| e.to_string())?;
    Ok((
        (p.x - f64::from(origin.x) / factor) * factor / scale,
        (p.y - f64::from(origin.y) / factor) * factor / scale,
    ))
}

#[cfg(not(target_os = "linux"))]
fn move_cursor(
    window: &tauri::WebviewWindow,
    p: PhysicalPosition<f64>,
) -> std::result::Result<(), String> {
    window.set_cursor_position(p).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
fn move_cursor(
    window: &tauri::WebviewWindow,
    p: PhysicalPosition<f64>,
) -> std::result::Result<(), String> {
    use gtk::{gdk::prelude::*, prelude::*};
    let gtk = window.gtk_window().map_err(|e| e.to_string())?;
    let display = gtk.display();
    let pointer = display
        .default_seat()
        .and_then(|s| s.pointer())
        .ok_or("No native pointer")?;
    let screen = gtk::prelude::GtkWindowExt::screen(&gtk).ok_or("No native screen")?;
    let origin = window.inner_position().map_err(|e| e.to_string())?;
    let factor = window.scale_factor().map_err(|e| e.to_string())?;
    // GDK warp takes root logical coordinates. Tao 0.37 adds a physical
    // window origin to a logical offset, which is incorrect at HiDPI.
    let x = ((f64::from(origin.x) + p.x) / factor).round() as i32;
    let y = ((f64::from(origin.y) + p.y) / factor).round() as i32;
    pointer.warp(&screen, x, y);
    display.sync();
    let (_, actual_x, actual_y) = pointer.position_double();
    if (actual_x - f64::from(x)).abs() > 1. || (actual_y - f64::from(y)).abs() > 1. {
        return Err("Native cursor did not reach the requested position".into());
    }
    Ok(())
}
