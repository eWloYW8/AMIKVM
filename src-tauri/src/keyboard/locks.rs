//! Lock synchronization policy and ownership live in Rust. OEM bit 512 reverses
//! the default remote-to-local direction. Never infer a remote LED from a write.
mod native;
use crate::{commands::AppState, session::Session};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Inactive,
    Waiting,
    Pending,
    Synchronized,
    Unavailable,
    Failed,
}
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub server: Option<Uuid>,
    pub phase: Phase,
    pub reverse: bool,
    pub mask: u8,
    pub error: Option<String>,
}
struct RemoteJob {
    desired: u8,
    started: Instant,
    sent: bool,
}
struct Owner {
    id: Uuid,
    token: Uuid,
    reverse: bool,
    baseline: Option<(u8, u8)>,
    remote: Option<RemoteJob>,
}
struct Pending {
    bits: u8,
    mask: u8,
    started: Instant,
    restoring: bool,
}
#[derive(Default)]
pub struct State {
    pub status: Status,
    owner: Option<Owner>,
    restore: Option<(u8, u8)>,
    pending: Option<Pending>,
    blocked: Option<(u8, u8)>,
    retry_at: Option<Instant>,
    defer_until: Option<Instant>,
}
impl State {
    fn deactivate(&mut self) {
        if let Some(owner) = self.owner.take() {
            if let Some(baseline) = owner.baseline {
                self.restore = Some(baseline);
            }
            self.blocked = None;
            self.status = Status::default();
        }
    }
    fn fail(&mut self, error: String) {
        self.status.phase = Phase::Failed;
        self.status.error = Some(error);
    }
}

/// Invalidate queued remote writes synchronously, before asynchronous release.
pub fn cancel(app: &AppHandle, id: Option<Uuid>) {
    let mut changed = false;
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut locks) = state.host_keyboard.locks.lock() {
            if id.is_none() || locks.owner.as_ref().is_some_and(|o| Some(o.id) == id) {
                let previous = locks.status.clone();
                locks.deactivate();
                changed = previous != locks.status;
            }
        }
    }
    if changed {
        let _ = app.emit("ui-changed", ());
    }
    request(app);
}
pub fn physical_key(app: &AppHandle, id: Uuid, code: &str) {
    if matches!(code, "CapsLock" | "NumLock" | "ScrollLock")
        && crate::pointer_capture::selected(app, id)
    {
        if let Ok(mut locks) = app.state::<AppState>().host_keyboard.locks.lock() {
            if !locks.owner.as_ref().is_some_and(|o| o.id == id) {
                return;
            }
            // Let the physical toggle and its IVTP 20 feedback settle first.
            locks.defer_until = Some(Instant::now() + Duration::from_millis(350));
            if locks.pending.as_ref().is_some_and(|p| !p.restoring) {
                // A physical toggle supersedes an already submitted native
                // change; it is not evidence that that change failed.
                locks.pending = None;
                locks.blocked = None;
                locks.retry_at = None;
            }
            if let Some(owner) = locks.owner.as_mut().filter(|o| o.reverse) {
                owner.token = Uuid::new_v4();
                owner.remote = None;
            }
        }
    }
}
#[cfg(target_os = "windows")]
pub fn native_input() -> bool {
    native::handles_input()
}

/// Windows lock keys have a single native source; WebView2 copies are ignored.
#[cfg(target_os = "windows")]
pub(super) async fn physical_event(app: &AppHandle, code: &'static str, pressed: bool) {
    let state = app.state::<AppState>();
    let id = state.ui.lock().ok().and_then(|ui| ui.selected);
    let Some(id) = id else { return };
    if !crate::pointer_capture::selected(app, id)
        || !app
            .get_webview_window("main")
            .is_some_and(|w| w.is_focused().unwrap_or(false))
    {
        return;
    }
    let session = state.sessions.lock().await.get(&id).cloned();
    if let Some(session) = session {
        if session.snapshot.lock().is_ok_and(|s| s.input_focused) {
            let _ = session.native_lock_key(code, pressed).await;
        }
    }
}

