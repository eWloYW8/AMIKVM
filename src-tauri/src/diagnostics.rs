//! Rust presentation observers record changes, never pixels, credentials or input.
use amikvm_core::diagnostics::{Category, Level};
use serde_json::json;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub fn record(
    app: &AppHandle,
    level: Level,
    category: Category,
    id: Option<Uuid>,
    source: &'static str,
    details: impl AsRef<str>,
) {
    if let Some(state) = app.try_state::<crate::commands::AppState>() {
        state.diagnostics.push(level, category, id, source, details);
    }
}
pub fn changed(
    app: &AppHandle,
    category: Category,
    id: Option<Uuid>,
    key: &str,
    source: &'static str,
    value: serde_json::Value,
    level: Level,
) {
    if let Some(state) = app.try_state::<crate::commands::AppState>() {
        let details = value.to_string();
        state
            .diagnostics
            .changed(category, id, key, &details, level, source, &details);
    }
}
pub fn session(app: &AppHandle, s: &crate::session::Snapshot) {
    let id = Some(s.server_id);
    changed(
        app,
        Category::Session,
        id,
        "phase",
        "会话状态已改变",
        json!({"phase":s.phase,"message":s.message}),
        if s.phase == "error" {
            Level::Error
        } else {
            Level::Info
        },
    );
    changed(
        app,
        Category::Sharing,
        id,
        "role",
        "控制权限已改变",
        json!({"role":s.sharing.role,"control":s.can_control,"requests":s.sharing.requests.len(),"message":s.sharing.message}),
        Level::Info,
    );
    changed(
        app,
        Category::Input,
        id,
        "settings",
        "键鼠输入状态已改变",
        json!({"encrypted":s.input_encryption,"required":s.encryption_required,"mouseMode":s.mouse_mode}),
        Level::Info,
    );
    changed(
        app,
        Category::Session,
        id,
        "video",
        "画面状态已改变",
        json!({"signal":s.video_signal,"source":[s.video_source_width,s.video_source_height],"output":[s.video_width,s.video_height],"power":s.power}),
        Level::Info,
    );
    changed(
        app,
        Category::Recording,
        id,
        "recording",
        "录制状态已改变",
        json!({"active":s.recording,"paused":s.recording_paused,"policy":s.recording_policy,"message":s.recording_message}),
        Level::Info,
    );
    if let Some(record) = s.ipmi.records.last() {
        changed(
            app,
            Category::Ipmi,
            id,
            "request",
            "IPMI 请求状态已改变",
            json!({"sequence":record.sequence,"requestId":record.request_id,"operation":record.operation,"phase":record.phase,"requestBytes":record.command.len(),"responseBytes":record.response.as_ref().map(|r|r.data.len()),"completion":record.response.as_ref().map(|r|r.completion_code),"message":record.message}),
            Level::Info,
        );
    }
    changed(
        app,
        Category::Ipmi,
        id,
        "boot",
        "启动选项状态已改变",
        json!({"phase":s.ipmi.boot.phase,"revision":s.ipmi.boot.revision,"message":s.ipmi.boot.message}),
        Level::Info,
    );
    for media in &s.media {
        let key = match media.kind {
            amikvm_core::media::scsi::Kind::Cdrom => "cd",
            amikvm_core::media::scsi::Kind::HardDisk => "disk",
            amikvm_core::media::scsi::Kind::Floppy => "floppy",
        };
        changed(
            app,
            Category::Media,
            id,
            &format!("{key}-{}", media.id),
            "介质状态已改变",
            json!({"id":media.id,"slot":media.slot,"phase":media.phase,"message":media.message}),
            if media.phase == "error" {
                Level::Error
            } else {
                Level::Info
            },
        );
    }
}

pub fn install(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        use tauri::Emitter;
        let mut previous = String::new();
        let mut clock = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            clock.tick().await;
            let state = app.state::<crate::commands::AppState>();
            if state
                .shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
            let snapshot = state.diagnostics.snapshot(&Default::default());
            let signature = format!(
                "{}:{}:{:?}:{:?}:{}:{}",
                snapshot.total,
                snapshot.file_dropped,
                snapshot.file,
                snapshot.file_error,
                snapshot.enabled,
                snapshot.console
            );
            if previous != signature {
                previous = signature;
                app.emit("ui-changed", ()).ok();
            }
        }
    });
}
