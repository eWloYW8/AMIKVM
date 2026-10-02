//! Serializes relative movement/calibration with HID encryption and other session writes.
use crate::{commands::AppState, session::Snapshot, video::Video};
use amikvm_core::{
    Error, Result,
    input::{
        self,
        mouse::{Command, Settings},
    },
    protocol, transport,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{io::AsyncWrite, sync::oneshot};
use uuid::Uuid;

pub enum Operation {
    Command {
        command: Command,
        token: Option<Uuid>,
        reply: oneshot::Sender<Result<()>>,
    },
    Key {
        code: String,
        pressed: bool,
        token: Uuid,
    },
    Pointer(input::Event),
}
#[derive(Default)]
pub struct Effect {
    pub reports: Vec<Vec<u8>>,
    pub release_keyboard: bool,
    pub notify: bool,
}
fn context(s: &mut Snapshot) -> bool {
    s.mouse.context(
        s.mouse_mode == Some(1),
        s.phase == "connected" && s.video_connected && s.video_signal && s.can_control,
        if s.video_source_width > 0 {
            s.video_source_width
        } else {
            s.video_width
        },
        if s.video_source_height > 0 {
            s.video_source_height
        } else {
            s.video_height
        },
    )
}
pub fn process(
    app: &AppHandle,
    snapshot: &Arc<Mutex<Snapshot>>,
    operation: Operation,
) -> (Result<Effect>, Option<oneshot::Sender<Result<()>>>) {
    let (operation, reply) = match operation {
        Operation::Command {
            command,
            token,
            reply,
        } => ((Some((command, token)), None, None), Some(reply)),
        Operation::Key {
            code,
            pressed,
            token,
        } => ((None, Some((code, pressed, token)), None), None),
        Operation::Pointer(event) => ((None, None, Some(event)), None),
    };
    let result = (|| {
        let mut s = snapshot
            .lock()
            .map_err(|_| Error::Invalid("Mouse state unavailable".into()))?;
        let cancelled = context(&mut s);
        if !s.can_control || !s.video_connected {
            return Err(Error::Authentication(
                "Control permission is required".into(),
            ));
        }
        let mut command = operation.0;
        if let Some((code, pressed, token)) = operation.1 {
            if s.mouse.token != Some(token) {
                return Ok(Effect::default());
            }
            command = s.mouse.key(&code, pressed).map(|c| (c, Some(token)));
        }
        if let Some((command, token)) = command {
            let start = matches!(command, Command::Start);
            let reports = s
                .mouse
                .command(command, token, std::time::Instant::now())?
                .into_iter()
                .map(Vec::from)
                .collect();
            if start {
                s.software_keys.clear();
            }
            let settings = s.mouse.settings;
            let id = s.server_id;
            drop(s);
            save_settings(app, id, settings);
            return Ok(Effect {
                reports,
                release_keyboard: start,
                notify: true,
            });
        }
        if let Some(event) = operation.2 {
            if s.mouse.active() {
                return Ok(Effect::default());
            }
            let reports = input::mouse(&event, s.mouse_mode.unwrap_or(2) == 2)?;
            s.mouse.movement(&reports);
            return Ok(Effect {
                reports,
                notify: cancelled,
                ..Default::default()
            });
        }
        Ok(Effect {
            notify: cancelled,
            ..Default::default()
        })
    })();
    (result, reply)
}
fn save_settings(app: &AppHandle, id: Uuid, settings: Settings) {
    if let Ok(mut ui) = app.state::<AppState>().ui.lock() {
        ui.mouse_settings.insert(id, settings);
    }
}
pub fn tick(
    app: &AppHandle,
    snapshot: &Arc<Mutex<Snapshot>>,
    video: &Arc<Mutex<Video>>,
) -> Vec<Vec<u8>> {
    let mut reports = vec![];
    let mut changed = false;
    if let Ok(mut s) = snapshot.lock() {
        changed = context(&mut s);
        let id = s.server_id;
        drop(s);
        let selected = app
            .state::<AppState>()
            .ui
            .lock()
            .is_ok_and(|ui| ui.selected == Some(id));
        let closing = app.state::<AppState>().shutdown.blocks_connection(id);
        if selected && !closing {
            if let Ok(mut s) = snapshot.lock() {
                reports = s
                    .mouse
                    .tick(std::time::Instant::now())
                    .into_iter()
                    .map(Vec::from)
                    .collect();
                changed |= !reports.is_empty();
            }
        }
    }
    if changed {
        display(app, snapshot, video);
        notify(app, snapshot);
    }
    reports
}
pub fn display(app: &AppHandle, snapshot: &Arc<Mutex<Snapshot>>, video: &Arc<Mutex<Video>>) {
    let Some((id, active, point)) = snapshot
        .lock()
        .ok()
        .map(|s| (s.server_id, s.mouse.active(), s.mouse.reference))
    else {
        return;
    };
    let hidden = app
        .state::<AppState>()
        .ui
        .lock()
        .is_ok_and(|ui| ui.hidden_local_cursor.contains(&id));
    if let Ok(mut v) = video.lock() {
        v.reference(if active || !hidden { point } else { None });
    }
}
pub fn notify(app: &AppHandle, snapshot: &Arc<Mutex<Snapshot>>) {
    if let Ok(s) = snapshot.lock() {
        crate::diagnostics::session(&app, &s);
        let _ = app.emit("session-state", s.clone());
    }
}
pub fn message(app: &AppHandle, snapshot: &Arc<Mutex<Snapshot>>, message: String) {
    if let Ok(mut s) = snapshot.lock() {
        s.mouse.message = Some(message);
    }
    notify(app, snapshot);
}
pub struct Written {
    pub sent: bool,
    pub complete: bool,
}
pub async fn write<W: AsyncWrite + Unpin>(
    writer: &mut W,
    snapshot: &Arc<Mutex<Snapshot>>,
    sequence: &AtomicU32,
    cipher: Option<&input::encryption::Cipher>,
    reports: &[Vec<u8>],
) -> Result<Written> {
    let mut sent = false;
    for report in reports {
        if !snapshot.lock().is_ok_and(|s| {
            s.can_control
                && s.video_connected
                && ((s.mouse_mode.unwrap_or(2) == 2) == (report.len() == 6))
        }) {
            return Ok(Written {
                sent,
                complete: false,
            });
        }
        let bytes = protocol::input_report_with_cipher(
            sequence.fetch_add(1, Ordering::Relaxed),
            true,
            report,
            cipher,
        )?;
        transport::write_packet(writer, &bytes).await?;
        sent = true;
    }
    Ok(Written {
        sent,
        complete: true,
    })
}
