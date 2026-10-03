use amikvm_core::{
    Error, Result,
    auth::{SessionConfig, WebSession},
    input::{self, Event, TextMode},
    protocol::{self, Control, Fragments},
    transport,
    video::{Cursor, Decoder},
};
use serde::Serialize;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, oneshot, watch},
};
use uuid::Uuid;

mod queue;
mod text;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub server_id: Uuid,
    pub phase: String,
    pub message: Option<String>,
    pub config: Option<SessionConfig>,
    pub power: amikvm_core::power::State,
    pub mouse_mode: Option<u8>,
    pub mouse: input::mouse::State,
    pub lock_leds: u8,
    pub lock_leds_known: bool,
    pub input_focused: bool,
    pub software_keys: Vec<u8>,
    pub keyboard_options: input::routing::Options,
    pub text_input: input::TextStatus,
    pub mouse_capture: input::capture::State,
    pub local_cursor: input::cursor::State,
    pub input_encryption: bool,
    pub encryption_required: bool,
    pub host_display: Option<u16>,
    pub host_display_supported: Option<bool>,
    pub service: amikvm_core::service::State,
    pub recovery: amikvm_core::recovery::State,
    pub ipmi: amikvm_core::ipmi::State,
    pub can_control: bool,
    pub frames_received: u64,
    pub bytes_received: u64,
    pub video_width: u32,
    pub video_height: u32,
    pub video_source_width: u32,
    pub video_source_height: u32,
    pub video_signal: bool,
    pub video_connected: bool,
    pub web_only: bool,
    pub video_config: Option<amikvm_core::video::config::EngineConfig>,
    #[serde(skip)]
    video_config_revision: u64,
    pub bandwidth: Option<u32>,
    pub bandwidth_measuring: bool,
    pub measured_bytes_per_second: Option<u64>,
    #[serde(skip)]
    pub bandwidth_requested: Option<std::time::Instant>,
    pub own_session_id: Option<u8>,
    pub users: Vec<amikvm_core::sharing::User>,
    pub sharing: amikvm_core::sharing::State,
    pub recording: bool,
    pub recording_paused: bool,
    pub recording_path: Option<String>,
    pub recording_elapsed_ms: u64,
    pub recording_limit_seconds: u16,
    pub recording_message: Option<String>,
    pub recording_policy: amikvm_core::recording::Policy,
    pub recording_outputs: Vec<String>,
    pub recording_written_ms: u64,
    pub recording_skipped_ms: u64,
    pub media: Vec<amikvm_core::media::redirect::Status>,
}

impl Snapshot {
    pub fn pending(id: Uuid) -> Self {
        Self {
            server_id: id,
            phase: "authenticating".into(),
            message: None,
            config: None,
            power: Default::default(),
            mouse_mode: None,
            mouse: input::mouse::State::default(),
            lock_leds: 0,
            lock_leds_known: false,
            input_focused: false,
            software_keys: vec![],
            keyboard_options: Default::default(),
            text_input: Default::default(),
            mouse_capture: Default::default(),
            local_cursor: crate::cursor::state(),
            input_encryption: false,
            encryption_required: false,
            host_display: None,
            host_display_supported: None,
            service: Default::default(),
            recovery: Default::default(),
            ipmi: amikvm_core::ipmi::State::default(),
            can_control: false,
            frames_received: 0,
            bytes_received: 0,
            video_width: 0,
            video_height: 0,
            video_source_width: 0,
            video_source_height: 0,
            video_signal: false,
            video_connected: false,
            web_only: false,
            video_config: None,
            video_config_revision: 0,
            bandwidth: None,
            bandwidth_measuring: false,
            measured_bytes_per_second: None,
            bandwidth_requested: None,
            own_session_id: None,
            users: vec![],
            sharing: amikvm_core::sharing::State::default(),
            recording: false,
            recording_paused: false,
            recording_path: None,
            recording_elapsed_ms: 0,
            recording_limit_seconds: 20,
            recording_message: None,
            recording_policy: amikvm_core::recording::Policy::default(),
            recording_outputs: Vec::new(),
            recording_written_ms: 0,
            recording_skipped_ms: 0,
            media: vec![],
        }
    }
}

pub struct Session {
    pub snapshot: Arc<Mutex<Snapshot>>,
    pub video: Arc<Mutex<crate::video::Video>>,
    pub media: Arc<crate::media::Manager>,
    pub recordings: Arc<crate::recordings::Manager>,
    pub captures: Arc<crate::captures::Manager>,
    pub recording_operation: Arc<AsyncMutex<()>>,
    pub file_dialog: AsyncMutex<()>,
    video_configuration: AsyncMutex<()>,
    sender: queue::Sender<Outgoing>,
    ready: Arc<AtomicBool>,
    keyboard: Arc<AsyncMutex<input::State>>,
    text_active: AtomicBool,
    text_generation: watch::Sender<u64>,
    text_done: tokio::sync::Notify,
    input_app: AppHandle,
    cancel: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
}

