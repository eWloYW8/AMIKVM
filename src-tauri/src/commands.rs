use crate::session::{Session, Snapshot};
use amikvm_core::{
    auth::WebSession,
    protocol::Control,
    server::{Server, ServerInput, ServerStore},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

pub struct AppState {
    pub(crate) store: Mutex<ServerStore>,
    pub(crate) macros: Mutex<amikvm_core::input::macros::Store>,
    pub(crate) preferences: Mutex<amikvm_core::preferences::Store>,
    pub(crate) diagnostics: Arc<amikvm_core::diagnostics::Recorder>,
    pub(crate) log_dialog: AsyncMutex<()>,
    pub(crate) host_keyboard: crate::keyboard::Host,
    pub(crate) sessions: AsyncMutex<HashMap<Uuid, Arc<Session>>>,
    pub(crate) connecting: Mutex<std::collections::HashSet<Uuid>>,
    pub(crate) connection_changes: tokio::sync::Notify,
    pub(crate) ui: Mutex<crate::ui::UiState>,
    pub(crate) folders: Arc<crate::folders::Manager>,
    pub(crate) playback: Arc<crate::playback::Manager>,
    pub(crate) shutdown: crate::shutdown::Manager,
    pub(crate) closing_servers: Mutex<std::collections::HashSet<Uuid>>,
    pub(crate) shutting_down: std::sync::atomic::AtomicBool,
    pub(crate) exit_ready: std::sync::atomic::AtomicBool,
}

pub(crate) type Response<T> = Result<T, String>;

impl AppState {
    pub fn connection_closing(&self, id: Uuid) -> bool {
        self.shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
            || self
                .closing_servers
                .lock()
                .map(|s| s.contains(&id))
                .unwrap_or(true)
    }
    pub fn new(path: PathBuf) -> amikvm_core::Result<Self> {
        let folders = crate::folders::Manager::open(path.parent().unwrap().join("folders"))?;
        let macros =
            amikvm_core::input::macros::Store::open(path.parent().unwrap().join("macros.json"))?;
        let preferences =
            amikvm_core::preferences::Store::open(path.parent().unwrap().join("preferences.json"))?;
        let ui = crate::ui::UiState {
            language: preferences.language(),
            ..Default::default()
        };
        Ok(Self {
            store: Mutex::new(ServerStore::open(path)?),
            macros: Mutex::new(macros),
            preferences: Mutex::new(preferences),
            diagnostics: Arc::new(Default::default()),
            log_dialog: AsyncMutex::new(()),
            host_keyboard: Default::default(),
            sessions: AsyncMutex::new(HashMap::new()),
            connecting: Mutex::new(Default::default()),
            connection_changes: tokio::sync::Notify::new(),
            ui: Mutex::new(ui),
            folders: Arc::new(folders),
            playback: Arc::new(crate::playback::Manager::default()),
            shutdown: Default::default(),
            closing_servers: Default::default(),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            exit_ready: std::sync::atomic::AtomicBool::new(false),
        })
    }
}

fn credential(id: Uuid) -> Response<keyring::Entry> {
    keyring::Entry::new("io.amikvm.desktop", &id.to_string())
        .map_err(|e| format!("Credential store: {e}"))
}

#[tauri::command]
pub fn list_servers(state: State<'_, AppState>) -> Response<Vec<Server>> {
    Ok(state
        .store
        .lock()
        .map_err(|_| "Server database unavailable")?
        .list())
}

#[tauri::command]
pub async fn save_server(
    state: State<'_, AppState>,
    mut input: ServerInput,
    password: Option<String>,
    remember_password: bool,
) -> Response<Server> {
    if let Some(password) = password.as_deref() {
        state.diagnostics.protect(password);
    }
    input.validate().map_err(|e| e.to_string())?;
    if let Some(id) = input.id {
        if state
            .sessions
            .lock()
            .await
            .get(&id)
            .is_some_and(|s| s.snapshot.lock().is_ok_and(|v| crate::ui::active(&v)))
            || state
                .connecting
                .lock()
                .map_err(|_| "Session state unavailable")?
                .contains(&id)
        {
            return Err("Disconnect this server before editing it".into());
        }
    }
    let mut store = state
        .store
        .lock()
        .map_err(|_| "Server database unavailable")?;
    let previous = input
        .id
        .map(|id| store.get(id))
        .transpose()
        .map_err(|e| e.to_string())?;
    let id = input.id.unwrap_or_else(Uuid::new_v4);
    input.id = Some(id);
    let saved = if remember_password {
        if let Some(password) = password.filter(|p| !p.is_empty()) {
            credential(id)?
                .set_password(&password)
                .map_err(|e| format!("Could not save password: {e}"))?;
            true
        } else {
            previous.as_ref().is_some_and(|s| s.credential_saved)
        }
    } else {
        false
    };
    let server = store.save(input, saved).map_err(|e| e.to_string())?;
    if !remember_password && previous.as_ref().is_some_and(|s| s.credential_saved) {
        let _ = credential(server.id)?.delete_credential();
    }
    state.diagnostics.push(
        amikvm_core::diagnostics::Level::Info,
        amikvm_core::diagnostics::Category::Server,
        Some(server.id),
        "服务器已保存",
        "",
    );
    Ok(server)
}

#[tauri::command]
pub async fn delete_server(state: State<'_, AppState>, id: Uuid) -> Response<()> {
    if state.folders.contains_server(id) {
        return Err("请先同步或丢弃该服务器的文件夹映射".into());
    }
    if state
        .sessions
        .lock()
        .await
        .get(&id)
        .is_some_and(|s| s.snapshot.lock().is_ok_and(|v| crate::ui::active(&v)))
        || state
            .connecting
            .lock()
            .map_err(|_| "Session state unavailable")?
            .contains(&id)
    {
        return Err("Disconnect this server before deleting it".into());
    }
    state
        .store
        .lock()
        .map_err(|_| "Server database unavailable")?
        .remove(id)
        .map_err(|e| e.to_string())?;
    let _ = credential(id).and_then(|entry| entry.delete_credential().map_err(|e| e.to_string()));
    state.diagnostics.push(
        amikvm_core::diagnostics::Level::Info,
        amikvm_core::diagnostics::Category::Server,
        Some(id),
        "服务器已删除",
        "",
    );
    Ok(())
}

#[tauri::command]
pub async fn subscribe_video(
    state: State<'_, AppState>,
    id: Uuid,
    on_frame: tauri::ipc::Channel<tauri::ipc::Response>,
) -> Response<u32> {
    let session_video = {
        let sessions = state.sessions.lock().await;
        sessions.get(&id).map(|s| s.video.clone()).or_else(|| {
            sessions
                .values()
                .find(|s| s.captures.id == id)
                .map(|s| s.captures.video.clone())
        })
    };
    let video = if let Some(video) = session_video {
        video
    } else {
        state
            .playback
            .get(id)
            .ok_or("Video source not found")?
            .video
            .clone()
    };
    video
        .lock()
        .map_err(|_| "Video state unavailable")?
        .subscribe(on_frame)
}

#[tauri::command]
pub async fn unsubscribe_video(
    state: State<'_, AppState>,
    id: Uuid,
    channel_id: u32,
) -> Response<()> {
    let session_video = {
        let sessions = state.sessions.lock().await;
        sessions.get(&id).map(|s| s.video.clone()).or_else(|| {
            sessions
                .values()
                .find(|s| s.captures.id == id)
                .map(|s| s.captures.video.clone())
        })
    };
    if let Some(video) = session_video {
        video
            .lock()
            .map_err(|_| "Video state unavailable")?
            .unsubscribe(channel_id);
    } else if let Some(player) = state.playback.get(id) {
        player
            .video
            .lock()
            .map_err(|_| "Video state unavailable")?
            .unsubscribe(channel_id);
    }
    Ok(())
}

#[tauri::command]
pub async fn connect_server(
    app: AppHandle,
    state: State<'_, AppState>,
    id: Uuid,
    password: Option<String>,
) -> Response<Snapshot> {
    connect_server_mode(app, state, id, password, false).await
}

pub async fn connect_server_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    id: Uuid,
    password: Option<String>,
    web_only: bool,
) -> Response<Snapshot> {
    if state.connection_closing(id) {
        return Err("Application is closing".into());
    }
    if state.shutdown.blocks_connection(id) {
        return Err("请先处理当前关闭操作。".into());
    }
    {
        let mut connecting = state
            .connecting
            .lock()
            .map_err(|_| "Session state unavailable")?;
        if state.connection_closing(id) {
            return Err("Application is closing".into());
        }
        if !connecting.insert(id) {
            return Err("Connection already in progress".into());
        }
    }
    let _ = app.emit("ui-changed", serde_json::json!({}));
    let result = async {
        let previous = {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get(&id) {
                let snapshot = session.snapshot.lock().map_err(|_| "Session unavailable")?;
                if !["error", "disconnected"].contains(&snapshot.phase.as_str())
                    && !(snapshot.phase == "connected" && snapshot.web_only && !web_only)
                {
                    return Err("Server already connected".into());
                }
            }
            sessions.remove(&id)
        };
        // Wait for old download/capture jobs and Web logout before a new login.
        if let Some(previous) = previous {
            previous.stop().await;
        }
        let server = state
            .store
            .lock()
            .map_err(|_| "Server database unavailable")?
            .get(id)
            .map_err(|e| e.to_string())?;
        let password = match password.filter(|s| !s.is_empty()) {
            Some(password) => password,
            None => credential(id)?.get_password().map_err(|_| {
                "No saved password is available; enter a password to connect".to_owned()
            })?,
        };
        state.diagnostics.protect(&password);
        state.diagnostics.push(
            amikvm_core::diagnostics::Level::Info,
            amikvm_core::diagnostics::Category::Session,
            Some(id),
            "正在登录服务器",
            if web_only { "web" } else { "console" },
        );
        let web = if web_only {
            WebSession::login_web(server, &password).await
        } else {
            WebSession::login(server, &password).await
        }
        .map_err(|e| e.to_string())?;
        web.protect_diagnostics(&state.diagnostics);
        if state.connection_closing(id) {
            let _ = web.logout().await;
            return Err("Application is closing".into());
        }
        let session = Arc::new(
            Session::start(app.clone(), web, web_only)
                .await
                .map_err(|e| e.to_string())?,
        );
        if state.connection_closing(id) {
            session.stop().await;
            return Err("Application is closing".into());
        }
        let snapshot = session
            .snapshot
            .lock()
            .map_err(|_| "Session unavailable")?
            .clone();
        state.sessions.lock().await.insert(id, session);
        state
            .store
            .lock()
            .map_err(|_| "Server database unavailable")?
            .connected(id)
            .map_err(|e| e.to_string())?;
        Ok(snapshot)
    }
    .await;
    state
        .connecting
        .lock()
        .map_err(|_| "Session state unavailable")?
        .remove(&id);
    state.connection_changes.notify_waiters();
    if let Err(error) = &result {
        state.diagnostics.push(
            amikvm_core::diagnostics::Level::Error,
            amikvm_core::diagnostics::Category::Session,
            Some(id),
            "连接失败",
            error,
        );
    }
    let _ = app.emit("ui-changed", serde_json::json!({}));
    result
}

#[tauri::command]
pub async fn disconnect_server(
    app: AppHandle,
    state: State<'_, AppState>,
    id: Uuid,
) -> Response<()> {
    state
        .shutdown
        .request(&app, amikvm_core::sharing::exit::Scope::Server(id))
}

#[tauri::command]
pub async fn session_snapshot(state: State<'_, AppState>) -> Response<Vec<Snapshot>> {
    state
        .sessions
        .lock()
        .await
        .values()
        .map(|s| {
            s.snapshot
                .lock()
                .map(|v| v.clone())
                .map_err(|_| "Session unavailable".into())
        })
        .collect()
}

#[tauri::command]
pub async fn send_control(state: State<'_, AppState>, id: Uuid, control: Control) -> Response<()> {
    let session = state
        .sessions
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or("Session not found")?;
    session.control(control).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_hid(
    state: State<'_, AppState>,
    id: Uuid,
    mouse: bool,
    report: Vec<u8>,
) -> Response<()> {
    let session = state
        .sessions
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or("Session not found")?;
    session.hid(mouse, &report).await.map_err(|e| e.to_string())
}
