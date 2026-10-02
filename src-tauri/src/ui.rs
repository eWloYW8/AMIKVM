//! Rust owns navigation, forms, presentation models, action routing and input conversion.
//! The webview renders this declarative tree and forwards browser interaction data.
mod view;

use crate::{
    commands::{self, AppState, Response},
    session::Snapshot,
};
use amikvm_core::{
    protocol::Control,
    server::{ApiMode, ServerInput},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

#[derive(Default, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    All,
    Favorites,
    Recent,
    Logs,
}

#[derive(Default, Clone)]
pub enum Dialog {
    #[default]
    None,
    Server(Option<Uuid>),
    Password(Uuid, Option<amikvm_core::video::remote_capture::Kind>),
    Text(Uuid),
    Media(Uuid, amikvm_core::media::scsi::Kind),
    Folder(Uuid),
    FolderSync(Uuid),
    Macros(Option<Uuid>),
    MacroEdit(Option<Uuid>, Option<Uuid>),
    Recordings(Uuid),
    Captures(Uuid),
    Ipmi(Uuid),
    Boot(Uuid),
    Sharing(Uuid),
    Confirmation,
    About,
    Connection(Uuid),
}

#[derive(Clone)]
pub struct Confirmation {
    pub id: Uuid,
    pub message: String,
    pub intent: Value,
    pub previous: Dialog,
}

#[derive(Default, Clone, Copy)]
pub enum Zoom {
    #[default]
    Fit,
    Actual,
    Host,
    Percent(u16),
}

impl Zoom {
    pub fn value(self) -> String {
        match self {
            Self::Fit => "fit".into(),
            Self::Actual => "actual".into(),
            Self::Host => "host".into(),
            Self::Percent(percent) => percent.to_string(),
        }
    }
    pub fn percent(self) -> u16 {
        match self {
            Self::Percent(percent) => percent,
            _ => 100,
        }
    }
}

#[derive(Default, Clone)]
pub struct UiState {
    pub language: amikvm_core::preferences::Language,
    pub view: View,
    pub query: String,
    pub selected: Option<Uuid>,
    pub dialog: Dialog,
    pub menu: Option<Uuid>,
    pub paused: HashSet<Uuid>,
    pub hidden_local_cursor: HashSet<Uuid>,
    pub mouse_settings: HashMap<Uuid, amikvm_core::input::mouse::Settings>,
    pub zoom: HashMap<Uuid, Zoom>,
    pub error: Option<String>,
    pub folders: Vec<crate::folders::Snapshot>,
    pub macros: Vec<amikvm_core::input::macros::Macro>,
    pub soft_keyboard: HashSet<Uuid>,
    pub soft_layout: HashMap<Uuid, amikvm_core::input::layout::Layout>,
    pub host_keyboard: crate::keyboard::Snapshot,
    pub playback_selected: bool,
    pub playback: Option<crate::playback::Snapshot>,
    pub recordings: HashMap<Uuid, crate::recordings::Snapshot>,
    pub captures: HashMap<Uuid, crate::captures::Snapshot>,
    pub recording_seconds: HashMap<Uuid, u16>,
    pub recording_policies: HashMap<Uuid, amikvm_core::recording::Policy>,
    pub confirmation: Option<Confirmation>,
    pub close_plan: Option<amikvm_core::sharing::exit::Plan>,
    pub log_filter: amikvm_core::diagnostics::Filter,
    pub logs: amikvm_core::diagnostics::Snapshot,
}