enum Outgoing {
    Bytes(Vec<u8>),
    Power {
        operation: protocol::PowerOperation,
        reply: oneshot::Sender<Result<()>>,
    },
    PowerStatus,
    VideoConfig {
        setting: amikvm_core::video::config::Setting,
        reply: oneshot::Sender<Result<()>>,
    },
    HostDisplay {
        locked: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    Mouse(crate::mouse::Operation),
    ReleaseMouse,
    SharingAnswer(Vec<u8>),
    Handoff {
        bytes: Vec<u8>,
        reply: Option<oneshot::Sender<Result<()>>>,
    },
    Hid {
        mouse: bool,
        report: Vec<u8>,
        reply: Option<oneshot::Sender<Result<()>>>,
        text_generation: Option<u64>,
        locks_token: Option<Uuid>,
    },
    Encryption {
        enabled: bool,
        requested: bool,
        reply: Option<oneshot::Sender<Result<()>>>,
    },
}

fn update(app: &AppHandle, snapshot: &Arc<Mutex<Snapshot>>, f: impl FnOnce(&mut Snapshot)) {
    if let Ok(mut value) = snapshot.lock() {
        f(&mut value);
        crate::diagnostics::session(&app, &value);
        let _ = app.emit("session-state", value.clone());
    }
}

async fn retry_connection(
    app: &AppHandle,
    snapshot: &Arc<Mutex<Snapshot>>,
    cancel: &mut watch::Receiver<bool>,
    error: &Error,
) -> bool {
    if *cancel.borrow() || !amikvm_core::recovery::retryable(error) {
        return false;
    }
    let mut scheduled = false;
    update(app, snapshot, |s| {
        if let Some(config) = &s.config {
            scheduled = s.recovery.schedule(
                config.retry_count,
                config.retry_interval,
                std::time::Instant::now(),
                error.to_string(),
            );
            if scheduled {
                s.phase = "reconnecting".into();
                s.message = Some("连接中断，正在自动重试".into());
            }
        }
    });
    if !scheduled {
        return false;
    }
    let mut clock = tokio::time::interval(Duration::from_secs(1));
    loop {
        if *cancel.borrow() {
            return false;
        }
        let connecting = snapshot
            .lock()
            .is_ok_and(|s| s.recovery.stage == amikvm_core::recovery::Stage::Connecting);
        if connecting {
            return true;
        }
        tokio::select! {
            biased;
            _ = cancel.changed() => return false,
            _ = clock.tick() => update(app, snapshot, |s| { s.recovery.tick(std::time::Instant::now()); }),
        }
    }
}

impl Session {
    pub async fn start(app: AppHandle, web: WebSession, web_only: bool) -> Result<Self> {
        let keyboard = Arc::new(AsyncMutex::new(input::State::default()));
        let text_generation = watch::channel(0).0;
        let sequence = Arc::new(AtomicU32::new(0));
        let input_app = app.clone();
        let web = Arc::new(web);
        if web_only || web.config.privileges & 1 == 0 || !web.config.kvm_enabled {
            if !web_only && web.config.privileges & 2 == 0 {
                let _ = web.logout().await;
                return Err(Error::Authentication(
                    "This account has no KVM or virtual media privilege".into(),
                ));
            }
            let mut initial = Snapshot::pending(web.server.id);
            initial.phase = "connected".into();
            initial.web_only = web_only;
            initial.message = Some(
                if web_only {
                    "Web 会话已连接，可抓取 BMC 画面"
                } else {
                    "虚拟介质会话已连接；KVM 不可用"
                }
                .into(),
            );
            initial.config = Some(web.config.clone());
            let snapshot = Arc::new(Mutex::new(initial));
            let (cancel, mut cancel_rx) = watch::channel(false);
            let (finish_tx, finished) = watch::channel(false);
            let media = Arc::new(crate::media::Manager::new(
                web.clone(),
                snapshot.clone(),
                app.clone(),
                cancel_rx.clone(),
            ));
            let worker_media = media.clone();
            let recordings = Arc::new(crate::recordings::Manager::new(web.clone(), app.clone()));
            let worker_recordings = recordings.clone();
            let captures = Arc::new(crate::captures::Manager::new(web.clone(), app.clone()));
            let worker_captures = captures.clone();
            let worker_snapshot = snapshot.clone();
            let (sender, _receiver) = queue::Sender::channel(1);
            tauri::async_runtime::spawn(async move {
                update(&app, &worker_snapshot, |_| {});
                let _ = cancel_rx.changed().await;
                worker_media.stop_all().await;
                worker_recordings.shutdown().await;
                worker_captures.shutdown().await;
                update(&app, &worker_snapshot, |s| s.phase = "disconnected".into());
                let _ = tokio::time::timeout(Duration::from_secs(2), web.logout()).await;
                let _ = finish_tx.send(true);
            });
            return Ok(Self {
                snapshot,
                video: Arc::new(Mutex::new(crate::video::Video::default())),
                media,
                recordings,
                captures,
                recording_operation: Arc::new(AsyncMutex::new(())),
                file_dialog: AsyncMutex::new(()),
                video_configuration: AsyncMutex::new(()),
                sender,
                ready: Arc::new(AtomicBool::new(false)),
                keyboard,
                text_active: AtomicBool::new(false),
                text_generation,
                text_done: tokio::sync::Notify::new(),
                input_app,
                cancel,
                finished,
            });
        }
        let mut initial = Snapshot::pending(web.server.id);
        initial.phase = "negotiating".into();
        if let Ok(ui) = app.state::<crate::commands::AppState>().ui.lock() {
            if let Some(settings) = ui.mouse_settings.get(&web.server.id) {
                initial.mouse.settings = *settings;
            }
        }
        initial.config = Some(web.config.clone());
        let snapshot = Arc::new(Mutex::new(initial));
        let video = Arc::new(Mutex::new(crate::video::Video::default()));
        let worker_video = video.clone();
        let (sender, mut receiver) = queue::Sender::<Outgoing>::channel(128);
        let ready = Arc::new(AtomicBool::new(false));
        let worker_snapshot = snapshot.clone();
        let (cancel, mut cancel_rx) = watch::channel(false);
        let media = Arc::new(crate::media::Manager::new(
            web.clone(),
            snapshot.clone(),
            app.clone(),
            cancel_rx.clone(),
        ));
        let worker_media = media.clone();
        let recordings = Arc::new(crate::recordings::Manager::new(web.clone(), app.clone()));
        let worker_recordings = recordings.clone();
        let captures = Arc::new(crate::captures::Manager::new(web.clone(), app.clone()));
        let worker_captures = captures.clone();
        let worker_cancel = cancel.clone();
        let recording_operation = Arc::new(AsyncMutex::new(()));
        let worker_recording_operation = recording_operation.clone();
        let (finish_tx, finished) = watch::channel(false);
        let worker_sender = sender.clone();
        let worker_ready = ready.clone();
        let worker_keyboard = keyboard.clone();
        let worker_text_generation = text_generation.clone();
        tauri::async_runtime::spawn(async move {
            update(&app, &worker_snapshot, |_| {});
            let session_web = web;
            let mut previous: Option<(u8, amikvm_core::sharing::Role)> = None;
            let result: Result<()> = 'connections: loop {
                if *cancel_rx.borrow() {
                    break Ok(());
                }
                let config = worker_snapshot
                    .lock()
                    .ok()
                    .and_then(|s| s.config.clone())
                    .unwrap_or_else(|| session_web.config.clone());
                let web = Arc::new(session_web.with_config(config));
                let connection = tokio::select! {
                    biased;
                    _ = cancel_rx.changed() => break Ok(()),
                    opened = web.open_video() => match opened {
                        Ok(connection) => connection,
                        Err(error) => {
                            if retry_connection(&app, &worker_snapshot, &mut cancel_rx, &error).await { continue; }
                            break if *cancel_rx.borrow() { Ok(()) } else { Err(error) };
                        }
                    },
                };
                worker_text_generation.send_modify(|value| *value = value.wrapping_add(1));
                update(&app, &worker_snapshot, |s| {
                    s.service = Default::default();
                    s.power = Default::default();
                    s.host_display_supported = None;
                    s.host_display = None;
                    s.video_config = None;
                    if s.recovery.attempt > 0 {
                        s.recovery.authenticating();
                    }
                });
                let link_generation = worker_sender.generation();
                let (mut reader, mut writer) = tokio::io::split(connection.stream);
                let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
                let (write_failure, mut write_failure_rx) = watch::channel(false);
                let writer_sequence = sequence.clone();
                let writer_ready = worker_ready.clone();
                let writer_cancel = write_failure.clone();
                let writer_sender = worker_sender.clone();
                let writer_snapshot = worker_snapshot.clone();
                let writer_web = web.clone();
                let writer_app = app.clone();
                let writer_media = worker_media.clone();
                let writer_video = worker_video.clone();
                let writer_keyboard = worker_keyboard.clone();
                let writer_text_generation = worker_text_generation.clone();
                let writer_task = tauri::async_runtime::spawn(async move {
                    let mut heartbeat = tokio::time::interval(Duration::from_secs(1));
                    let mut mouse_clock = tokio::time::interval(Duration::from_millis(25));
                    let mut had_input = false;
                    let mut pointer = input::pointer::State::default();
                    let mut cipher = None;
                    let result = 'writer: loop {
                        tokio::select! {
                            biased;
                            _ = &mut stop_rx => break Ok(()),
                            _ = mouse_clock.tick() => {
                                let reports = crate::mouse::tick(&writer_app, &writer_snapshot, &writer_video);
                                if !reports.is_empty() {
                                    match crate::mouse::write(&writer_app, &mut writer, &writer_snapshot, &writer_sequence, cipher.as_ref(), &reports, None, &mut pointer).await {
                                        Ok(written) => { had_input |= written.sent; },
                                        Err(error) => break Err(error),
                                    }
                                }
                            },
                            outgoing = queue::receive(&mut receiver, link_generation) => match outgoing {
                                Some(Outgoing::Mouse(operation)) => {
                                    let (effect, reply) = crate::mouse::process(&writer_app, &writer_snapshot, operation);
                                    let effect = match effect {
                                        Ok(effect) => effect,
                                        Err(error) => {
                                            if let Some(reply) = reply { let _ = reply.send(Err(error)); }
                                            else { crate::mouse::message(&writer_app, &writer_snapshot, error.to_string()); }
                                            continue;
                                        }
                                    };
                                    if effect.release_keyboard && writer_snapshot.lock().is_ok_and(|s| s.can_control && s.video_connected) {
                                        writer_text_generation.send_modify(|value| *value = value.wrapping_add(1));
                                        writer_keyboard.lock().await.clear();
                                        let release = match protocol::input_report_with_cipher(writer_sequence.fetch_add(1, Ordering::Relaxed), false, &[0; 8], cipher.as_ref()) {
                                            Ok(bytes) => bytes, Err(error) => break Err(error),
                                        };
                                        if let Err(error) = transport::write_packet(&mut writer, &release).await { break Err(error); }
                                    }
                                    let result = crate::mouse::write(&writer_app, &mut writer, &writer_snapshot, &writer_sequence, cipher.as_ref(), &effect.reports, effect.pointer, &mut pointer).await;
                                    crate::mouse::display(&writer_app, &writer_snapshot, &writer_video);
                                    if effect.notify { crate::mouse::notify(&writer_app, &writer_snapshot); }
                                    match result {
                                        Ok(written) => {
                                            had_input |= written.sent;
                                            if let Some(reply) = reply {
                                                let result = if written.complete { Ok(()) } else { Err(Error::Invalid("鼠标模式或控制权限变化，操作未完整写出".into())) };
                                                let _ = reply.send(result);
                                            }
                                        },
                                        Err(error) => {
                                            if let Some(reply) = reply { let _ = reply.send(Err(Error::Protocol(error.to_string()))); }
                                            break Err(error);
                                        },
                                    }
                                },
                                Some(Outgoing::ReleaseMouse) => {
                                    if !writer_ready.load(Ordering::Acquire) || !writer_snapshot.lock().is_ok_and(|s| s.can_control && s.video_connected) { continue; }
                                    if let Err(error) = crate::mouse::release(&mut writer, &writer_snapshot, &writer_sequence, cipher.as_ref(), &mut pointer).await { break Err(error); }
                                },
                                Some(Outgoing::Bytes(bytes)) => {
                                    if let Err(error) = transport::write_packet(&mut writer, &bytes).await { break Err(error); }
                                },
                                Some(Outgoing::Power { operation, reply }) => {
                                    let prepared = (|| {
                                        let mut snapshot = writer_snapshot.lock().map_err(|_| Error::Invalid("Session unavailable".into()))?;
                                        if !writer_ready.load(Ordering::Acquire) || !snapshot.video_connected {
                                            return Err(Error::Invalid("Session is not connected".into()));
                                        }
                                        if !snapshot.can_control {
                                            return Err(Error::Authentication("Session has view-only permissions".into()));
                                        }
                                        if !snapshot.config.as_ref().is_some_and(|c| c.privileges & 256 != 0) {
                                            return Err(Error::Authentication("This account has no server power privilege".into()));
                                        }
                                        let bytes = snapshot.power.begin(operation, std::time::Instant::now())?;
                                        crate::diagnostics::session(&writer_app, &snapshot);
                                        let _ = writer_app.emit("session-state", snapshot.clone());
                                        Ok(bytes)
                                    })();
                                    let bytes = match prepared {
                                        Ok(bytes) => bytes,
                                        Err(error) => { let _ = reply.send(Err(error)); continue; }
                                    };
                                    if let Err(error) = transport::write_packet(&mut writer, &bytes).await {
                                        let _ = reply.send(Err(Error::Protocol(error.to_string()))); break Err(error);
                                    }
                                    let _ = reply.send(Ok(()));
                                },
                                Some(Outgoing::PowerStatus) => {
                                    let bytes = if let Ok(mut snapshot) = writer_snapshot.lock() {
                                        if !snapshot.video_connected { continue; }
                                        let bytes = snapshot.power.query(std::time::Instant::now());
                                        if bytes.is_some() {
                                            crate::diagnostics::session(&writer_app, &snapshot);
                                            let _ = writer_app.emit("session-state", snapshot.clone());
                                        }
                                        bytes
                                    } else { None };
                                    if let Some(bytes) = bytes {
                                        if let Err(error) = transport::write_packet(&mut writer, &bytes).await { break Err(error); }
                                    }
                                },
                                Some(Outgoing::VideoConfig { setting, reply }) => {
                                    let prepared = (|| {
                                        let snapshot = writer_snapshot.lock().map_err(|_| Error::Invalid("Session unavailable".into()))?;
                                        if !writer_ready.load(Ordering::Acquire) || !snapshot.video_connected {
                                            return Err(Error::Invalid("Session is not connected".into()));
                                        }
                                        if !snapshot.can_control {
                                            return Err(Error::Authentication("Control permission is required".into()));
                                        }
                                        let base = snapshot.video_config.ok_or_else(|| Error::Invalid("Waiting for BMC video configuration".into()))?;
                                        Ok((base.change(setting, snapshot.host_display)?, snapshot.video_config_revision))
                                    })();
                                    let (config, revision) = match prepared {
                                        Ok(config) => config,
                                        Err(error) => { let _ = reply.send(Err(error)); continue; }
                                    };
                                    let bytes = config.packet().expect("validated AST configuration");
                                    if let Err(error) = transport::write_packet(&mut writer, &bytes).await {
                                        let _ = reply.send(Err(Error::Protocol(error.to_string()))); break Err(error);
                                    }
                                    update(&writer_app, &writer_snapshot, |s| {
                                        // A newer BMC 4099 response takes precedence over local prediction.
                                        if s.can_control && s.video_config_revision == revision { s.video_config = Some(config); }
                                    });
                                    let _ = reply.send(Ok(()));
                                },
                                Some(Outgoing::HostDisplay { locked, reply }) => {
                                    let prepared = (|| {
                                        let snapshot = writer_snapshot.lock().map_err(|_| Error::Invalid("Session unavailable".into()))?;
                                        if !writer_ready.load(Ordering::Acquire) || !snapshot.video_connected {
                                            return Err(Error::Invalid("Session is not connected".into()));
                                        }
                                        if !snapshot.can_control {
                                            return Err(Error::Authentication("Control permission is required".into()));
                                        }
                                        if !amikvm_core::video::config::host_display_available(snapshot.host_display, snapshot.host_display_supported) {
                                            return Err(Error::Invalid("BMC 已禁用主机显示控制。".into()));
                                        }
                                        Control::HostDisplay { locked }.encode()
                                    })();
                                    let bytes = match prepared {
                                        Ok(bytes) => bytes,
                                        Err(error) => { let _ = reply.send(Err(error)); continue; }
                                    };
                                    if let Err(error) = transport::write_packet(&mut writer, &bytes).await {
                                        let _ = reply.send(Err(Error::Protocol(error.to_string()))); break Err(error);
                                    }
                                    let _ = reply.send(Ok(()));
                                },
                                Some(Outgoing::SharingAnswer(bytes)) => {
                                    if !writer_snapshot.lock().is_ok_and(|s| s.sharing.can_control()) { continue; }
                                    if let Err(error) = transport::write_packet(&mut writer, &bytes).await { break Err(error); }
                                },
                                Some(Outgoing::Handoff { bytes, reply }) => {
                                    if !writer_snapshot.lock().is_ok_and(|s| s.sharing.role == amikvm_core::sharing::Role::Master && s.sharing.handoff.is_some()) {
                                        if let Some(reply) = reply { let _ = reply.send(Err(Error::Invalid("Control transfer was cancelled".into()))); }
                                        continue;
                                    }
                                    // Release held input while we still own BMC control, before
                                    // sending the transfer. Local input is already disabled.
                                    if had_input {
                                        let release = match protocol::input_report_with_cipher(
                                            writer_sequence.fetch_add(1, Ordering::Relaxed), false, &[0; 8], cipher.as_ref()
                                        ) { Ok(bytes) => bytes, Err(error) => break 'writer Err(error) };
                                        if let Err(error) = transport::write_packet(&mut writer, &release).await { break 'writer Err(error); }
                                        if let Err(error) = crate::mouse::release(&mut writer, &writer_snapshot, &writer_sequence, cipher.as_ref(), &mut pointer).await { break 'writer Err(error); }
                                        had_input = false;
                                    }
                                    writer_media.stop_active().await;
                                    if !writer_snapshot.lock().is_ok_and(|s| s.sharing.role == amikvm_core::sharing::Role::Master && s.sharing.handoff.is_some()) {
                                        if let Some(reply) = reply { let _ = reply.send(Err(Error::Invalid("Control permission changed before transfer".into()))); }
                                        continue;
                                    }
                                    if let Err(error) = transport::write_packet(&mut writer, &bytes).await {
                                        if let Some(reply) = reply { let _ = reply.send(Err(Error::Protocol(error.to_string()))); }
                                        break Err(error);
                                    }
                                    if let Some(reply) = reply { let _ = reply.send(Ok(())); }
                                },
                                Some(Outgoing::Hid { mouse, report, reply, text_generation, locks_token }) => {
                                    // Window getters marshal to the UI thread. Never retain
                                    // the snapshot mutex while waiting for that thread.
                                    let locks_allowed = locks_token.is_none_or(|token| {
                                        let focused = writer_app.get_webview_window("main").is_some_and(|w| w.is_focused().unwrap_or(false));
                                        let owner = writer_snapshot.lock().ok().map(|s| (s.server_id, s.input_focused && s.video_signal && s.video_connected));
                                        focused && owner.is_some_and(|(id, allowed)| allowed && crate::keyboard::locks::owns(&writer_app, id, token))
                                    });
                                    let mouse_allowed = !mouse || (crate::mouse::pointer_owner(&writer_app, &writer_snapshot)
                                        && writer_snapshot.lock().is_ok_and(|s| s.video_connected && s.video_signal && !s.mouse.active()
                                            && !s.mouse_capture.requested() && ((s.mouse_mode.unwrap_or(2) == 2) == (report.len() == 6))));
                                    // A report queued before a BMC permission change must not
                                    // reach the host after another session acquires control.
                                    if !writer_ready.load(Ordering::Acquire)
                                        || !writer_snapshot.lock().is_ok_and(|s| s.can_control)
                                        || text_generation.is_some_and(|value| value != *writer_text_generation.borrow())
                                        || reply.as_ref().is_some_and(|reply| reply.is_closed())
                                        || !locks_allowed
                                        || !mouse_allowed
                                        || (!mouse && report.iter().any(|v| *v != 0)
                                            && writer_snapshot.lock().is_ok_and(|s| s.mouse.active())) {
                                        if let Some(reply) = reply { let _ = reply.send(Err(Error::Invalid("文本输入已停止".into()))); }
                                        continue;
                                    }
                                    let bytes = match protocol::input_report_with_cipher(writer_sequence.fetch_add(1, Ordering::Relaxed), mouse, &report, cipher.as_ref()) {
                                        Ok(bytes) => bytes,
                                        Err(error) => {
                                            if let Some(reply) = reply { let _ = reply.send(Err(Error::Protocol(error.to_string()))); }
                                            break Err(error);
                                        },
                                    };
                                    if let Err(error) = transport::write_packet(&mut writer, &bytes).await {
                                        if let Some(reply) = reply { let _ = reply.send(Err(Error::Protocol(error.to_string()))); }
                                        break Err(error);
                                    }
                                    had_input = true;
                                    if mouse { pointer.written(&report); }
                                    if let Some(reply) = reply { let _ = reply.send(Ok(())); }
                                },
                                Some(Outgoing::Encryption { enabled, requested, reply }) => {
                                    let replacement = if enabled { writer_web.input_cipher().map(Some) } else { Ok(None) };
                                    let replacement = match replacement {
                                        Ok(cipher) => cipher,
                                        Err(error) => {
                                            // A rejected local request keeps the existing mode. A BMC
                                            // requirement cannot fall back to clear input.
                                            if let Some(reply) = reply { let _ = reply.send(Err(error)); continue; }
                                            break Err(error);
                                        }
                                    };
                                    if requested {
                                        let bytes = Control::InputEncryption { enabled }.encode().expect("encryption command");
                                        if let Err(error) = transport::write_packet(&mut writer, &bytes).await {
                                            if let Some(reply) = reply { let _ = reply.send(Err(Error::Protocol(error.to_string()))); }
                                            break Err(error);
                                        }
                                    }
                                    cipher = replacement;
                                    update(&writer_app, &writer_snapshot, |s| {
                                        s.input_encryption = enabled;
                                        s.encryption_required = enabled && !requested;
                                    });
                                    if let Some(reply) = reply { let _ = reply.send(Ok(())); }
                                },
                                None => break Ok(()),
                            },
                            _ = heartbeat.tick(), if writer_ready.load(Ordering::Acquire) => {
                                if let Err(error) = transport::write_packet(&mut writer, &protocol::command(57, 0)).await { break Err(error); }
                            }
                        }
                    };
                    if result.is_err() {
                        writer_sender.advance();
                        let _ = writer_cancel.send(true);
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(2), async {
                        if result.is_ok() {
                            if had_input && writer_snapshot.lock().is_ok_and(|s| s.can_control) {
                                if let Ok(bytes) = protocol::input_report_with_cipher(
                                    writer_sequence.fetch_add(1, Ordering::Relaxed),
                                    false,
                                    &[0; 8],
                                    cipher.as_ref(),
                                ) {
                                    let _ = writer.write_all(&bytes).await;
                                }
                                let _ = crate::mouse::release(
                                    &mut writer,
                                    &writer_snapshot,
                                    &writer_sequence,
                                    cipher.as_ref(),
                                    &mut pointer,
                                )
                                .await;
                            }
                            let _ = writer.write_all(&protocol::command(8, 0)).await;
                        }
                        let _ = writer.shutdown().await;
                    })
                    .await;
                    (result, receiver)
                });
                let mut fragments = Fragments::default();
                let mut approved = false;
                let mut authentication_sent = previous.is_some();
                let mut first_client = false;
                let mut decoder = Decoder::default();
                let mut cursor = Cursor::default();
                let mut last_video_state = std::time::Instant::now();
                let result: Result<()> = async {
                let hostname = hostname::get().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|_| "AMIKVM".into());
                let mac = mac_address::get_mac_address().ok().flatten().map(|m| m.to_string().replace(':', "-")).unwrap_or_default();
                if let Some((id, _)) = previous {
                    worker_sender.send(Outgoing::Bytes(web.reconnect_packet(&connection.local_address.ip().to_string(), &hostname, &mac, id)?)).await.map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                }
                let authentication_started = std::time::Instant::now();
                let mut state_clock = tokio::time::interval(Duration::from_secs(1));
                'packets: loop {
                    // Timer ticks must not drop a partially read IVTP header/body.
                    let packet_future = transport::read_packet(&mut reader);
                    tokio::pin!(packet_future);
                    let read = loop {
                        tokio::select! {
                            biased;
                            _ = cancel_rx.changed() => break 'packets,
                            _ = write_failure_rx.changed() => {
                                return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "KVM writer stopped")));
                            },
                            _ = state_clock.tick() => {
                                if !approved && authentication_started.elapsed() >= Duration::from_secs(30) {
                                    return Err(Error::Timeout("KVM authentication"));
                                }
                                let mut query_power = false;
                                if let Ok(mut snapshot) = worker_snapshot.lock() {
                                    let now = std::time::Instant::now();
                                    let ipmi_changed = snapshot.ipmi.expire(now);
                                    let sharing_changed = snapshot.sharing.expire(now);
                                    let power_changed = snapshot.power.expire(now);
                                    query_power = approved && snapshot.power.query_due(now);
                                    snapshot.can_control = snapshot.sharing.can_control();
                                    if ipmi_changed || sharing_changed || power_changed {
                                        crate::diagnostics::session(&app, &snapshot);
                                        let _ = app.emit("session-state", snapshot.clone());
                                    }
                                }
                                if query_power {
                                    worker_sender.send(Outgoing::PowerStatus).await.map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                                }
                            }
                            read = &mut packet_future => break read,
                        }
                    };
                    let packet = read?;
                    let stale_measurement = worker_snapshot.lock().is_ok_and(|s| {
                        packet.header.kind != 17
                            && s.bandwidth_requested
                                .is_some_and(|t| t.elapsed() >= Duration::from_secs(30))
                    });
                    if stale_measurement {
                        update(&app, &worker_snapshot, |s| {
                            s.bandwidth_measuring = false;
                            s.bandwidth_requested = None;
                            s.message = Some("服务器未开始带宽测量，可以重试".into());
                        });
                    }
                    let header = packet.header;
                    let body = packet.body;
                    crate::diagnostics::record(
                        &app,
                        amikvm_core::diagnostics::Level::Debug,
                        amikvm_core::diagnostics::Category::Protocol,
                        Some(web.server.id),
                        "收到 IVTP 报文",
                        format!(
                            "kind={} status={} bytes={}",
                            header.kind,
                            header.status,
                            body.len()
                        ),
                    );
                    if let Ok(mut snapshot) = worker_snapshot.lock() {
                        snapshot.bytes_received += packet.wire_bytes;
                    }
                    match header.kind {
                        8 => {
                            update(&app, &worker_snapshot, |s| {
                                s.phase = "disconnected".into();
                                s.message = Some(
                                    amikvm_core::service::end_reason(header.status)
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| {
                                            format!("BMC terminated session ({})", header.status)
                                        }),
                                );
                            });
                            break;
                        }
                        22 => {
                            return Err(amikvm_core::error::VideoSessionError::SessionLimit(header.status).into());
                        }
                        23 => {
                            let local_macs = if !body.is_empty() && !web.config.single_port {
                                mac_address::MacAddressIterator::new()
                                    .map_err(|_| amikvm_core::error::VideoSessionError::LocalAddressesUnavailable)?
                                    .map(|mac| mac.bytes())
                                    .collect::<Vec<_>>()
                            } else { vec![] };
                            first_client = protocol::session::hello(header.status, &body, web.config.single_port, &local_macs)?;
                            if authentication_sent { continue; }
                            let auth = web.authentication_packet(
                                &connection.local_address.ip().to_string(),
                                &hostname,
                                &mac,
                            )?;
                            if web.config.oem_features & 32 != 0 {
                                worker_sender
                                    .send(Outgoing::Bytes(protocol::command(58, 0)))
                                    .await
                                    .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            }
                            worker_sender
                                .send(Outgoing::Bytes(auth))
                                .await
                                .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            worker_sender
                                .send(Outgoing::Bytes(protocol::command(6, 0)))
                                .await
                                .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            worker_sender
                                .send(Outgoing::Bytes(protocol::command(128, 0)))
                                .await
                                .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            authentication_sent = true;
                        }
                        19 => {
                            let session_id = protocol::session::validation(&body, authentication_sent)?;
                            // A later refusal still terminates the session. A
                            // repeated success must not reset confirmed control
                            // permissions or re-send post-authentication traffic.
                            if approved {
                                update(&app, &worker_snapshot, |s| s.own_session_id = session_id);
                                continue;
                            }
                            approved = true;
                            if previous.is_none() {
                                worker_sender.send(Outgoing::Bytes(web.cookie_packet()?)).await.map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            }
                            for bytes in [
                                protocol::packet(51, 0, &[2])?,
                                protocol::command(40, 0),
                                protocol::command(39, 0),
                                protocol::command(11, 1),
                            ] {
                                worker_sender
                                    .send(Outgoing::Bytes(bytes))
                                    .await
                                    .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            }
                            update(&app, &worker_snapshot, |s| {
                                s.phase = "connected".into();
                                s.video_connected = true;
                                let confirmed_role = s.sharing.role;
                                s.sharing.authenticated(first_client);
                                if confirmed_role != amikvm_core::sharing::Role::Disconnected {
                                    s.sharing.role = confirmed_role;
                                } else if let Some((_, role)) = previous {
                                    s.sharing.role = role;
                                }
                                s.recovery.authenticated();
                                s.message = None;
                                s.can_control = s.sharing.can_control();
                                s.own_session_id = session_id;
                            });
                            worker_sender.send(Outgoing::PowerStatus).await.map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            worker_ready.store(true, Ordering::Release);
                        }
                        9 => {
                            if let Ok(mut video) = worker_video.lock() {
                                video.no_signal();
                            }
                            update(&app, &worker_snapshot, |s| {
                                s.message = Some("No video signal".into());
                                s.video_signal = false;
                            });
                        }
                        25 if approved => {
                            if let Some(frame) = fragments.push(&body)? {
                                if decoder.decode(&frame)? {
                                    if let Ok(mut video) = worker_video.lock() {
                                        video.publish(&decoder, &cursor);
                                    }
                                    let mut notify = false;
                                    if let Ok(mut s) = worker_snapshot.lock() {
                                        notify = !s.video_signal
                                            || s.video_width != decoder.width
                                            || s.video_height != decoder.height
                                            || s.video_source_width != decoder.source_width
                                            || s.video_source_height != decoder.source_height;
                                        s.frames_received += 1;
                                        s.video_width = decoder.width;
                                        s.video_height = decoder.height;
                                        s.video_source_width = decoder.source_width;
                                        s.video_source_height = decoder.source_height;
                                        s.video_signal = true;
                                        s.message = None;
                                    }
                                    if notify
                                        || last_video_state.elapsed() >= Duration::from_millis(500)
                                    {
                                        update(&app, &worker_snapshot, |_| {});
                                        last_video_state = std::time::Instant::now();
                                    }
                                }
                            }
                        }
                        4098 if approved => {
                            cursor.update(&body)?;
                            if decoder.width > 0 {
                                if let Ok(mut video) = worker_video.lock() {
                                    video.update_cursor(&decoder, &cursor);
                                }
                            }
                        }
                        4099 if approved => {
                            let config = amikvm_core::video::config::EngineConfig::parse(&body)?;
                            update(&app, &worker_snapshot, |s| {
                                s.video_config = Some(config);
                                s.video_config_revision = s.video_config_revision.wrapping_add(1);
                            });
                        }
                        17 if approved => {
                            let measurement = packet.bandwidth.ok_or_else(|| {
                                Error::Protocol("Missing bandwidth measurement".into())
                            })?;
                            let bandwidth = measurement.preset();
                            worker_sender
                                .send(Outgoing::Bytes(
                                    Control::Bandwidth {
                                        bytes_per_second: bandwidth,
                                    }
                                    .encode()?,
                                ))
                                .await
                                .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            update(&app, &worker_snapshot, |s| {
                                s.bandwidth = Some(bandwidth);
                                s.bandwidth_measuring = false;
                                s.bandwidth_requested = None;
                                s.measured_bytes_per_second =
                                    Some(measurement.bytes_per_second() as u64);
                            });
                        }
                        10 => update(&app, &worker_snapshot, |s| {
                            let mode = body.first().copied();
                            if s.mouse_mode != mode { s.mouse_capture.release(); }
                            s.mouse_mode = mode;
                        }),
                        20 => {
                            if let Some(bits) = body.first() {
                                update(&app, &worker_snapshot, |s| {
                                    s.lock_leds = bits & 7;
                                    s.lock_leds_known = true;
                                });
                                crate::keyboard::locks::request(&app);
                            }
                        },
                        14 | 15 => {
                            worker_sender
                                .send(Outgoing::Encryption {
                                    enabled: true,
                                    requested: false,
                                    reply: None,
                                })
                                .await
                                .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                        }
                        32 | 33 | 50 => {
                            let effect = {
                                let mut snapshot = worker_snapshot
                                    .lock()
                                    .map_err(|_| Error::Invalid("Session unavailable".into()))?;
                                let effect = snapshot.sharing.receive(
                                    header.kind,
                                    header.status,
                                    &body,
                                    std::time::Instant::now(),
                                )?;
                                snapshot.can_control = snapshot.sharing.can_control();
                                if effect.close {
                                    snapshot.message = snapshot.sharing.message.clone();
                                }
                                crate::diagnostics::session(&app, &snapshot);
                                let _ = app.emit("session-state", snapshot.clone());
                                effect
                            };
                            if worker_snapshot.lock().is_ok_and(|s| !s.can_control) {
                                worker_text_generation.send_modify(|value| *value = value.wrapping_add(1));
                                worker_keyboard.lock().await.clear();
                                update(&app, &worker_snapshot, |s| {
                                    s.software_keys.clear();
                                    s.mouse_capture.release();
                                });
                            }
                            if effect.lost_control {
                                worker_media.stop_active().await;
                            }
                            if let Some(reply) = effect.reply {
                                worker_sender
                                    .send(Outgoing::SharingAnswer(reply))
                                    .await
                                    .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            }
                            if effect.close {
                                break;
                            }
                            if effect.gained_control {
                                for kind in [40, 20] {
                                    worker_sender
                                        .send(Outgoing::Bytes(protocol::command(kind, 0)))
                                        .await
                                        .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                                }
                            }
                            worker_sender
                                .send(Outgoing::Bytes(protocol::command(39, 0)))
                                .await
                                .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                        }
                        37 | 38 | 56 => {
                            let (changed, close, config) = {
                                let mut snapshot = worker_snapshot
                                    .lock()
                                    .map_err(|_| Error::Invalid("Session unavailable".into()))?;
                                let mut config = snapshot.config.clone().ok_or_else(|| {
                                    Error::Invalid("Session configuration unavailable".into())
                                })?;
                                let (changed, close) = match header.kind {
                                    37 => {
                                        let effect = snapshot
                                            .service
                                            .receive_services(&body, &mut config)?;
                                        (effect.media_changed, effect.close)
                                    }
                                    38 => {
                                        let changed =
                                            snapshot.service.receive_media(&body, &mut config)?;
                                        let media = snapshot.service.media.clone().unwrap();
                                        if snapshot.mouse_mode != Some(media.mouse_mode) { snapshot.mouse_capture.release(); }
                                        snapshot.mouse_mode = Some(media.mouse_mode);
                                        snapshot.host_display_supported =
                                            Some(media.host_display_control);
                                        (changed, false)
                                    }
                                    _ => (
                                        snapshot.service.receive_instances(&body, &mut config)?,
                                        false,
                                    ),
                                };
                                if close {
                                    snapshot.phase = "disconnected".into();
                                    snapshot.can_control = false;
                                    snapshot.message = snapshot.service.notice.map(str::to_owned);
                                }
                                snapshot.config = Some(config.clone());
                                (changed, close, config)
                            };
                            if close {
                                break;
                            }
                            if changed {
                                worker_media.reconfigure(config).await;
                            }
                            update(&app, &worker_snapshot, |_| {});
                        }
                        39 => {
                            let users = amikvm_core::sharing::users(&body)?;
                            update(&app, &worker_snapshot, |s| s.users = users);
                        }
                        34 if approved => {
                            update(&app, &worker_snapshot, |s| s.power.status_reply(header.status, std::time::Instant::now()))
                        }
                        36 if approved => {
                            update(&app, &worker_snapshot, |s| {
                                s.power.acknowledge(header.status, std::time::Instant::now());
                            });
                        }
                        52 if header.status <= 3 => update(&app, &worker_snapshot, |s| {
                            s.host_display = Some(header.status)
                        }),
                        49 => {
                            let follow_up = if let Ok(mut snapshot) = worker_snapshot.lock() {
                                let next = match amikvm_core::ipmi::Response::parse(
                                    header.status,
                                    &body,
                                ) {
                                    Ok(response) => {
                                        snapshot.ipmi.receive(response, std::time::Instant::now())
                                    }
                                    Err(error) => {
                                        snapshot.ipmi.response_error = Some(error.to_string());
                                        None
                                    }
                                };
                                crate::diagnostics::session(&app, &snapshot);
                                let _ = app.emit("session-state", snapshot.clone());
                                next
                            } else {
                                None
                            };
                            if let Some(request) = follow_up {
                                worker_sender
                                    .send(Outgoing::Bytes(request.encode()?))
                                    .await
                                    .map_err(|_| Error::Io(std::io::ErrorKind::ConnectionAborted.into()))?;
                            }
                        }
                        _ => {}
                    }
                }
                Ok(())
            }
            .await;
                // Stop this transport first; no command from it may enter the next one.
                worker_ready.store(false, Ordering::Release);
                worker_text_generation.send_modify(|value| *value = value.wrapping_add(1));
                update(&app, &worker_snapshot, |s| {
                    s.video_connected = false;
                    s.lock_leds_known = false;
                    s.input_focused = false;
                    s.mouse_capture.release();
                    s.video_signal = false;
                    if s.phase != "disconnected" {
                        s.phase = if result
                            .as_ref()
                            .err()
                            .is_some_and(amikvm_core::recovery::retryable)
                        {
                            "reconnecting"
                        } else {
                            "disconnected"
                        }
                        .into();
                    }
                });
                worker_sender.advance();
                let _ = stop_tx.send(());
                let writer_result = writer_task.await;
                let result = match writer_result {
                    Ok((writer_result, returned)) => {
                        receiver = returned;
                        // Preserve explicit BMC/session shutdown. A writer fault wakes the
                        // reader as an IO error and retains the actual transport cause.
                        if result
                            .as_ref()
                            .err()
                            .is_some_and(amikvm_core::recovery::retryable)
                        {
                            writer_result.and(result)
                        } else {
                            result
                        }
                    }
                    Err(error) => break 'connections Err(Error::Protocol(error.to_string())),
                };
                while receiver.try_recv().is_ok() {}
                worker_keyboard.lock().await.clear();
                if approved {
                    previous = worker_snapshot
                        .lock()
                        .ok()
                        .map(|s| (s.own_session_id.unwrap_or(255), s.sharing.role));
                }
                update(&app, &worker_snapshot, |s| {
                    s.can_control = false;
                    s.video_connected = false;
                    s.lock_leds_known = false;
                    s.input_focused = false;
                    s.video_signal = false;
                    s.software_keys.clear();
                    s.input_encryption = false;
                    s.encryption_required = false;
                    s.ipmi.close();
                    s.power.close();
                    s.sharing.close();
                    s.users.clear();
                    s.mouse.context(
                        s.mouse_mode == Some(1),
                        false,
                        s.video_width,
                        s.video_height,
                    );
                    s.bandwidth_measuring = false;
                    s.bandwidth_requested = None;
                });
                worker_media.stop_active().await;
                let recording_guard = worker_recording_operation.lock().await;
                let recording = worker_video
                    .lock()
                    .ok()
                    .and_then(|mut v| v.recording.take());
                if let Some(recording) = recording {
                    let _ = tauri::async_runtime::spawn_blocking(move || recording.finish()).await;
                }
                if let Ok(mut video) = worker_video.lock() {
                    video.clear_frame();
                }
                drop(recording_guard);
                if *cancel_rx.borrow() {
                    break Ok(());
                }
                match result {
                    Err(error)
                        if previous.is_none_or(|(_, role)| {
                            role != amikvm_core::sharing::Role::Rejected
                        }) =>
                    {
                        if retry_connection(&app, &worker_snapshot, &mut cancel_rx, &error).await {
                            continue;
                        }
                        break if *cancel_rx.borrow() {
                            Ok(())
                        } else {
                            Err(error)
                        };
                    }
                    result => break result,
                }
            };
            worker_ready.store(false, Ordering::Release);
            worker_text_generation.send_modify(|value| *value = value.wrapping_add(1));
            let _ = worker_cancel.send(true);
            worker_media.stop_all().await;
            worker_recordings.shutdown().await;
            worker_captures.shutdown().await;
            worker_keyboard.lock().await.clear();
            let recording_guard = worker_recording_operation.lock().await;
            let recording = worker_video
                .lock()
                .ok()
                .and_then(|mut v| v.recording.take());
            if let Some(recording) = recording {
                let _ = tauri::async_runtime::spawn_blocking(move || recording.finish()).await;
            }
            if let Ok(mut video) = worker_video.lock() {
                video.clear_frame();
                video.close();
            }
            drop(recording_guard);
            update(&app, &worker_snapshot, |s| {
                s.phase = if result.is_err() {
                    "error"
                } else {
                    "disconnected"
                }
                .into();
                if let Err(error) = &result {
                    s.message = Some(error.to_string());
                }
                if result
                    .as_ref()
                    .err()
                    .is_none_or(|error| !amikvm_core::recovery::retryable(error))
                {
                    s.recovery = Default::default();
                }
                s.can_control = false;
                s.video_connected = false;
                s.lock_leds_known = false;
                s.input_focused = false;
                s.mouse_capture.release();
                s.video_signal = false;
                s.sharing.close();
                s.ipmi.close();
                s.power.close();
            });
            let _ = tokio::time::timeout(Duration::from_secs(2), session_web.logout()).await;
            let _ = finish_tx.send(true);
        });
        Ok(Self {
            snapshot,
            video,
            media,
            recordings,
            captures,
            recording_operation,
            file_dialog: AsyncMutex::new(()),
            video_configuration: AsyncMutex::new(()),
            sender,
            ready,
            keyboard,
            text_active: AtomicBool::new(false),
            text_generation,
            text_done: tokio::sync::Notify::new(),
            input_app,
            cancel,
            finished,
        })
    }

    pub async fn control(&self, control: Control) -> Result<()> {
        if !self.ready.load(Ordering::Acquire) {
            return Err(Error::Invalid("Session is not connected".into()));
        }
        // All desktop requests use the same allocator, including direct IPC calls.
        // A caller-supplied identifier cannot collide with the OEM boot operation.
        if let Control::Ipmi { command, .. } = &control {
            return self.send_ipmi(command.clone()).await;
        }
        if matches!(control, Control::RequestControl) {
            let permit = self
                .sender
                .reserve()
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
            let mut snapshot = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            if !self.ready.load(Ordering::Acquire) {
                return Err(Error::Invalid("Session is not connected".into()));
            }
            let bytes = snapshot
                .sharing
                .request_control(std::time::Instant::now())?;
            permit.send(Outgoing::Bytes(bytes));
            crate::diagnostics::session(&self.input_app, &snapshot);
            let _ = self.input_app.emit("session-state", snapshot.clone());
            return Ok(());
        }
        if matches!(control, Control::Power { .. })
            && !self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?
                .config
                .as_ref()
                .is_some_and(|c| c.privileges & 256 != 0)
        {
            return Err(Error::Authentication(
                "This account has no server power privilege".into(),
            ));
        }
        if !matches!(
            control,
            Control::Pause
                | Control::Resume
                | Control::Refresh
                | Control::PowerStatus
                | Control::Bandwidth { .. }
                | Control::DetectBandwidth
                | Control::ActiveUsers
                | Control::RequestControl
        ) && !self
            .snapshot
            .lock()
            .map_err(|_| Error::Invalid("Session unavailable".into()))?
            .can_control
        {
            return Err(Error::Authentication(
                "Session has view-only permissions".into(),
            ));
        }
        if let Control::HostDisplay { locked } = control {
            let (reply, result) = oneshot::channel();
            self.sender
                .send(Outgoing::HostDisplay { locked, reply })
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
            return result
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
        }
        if let Control::Power { operation } = control {
            let (reply, result) = oneshot::channel();
            self.sender
                .send(Outgoing::Power { operation, reply })
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
            return result
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
        }
        if matches!(control, Control::PowerStatus) {
            self.sender
                .send(Outgoing::PowerStatus)
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
            return Ok(());
        }
        if let Control::InputEncryption { enabled } = control {
            self.release_input(true).await?;
            let (reply, result) = oneshot::channel();
            self.sender
                .send(Outgoing::Encryption {
                    enabled,
                    requested: true,
                    reply: Some(reply),
                })
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
            return result
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
        }
        let bytes = control.encode()?;
        let packet_kind = u16::from_le_bytes([bytes[0], bytes[1]]);
        let packet_length = bytes.len();
        if matches!(control, Control::MouseMode { .. }) {
            self.release_input(false).await?;
        }
        // A new software layout must not carry latched keys from the old layout.
        if matches!(control, Control::KeyboardLayout { .. }) {
            self.release_software().await?;
        }
        let detecting = matches!(control, Control::DetectBandwidth);
        if matches!(
            control,
            Control::DetectBandwidth | Control::Bandwidth { .. }
        ) {
            let mut snapshot = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            if snapshot.bandwidth_measuring {
                return Err(Error::Invalid(
                    "Bandwidth measurement is already running".into(),
                ));
            }
            if detecting {
                snapshot.bandwidth_measuring = true;
                snapshot.bandwidth_requested = Some(std::time::Instant::now());
            }
        }
        let result = self
            .sender
            .send(Outgoing::Bytes(bytes))
            .await
            .map_err(|_| Error::Protocol("Connection closed".into()));
        if let Ok(mut snapshot) = self.snapshot.lock() {
            if result.is_err() && detecting {
                snapshot.bandwidth_measuring = false;
                snapshot.bandwidth_requested = None;
            }
            if result.is_ok() {
                crate::diagnostics::record(
                    &self.input_app,
                    amikvm_core::diagnostics::Level::Debug,
                    amikvm_core::diagnostics::Category::Protocol,
                    Some(snapshot.server_id),
                    "IVTP 控制报文已排队",
                    format!("kind={packet_kind}, bytes={packet_length}"),
                );
                match &control {
                    Control::Bandwidth { bytes_per_second } => {
                        snapshot.bandwidth = Some(*bytes_per_second)
                    }
                    Control::KeyboardLayout { layout } => {
                        if let Some(config) = snapshot.config.as_mut() {
                            config.keyboard_layout.clone_from(layout);
                        }
                    }
                    _ => {}
                }
            }
        }
        result
    }

    async fn dispatch_ipmi(
        &self,
        build: impl FnOnce(
            &mut amikvm_core::ipmi::State,
            std::time::Instant,
        ) -> Result<amikvm_core::ipmi::Request>
        + Send,
    ) -> Result<()> {
        if !self.ready.load(Ordering::Acquire) {
            return Err(Error::Invalid("Session is not connected".into()));
        }
        let request = {
            let mut snapshot = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            if !snapshot.can_control {
                return Err(Error::Authentication(
                    "Session has view-only permissions".into(),
                ));
            }
            let request = build(&mut snapshot.ipmi, std::time::Instant::now())?;
            crate::diagnostics::session(&self.input_app, &snapshot);
            let _ = self.input_app.emit("session-state", snapshot.clone());
            request
        };
        let result = async {
            self.sender
                .send(Outgoing::Bytes(request.encode()?))
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))
        }
        .await;
        if let Err(error) = &result {
            update(&self.input_app, &self.snapshot, |snapshot| {
                snapshot.ipmi.send_failed(&request, error.to_string())
            });
        }
        result
    }
    pub async fn send_ipmi(&self, command: Vec<u8>) -> Result<()> {
        self.dispatch_ipmi(move |state, now| state.begin_raw(command, now))
            .await
    }
    pub async fn read_boot(&self) -> Result<()> {
        self.dispatch_ipmi(|state, now| state.begin_boot_read(now))
            .await
    }
    pub async fn apply_boot(
        &self,
        device: amikvm_core::ipmi::BootDevice,
        next_boot_only: bool,
    ) -> Result<()> {
        self.dispatch_ipmi(move |state, now| state.begin_boot_write(device, next_boot_only, now))
            .await
    }
    pub fn clear_ipmi_history(&self) {
        update(&self.input_app, &self.snapshot, |snapshot| {
            snapshot.ipmi.clear_completed()
        });
    }

    pub async fn configure_video(
        &self,
        setting: amikvm_core::video::config::Setting,
    ) -> Result<()> {
        let _operation = self.video_configuration.lock().await;
        if !self.ready.load(Ordering::Acquire) {
            return Err(Error::Invalid("Session is not connected".into()));
        }
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Outgoing::VideoConfig { setting, reply })
            .await
            .map_err(|_| Error::Protocol("Connection closed".into()))?;
        result
            .await
            .map_err(|_| Error::Protocol("Connection closed".into()))?
    }
    pub fn can_record(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn sharing_policy(&self, policy: amikvm_core::sharing::Policy) -> Result<()> {
        if !self.ready.load(Ordering::Acquire) {
            return Err(Error::Invalid("Session is not connected".into()));
        }
        let mut snapshot = self
            .snapshot
            .lock()
            .map_err(|_| Error::Invalid("Session unavailable".into()))?;
        snapshot.sharing.set_policy(policy)?;
        crate::diagnostics::session(&self.input_app, &snapshot);
        let _ = self.input_app.emit("session-state", snapshot.clone());
        Ok(())
    }

    pub async fn share(
        &self,
        operation: &str,
        user_id: u8,
        request_token: Option<Uuid>,
        identity: Option<amikvm_core::sharing::Identity>,
    ) -> Result<()> {
        self.share_inner(operation, user_id, request_token, identity, None)
            .await
    }

    pub async fn transfer_on_exit(&self, user: amikvm_core::sharing::User) -> Result<()> {
        let (reply, written) = oneshot::channel();
        // Queue backpressure is part of the write deadline, too. Shutdown must
        // not wait indefinitely for capacity before it starts waiting for ACK.
        tokio::time::timeout(Duration::from_secs(20), async {
            self.share_inner(
                "transfer",
                user.id,
                None,
                Some(user.identity()),
                Some(reply),
            )
            .await?;
            written.await.map_err(|_| {
                Error::Protocol("Connection closed before transfer was written".into())
            })?
        })
        .await
        .map_err(|_| Error::Timeout("Control transfer write"))?
    }

    async fn share_inner(
        &self,
        operation: &str,
        user_id: u8,
        request_token: Option<Uuid>,
        identity: Option<amikvm_core::sharing::Identity>,
        handoff_reply: Option<oneshot::Sender<Result<()>>>,
    ) -> Result<()> {
        if !self.ready.load(Ordering::Acquire) {
            return Err(Error::Invalid("Session is not connected".into()));
        }
        // Reserve queue capacity before validating/updating the request. No await
        // separates deadline validation from queueing an answer or transfer.
        let permit = self
            .sender
            .reserve()
            .await
            .map_err(|_| Error::Protocol("Connection closed".into()))?;
        let handoff = matches!(operation, "grant" | "transfer");
        {
            let mut snapshot = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            if !snapshot.can_control {
                return Err(Error::Authentication(
                    "Control permission is required".into(),
                ));
            }
            if snapshot.own_session_id == Some(user_id) {
                return Err(Error::Invalid(
                    "Cannot transfer or remove this session".into(),
                ));
            }
            let bytes = match operation {
                "grant" | "partial" | "deny" | "block_partial" | "block_deny" => {
                    let token = request_token
                        .ok_or_else(|| Error::Invalid("缺少当前权限申请编号".into()))?;
                    snapshot
                        .sharing
                        .decide(token, user_id, operation, std::time::Instant::now())?
                }
                "transfer" | "disconnect" => {
                    let user = snapshot
                        .users
                        .iter()
                        .find(|u| u.id == user_id && identity.as_ref() == Some(&u.identity()))
                        .ok_or_else(|| Error::Invalid("User session no longer exists".into()))?;
                    if operation == "transfer" {
                        let user = user.clone();
                        snapshot.sharing.transfer(user, std::time::Instant::now())?
                    } else {
                        amikvm_core::sharing::disconnect(user_id)?
                    }
                }
                _ => return Err(Error::Invalid("Unknown sharing operation".into())),
            };
            snapshot.can_control = snapshot.sharing.can_control();
            if handoff {
                snapshot.software_keys.clear();
            }
            permit.send(if handoff {
                Outgoing::Handoff {
                    bytes,
                    reply: handoff_reply,
                }
            } else if operation == "disconnect" {
                Outgoing::Bytes(bytes)
            } else {
                Outgoing::SharingAnswer(bytes)
            });
            crate::diagnostics::session(&self.input_app, &snapshot);
            let _ = self.input_app.emit("session-state", snapshot.clone());
        }
        if handoff {
            self.keyboard.lock().await.clear();
        }
        Ok(())
    }

    pub async fn hid(&self, mouse: bool, report: &[u8]) -> Result<()> {
        if !self.ready.load(Ordering::Acquire) {
            return Err(Error::Invalid("Session is not connected".into()));
        }
        if !self
            .snapshot
            .lock()
            .map_err(|_| Error::Invalid("Session unavailable".into()))?
            .can_control
        {
            return Err(Error::Authentication(
                "Session has view-only permissions".into(),
            ));
        }
        if (mouse && ![4, 6].contains(&report.len())) || (!mouse && report.len() != 8) {
            return Err(Error::Invalid("Invalid HID report length".into()));
        }
        self.sender
            .send(Outgoing::Hid {
                mouse,
                report: report.to_vec(),
                reply: None,
                text_generation: None,
                locks_token: None,
            })
            .await
            .map_err(|_| Error::Protocol("Connection closed".into()))
    }

    pub async fn stop(&self) {
        self.focus(false);
        self.cancel_text();
        self.cancel_capture();
        let _ = self.cancel.send(true);
        let mut finished = self.finished.clone();
        while !*finished.borrow_and_update() {
            if finished.changed().await.is_err() {
                break;
            }
        }
    }

    pub async fn tap(&self, report: [u8; 8]) -> Result<()> {
        self.key_input_ready()?;
        let mut keyboard = self.keyboard.lock().await;
        self.key_input_ready()?;
        let baseline = keyboard.report();
        self.send_keyboard(&mut keyboard, input::merge_reports(baseline, report))
            .await?;
        tokio::time::sleep(Duration::from_millis(35)).await;
        self.send_keyboard(&mut keyboard, baseline).await
    }

    fn input_ready(&self) -> Result<()> {
        if !self.ready.load(Ordering::Acquire) {
            return Err(Error::Invalid("Session is not connected".into()));
        }
        if !self
            .snapshot
            .lock()
            .map_err(|_| Error::Invalid("Session unavailable".into()))?
            .can_control
        {
            return Err(Error::Authentication(
                "Session has view-only permissions".into(),
            ));
        }
        Ok(())
    }

    async fn send_keyboard(&self, keyboard: &mut input::State, report: [u8; 8]) -> Result<()> {
        if let Err(error) = self.hid(false, &report).await {
            keyboard.clear();
            update(&self.input_app, &self.snapshot, |s| s.software_keys.clear());
            return Err(error);
        }
        Ok(())
    }

    pub async fn toggle_modifier(&self, code: &str) -> Result<()> {
        self.key_input_ready()?;
        let mut keyboard = self.keyboard.lock().await;
        self.key_input_ready()?;
        let report = keyboard.toggle_modifier(code)?;
        self.send_keyboard(&mut keyboard, report).await?;
        update(&self.input_app, &self.snapshot, |s| {
            s.software_keys = keyboard.software_keys()
        });
        Ok(())
    }

    pub async fn release_software(&self) -> Result<()> {
        self.cancel_text();
        let mut keyboard = self.keyboard.lock().await;
        let report = keyboard.release_software();
        update(&self.input_app, &self.snapshot, |s| s.software_keys.clear());
        if self.input_ready().is_ok() {
            self.send_keyboard(&mut keyboard, report).await?;
        }
        Ok(())
    }

    pub(crate) fn cancel_capture(&self) {
        let changed = self
            .snapshot
            .lock()
            .is_ok_and(|mut s| s.mouse_capture.release());
        if changed {
            crate::mouse::notify(&self.input_app, &self.snapshot);
        }
    }

    pub(crate) fn cancel_cursor(&self) {
        if let Ok(mut s) = self.snapshot.lock() {
            s.local_cursor.cancel();
        }
    }

    pub async fn capture_request(&self, enabled: bool) -> Result<()> {
        if enabled {
            self.key_input_ready()?;
            let id = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?
                .server_id;
            if !crate::pointer_capture::selected(&self.input_app, id) {
                return Err(Error::Invalid(
                    "请在有画面的相对鼠标模式下取得控制权限".into(),
                ));
            }
            if !self
                .snapshot
                .lock()
                .is_ok_and(|s| crate::pointer_capture::eligible(&s))
            {
                return Err(Error::Invalid(
                    "请在有画面的相对鼠标模式下取得控制权限".into(),
                ));
            }
        }
        self.release_input(true).await?;
        if enabled {
            self.key_input_ready()?;
            let id = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?
                .server_id;
            if !crate::pointer_capture::selected(&self.input_app, id) {
                return Err(Error::Invalid(
                    "请在有画面的相对鼠标模式下取得控制权限".into(),
                ));
            }
            let mut s = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            if !crate::pointer_capture::eligible(&s) {
                return Err(Error::Invalid(
                    "请在有画面的相对鼠标模式下取得控制权限".into(),
                ));
            }
            s.mouse_capture.request();
            drop(s);
            crate::mouse::notify(&self.input_app, &self.snapshot);
        }
        Ok(())
    }

    async fn cursor_shortcut(&self) -> Result<()> {
        let (id, mode, requested) = self
            .snapshot
            .lock()
            .map(|s| (s.server_id, s.mouse_mode, s.mouse_capture.requested()))
            .map_err(|_| Error::Invalid("Session unavailable".into()))?;
        if mode == Some(3) {
            self.capture_request(!requested).await
        } else {
            let state = self.input_app.state::<crate::commands::AppState>();
            let mut ui = state
                .ui
                .lock()
                .map_err(|_| Error::Invalid("Interface state unavailable".into()))?;
            if !ui.hidden_local_cursor.remove(&id) {
                ui.hidden_local_cursor.insert(id);
            }
            drop(ui);
            self.mouse_display();
            self.input_app.emit("ui-changed", ()).ok();
            Ok(())
        }
    }

    pub async fn input(&self, mut event: Event) -> Result<()> {
        if let Event::Viewport { viewport } = event {
            let id = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?
                .server_id;
            if viewport.is_some() && !crate::pointer_capture::selected(&self.input_app, id) {
                return Ok(());
            }
            let mut s = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            let changed = s.local_cursor.viewport(viewport)?;
            if changed {
                s.mouse.suspend();
            }
            drop(s);
            if changed {
                crate::mouse::notify(&self.input_app, &self.snapshot);
            }
            return Ok(());
        }
        if let Event::Focus { focused } = event {
            self.focus(focused);
            if !focused {
                self.release_input(false).await?;
            }
            return Ok(());
        }
        if let Event::Key { modifiers, .. } = &event {
            let (layout, caps) = {
                let snapshot = self
                    .snapshot
                    .lock()
                    .map_err(|_| Error::Invalid("Session unavailable".into()))?;
                let layout = snapshot
                    .config
                    .as_ref()
                    .map(|config| config.keyboard_layout.as_str())
                    .unwrap_or("AD");
                let layout = if layout == "AD" {
                    self.input_app
                        .state::<crate::commands::AppState>()
                        .host_keyboard
                        .snapshot
                        .lock()
                        .ok()
                        .and_then(|s| s.layout)
                } else {
                    input::layout::Layout::parse(layout).ok()
                };
                let caps = if snapshot.lock_leds_known {
                    snapshot.lock_leds & 2 != 0
                } else {
                    modifiers.is_some_and(|m| m.caps_lock)
                };
                (layout, caps)
            };
            self.keyboard.lock().await.physical_layout(layout, caps);
        }
        #[cfg(target_os = "linux")]
        if let Event::Key {
            code,
            key,
            pressed: true,
            modifiers,
            location,
        } = &mut event
        {
            let unresolved_keypad = *location == 3
                && matches!(key.as_str(), "," | ".")
                && !matches!(
                    self.keyboard
                        .lock()
                        .await
                        .physical_code(code, key, *location)
                        .as_str(),
                    "Comma" | "Period"
                );
            if key == "Unidentified" || key == "Dead" || unresolved_keypad {
                if let Some(logical) =
                    crate::keyboard::native::logical_key(&self.input_app, code, *modifiers).await
                {
                    *key = logical.into();
                }
            }
        }
        if matches!(
            &event,
            Event::Key { pressed: true, .. } | Event::Pointer { buttons: 1.., .. }
        ) {
            self.focus(true);
        }
        if let Event::Key {
            code,
            key,
            location,
            ..
        } = &event
        {
            let id = self.snapshot.lock().ok().map(|s| s.server_id);
            if let Some(id) = id {
                let code = self
                    .keyboard
                    .lock()
                    .await
                    .physical_code(code, key, *location);
                crate::keyboard::locks::physical_key(&self.input_app, id, &code);
            }
        }
        if let Event::PointerCapture {
            token,
            locked,
            failed,
        } = &event
        {
            let focused = !locked
                || self
                    .input_app
                    .get_webview_window("main")
                    .is_some_and(|window| window.is_focused().unwrap_or(false));
            let id = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?
                .server_id;
            let selected = crate::pointer_capture::selected(&self.input_app, id);
            let change = {
                let mut s = self
                    .snapshot
                    .lock()
                    .map_err(|_| Error::Invalid("Session unavailable".into()))?;
                let allowed = focused && selected && crate::pointer_capture::eligible(&s);
                s.mouse_capture.event(*token, *locked && allowed, *failed)
            };
            if change == input::capture::Change::Ignored {
                return Ok(());
            }
            crate::mouse::notify(&self.input_app, &self.snapshot);
            if change == input::capture::Change::Locked {
                return Ok(());
            }
            event = Event::ReleaseAll;
        }
        if matches!(&event, Event::Key { code, key, location, pressed: true, .. }
            if input::physical::resolved_code(code, key, *location) == "Escape")
            && self
                .snapshot
                .lock()
                .is_ok_and(|s| s.mouse_capture.requested())
        {
            event = Event::ReleaseAll;
        }
        if matches!(
            event,
            Event::Release | Event::ReleaseAll | Event::SoftKey { pressed: false, .. }
        ) {
            self.cancel_text();
        }
        if matches!(event, Event::Release | Event::ReleaseAll) {
            self.cancel_capture();
        }
        if matches!(event, Event::Pointer { buttons, .. } if buttons != 0) {
            self.cancel_text();
        }
        if self.text_active.load(Ordering::Acquire)
            && matches!(
                event,
                Event::Key { .. } | Event::SoftKey { pressed: true, .. }
            )
        {
            return Ok(());
        }
        if let Event::Key {
            code,
            key,
            location,
            pressed,
            modifiers,
        } = &event
        {
            let id = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?
                .server_id;
            let active_console = self
                .input_app
                .state::<crate::commands::AppState>()
                .ui
                .lock()
                .is_ok_and(|ui| {
                    ui.selected == Some(id)
                        && !ui.playback_selected
                        && matches!(ui.dialog, crate::ui::Dialog::None)
                });
            if !active_console {
                self.keyboard.lock().await.release_key(code, key, *location);
                return Ok(());
            }
            // JViewer replaces its normal key listener during calibration.
            // Route the wizard before menu shortcuts, including Alt+T and Full.
            let calibration = self
                .snapshot
                .lock()
                .ok()
                .filter(|s| s.can_control && s.mouse.active())
                .and_then(|s| s.mouse.token);
            if let Some(token) = calibration {
                self.sender
                    .send(Outgoing::Mouse(crate::mouse::Operation::Key {
                        code: code.clone(),
                        key: key.clone(),
                        location: *location,
                        pressed: *pressed,
                        modifiers: *modifiers,
                        token,
                    }))
                    .await
                    .map_err(|_| Error::Protocol("Connection closed".into()))?;
                return Ok(());
            }
            let local = if *pressed {
                let (options, mouse_mode) = self
                    .snapshot
                    .lock()
                    .map(|s| (s.keyboard_options, s.mouse_mode))
                    .map_err(|_| Error::Invalid("Session unavailable".into()))?;
                let keyboard = self.keyboard.lock().await;
                let routed_code = keyboard.local_code(code, key, *location);
                keyboard
                    .local_modifiers(key, *modifiers)
                    .and_then(|modifiers| {
                        input::routing::local(&routed_code, modifiers, options, mouse_mode)
                    })
            } else {
                None
            };
            if let Some(action) = local {
                use input::routing::Action;
                if matches!(
                    action,
                    Action::Pause
                        | Action::Resume
                        | Action::Refresh
                        | Action::Capture
                        | Action::Fullscreen
                        | Action::HostDisplay
                ) {
                    self.release_input(true).await?;
                    let id = self
                        .snapshot
                        .lock()
                        .map_err(|_| Error::Invalid("Session unavailable".into()))?
                        .server_id;
                    return match action {
                        Action::Pause | Action::Resume | Action::Refresh => {
                            crate::ui::video_control(
                                &self.input_app,
                                id,
                                match action {
                                    Action::Pause => Control::Pause,
                                    Action::Resume => Control::Resume,
                                    _ => Control::Refresh,
                                },
                            )
                            .await
                            .map_err(Error::Invalid)
                        }
                        Action::Capture => crate::ui::capture(&self.input_app, id)
                            .await
                            .map_err(Error::Invalid),
                        Action::Fullscreen => {
                            crate::ui::fullscreen(&self.input_app).map_err(Error::Invalid)
                        }
                        Action::HostDisplay => {
                            let locked = self.snapshot.lock().ok().and_then(|s| {
                                (s.can_control
                                    && amikvm_core::video::config::host_display_available(
                                        s.host_display,
                                        s.host_display_supported,
                                    ))
                                .then_some(s.host_display != Some(1))
                            });
                            if let Some(locked) = locked {
                                self.control(Control::HostDisplay { locked }).await
                            } else {
                                Ok(()) // The original disabled host-display menu consumes Alt+N.
                            }
                        }
                        _ => unreachable!(),
                    };
                }
                if action == Action::Cursor {
                    return self.cursor_shortcut().await;
                }
                if action == Action::Calibrate {
                    return self.mouse_command(input::mouse::Command::Start, None).await;
                }
                let mut keyboard = self.keyboard.lock().await;
                let report = if action == Action::Paste {
                    keyboard.release_physical()
                } else {
                    keyboard.clear();
                    [0; 8]
                };
                update(&self.input_app, &self.snapshot, |s| {
                    s.software_keys = keyboard.software_keys()
                });
                if self.input_ready().is_ok() {
                    self.send_keyboard(&mut keyboard, report).await?;
                }
                drop(keyboard);
                if action == Action::Paste {
                    let state = self.input_app.state::<crate::commands::AppState>();
                    let id = self
                        .snapshot
                        .lock()
                        .map_err(|_| Error::Invalid("Session unavailable".into()))?
                        .server_id;
                    let session = state
                        .sessions
                        .lock()
                        .await
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| Error::Invalid("Session not found".into()))?;
                    return session.paste().await;
                }
                if action == Action::Log {
                    let app = self.input_app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = crate::ui::toggle_log_file(app.clone()).await {
                            crate::diagnostics::record(
                                &app,
                                amikvm_core::diagnostics::Level::Error,
                                amikvm_core::diagnostics::Category::Interface,
                                None,
                                "操作失败",
                                &error,
                            );
                            let state = app.state::<crate::commands::AppState>();
                            if let Ok(mut ui) = state.ui.lock() {
                                ui.error = Some(error);
                            }
                            app.emit("ui-changed", ()).ok();
                        }
                    });
                } else {
                    let state = self.input_app.state::<crate::commands::AppState>();
                    state
                        .ui
                        .lock()
                        .map_err(|_| Error::Invalid("Interface state unavailable".into()))?
                        .dialog = crate::ui::Dialog::About;
                    self.input_app.emit("ui-changed", ()).ok();
                }
                return Ok(());
            }
            // The canvas still forwards raw keyboard events for local actions
            // while paused or view-only; ordinary keys remain Rust-gated.
            let (id, can_control) = self
                .snapshot
                .lock()
                .map(|s| (s.server_id, s.can_control))
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            let blocked = !can_control
                || self
                    .input_app
                    .state::<crate::commands::AppState>()
                    .ui
                    .lock()
                    .map(|ui| {
                        !matches!(ui.dialog, crate::ui::Dialog::None) || ui.paused.contains(&id)
                    })
                    .unwrap_or(true);
            if blocked {
                self.keyboard.lock().await.release_key(code, key, *location);
                return Ok(());
            }
        }
        match &event {
            Event::Key {
                code,
                key,
                location,
                pressed,
                modifiers,
            } => {
                let mut keyboard = self.keyboard.lock().await;
                if let Err(error) = self.input_ready() {
                    if !pressed {
                        keyboard.release_key(code, key, *location);
                        return Ok(());
                    }
                    return Err(error);
                }
                let host = self
                    .snapshot
                    .lock()
                    .map_err(|_| Error::Invalid("Session unavailable".into()))?
                    .keyboard_options
                    .host;
                let event = input::physical::Key {
                    code,
                    key,
                    location: *location,
                    pressed: *pressed,
                    modifiers: *modifiers,
                };
                for report in keyboard.physical_key(event, host) {
                    self.send_keyboard(&mut keyboard, report).await?;
                }
            }
            Event::SoftKey { code, pressed } => {
                if *pressed {
                    self.key_input_ready()?;
                }
                let mut keyboard = self.keyboard.lock().await;
                if let Err(error) = self.input_ready() {
                    if !pressed {
                        keyboard.soft_key(code, false);
                        update(&self.input_app, &self.snapshot, |s| {
                            s.software_keys = keyboard.software_keys()
                        });
                        return Ok(());
                    }
                    return Err(error);
                }
                let report = keyboard
                    .soft_key(code, *pressed)
                    .ok_or_else(|| Error::Invalid("无法识别软键盘按键".into()))?;
                self.send_keyboard(&mut keyboard, report).await?;
                update(&self.input_app, &self.snapshot, |s| {
                    s.software_keys = keyboard.software_keys()
                });
            }
            Event::Release | Event::ReleaseAll => {
                self.release_input(matches!(event, Event::ReleaseAll))
                    .await?
            }
            Event::Pointer { buttons, wheel, .. } => {
                self.input_ready()?;
                if *buttons != 0 || *wheel != 0.0 {
                    let mut keyboard = self.keyboard.lock().await;
                    if let Some(report) = keyboard.flush_physical() {
                        self.send_keyboard(&mut keyboard, report).await?;
                    }
                }
                self.sender
                    .send(Outgoing::Mouse(crate::mouse::Operation::Pointer(event)))
                    .await
                    .map_err(|_| Error::Protocol("Connection closed".into()))?;
            }
            Event::PointerCapture { .. } => {
                unreachable!("capture callback handled before input routing")
            }
            Event::Focus { .. } | Event::Viewport { .. } => {
                unreachable!("context handled before input routing")
            }
        }
        Ok(())
    }

    pub fn focus(&self, focused: bool) {
        let id = match self.snapshot.lock() {
            Ok(s) => s.server_id,
            Err(_) => return,
        };
        // DOM focus persists across native window blur. Native ownership is
        // checked separately by the policy and writer, including refocus.
        let focused = focused && crate::pointer_capture::selected(&self.input_app, id);
        let mut changed = false;
        if let Ok(mut snapshot) = self.snapshot.lock() {
            let focused = focused && snapshot.video_connected && snapshot.can_control;
            if snapshot.input_focused != focused {
                snapshot.input_focused = focused;
                changed = true;
            }
        }
        if !focused {
            crate::keyboard::locks::cancel(&self.input_app, Some(id));
        } else if changed {
            crate::keyboard::locks::request(&self.input_app);
        }
    }

    /// No modifiers or user-held keys may accompany an automatic lock toggle.
    /// The writer checks ownership again at dispatch; key-up always follows a
    /// successful key-down even if focus is lost during the 35 ms interval.
    pub async fn sync_locks(&self, token: Uuid, desired: u8, mask: u8) -> Result<bool> {
        let keyboard = self.keyboard.lock().await;
        if !keyboard.idle() || self.key_input_ready().is_err() {
            return Ok(false);
        }
        let id = self
            .snapshot
            .lock()
            .map_err(|_| Error::Invalid("Session unavailable".into()))?
            .server_id;
        for (bit, usage) in [(1, 0x53), (2, 0x39), (4, 0x47)] {
            if !crate::keyboard::locks::owns(&self.input_app, id, token) {
                return Ok(false);
            }
            self.key_input_ready()?;
            let leds = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?
                .lock_leds;
            if (leds ^ desired) & mask & bit == 0 {
                continue;
            }
            let mut report = [0; 8];
            report[2] = usage;
            if let Err(error) = self.lock_report(report, Some(token)).await {
                let _ = self.hid(false, &[0; 8]).await;
                return Err(error);
            }
            tokio::time::sleep(Duration::from_millis(35)).await;
            // Do not invalidate this release when a newer job supersedes us.
            if let Err(error) = self.lock_report([0; 8], None).await {
                let _ = self.hid(false, &[0; 8]).await;
                return Err(error);
            }
        }
        Ok(true)
    }
    async fn lock_report(&self, report: [u8; 8], token: Option<Uuid>) -> Result<()> {
        let (reply, written) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(2), async {
            self.sender
                .send(Outgoing::Hid {
                    mouse: false,
                    report: report.to_vec(),
                    reply: Some(reply),
                    text_generation: None,
                    locks_token: token,
                })
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
            written
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?
        })
        .await
        .map_err(|_| Error::Timeout("Keyboard lock synchronization"))?
    }

    async fn release_input(&self, all: bool) -> Result<()> {
        self.cancel_text();
        self.cancel_capture();
        self.cancel_cursor();
        if all {
            update(&self.input_app, &self.snapshot, |s| s.mouse.suspend());
        }
        let mut keyboard = self.keyboard.lock().await;
        let report = if all {
            keyboard.clear();
            update(&self.input_app, &self.snapshot, |s| s.software_keys.clear());
            [0; 8]
        } else {
            keyboard.release_physical()
        };
        if self.input_ready().is_err() {
            return Ok(());
        }
        self.send_keyboard(&mut keyboard, report).await?;
        self.sender
            .send(Outgoing::ReleaseMouse)
            .await
            .map_err(|_| Error::Protocol("Connection closed".into()))
    }

    pub async fn mouse_command(
        &self,
        command: input::mouse::Command,
        token: Option<Uuid>,
    ) -> Result<()> {
        self.input_ready()?;
        let (reply, written) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(20), async {
            self.sender
                .send(Outgoing::Mouse(crate::mouse::Operation::Command {
                    command,
                    token,
                    reply,
                }))
                .await
                .map_err(|_| Error::Protocol("Connection closed".into()))?;
            written.await.map_err(|_| {
                Error::Protocol("Connection closed before mouse operation completed".into())
            })?
        })
        .await
        .map_err(|_| Error::Timeout("Mouse operation write"))?
    }

    pub fn mouse_display(&self) {
        crate::mouse::display(&self.input_app, &self.snapshot, &self.video);
    }

    fn key_input_ready(&self) -> Result<()> {
        self.input_ready()?;
        if self.text_active.load(Ordering::Acquire) {
            return Err(Error::Invalid("请先停止当前文本输入".into()));
        }
        if self.snapshot.lock().is_ok_and(|s| s.mouse.active()) {
            return Err(Error::Invalid("请先结束当前鼠标校准".into()));
        }
        Ok(())
    }
}
