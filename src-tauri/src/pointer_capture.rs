//! Rust owns capture policy; the renderer only operates the system webview API.
use crate::session::Snapshot;
use tauri::Manager;
use uuid::Uuid;

pub fn eligible(s: &Snapshot) -> bool {
    s.phase == "connected"
        && s.video_connected
        && s.video_signal
        && s.can_control
        && matches!(s.mouse_mode, Some(1 | 3))
        && !s.mouse.active()
}

pub fn selected(app: &tauri::AppHandle, id: Uuid) -> bool {
    app.try_state::<crate::commands::AppState>()
        .is_some_and(|state| {
            !state.shutdown.blocks_connection(id)
                && state.ui.lock().is_ok_and(|ui| {
                    ui.selected == Some(id)
                        && matches!(ui.dialog, crate::ui::Dialog::None)
                        && !ui.paused.contains(&id)
                        && !ui.playback_selected
                })
        })
}

pub async fn release_inactive(app: &tauri::AppHandle) {
    let state = app.state::<crate::commands::AppState>();
    let sessions: Vec<_> = state.sessions.lock().await.values().cloned().collect();
    for session in sessions {
        let captured = session
            .snapshot
            .lock()
            .ok()
            .filter(|s| s.mouse_capture.requested())
            .map(|s| s.server_id);
        if captured.is_some_and(|id| !selected(app, id)) {
            let _ = session.input(amikvm_core::input::Event::ReleaseAll).await;
        }
    }
}

pub fn install(app: &tauri::AppHandle) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    if let Some(window) = app.get_webview_window("main") {
        use webkit2gtk::{PermissionRequestExt, WebViewExt, glib::ObjectExt};
        let handle = app.clone();
        window.with_webview(move |webview| {
            webview
                .inner()
                .connect_permission_request(move |_, request| {
                    if !request.is::<webkit2gtk::PointerLockPermissionRequest>() {
                        return false;
                    }
                    let allowed =
                        handle
                            .try_state::<crate::commands::AppState>()
                            .is_some_and(|state| {
                                let id = state.ui.lock().ok().and_then(|ui| ui.selected);
                                id.is_some_and(|id| {
                                    selected(&handle, id)
                                        && state.sessions.try_lock().is_ok_and(|sessions| {
                                            sessions.get(&id).is_some_and(|session| {
                                                session.snapshot.lock().is_ok_and(|s| {
                                                    eligible(&s) && s.mouse_capture.requested()
                                                })
                                            })
                                        })
                                })
                            });
                    if allowed {
                        request.allow();
                    } else {
                        request.deny();
                    }
                    true
                });
        })?;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = app;
    Ok(())
}
