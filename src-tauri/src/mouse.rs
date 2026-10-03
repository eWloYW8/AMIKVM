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
        key: String,
        location: u8,
        pressed: bool,
        modifiers: Option<input::routing::Modifiers>,
        token: Uuid,
    },
    Pointer(input::Event),
}
#[derive(Default)]
pub struct Effect {
    pub reports: Vec<Vec<u8>>,
    pub release_keyboard: bool,
    pub notify: bool,
    pub pointer: Option<Pointer>,
}
#[derive(Clone, Copy)]
pub struct Pointer {
    pub token: Option<Uuid>,
    pub position: (f64, f64),
}
fn context(s: &mut Snapshot) -> bool {
    let width = if s.video_source_width > 0 {
        s.video_source_width
    } else {
        s.video_width
    };
    let height = if s.video_source_height > 0 {
        s.video_source_height
    } else {
        s.video_height
    };
    let mut changed = s.mouse.context(
        s.mouse_mode == Some(1),
        s.phase == "connected" && s.video_connected && s.video_signal && s.can_control,
        width,
        height,
    );
    changed |= s
        .mouse
        .viewport(s.local_cursor.viewport.and_then(|v| v.area(width, height)));
    if changed {
        s.local_cursor.cancel();
    }
    let released = !crate::pointer_capture::eligible(s) && s.mouse_capture.release();
    changed || released
}
pub fn pointer_owner(app: &AppHandle, snapshot: &Arc<Mutex<Snapshot>>) -> bool {
    let id = snapshot.lock().ok().map(|s| s.server_id);
    // Native window getters can wait for the UI thread. Hold no state mutex.
    app.get_webview_window("main")
        .is_some_and(|w| w.is_focused().unwrap_or(false))
        && id.is_some_and(|id| crate::pointer_capture::selected(app, id))
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
            key,
            location,
            pressed,
            modifiers,
            token,
        } => (
            (
                None,
                Some((code, key, location, pressed, modifiers, token)),
                None,
            ),
            None,
        ),
        Operation::Pointer(event) => ((None, None, Some(event)), None),
    };
    let result = (|| {
        if (operation.1.is_some() || operation.2.is_some()) && !pointer_owner(app, snapshot) {
            return Ok(Effect::default());
        }
        let current = operation
            .2
            .as_ref()
            .and_then(|_| crate::cursor::position(app, snapshot));
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
        if let Some((code, key, location, pressed, modifiers, token)) = operation.1 {
            if s.mouse.token != Some(token) {
                return Ok(Effect::default());
            }
            command = s
                .mouse
                .key(input::physical::Key {
                    code: &code,
                    key: &key,
                    location,
                    pressed,
                    modifiers,
                })
                .map(|c| (c, Some(token)));
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
                s.mouse_capture.release();
            }
            let settings = s.mouse.settings;
            let id = s.server_id;
            drop(s);
            save_settings(app, id, settings);
            return Ok(Effect {
                reports,
                release_keyboard: start,
                notify: true,
                pointer: None,
            });
        }
        if let Some(mut event) = operation.2 {
            let (token, mut position) = match &event {
                input::Event::Pointer { capture, x, y, .. } => (*capture, (*x, *y)),
                _ => return Err(Error::Invalid("Expected a pointer event".into())),
            };
            if !s.video_signal || s.mouse.active() || !s.mouse_capture.allows(token) {
                return Ok(Effect::default());
            }
            if let input::Event::Pointer {
                x,
                y,
                width,
                height,
                dx,
                dy,
                entered,
                buttons,
                wheel,
                ..
            } = &mut event
            {
                if ![*x, *y, *dx, *dy, *wheel].iter().all(|v| v.is_finite())
                    || *width == 0
                    || *height == 0
                {
                    return Err(Error::Invalid("Invalid pointer movement".into()));
                }
                if s.mouse_mode == Some(1) && token.is_none() {
                    if let Some((px, py, previous_buttons)) = s.local_cursor.stale(*x, *y, current)
                    {
                        if *buttons == previous_buttons && *wheel == 0. {
                            return Ok(Effect::default());
                        }
                        // Keep a queued button/wheel change, but not its old
                        // coordinates from before the native warp.
                        *x = px;
                        *y = py;
                        *dx = 0.;
                        *dy = 0.;
                        position = (px, py);
                    }
                }
                if s.mouse_mode == Some(1)
                    && token.is_none()
                    && s.local_cursor.synthetic(*x, *y, *buttons, *wheel)
                {
                    s.mouse_capture.baseline(*x, *y, *width, *height);
                    return Ok(Effect::default());
                }
                if s.mouse_mode == Some(1) && s.mouse.needs_sync {
                    let mut reports: Vec<Vec<u8>> = s
                        .mouse
                        .command(Command::Synchronize, None, std::time::Instant::now())?
                        .into_iter()
                        .map(Vec::from)
                        .collect();
                    if *buttons != 0 || *wheel != 0. {
                        reports.extend(input::mouse(
                            &input::Event::Pointer {
                                buttons: *buttons,
                                x: *x,
                                y: *y,
                                width: *width,
                                height: *height,
                                dx: 0.,
                                dy: 0.,
                                wheel: *wheel,
                                capture: token,
                                entered: false,
                            },
                            false,
                            0,
                        )?);
                    }
                    s.mouse_capture.baseline(*x, *y, *width, *height);
                    return Ok(Effect {
                        reports,
                        notify: true,
                        ..Default::default()
                    });
                }
                if token.is_none() {
                    (*dx, *dy) = s.mouse_capture.movement(*x, *y, *width, *height, *entered);
                }
                if s.mouse_mode.unwrap_or(2) != 2 {
                    let source = (
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
                    );
                    (*dx, *dy) = s.mouse_capture.scaled(*dx, *dy, *width, *height, source);
                }
            }
            let source_width = if s.video_source_width > 0 {
                s.video_source_width
            } else {
                s.video_width
            };
            let reports = input::mouse(&event, s.mouse_mode.unwrap_or(2) == 2, source_width)?;
            return Ok(Effect {
                reports,
                notify: cancelled,
                pointer: Some(Pointer { token, position }),
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
        let selected = pointer_owner(app, snapshot);
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
    app: &AppHandle,
    writer: &mut W,
    snapshot: &Arc<Mutex<Snapshot>>,
    sequence: &AtomicU32,
    cipher: Option<&input::encryption::Cipher>,
    reports: &[Vec<u8>],
    pointer: Option<Pointer>,
    state: &mut input::pointer::State,
) -> Result<Written> {
    let mut sent = false;
    for report in reports {
        if !pointer_owner(app, snapshot) {
            return Ok(Written {
                sent,
                complete: false,
            });
        }
        if !snapshot.lock().is_ok_and(|s| {
            s.can_control
                && s.video_connected
                && s.video_signal
                && ((s.mouse_mode.unwrap_or(2) == 2) == (report.len() == 6))
                && pointer.is_none_or(|pointer| {
                    s.video_signal
                        && !s.mouse.active()
                        && s.mouse_capture.allows(pointer.token)
                        && (pointer.token.is_none() || crate::pointer_capture::eligible(&s))
                })
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
        state.written(report);
        if pointer.is_some() {
            if let Ok(mut s) = snapshot.lock() {
                s.mouse.movement(std::slice::from_ref(report));
            }
        }
        sent = true;
    }
    if sent {
        crate::cursor::follow(
            app,
            snapshot,
            pointer.map(|p| p.position),
            reports.last().unwrap()[0],
        )
        .await?;
    }
    Ok(Written {
        sent,
        complete: true,
    })
}

pub async fn release<W: AsyncWrite + Unpin>(
    writer: &mut W,
    snapshot: &Arc<Mutex<Snapshot>>,
    sequence: &AtomicU32,
    cipher: Option<&input::encryption::Cipher>,
    state: &mut input::pointer::State,
) -> Result<()> {
    let absolute = snapshot
        .lock()
        .map_err(|_| Error::Invalid("Mouse state unavailable".into()))?
        .mouse_mode
        .unwrap_or(2)
        == 2;
    if let Some(report) = state.release(absolute) {
        let bytes = protocol::input_report_with_cipher(
            sequence.fetch_add(1, Ordering::Relaxed),
            true,
            &report,
            cipher,
        )?;
        transport::write_packet(writer, &bytes).await?;
        state.written(&report);
    }
    Ok(())
}