pub fn owns(app: &AppHandle, id: Uuid, token: Uuid) -> bool {
    app.try_state::<AppState>().is_some_and(|state| {
        !state
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
            && crate::pointer_capture::selected(app, id)
            && state.host_keyboard.locks.lock().is_ok_and(|locks| {
                locks
                    .owner
                    .as_ref()
                    .is_some_and(|o| o.id == id && o.token == token && o.reverse)
            })
    })
}
pub fn request(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || tick(&handle));
}

fn target(app: &AppHandle) -> Result<Option<Arc<Session>>, ()> {
    let state = app.state::<AppState>();
    let selected = state.ui.lock().map_err(|_| ())?.selected;
    let Some(id) = selected else {
        return Ok(None);
    };
    if state
        .shutting_down
        .load(std::sync::atomic::Ordering::Acquire)
        || !crate::pointer_capture::selected(app, id)
        || !app
            .get_webview_window("main")
            .is_some_and(|w| w.is_focused().unwrap_or(false))
    {
        return Ok(None);
    }
    let session = state.sessions.try_lock().map_err(|_| ())?.get(&id).cloned();
    Ok(session.filter(|s| {
        s.snapshot.lock().is_ok_and(|s| {
            s.input_focused
                && s.phase == "connected"
                && s.video_connected
                && s.video_signal
                && s.can_control
                && !s.mouse.active()
        })
    }))
}