#[derive(Serialize)]
pub struct Node {
    pub kind: &'static str,
    pub props: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

#[derive(Serialize)]
pub struct Model {
    pub root: Node,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Intent {
    About,
    ConnectionInfo {
        id: Uuid,
    },
    LogConfigure {
        values: Value,
    },
    LogFilter {
        values: Value,
    },
    LogPage {
        value: usize,
    },
    LogClear,
    LogFile,
    LogExport,
    SetLanguage {
        value: amikvm_core::preferences::Language,
    },
    ConfirmAction {
        message: String,
        intent: Value,
    },
    ConfirmApply {
        id: Uuid,
    },
    Navigate {
        view: View,
    },
    Search {
        value: String,
    },
    Select {
        id: Uuid,
    },
    Add,
    Edit {
        id: Uuid,
    },
    Menu {
        id: Uuid,
    },
    CloseDialog,
    DismissError,
    InteractionError {
        message: String,
    },
    Connect {
        id: Uuid,
    },
    Password {
        id: Uuid,
        values: Value,
        #[serde(default)]
        capture: Option<amikvm_core::video::remote_capture::Kind>,
    },
    Save {
        values: Value,
    },
    Disconnect {
        id: Uuid,
    },
    Favorite {
        id: Uuid,
    },
    Remove {
        id: Uuid,
    },
    Control {
        id: Uuid,
        control: Control,
        #[serde(default)]
        value: Option<Value>,
    },
    Pause {
        id: Uuid,
    },
    LocalCursor {
        id: Uuid,
    },
    Mouse {
        id: Uuid,
        command: amikvm_core::input::mouse::Command,
        token: Option<Uuid>,
    },
    MouseSettings {
        id: Uuid,
        values: Value,
    },
    CaptureDialog {
        id: Uuid,
        kind: amikvm_core::video::remote_capture::Kind,
    },
    CaptureRefresh {
        id: Uuid,
        kind: amikvm_core::video::remote_capture::Kind,
    },
    CaptureCancel {
        id: Uuid,
    },
    CaptureSave {
        id: Uuid,
    },
    Shortcut {
        id: Uuid,
        name: String,
    },
    MacroDialog {
        id: Option<Uuid>,
    },
    MacroEdit {
        id: Option<Uuid>,
        macro_id: Option<Uuid>,
    },
    MacroSave {
        id: Option<Uuid>,
        macro_id: Option<Uuid>,
        values: Value,
    },
    MacroRemove {
        macro_id: Uuid,
    },
    MacroRun {
        id: Uuid,
        macro_id: Uuid,
    },
    SoftKeyboard {
        id: Uuid,
    },
    SoftModifier {
        id: Uuid,
        code: String,
    },
    SoftLayout {
        id: Uuid,
        value: String,
    },
    Fullscreen,
    Quit,
    ExitChoose {
        plan: Uuid,
        server: Uuid,
        value: String,
    },
    ExitFinish {
        plan: Uuid,
        transfer: bool,
    },
    ExitCancel {
        plan: Uuid,
    },
    Ipmi {
        id: Uuid,
        values: Value,
    },
    IpmiDialog {
        id: Uuid,
    },
    IpmiClear {
        id: Uuid,
    },
    BootDialog {
        id: Uuid,
    },
    BootRefresh {
        id: Uuid,
    },
    BootApply {
        id: Uuid,
        values: Value,
    },
    Input {
        id: Uuid,
        event: amikvm_core::input::Event,
    },
    Share {
        id: Uuid,
        operation: String,
        user_id: u8,
        #[serde(default)]
        request_token: Option<Uuid>,
        #[serde(default)]
        identity: Option<amikvm_core::sharing::Identity>,
    },
    SharingPolicy {
        id: Uuid,
        value: amikvm_core::sharing::Policy,
    },
    SharingDialog {
        id: Uuid,
    },
    Record {
        id: Uuid,
        #[serde(default)]
        values: Option<Value>,
    },
    RecordPause {
        id: Uuid,
    },
    RecordingsDialog {
        id: Uuid,
    },
    RecordingsRefresh {
        id: Uuid,
    },
    RecordingsDownload {
        id: Uuid,
        file: String,
        #[serde(default)]
        play: bool,
    },
    RecordingsCancel {
        id: Uuid,
    },
    PlaybackView,
    PlaybackOpen,
    PlaybackControl {
        id: Uuid,
        operation: String,
    },
    PlaybackSeek {
        id: Uuid,
        value: String,
    },
    PlaybackClose {
        id: Uuid,
    },
    Capture {
        id: Uuid,
    },
    Zoom {
        id: Uuid,
        value: String,
    },
    ZoomStep {
        id: Uuid,
        direction: i8,
    },
    VideoConfig {
        id: Uuid,
        setting: String,
        value: String,
    },
    TextDialog {
        id: Uuid,
    },
    Text {
        id: Uuid,
        values: Value,
    },
    MediaDialog {
        id: Uuid,
        kind: amikvm_core::media::scsi::Kind,
    },
    MediaStart {
        id: Uuid,
        kind: amikvm_core::media::scsi::Kind,
        values: Value,
    },
    MediaStop {
        id: Uuid,
        kind: amikvm_core::media::scsi::Kind,
        slot: u8,
    },
    FolderDialog {
        id: Uuid,
    },
    FolderStart {
        id: Uuid,
        values: Value,
    },
    FolderPrepare {
        id: Uuid,
    },
    FolderApply {
        id: Uuid,
        overwrite: bool,
    },
    FolderDiscard {
        id: Uuid,
    },
    FolderCancel {
        id: Uuid,
    },
}

pub fn active(snapshot: &Snapshot) -> bool {
    matches!(
        snapshot.phase.as_str(),
        "authenticating" | "negotiating" | "connected"
    )
}

pub async fn toggle_log_file(app: AppHandle) -> Response<()> {
    let state = app.state::<AppState>();
    let Ok(_guard) = state.log_dialog.try_lock() else {
        return Ok(());
    };
    if state.shutdown.snapshot().is_some()
        || state
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err("Application is closing".into());
    }
    let logger = state.diagnostics.clone();
    if logger.snapshot(&Default::default()).file.is_some() {
        logger.push(
            amikvm_core::diagnostics::Level::Info,
            amikvm_core::diagnostics::Category::Application,
            None,
            "文件日志已停止",
            "",
        );
        tauri::async_runtime::spawn_blocking(move || logger.stop_file())
            .await
            .map_err(|e| e.to_string())??;
    } else {
        let language = state
            .ui
            .lock()
            .map_err(|_| "Interface state unavailable")?
            .language;
        let picker = app.clone();
        let path = tauri::async_runtime::spawn_blocking(move || {
            picker
                .dialog()
                .file()
                .set_title(crate::locale::text_in(language, "选择追加日志文件"))
                .add_filter(
                    crate::locale::text_in(language, "日志文件"),
                    &["log", "jsonl"],
                )
                .set_file_name(format!("AMIKVM-{}.log", amikvm_core::server::now()))
                .blocking_save_file()
        })
        .await
        .map_err(|e| e.to_string())?;
        if let Some(path) = path {
            if state.shutdown.snapshot().is_some()
                || state
                    .shutting_down
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err("Application is closing".into());
            }
            let path = path.into_path().map_err(|e| e.to_string())?;
            tauri::async_runtime::spawn_blocking(move || logger.start_file(path))
                .await
                .map_err(|e| e.to_string())??;
        }
    }
    app.emit("ui-changed", ()).ok();
    Ok(())
}

