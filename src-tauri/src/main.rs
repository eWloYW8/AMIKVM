#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod captures;
mod clipboard;
mod commands;
mod diagnostics;
mod folders;
mod keyboard;
#[macro_use]
mod locale;
mod media;
mod mouse;
mod playback;
mod recording;
mod recordings;
mod session;
mod shutdown;
mod ui;
mod video;

use commands::AppState;
use tauri::Manager;

fn main() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let builder = builder.plugin(tauri_plugin_clipboard_manager::init());
    builder
        .setup(|app| {
            let path = app.path().app_config_dir()?.join("servers.json");
            app.manage(AppState::new(path)?);
            keyboard::install(app.handle())?;
            diagnostics::record(
                app.handle(),
                amikvm_core::diagnostics::Level::Info,
                amikvm_core::diagnostics::Category::Application,
                None,
                "应用已启动",
                concat!(
                    env!("CARGO_PKG_VERSION"),
                    " · ",
                    env!("AMIKVM_BUILD_TARGET")
                ),
            );
            diagnostics::install(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_servers,
            commands::save_server,
            commands::delete_server,
            commands::connect_server,
            commands::disconnect_server,
            commands::send_control,
            commands::send_hid,
            commands::session_snapshot,
            ui::ui_snapshot,
            ui::ui_action,
            ui::ui_input,
            commands::subscribe_video,
            commands::unsubscribe_video,
        ])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Focused(false)) {
                let app = window.app_handle().clone();
                if let Some(state) = app.try_state::<AppState>() {
                    if let Ok(sessions) = state.sessions.try_lock() {
                        for session in sessions.values() {
                            session.cancel_text();
                        }
                    }
                }
                tauri::async_runtime::spawn(async move {
                    if let Some(state) = app.try_state::<AppState>() {
                        let sessions: Vec<_> =
                            state.sessions.lock().await.values().cloned().collect();
                        for session in sessions {
                            let _ = session.input(amikvm_core::input::Event::ReleaseAll).await;
                        }
                    }
                });
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if !app
                    .state::<AppState>()
                    .exit_ready
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    api.prevent_close();
                    shutdown::request(app, amikvm_core::sharing::exit::Scope::Application);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("AMIKVM desktop failed to start")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !app
                    .state::<AppState>()
                    .exit_ready
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    api.prevent_exit();
                    shutdown::request(app, amikvm_core::sharing::exit::Scope::Application);
                }
            }
        });
}