fn tick(app: &AppHandle) {
    let Ok(target) = target(app) else {
        return;
    };
    let snapshot = target
        .as_ref()
        .and_then(|s| s.snapshot.lock().ok().map(|s| s.clone()));
    let state = app.state::<AppState>();
    let Ok(mut locks) = state.host_keyboard.locks.lock() else {
        return;
    };
    let previous = locks.status.clone();
    let observation = native::read();
    let key = snapshot.as_ref().map(|s| {
        (
            s.server_id,
            s.config.as_ref().is_some_and(|c| c.oem_features & 512 != 0),
        )
    });
    if locks.owner.as_ref().map(|o| (o.id, o.reverse)) != key {
        locks.deactivate();
    }
    let mut remote = None;
    (|| {
        let local = match observation {
            Ok(local) => local,
            Err(error) => {
                locks.fail(error);
                return;
            }
        };
        let now = Instant::now();
        if let Some(pending) = &locks.pending {
            if (local.bits ^ pending.bits) & pending.mask == 0 {
                if pending.restoring {
                    locks.restore = None;
                }
                locks.pending = None;
                locks.blocked = None;
                locks.retry_at = None;
            } else if now.duration_since(pending.started) < Duration::from_secs(1) {
                locks.status.phase = Phase::Pending;
                return;
            } else {
                locks.blocked = Some((pending.bits, pending.mask));
                locks.pending = None;
                locks.retry_at = Some(now + Duration::from_secs(5));
                locks.fail("本机锁定键状态未确认，同步已停止".into());
                return;
            }
        }
        if let Some((bits, mask)) = locks.restore {
            if local.writable & mask != mask {
                locks.fail("无法恢复本机锁定键状态".into());
                return;
            }
            if (local.bits ^ bits) & mask != 0 {
                if locks.retry_at.is_some_and(|at| now < at) {
                    return;
                }
                write(&mut locks, bits, mask, true, now);
                return;
            }
            locks.restore = None;
            locks.blocked = None;
            locks.retry_at = None;
        }
        let Some((id, reverse)) = key else {
            locks.status = Status::default();
            return;
        };
        if locks.owner.is_none() {
            locks.owner = Some(Owner {
                id,
                reverse,
                token: Uuid::new_v4(),
                baseline: (!reverse && local.writable != 0).then_some((local.bits, local.writable)),
                remote: None,
            });
            locks.status = Status {
                server: Some(id),
                reverse,
                mask: if reverse {
                    local.readable
                } else {
                    local.writable
                },
                ..Default::default()
            };
        }
        let mask = locks.status.mask;
        let snapshot = snapshot.as_ref().unwrap();
        if mask == 0 {
            locks.status.phase = Phase::Unavailable;
            return;
        }
        if !snapshot.lock_leds_known {
            locks.status.phase = Phase::Waiting;
            return;
        }
        if locks.defer_until.is_some_and(|at| now < at) {
            locks.status.phase = Phase::Pending;
            return;
        }
        if (snapshot.lock_leds ^ local.bits) & mask == 0 {
            locks.owner.as_mut().unwrap().remote = None;
            locks.status.phase = Phase::Synchronized;
            locks.status.error = None;
        } else if !reverse {
            let desired = snapshot.lock_leds & mask;
            if locks.blocked != Some((desired, mask)) {
                write(&mut locks, desired, mask, false, now);
            }
        } else {
            let desired = local.bits & mask;
            let owner = locks.owner.as_mut().unwrap();
            if owner
                .remote
                .as_ref()
                .is_some_and(|job| job.desired != desired)
            {
                // A local state change supersedes even a queued/partially sent job.
                owner.token = Uuid::new_v4();
                owner.remote = None;
            }
            if let Some(job) = &owner.remote {
                if job.sent && now.duration_since(job.started) >= Duration::from_secs(3) {
                    locks.fail("远端锁定键状态未确认，同步已停止".into());
                }
            } else {
                // Each job has its own generation, including jobs following an
                // early BMC confirmation while the previous key-up is pending.
                owner.token = Uuid::new_v4();
                owner.remote = Some(RemoteJob {
                    desired,
                    started: now,
                    sent: false,
                });
                remote = Some((owner.token, desired, mask));
                locks.status.phase = Phase::Pending;
                locks.status.error = None;
            }
        }
    })();
    let changed = locks.status != previous;
    drop(locks);
    if changed {
        let _ = app.emit("ui-changed", ());
    }
    if let Some((token, desired, mask)) = remote {
        let session = target.unwrap();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let result = session.sync_locks(token, desired, mask).await;
            let state = app.state::<AppState>();
            if let Ok(mut locks) = state.host_keyboard.locks.lock() {
                if locks.owner.as_ref().is_some_and(|o| o.token == token) {
                    match result {
                        Ok(true) => {
                            if let Some(job) = locks.owner.as_mut().unwrap().remote.as_mut() {
                                job.sent = true;
                            }
                        }
                        Ok(false) => locks.owner.as_mut().unwrap().remote = None,
                        Err(error) => {
                            if let Some(job) = locks.owner.as_mut().unwrap().remote.as_mut() {
                                job.sent = true;
                            }
                            locks.fail(error.to_string());
                        }
                    }
                }
            }
            request(&app);
        });
    }
}
fn write(locks: &mut State, bits: u8, mask: u8, restoring: bool, now: Instant) {
    match native::write(bits, mask) {
        Ok(()) => {
            if native::read().is_ok_and(|o| (o.bits ^ bits) & mask == 0) {
                locks.pending = None;
                locks.blocked = None;
                locks.retry_at = None;
                if restoring {
                    locks.restore = None;
                }
            } else {
                locks.pending = Some(Pending {
                    bits,
                    mask,
                    started: now,
                    restoring,
                });
            }
            locks.status.phase = Phase::Pending;
            locks.status.error = None;
        }
        Err(error) => {
            locks.blocked = Some((bits, mask));
            locks.retry_at = Some(now + Duration::from_secs(5));
            locks.fail(error);
        }
    }
}
pub fn install(app: &AppHandle) {
    if let Err(error) = native::install(app) {
        if let Ok(mut locks) = app.state::<AppState>().host_keyboard.locks.lock() {
            locks.fail(error);
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            request(&app);
            if app
                .state::<AppState>()
                .exit_ready
                .load(std::sync::atomic::Ordering::Acquire)
            {
                break;
            }
        }
    });
}
/// Final synchronous attempt also covers shutdown before the timer's next tick.
pub fn shutdown(app: &AppHandle) {
    cancel(app, None);
    tick(app);
    if let Ok(mut locks) = app.state::<AppState>().host_keyboard.locks.lock() {
        if locks.pending.is_none() {
            if let Some((bits, mask)) = locks.restore {
                let _ = native::write(bits, mask);
            }
        }
        locks.deactivate();
    }
    native::uninstall();
}