async fn export_logs(app: AppHandle) -> Response<()> {
    let state = app.state::<AppState>();
    let Ok(_guard) = state.log_dialog.try_lock() else {
        return Ok(());
    };
    let (language, filter) = {
        let ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
        (ui.language, ui.log_filter.clone())
    };
    let picker = app.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        picker
            .dialog()
            .file()
            .set_title(crate::locale::text_in(language, "导出筛选后的日志"))
            .add_filter(
                crate::locale::text_in(language, "日志文件"),
                &["log", "jsonl"],
            )
            .set_file_name(format!("AMIKVM-export-{}.log", amikvm_core::server::now()))
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(path) = path {
        if state
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("Application is closing".into());
        }
        let path = path.into_path().map_err(|e| e.to_string())?;
        let logger = state.diagnostics.clone();
        let count = tauri::async_runtime::spawn_blocking(move || logger.export(&filter, &path))
            .await
            .map_err(|e| e.to_string())??;
        state.diagnostics.push(
            amikvm_core::diagnostics::Level::Info,
            amikvm_core::diagnostics::Category::Application,
            None,
            "日志已导出",
            count.to_string(),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn ui_snapshot(state: State<'_, AppState>) -> Response<Model> {
    let servers = commands::list_servers(state.clone())?;
    let mut sessions = commands::session_snapshot(state.clone()).await?;
    for id in state
        .connecting
        .lock()
        .map_err(|_| "Session state unavailable")?
        .iter()
    {
        if !sessions.iter().any(|s| s.server_id == *id) {
            sessions.push(Snapshot::pending(*id));
        }
    }
    let mut ui = state
        .ui
        .lock()
        .map_err(|_| "Interface state unavailable")?
        .clone();
    ui.close_plan = state.shutdown.snapshot();
    ui.folders = state.folders.snapshots();
    ui.playback = state.playback.snapshot();
    ui.recordings = state
        .sessions
        .lock()
        .await
        .iter()
        .map(|(id, session)| (*id, session.recordings.snapshot()))
        .collect();
    ui.captures = state
        .sessions
        .lock()
        .await
        .iter()
        .map(|(id, session)| (*id, session.captures.snapshot()))
        .collect();
    ui.macros = state.macros.lock().map_err(|_| "组合键存储不可用")?.list();
    ui.host_keyboard = state
        .host_keyboard
        .snapshot
        .lock()
        .map_err(|_| "本机键盘状态不可用")?
        .clone();
    ui.logs = state.diagnostics.snapshot(&ui.log_filter);
    Ok(Model {
        root: crate::locale::scope(ui.language, || view::build(&ui, &servers, &sessions)),
    })
}

#[tauri::command]
pub async fn ui_action(
    app: AppHandle,
    state: State<'_, AppState>,
    intent: Intent,
) -> Response<Model> {
    if let Err(message) = route(&app, state.clone(), intent).await {
        state.diagnostics.push(
            amikvm_core::diagnostics::Level::Error,
            amikvm_core::diagnostics::Category::Interface,
            None,
            "操作失败",
            &message,
        );
        state
            .ui
            .lock()
            .map_err(|_| "Interface state unavailable")?
            .error = Some(message);
    }
    ui_snapshot(state).await
}

#[tauri::command]
pub async fn ui_input(
    state: State<'_, AppState>,
    id: Uuid,
    event: amikvm_core::input::Event,
) -> Response<()> {
    if state.shutdown.blocks_connection(id)
        && !matches!(
            event,
            amikvm_core::input::Event::Release
                | amikvm_core::input::Event::ReleaseAll
                | amikvm_core::input::Event::Key { pressed: false, .. }
                | amikvm_core::input::Event::SoftKey { pressed: false, .. }
        )
    {
        return Err("正在处理关闭选择，输入暂时停止。".into());
    }
    let session = state.sessions.lock().await.get(&id).cloned();
    let Some(session) = session else {
        if matches!(
            event,
            amikvm_core::input::Event::Release
                | amikvm_core::input::Event::ReleaseAll
                | amikvm_core::input::Event::Key { pressed: false, .. }
                | amikvm_core::input::Event::SoftKey { pressed: false, .. }
        ) {
            return Ok(());
        }
        return Err("Session not found".into());
    };
    let result = session.input(event).await.map_err(|e| e.to_string());
    if let Err(error) = &result {
        state.diagnostics.changed(
            amikvm_core::diagnostics::Category::Input,
            Some(id),
            "input-error",
            error,
            amikvm_core::diagnostics::Level::Error,
            "键鼠输入失败",
            error,
        );
    }
    result
}

fn text(values: &Value, key: &str) -> String {
    match &values[key] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}
fn checked(values: &Value, key: &str) -> bool {
    values[key].as_bool().unwrap_or(false)
}

pub async fn select_playback(app: &AppHandle) -> Response<()> {
    let state = app.state::<AppState>();
    let previous = state.ui.lock().map_err(|_| "界面状态不可用")?.selected;
    if let Some(previous) = previous {
        let session = state.sessions.lock().await.get(&previous).cloned();
        if let Some(session) = session {
            session
                .input(amikvm_core::input::Event::ReleaseAll)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let mut ui = state.ui.lock().map_err(|_| "界面状态不可用")?;
    ui.playback_selected = true;
    ui.selected = None;
    ui.menu = None;
    ui.dialog = Dialog::None;
    Ok(())
}

async fn save_jpeg(
    app: &AppHandle,
    frame: std::sync::Arc<Vec<u8>>,
    prefix: &str,
) -> Response<Option<String>> {
    let dialog_app = app.clone();
    let filename = format!("{prefix}-{}.jpeg", amikvm_core::server::now());
    let path = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .file()
            .add_filter("JPEG", &["jpeg", "jpg"])
            .set_file_name(filename)
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(path) = path else {
        return Ok(None);
    };
    let mut path = path.into_path().map_err(|e| e.to_string())?;
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("jpeg") || e.eq_ignore_ascii_case("jpg"))
    {
        let mut name = path.into_os_string();
        name.push(".jpeg");
        path = name.into();
    }
    let display_path = path.display().to_string();
    tauri::async_runtime::spawn_blocking(move || -> Response<()> {
        if frame.len() < 12 {
            return Err("截图帧不完整".into());
        }
        let width = u32::from_le_bytes(frame[..4].try_into().unwrap());
        let height = u32::from_le_bytes(frame[4..8].try_into().unwrap());
        let encoded = amikvm_core::video::capture::jpeg(width, height, &frame[12..])
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        file.write_all(&encoded).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(Some(display_path))
}

async fn open_capture(
    state: State<'_, AppState>,
    id: Uuid,
    kind: amikvm_core::video::remote_capture::Kind,
) -> Response<()> {
    let session = state
        .sessions
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or("Session not found")?;
    if !session
        .snapshot
        .lock()
        .is_ok_and(|s| s.phase == "connected")
    {
        return Err("请等待服务器连接完成后再抓取画面".into());
    }
    session.captures.refresh(kind)?;
    let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
    ui.selected = Some(id);
    ui.playback_selected = false;
    ui.menu = None;
    ui.dialog = Dialog::Captures(id);
    Ok(())
}

async fn begin_connect(
    app: &AppHandle,
    state: State<'_, AppState>,
    id: Uuid,
    capture: Option<amikvm_core::video::remote_capture::Kind>,
) -> Response<()> {
    let server = state
        .store
        .lock()
        .map_err(|_| "Server database unavailable")?
        .get(id)
        .map_err(|e| e.to_string())?;
    let existing = state.sessions.lock().await.get(&id).cloned();
    let connecting = state
        .connecting
        .lock()
        .map_err(|_| "Session state unavailable")?
        .contains(&id);
    let usable = existing.as_ref().is_some_and(|s| {
        s.snapshot
            .lock()
            .is_ok_and(|v| active(&v) && !(v.web_only && capture.is_none()))
    });
    if usable || connecting {
        if let Some(kind) = capture {
            if connecting {
                return Err("请等待服务器连接完成后再抓取画面".into());
            }
            open_capture(state, id, kind).await?;
        } else {
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.selected = Some(id);
            ui.playback_selected = false;
            ui.menu = None;
        }
    } else if server.credential_saved {
        {
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.selected = Some(id);
            ui.playback_selected = false;
            ui.menu = None;
        }
        commands::connect_server_mode(app.clone(), state.clone(), id, None, capture.is_some())
            .await?;
        if let Some(kind) = capture {
            open_capture(state, id, kind).await?;
        }
    } else {
        let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
        ui.menu = None;
        ui.dialog = Dialog::Password(id, capture);
    }
    Ok(())
}

async fn route(app: &AppHandle, state: State<'_, AppState>, intent: Intent) -> Response<()> {
    let language = state
        .ui
        .lock()
        .map_err(|_| "Interface state unavailable")?
        .language;
    if state.shutdown.snapshot().is_some()
        && !matches!(
            intent,
            Intent::Quit
                | Intent::ExitChoose { .. }
                | Intent::ExitFinish { .. }
                | Intent::ExitCancel { .. }
                | Intent::DismissError
        )
    {
        return Err("请先处理当前关闭窗口。".into());
    }
    match intent {
        Intent::ConnectionInfo { id } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Connection(id)
        }
        Intent::About => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::About
        }
        Intent::LogConfigure { values } => {
            let minimum =
                serde_json::from_value(values["minimum"].clone()).map_err(|e| e.to_string())?;
            state.diagnostics.configure(
                checked(&values, "enabled"),
                minimum,
                checked(&values, "console"),
            );
            state.diagnostics.push(
                amikvm_core::diagnostics::Level::Info,
                amikvm_core::diagnostics::Category::Application,
                None,
                "日志设置已改变",
                "",
            );
        }
        Intent::LogFilter { values } => {
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.log_filter = amikvm_core::diagnostics::Filter {
                minimum: serde_json::from_value(values["minimum"].clone())
                    .map_err(|e| e.to_string())?,
                category: if text(&values, "category").is_empty() {
                    None
                } else {
                    Some(
                        serde_json::from_value(values["category"].clone())
                            .map_err(|e| e.to_string())?,
                    )
                },
                server: if text(&values, "server").is_empty() {
                    None
                } else {
                    Some(
                        text(&values, "server")
                            .parse()
                            .map_err(|_| "Invalid log server")?,
                    )
                },
                source_terms: crate::locale::source_terms(&text(&values, "query")),
                query: text(&values, "query").chars().take(256).collect(),
                page: 0,
            };
        }
        Intent::LogPage { value } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .log_filter
                .page = value
        }
        Intent::LogClear => {
            state.diagnostics.clear();
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .log_filter
                .page = 0;
        }
        Intent::LogFile => toggle_log_file(app.clone()).await?,
        Intent::LogExport => export_logs(app.clone()).await?,
        Intent::SetLanguage { value } => {
            let mut preferences = state
                .preferences
                .lock()
                .map_err(|_| "Preferences unavailable")?;
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            preferences
                .set_language(value)
                .map_err(|error| error.to_string())?;
            ui.language = value;
            app.emit("ui-changed", ()).ok();
        }
        Intent::ConfirmAction { message, intent } => {
            let action: Intent =
                serde_json::from_value(intent.clone()).map_err(|e| e.to_string())?;
            if matches!(
                action,
                Intent::ConfirmAction { .. } | Intent::ConfirmApply { .. } | Intent::CloseDialog
            ) {
                return Err("确认操作不能嵌套或确认关闭窗口。".into());
            }
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            if matches!(ui.dialog, Dialog::Confirmation) {
                return Err("请先处理当前确认窗口。".into());
            }
            ui.confirmation = Some(Confirmation {
                id: Uuid::new_v4(),
                message,
                intent,
                previous: ui.dialog.clone(),
            });
            ui.dialog = Dialog::Confirmation;
        }
        Intent::ConfirmApply { id } => {
            let action = {
                let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
                if !matches!(ui.dialog, Dialog::Confirmation)
                    || !ui.confirmation.as_ref().is_some_and(|c| c.id == id)
                {
                    return Err("该确认窗口已经结束。".into());
                }
                let confirmation = ui.confirmation.take().expect("checked confirmation");
                ui.dialog = confirmation.previous;
                serde_json::from_value(confirmation.intent).map_err(|e| e.to_string())?
            };
            // The stored action is executed once, then its backend checks run
            // against current permissions, request UUIDs and deadlines.
            return Box::pin(route(app, state, action)).await;
        }
        Intent::Quit => {
            state
                .shutdown
                .request(app, amikvm_core::sharing::exit::Scope::Application)?;
        }
        Intent::ExitChoose {
            plan,
            server,
            value,
        } => {
            let target = if value.is_empty() {
                None
            } else {
                Some(value.parse().map_err(|_| "无效的会话选择")?)
            };
            state.shutdown.choose(app, plan, server, target).await?;
        }
        Intent::ExitFinish { plan, transfer } => {
            state.shutdown.finish(app, plan, transfer).await?;
        }
        Intent::ExitCancel { plan } => {
            state.shutdown.cancel(app, plan)?;
        }
        Intent::Navigate { view } => {
            let previous = state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .selected;
            if let Some(previous) = previous {
                let previous_session = state.sessions.lock().await.get(&previous).cloned();
                if let Some(session) = previous_session {
                    session
                        .input(amikvm_core::input::Event::ReleaseAll)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.view = view;
            ui.playback_selected = false;
            ui.selected = None;
            ui.menu = None;
        }
        Intent::Search { value } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .query = value;
        }
        Intent::Select { id } => {
            let previous = state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .selected;
            if let Some(previous) = previous.filter(|v| *v != id) {
                let previous_session = state.sessions.lock().await.get(&previous).cloned();
                if let Some(session) = previous_session {
                    session
                        .input(amikvm_core::input::Event::ReleaseAll)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            state
                .store
                .lock()
                .map_err(|_| "Server database unavailable")?
                .get(id)
                .map_err(|e| e.to_string())?;
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.selected = Some(id);
            ui.view = View::All;
            ui.playback_selected = false;
        }
        Intent::Add => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Server(None);
        }
        Intent::Edit { id } => {
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.dialog = Dialog::Server(Some(id));
            ui.menu = None;
        }
        Intent::Menu { id } => {
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.menu = if ui.menu == Some(id) { None } else { Some(id) };
        }
        Intent::CloseDialog => {
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.dialog = if matches!(ui.dialog, Dialog::Confirmation) {
                ui.confirmation
                    .take()
                    .map(|c| c.previous)
                    .unwrap_or_default()
            } else {
                Dialog::None
            };
        }
        Intent::DismissError => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .error = None;
        }
        Intent::InteractionError { message } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .error = Some(message);
        }
        Intent::Connect { id } => {
            begin_connect(app, state.clone(), id, None).await?;
        }
        Intent::Password {
            id,
            values,
            capture,
        } => {
            let password = text(&values, "password");
            if password.is_empty() {
                return Err("请输入连接密码".into());
            }
            {
                let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
                ui.selected = Some(id);
                ui.playback_selected = false;
                ui.dialog = Dialog::None;
            }
            commands::connect_server_mode(
                app.clone(),
                state.clone(),
                id,
                Some(password),
                capture.is_some(),
            )
            .await?;
            if let Some(kind) = capture {
                open_capture(state.clone(), id, kind).await?;
            }
        }
        Intent::Save { values } => {
            let id = match state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog
            {
                Dialog::Server(id) => id,
                _ => return Err("Server form is not open".into()),
            };
            let input = ServerInput {
                id,
                name: text(&values, "name"),
                host: text(&values, "host"),
                web_port: text(&values, "webPort")
                    .parse()
                    .map_err(|_| "端口须为 1–65535 的整数")?,
                username: text(&values, "username"),
                https: text(&values, "scheme") == "https",
                api_mode: match text(&values, "apiMode").as_str() {
                    "auto" => ApiMode::Auto,
                    "rest" => ApiMode::Rest,
                    "rpc" => ApiMode::Rpc,
                    _ => return Err("未知的 BMC 接口".into()),
                },
                trust_invalid_certificate: checked(&values, "trustInvalidCertificate"),
                favorite: checked(&values, "favorite"),
                tags: text(&values, "tags")
                    .split(',')
                    .map(str::to_owned)
                    .collect(),
                notes: text(&values, "notes"),
            };
            commands::save_server(
                state.clone(),
                input,
                Some(text(&values, "password")),
                checked(&values, "rememberPassword"),
            )
            .await?;
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::None;
        }
        Intent::Disconnect { id } => {
            commands::disconnect_server(app.clone(), state.clone(), id).await?;
        }
        Intent::Favorite { id } => {
            let mut store = state
                .store
                .lock()
                .map_err(|_| "Server database unavailable")?;
            let s = store.get(id).map_err(|e| e.to_string())?;
            let mut input: ServerInput = (&s).into();
            input.favorite = !s.favorite;
            store
                .save(input, s.credential_saved)
                .map_err(|e| e.to_string())?;
        }
        Intent::Remove { id } => {
            commands::delete_server(state.clone(), id).await?;
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            ui.menu = None;
            if ui.selected == Some(id) {
                ui.selected = None;
            }
        }
        Intent::Control {
            id,
            mut control,
            value,
        } => {
            if let Some(value) = value {
                let string = value.as_str().ok_or("Expected an input value")?;
                match &mut control {
                    Control::MouseMode { mode } => {
                        *mode = string.parse().map_err(|_| "Invalid mouse mode")?
                    }
                    Control::KeyboardLayout { layout } => *layout = string.to_owned(),
                    Control::Bandwidth { bytes_per_second } => {
                        *bytes_per_second = string.parse().map_err(|_| "Invalid bandwidth")?
                    }
                    _ => return Err("This action does not accept an input value".into()),
                }
            }
            commands::send_control(state.clone(), id, control).await?;
        }
        Intent::LocalCursor { id } => {
            state
                .store
                .lock()
                .map_err(|_| "Server database unavailable")?
                .get(id)
                .map_err(|e| e.to_string())?;
            {
                let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
                if !ui.hidden_local_cursor.remove(&id) {
                    ui.hidden_local_cursor.insert(id);
                }
            }
            let session = state.sessions.lock().await.get(&id).cloned();
            if let Some(session) = session {
                session.mouse_display();
            }
        }
        Intent::Mouse { id, command, token } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .mouse_command(command, token)
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::MouseSettings { id, values } => {
            let threshold = text(&values, "threshold")
                .parse::<u16>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or("阈值须为 1–65535")?;
            let gain = text(&values, "gain")
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.01 && *v <= f64::from(u32::MAX) / 100.)
                .ok_or("请输入大于零的有效加速倍率")?;
            let settings = amikvm_core::input::mouse::Settings {
                threshold,
                acceleration: (gain * 100.).round() as u32,
            };
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .mouse_command(
                    amikvm_core::input::mouse::Command::Configure { settings },
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::Pause { id } => {
            let paused = state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .paused
                .contains(&id);
            commands::send_control(
                state.clone(),
                id,
                if paused {
                    Control::Resume
                } else {
                    Control::Pause
                },
            )
            .await?;
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            if paused {
                ui.paused.remove(&id);
            } else {
                ui.paused.insert(id);
            }
        }
        Intent::Shortcut { id, name } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            let report = amikvm_core::input::shortcut(&name).map_err(|e| e.to_string())?;
            session.tap(report).await.map_err(|e| e.to_string())?;
        }
        Intent::MacroDialog { id } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Macros(id);
        }
        Intent::MacroEdit { id, macro_id } => {
            if let Some(macro_id) = macro_id {
                state
                    .macros
                    .lock()
                    .map_err(|_| "组合键存储不可用")?
                    .get(macro_id)
                    .map_err(|e| e.to_string())?;
            }
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::MacroEdit(id, macro_id);
        }
        Intent::MacroSave {
            id,
            macro_id,
            values,
        } => {
            let codes = (0..amikvm_core::input::macros::MAX_KEYS)
                .map(|i| text(&values, &format!("key{i}")))
                .filter(|s| !s.is_empty())
                .collect();
            state
                .macros
                .lock()
                .map_err(|_| "组合键存储不可用")?
                .save(macro_id, text(&values, "name"), codes)
                .map_err(|e| e.to_string())?;
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Macros(id);
        }
        Intent::MacroRemove { macro_id } => {
            state
                .macros
                .lock()
                .map_err(|_| "组合键存储不可用")?
                .remove(macro_id)
                .map_err(|e| e.to_string())?;
        }
        Intent::MacroRun { id, macro_id } => {
            let report = state
                .macros
                .lock()
                .map_err(|_| "组合键存储不可用")?
                .get(macro_id)
                .and_then(|m| m.report())
                .map_err(|e| e.to_string())?;
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session.tap(report).await.map_err(|e| e.to_string())?;
        }
        Intent::SoftKeyboard { id } => {
            let closing = state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .soft_keyboard
                .contains(&id);
            if closing {
                let existing = state.sessions.lock().await.get(&id).cloned();
                if let Some(session) = existing {
                    session
                        .release_software()
                        .await
                        .map_err(|e| e.to_string())?;
                }
                state
                    .ui
                    .lock()
                    .map_err(|_| "Interface state unavailable")?
                    .soft_keyboard
                    .remove(&id);
            } else {
                state
                    .ui
                    .lock()
                    .map_err(|_| "Interface state unavailable")?
                    .soft_keyboard
                    .insert(id);
            }
        }
        Intent::SoftModifier { id, code } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .toggle_modifier(&code)
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::SoftLayout { id, value } => {
            let layout = if value == "follow" {
                None
            } else {
                Some(amikvm_core::input::layout::Layout::parse(&value).map_err(|e| e.to_string())?)
            };
            if let Some(session) = state.sessions.lock().await.get(&id).cloned() {
                session
                    .release_software()
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            match layout {
                Some(layout) => {
                    ui.soft_layout.insert(id, layout);
                }
                None => {
                    ui.soft_layout.remove(&id);
                }
            }
        }
        Intent::Fullscreen => {
            let window = app.get_webview_window("main").ok_or("Window unavailable")?;
            window
                .set_fullscreen(!window.is_fullscreen().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
        Intent::Zoom { id, value } => {
            let zoom = match value.as_str() {
                "fit" => Zoom::Fit,
                "actual" => Zoom::Actual,
                "host" => Zoom::Host,
                _ => {
                    let percent: u16 = value.parse().map_err(|_| "缩放比例无效")?;
                    if !(50..=150).contains(&percent) || percent % 10 != 0 {
                        return Err("缩放比例必须为 50%–150%，间隔 10%".into());
                    }
                    Zoom::Percent(percent)
                }
            };
            if matches!(zoom, Zoom::Host) {
                let session = state
                    .sessions
                    .lock()
                    .await
                    .get(&id)
                    .cloned()
                    .ok_or("Session not found")?;
                let (width, height) = {
                    let snapshot = session.snapshot.lock().map_err(|_| "Session unavailable")?;
                    if !snapshot.video_signal {
                        return Err("收到远程画面后才能适应主机尺寸".into());
                    }
                    (snapshot.video_width, snapshot.video_height)
                };
                let window = app.get_webview_window("main").ok_or("Window unavailable")?;
                let monitor = window
                    .current_monitor()
                    .map_err(|e| e.to_string())?
                    .ok_or("Monitor unavailable")?;
                let work = monitor.work_area();
                let scale = monitor.scale_factor();
                window.set_fullscreen(false).map_err(|e| e.to_string())?;
                window.unmaximize().map_err(|e| e.to_string())?;
                let size = tauri::LogicalSize::new(
                    (width as f64 + 232.0 + 260.0)
                        .max(900.0)
                        .min(work.size.width as f64 / scale),
                    (height as f64 + 166.0)
                        .max(600.0)
                        .min(work.size.height as f64 / scale),
                );
                window.set_size(size).map_err(|e| e.to_string())?;
            }
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .zoom
                .insert(id, zoom);
        }
        Intent::ZoomStep { id, direction } => {
            if ![-1, 1].contains(&direction) {
                return Err("缩放方向无效".into());
            }
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            let percent = ui.zoom.get(&id).copied().unwrap_or_default().percent();
            ui.zoom.insert(
                id,
                Zoom::Percent((percent as i16 + direction as i16 * 10).clamp(50, 150) as u16),
            );
        }
        Intent::VideoConfig { id, setting, value } => {
            let value = value.parse().map_err(|_| "视频设置无效")?;
            let setting = match setting.as_str() {
                "compression" => amikvm_core::video::config::Setting::Compression(value),
                "quality" => amikvm_core::video::config::Setting::Quality(value),
                _ => return Err("视频设置无效".into()),
            };
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .configure_video(setting)
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::Ipmi { id, values } => {
            let format = if values.get("format").is_some() {
                text(&values, "format")
            } else {
                "hex".into()
            };
            let bytes = amikvm_core::ipmi::parse_command(&format, &text(&values, "command"))
                .map_err(|e| e.to_string())?;
            commands::send_control(
                state.clone(),
                id,
                Control::Ipmi {
                    command: bytes,
                    request_id: 0,
                },
            )
            .await?;
        }
        Intent::IpmiDialog { id } => {
            let mut ui = state.ui.lock().map_err(|_| "界面状态不可用")?;
            ui.dialog = Dialog::Ipmi(id);
        }
        Intent::IpmiClear { id } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session.clear_ipmi_history();
        }
        Intent::BootDialog { id } | Intent::BootRefresh { id } => {
            {
                let mut ui = state.ui.lock().map_err(|_| "界面状态不可用")?;
                ui.dialog = Dialog::Boot(id);
            }
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session.read_boot().await.map_err(|e| e.to_string())?;
        }
        Intent::BootApply { id, values } => {
            let device =
                serde_json::from_value::<amikvm_core::ipmi::BootDevice>(values["device"].clone())
                    .map_err(|_| "请选择有效的启动设备")?;
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .apply_boot(device, checked(&values, "nextBootOnly"))
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::Input { id, event } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session.input(event).await.map_err(|e| e.to_string())?;
        }
        Intent::Share {
            id,
            operation,
            user_id,
            request_token,
            identity,
        } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .share(&operation, user_id, request_token, identity)
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::SharingDialog { id } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Sharing(id);
        }
        Intent::SharingPolicy { id, value } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session.sharing_policy(value).map_err(|e| e.to_string())?;
        }
        Intent::MediaDialog { id, kind } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Media(id, kind);
        }
        Intent::FolderDialog { id } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Folder(id);
        }
        Intent::FolderStart { id, values } => {
            use amikvm_core::media::scsi::Kind;
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            let Ok(_dialog) = session.file_dialog.try_lock() else {
                return Ok(());
            };
            let slot: u8 = text(&values, "slot")
                .parse()
                .map_err(|_| "请选择可用的硬盘实例")?;
            let size_mib: u32 = text(&values, "size").parse().map_err(|_| "镜像容量无效")?;
            if !(16..=2048).contains(&size_mib) {
                return Err("工作镜像容量必须为 16–2048 MiB".into());
            }
            let validate = || -> Response<()> {
                let snapshot = session.snapshot.lock().map_err(|_| "Session unavailable")?;
                let config = snapshot.config.as_ref().ok_or("服务器配置不可用")?;
                if snapshot.phase != "connected"
                    || config.privileges & 2 == 0
                    || !config.hd_enabled
                    || slot >= config.hd_instances
                {
                    return Err("服务器未连接，或没有可用的硬盘介质权限和实例".into());
                }
                if snapshot
                    .media
                    .iter()
                    .any(|m| m.kind != Kind::Cdrom && m.slot == slot && m.active())
                {
                    return Err("所选介质实例已被占用".into());
                }
                Ok(())
            };
            validate()?;
            let picker = app.clone();
            let root = tauri::async_runtime::spawn_blocking(move || {
                picker
                    .dialog()
                    .file()
                    .set_title(crate::locale::text_in(language, "选择要重定向的文件夹"))
                    .blocking_pick_folder()
            })
            .await
            .map_err(|e| e.to_string())?;
            let Some(root) = root else {
                return Ok(());
            };
            let picker = app.clone();
            let image = tauri::async_runtime::spawn_blocking(move || {
                picker
                    .dialog()
                    .file()
                    .set_title(crate::locale::text_in(language, "选择工作镜像的新文件名"))
                    .add_filter(crate::locale::text_in(language, "FAT 工作镜像"), &["img"])
                    .set_file_name(format!("AMIKVM-folder-{}.img", amikvm_core::server::now()))
                    .blocking_save_file()
            })
            .await
            .map_err(|e| e.to_string())?;
            let Some(image) = image else {
                return Ok(());
            };
            validate()?;
            if state
                .shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err("应用正在关闭".into());
            }
            let mut image = image.into_path().map_err(|e| e.to_string())?;
            if image.extension().is_none() {
                image.set_extension("img");
            }
            state
                .folders
                .create(
                    app.clone(),
                    session.media.clone(),
                    crate::folders::Options {
                        server_id: id,
                        slot,
                        root: root.into_path().map_err(|e| e.to_string())?,
                        image,
                        size_mib,
                        readonly: checked(&values, "readonly"),
                    },
                )
                .await
                .map_err(|e| e.to_string())?;
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::None;
        }
        Intent::FolderPrepare { id } => {
            state
                .folders
                .prepare(app, id)
                .await
                .map_err(|e| e.to_string())?;
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::FolderSync(id);
        }
        Intent::FolderApply { id, overwrite } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::None;
            state
                .folders
                .apply(app, id, overwrite)
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::FolderDiscard { id } => {
            state
                .folders
                .discard(app, id)
                .await
                .map_err(|e| e.to_string())?;
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::None;
        }
        Intent::FolderCancel { id } => {
            state.folders.cancel(id).map_err(|e| e.to_string())?;
        }
        Intent::MediaStart { id, kind, values } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            let slot: u8 = text(&values, "slot")
                .parse()
                .map_err(|_| "请选择可用的介质实例")?;
            let app = app.clone();
            let path = tauri::async_runtime::spawn_blocking(move || {
                let file = app.dialog().file();
                match kind {
                    amikvm_core::media::scsi::Kind::Cdrom => file.add_filter(
                        crate::locale::text_in(language, "CD/DVD 镜像"),
                        &["iso", "nrg"],
                    ),
                    _ => file.add_filter(
                        crate::locale::text_in(language, "磁盘镜像"),
                        &["img", "ima", "bin"],
                    ),
                }
                .blocking_pick_file()
            })
            .await
            .map_err(|e| e.to_string())?;
            let Some(path) = path else {
                return Ok(());
            };
            if session
                .snapshot
                .lock()
                .map_err(|_| "Session unavailable")?
                .phase
                != "connected"
            {
                return Err("介质连接开始前服务器已断开".into());
            }
            session
                .media
                .start(
                    kind,
                    slot,
                    path.into_path().map_err(|e| e.to_string())?,
                    checked(&values, "readonly"),
                    checked(&values, "usb"),
                    checked(&values, "boost"),
                )
                .await
                .map_err(|e| e.to_string())?;
            let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
            // A completed connection must not dismiss a different dialog that
            // the user opened while authentication was in progress.
            if matches!(ui.dialog, Dialog::Media(server, media_kind) if server == id && media_kind == kind)
            {
                ui.dialog = Dialog::None;
            }
        }
        Intent::MediaStop { id, kind, slot } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .media
                .stop(kind, slot)
                .await
                .map_err(|e| e.to_string())?;
        }
        Intent::Record { id, values } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            let Ok(_dialog_guard) = session.file_dialog.try_lock() else {
                return Ok(());
            };
            let recording_guard = session.recording_operation.lock().await;
            // The start form includes settings; the stop button does not.
            // An old stop click after the timer finished must never start a new file.
            let wants_start = values.is_some();
            let recording = {
                let mut video = session.video.lock().map_err(|_| "Video unavailable")?;
                if wants_start && video.recording.as_ref().is_some_and(|r| r.is_running()) {
                    return Err("此会话已经在录制".into());
                }
                video.recording.take()
            };
            if let Some(recording) = recording {
                let finished = tauri::async_runtime::spawn_blocking(move || recording.finish())
                    .await
                    .map_err(|e| e.to_string())?;
                if !wants_start {
                    finished.map_err(|e| e.to_string())?;
                }
            }
            if let Some(values) = values {
                drop(recording_guard);
                let limit_seconds = text(&values, "seconds")
                    .parse::<u16>()
                    .ok()
                    .filter(|s| (1..=1800).contains(s))
                    .ok_or("录制时长须为 1–1800 秒")?;
                let policy = serde_json::from_value::<amikvm_core::recording::Policy>(
                    values
                        .get("policy")
                        .cloned()
                        .unwrap_or_else(|| json!("normalized")),
                )
                .map_err(|_| "录制策略无效")?;
                {
                    let mut ui = state.ui.lock().map_err(|_| "Interface state unavailable")?;
                    ui.recording_seconds.insert(id, limit_seconds);
                    ui.recording_policies.insert(id, policy);
                }
                if !session.can_record() {
                    return Err("请先连接远程控制台".into());
                }
                let app = app.clone();
                let dialog_app = app.clone();
                let path = tauri::async_runtime::spawn_blocking(move || {
                    dialog_app
                        .dialog()
                        .file()
                        .add_filter("MP4", &["mp4"])
                        .set_file_name(format!("AMIKVM-{}.mp4", amikvm_core::server::now()))
                        .blocking_save_file()
                })
                .await
                .map_err(|e| e.to_string())?;
                let Some(path) = path else {
                    return Ok(());
                };
                let mut path = path.into_path().map_err(|e| e.to_string())?;
                if path.extension().is_none() {
                    path.set_extension("mp4");
                }
                let _recording_guard = session.recording_operation.lock().await;
                if !session.can_record() {
                    return Err("录制开始前连接已断开".into());
                }
                if session
                    .video
                    .lock()
                    .map_err(|_| "Video unavailable")?
                    .recording
                    .is_some()
                {
                    return Err("此会话已经在录制".into());
                }
                let frame = session
                    .video
                    .lock()
                    .map_err(|_| "Video unavailable")?
                    .recording_frame()
                    .map_err(|e| e.to_string())?;
                let snapshot = session.snapshot.clone();
                let worker_path = path.clone();
                let recording = tauri::async_runtime::spawn_blocking(move || {
                    crate::recording::Recording::start(
                        worker_path,
                        frame,
                        app,
                        snapshot,
                        limit_seconds,
                        policy,
                    )
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                {
                    let mut video = session.video.lock().map_err(|_| "Video unavailable")?;
                    recording.frame(video.recording_frame().map_err(|e| e.to_string())?);
                    video.recording = Some(recording);
                }
                let mut snapshot = session.snapshot.lock().map_err(|_| "Session unavailable")?;
                snapshot.recording_path = Some(path.to_string_lossy().into_owned());
            }
        }
        Intent::RecordPause { id } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            let mut snapshot = session.snapshot.lock().map_err(|_| "Session unavailable")?;
            let pause = !snapshot.recording_paused;
            session
                .video
                .lock()
                .map_err(|_| "Video unavailable")?
                .recording
                .as_ref()
                .ok_or("Recording is not running")?
                .pause(pause);
            snapshot.recording_paused = pause;
        }
        Intent::RecordingsDialog { id } | Intent::RecordingsRefresh { id } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("请先连接服务器")?;
            if !session
                .snapshot
                .lock()
                .is_ok_and(|s| s.phase == "connected")
            {
                return Err("服务器连接不可用".into());
            }
            state.ui.lock().map_err(|_| "界面状态不可用")?.dialog = Dialog::Recordings(id);
            session.recordings.refresh()?;
        }
        Intent::RecordingsCancel { id } => {
            if let Some(session) = state.sessions.lock().await.get(&id).cloned() {
                session.recordings.cancel();
            }
        }
        Intent::RecordingsDownload { id, file, play } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("请先连接服务器")?;
            if !session
                .snapshot
                .lock()
                .is_ok_and(|s| s.phase == "connected")
            {
                return Err("服务器连接不可用".into());
            }
            let entry = session.recordings.entry(&file)?;
            let Ok(_dialog_guard) = session.file_dialog.try_lock() else {
                return Ok(());
            };
            let (path, temporary) = if play {
                let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
                (directory.path().join("recording.ast"), Some(directory))
            } else {
                let name: String = entry
                    .name
                    .chars()
                    .map(|c| {
                        if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                            '_'
                        } else {
                            c
                        }
                    })
                    .collect();
                let dialog_app = app.clone();
                let path = tauri::async_runtime::spawn_blocking(move || {
                    dialog_app
                        .dialog()
                        .file()
                        .set_file_name(format!("{name}.ast"))
                        .blocking_save_file()
                })
                .await
                .map_err(|e| e.to_string())?;
                let Some(path) = path else {
                    return Ok(());
                };
                (path.into_path().map_err(|e| e.to_string())?, None)
            };
            session
                .recordings
                .download(file, path, temporary, state.playback.clone())?;
        }
        Intent::PlaybackView => select_playback(app).await?,
        Intent::PlaybackOpen => {
            let Ok(_dialog_guard) = state.playback.file_dialog.try_lock() else {
                return Ok(());
            };
            let dialog_app = app.clone();
            let path = tauri::async_runtime::spawn_blocking(move || {
                dialog_app
                    .dialog()
                    .file()
                    .add_filter(
                        crate::locale::text_in(language, "录像"),
                        &["mp4", "ast", "kvm", "dat"],
                    )
                    .add_filter(crate::locale::text_in(language, "所有文件"), &["*"])
                    .blocking_pick_file()
            })
            .await
            .map_err(|e| e.to_string())?;
            let Some(path) = path else {
                return Ok(());
            };
            if state
                .shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Ok(());
            }
            state
                .playback
                .open(
                    app.clone(),
                    path.into_path().map_err(|e| e.to_string())?,
                    None,
                )
                .await?;
            select_playback(app).await?;
        }
        Intent::PlaybackControl { id, operation } => {
            state
                .playback
                .get(id)
                .ok_or("录像已关闭")?
                .action(&operation, None, app)?;
        }
        Intent::PlaybackSeek { id, value } => {
            let position = value
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_mul(1000))
                .ok_or("回放位置无效")?;
            state
                .playback
                .get(id)
                .ok_or("录像已关闭")?
                .action("seek", Some(position), app)?;
        }
        Intent::PlaybackClose { id } => {
            if state.playback.get(id).is_some() {
                state.playback.close().await;
            }
        }
        Intent::CaptureDialog { id, kind } => {
            begin_connect(app, state.clone(), id, Some(kind)).await?;
        }
        Intent::CaptureRefresh { id, kind } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session.captures.refresh(kind)?;
        }
        Intent::CaptureCancel { id } => {
            if let Some(session) = state.sessions.lock().await.get(&id).cloned() {
                session.captures.cancel();
            }
        }
        Intent::CaptureSave { id } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            let Ok(_dialog_guard) = session.file_dialog.try_lock() else {
                return Ok(());
            };
            let frame = session
                .captures
                .video
                .lock()
                .map_err(|_| "捕获画面不可用")?
                .latest()
                .ok_or("收到 BMC 捕获画面后才能保存")?;
            if let Some(path) = save_jpeg(app, frame, "BMC-Capture").await? {
                session.captures.saved(path);
            }
        }
        Intent::Capture { id } => {
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            let Ok(_dialog_guard) = session.file_dialog.try_lock() else {
                return Ok(());
            };
            let frame = session
                .video
                .lock()
                .map_err(|_| "Video unavailable")?
                .latest()
                .ok_or("收到远程画面后才能截图")?;
            save_jpeg(app, frame, "AMIKVM").await?;
        }
        Intent::TextDialog { id } => {
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::Text(id);
        }
        Intent::Text { id, values } => {
            let mode = amikvm_core::input::TextMode::parse(&text(&values, "mode"))
                .map_err(|e| e.to_string())?;
            let session = state
                .sessions
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or("Session not found")?;
            session
                .type_text(&text(&values, "text"), mode)
                .await
                .map_err(|e| e.to_string())?;
            state
                .ui
                .lock()
                .map_err(|_| "Interface state unavailable")?
                .dialog = Dialog::None;
        }
    }
    let _ = app.emit("ui-changed", json!({}));
    Ok(())
}
