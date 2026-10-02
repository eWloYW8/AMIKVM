//! Offline playback: native readers/decoders, interruptible clock, pause and seek.
use crate::video::Video;
use amikvm_core::playback::{Data, Decoder, Frame, Reader};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: Uuid,
    pub path: String,
    pub name: String,
    pub format: String,
    pub phase: String,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub signal: bool,
    pub error: Option<String>,
}

struct Run {
    close: bool,
    paused: bool,
    stopped: bool,
    generation: u64,
    seek: u64,
    base: u64,
    anchor: Instant,
}
impl Run {
    fn position(&self) -> u64 {
        self.base.saturating_add(if self.paused || self.stopped {
            0
        } else {
            self.anchor.elapsed().as_millis() as u64
        })
    }
}
struct Control {
    run: Mutex<Run>,
    changed: Condvar,
}

pub struct Player {
    pub video: Arc<Mutex<Video>>,
    pub snapshot: Arc<Mutex<Snapshot>>,
    control: Arc<Control>,
    worker: Mutex<Option<JoinHandle<()>>>,
    _temporary: Option<tempfile::TempDir>,
}

#[derive(Default)]
pub struct Manager {
    player: Mutex<Option<Arc<Player>>>,
    operation: AsyncMutex<()>,
    pub file_dialog: AsyncMutex<()>,
}

impl Manager {
    pub fn current(&self) -> Option<Arc<Player>> {
        self.player.lock().ok()?.clone()
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.current()?.snapshot.lock().ok().map(|s| s.clone())
    }
    pub fn get(&self, id: Uuid) -> Option<Arc<Player>> {
        self.current()
            .filter(|p| p.snapshot.lock().is_ok_and(|s| s.id == id))
    }

    pub async fn open(
        &self,
        app: AppHandle,
        path: PathBuf,
        temporary: Option<tempfile::TempDir>,
    ) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let open_path = path.clone();
        let reader = tauri::async_runtime::spawn_blocking(move || Reader::open(&open_path))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let previous = self.player.lock().map_err(|_| "回放状态不可用")?.take();
        if let Some(previous) = previous {
            tauri::async_runtime::spawn_blocking(move || previous.close())
                .await
                .map_err(|e| e.to_string())?;
        }
        let snapshot = Arc::new(Mutex::new(Snapshot {
            id: Uuid::new_v4(),
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path: path.to_string_lossy().into_owned(),
            format: reader.metadata.format.into(),
            phase: "loading".into(),
            position_ms: 0,
            duration_ms: reader.metadata.duration_ms,
            width: 0,
            height: 0,
            signal: false,
            error: None,
        }));
        let control = Arc::new(Control {
            run: Mutex::new(Run {
                close: false,
                paused: false,
                stopped: false,
                generation: 0,
                seek: 0,
                base: 0,
                anchor: Instant::now(),
            }),
            changed: Condvar::new(),
        });
        let video = Arc::new(Mutex::new(Video::default()));
        let worker_control = control.clone();
        let worker_snapshot = snapshot.clone();
        let worker_video = video.clone();
        let worker_app = app.clone();
        let worker = std::thread::Builder::new()
            .name("amikvm-playback".into())
            .spawn(move || {
                let result = play(
                    &worker_app,
                    &path,
                    reader,
                    &worker_control,
                    &worker_snapshot,
                    &worker_video,
                );
                if let Err(error) = result {
                    update(&worker_app, &worker_snapshot, |s| {
                        s.phase = "error".into();
                        s.error = Some(error);
                    });
                }
            })
            .map_err(|e| e.to_string())?;
        *self.player.lock().map_err(|_| "回放状态不可用")? = Some(Arc::new(Player {
            video,
            snapshot,
            control,
            worker: Mutex::new(Some(worker)),
            _temporary: temporary,
        }));
        let _ = app.emit("ui-changed", ());
        Ok(())
    }

    pub async fn close(&self) {
        let _operation = self.operation.lock().await;
        let player = self.player.lock().ok().and_then(|mut p| p.take());
        if let Some(player) = player {
            let _ = tauri::async_runtime::spawn_blocking(move || player.close()).await;
        }
    }
}

impl Player {
    pub fn action(&self, action: &str, seek: Option<u64>, app: &AppHandle) -> Result<(), String> {
        let mut run = self.control.run.lock().map_err(|_| "回放时钟不可用")?;
        if self.snapshot.lock().is_ok_and(|s| s.phase == "error") {
            return Err("请重新打开录像文件".into());
        }
        match action {
            "pause" => {
                if run.stopped {
                    return Ok(());
                }
                run.base = run.position();
                run.anchor = Instant::now();
                run.paused = !run.paused;
            }
            "play" | "restart" => {
                if run.stopped || action == "restart" {
                    run.seek = 0;
                    run.base = 0;
                    run.generation = run.generation.wrapping_add(1);
                }
                run.anchor = Instant::now();
                run.stopped = false;
                run.paused = false;
            }
            "stop" => {
                run.stopped = true;
                run.paused = true;
                run.base = 0;
            }
            "seek" => {
                let duration = self
                    .snapshot
                    .lock()
                    .map_err(|_| "回放状态不可用")?
                    .duration_ms;
                let seek = seek.ok_or("缺少回放位置")?;
                if duration.is_none() && seek != 0 {
                    return Err("录像长度尚未确定，请等待读取完成".into());
                }
                if duration.is_some_and(|duration| seek > duration) {
                    return Err("回放位置超出录像长度".into());
                }
                run.seek = seek;
                run.base = seek;
                run.anchor = Instant::now();
                run.stopped = false;
                run.generation = run.generation.wrapping_add(1);
            }
            _ => return Err("未知回放操作".into()),
        }
        let phase = if run.stopped {
            "stopped"
        } else if action == "seek" || action == "restart" {
            "seeking"
        } else if run.paused {
            "paused"
        } else {
            "playing"
        };
        let position = run.position();
        update(app, &self.snapshot, |s| {
            s.phase = phase.into();
            s.position_ms = position;
            if run.stopped {
                s.signal = false;
            }
        });
        self.control.changed.notify_all();
        Ok(())
    }

    fn close(&self) {
        if let Ok(mut run) = self.control.run.lock() {
            run.close = true;
            self.control.changed.notify_all();
        }
        if let Some(worker) = self.worker.lock().ok().and_then(|mut w| w.take()) {
            let _ = worker.join();
        }
        if let Ok(mut video) = self.video.lock() {
            video.close();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Ok(mut run) = self.control.run.lock() {
            run.close = true;
            self.control.changed.notify_all();
        }
    }
}

fn update(app: &AppHandle, snapshot: &Mutex<Snapshot>, f: impl FnOnce(&mut Snapshot)) {
    if let Ok(mut snapshot) = snapshot.lock() {
        f(&mut snapshot);
        crate::diagnostics::changed(
            app,
            amikvm_core::diagnostics::Category::Playback,
            None,
            "player",
            "回放状态已改变",
            serde_json::json!({"phase":snapshot.phase,"file":snapshot.path,"error":snapshot.error,"signal":snapshot.signal}),
            if snapshot.error.is_some() {
                amikvm_core::diagnostics::Level::Error
            } else {
                amikvm_core::diagnostics::Level::Info
            },
        );
    }
    let _ = app.emit("ui-changed", ());
}

fn publish(
    app: &AppHandle,
    snapshot: &Mutex<Snapshot>,
    video: &Mutex<Video>,
    frame: Option<Frame<'_>>,
    signal: bool,
) {
    if let Some(frame) = frame {
        if let Ok(mut video) = video.lock() {
            video.publish_pixels(frame.width, frame.height, frame.rgba, None);
        }
        let mut changed = false;
        if let Ok(mut s) = snapshot.lock() {
            changed = s.width != frame.width || s.height != frame.height || s.signal != signal;
            s.width = frame.width;
            s.height = frame.height;
            s.signal = signal;
        }
        if changed {
            let _ = app.emit("ui-changed", ());
        }
    } else {
        update(app, snapshot, |s| s.signal = false);
    }
}

fn play(
    app: &AppHandle,
    path: &std::path::Path,
    mut reader: Reader,
    control: &Control,
    snapshot: &Mutex<Snapshot>,
    video: &Mutex<Video>,
) -> Result<(), String> {
    let mut decoder = Decoder::default();
    let mut generation = 0;
    let mut seeking = true;
    let mut target = 0;
    let mut signal = false;
    let mut emitted = Instant::now();
    loop {
        {
            let mut run = control.run.lock().map_err(|_| "回放时钟不可用")?;
            while run.stopped && !run.close {
                run = control.changed.wait(run).map_err(|_| "回放时钟不可用")?;
            }
            if run.close {
                return Ok(());
            }
            if run.generation != generation {
                generation = run.generation;
                target = run.seek;
                seeking = true;
                drop(run);
                // Open before dropping the previous reader, keeping a shared file lock.
                reader = Reader::open(path).map_err(|e| e.to_string())?;
                decoder = Default::default();
                signal = false;
            }
        }
        let packet = reader.read_next().map_err(|e| e.to_string())?;
        if seeking
            && packet
                .as_ref()
                .is_none_or(|packet| packet.timestamp_ms > target)
        {
            let mut run = control.run.lock().map_err(|_| "回放时钟不可用")?;
            if run.close {
                return Ok(());
            }
            if run.generation != generation || run.stopped {
                continue;
            }
            run.base = target;
            run.anchor = Instant::now();
            seeking = false;
            publish(app, snapshot, video, decoder.latest(), signal);
            update(app, snapshot, |s| {
                s.position_ms = target;
                s.phase = if run.paused { "paused" } else { "playing" }.into();
            });
        }
        let timestamp = packet.as_ref().map_or_else(
            || {
                snapshot
                    .lock()
                    .ok()
                    .and_then(|s| s.duration_ms)
                    .unwrap_or(target)
                    .max(target)
            },
            |p| p.timestamp_ms,
        );
        if !seeking {
            let mut run = control.run.lock().map_err(|_| "回放时钟不可用")?;
            loop {
                if run.close {
                    return Ok(());
                }
                if run.generation != generation || run.stopped {
                    break;
                }
                let position = run.position();
                if !run.paused && position >= timestamp {
                    break;
                }
                if emitted.elapsed() >= Duration::from_millis(250) {
                    update(app, snapshot, |s| {
                        s.position_ms = position.min(s.duration_ms.unwrap_or(position))
                    });
                    emitted = Instant::now();
                }
                run = control
                    .changed
                    .wait_timeout(run, Duration::from_millis(50))
                    .map_err(|_| "回放时钟不可用")?
                    .0;
            }
            if run.generation != generation || run.stopped {
                continue;
            }
        }
        let Some(packet) = packet else {
            let mut run = control.run.lock().map_err(|_| "回放时钟不可用")?;
            if run.generation != generation || run.stopped {
                continue;
            }
            run.base = timestamp;
            run.stopped = true;
            run.paused = true;
            update(app, snapshot, |s| {
                s.position_ms = timestamp;
                s.duration_ms = Some(s.duration_ms.unwrap_or(timestamp).max(timestamp));
                s.phase = "ended".into();
            });
            continue;
        };
        target = target.max(packet.timestamp_ms);
        if matches!(packet.data, Data::NoSignal) {
            signal = false;
        }
        let frame = decoder.decode(&packet.data).map_err(|e| e.to_string())?;
        let run = control.run.lock().map_err(|_| "回放时钟不可用")?;
        if run.close {
            return Ok(());
        }
        if run.generation != generation || run.stopped {
            continue;
        }
        if let Some(frame) = frame {
            signal = true;
            if !seeking {
                publish(app, snapshot, video, Some(frame), true);
            }
        } else if !seeking && matches!(packet.data, Data::NoSignal) {
            publish(app, snapshot, video, None, false);
        }
        if !seeking && emitted.elapsed() >= Duration::from_millis(250) {
            update(app, snapshot, |s| s.position_ms = packet.timestamp_ms);
            emitted = Instant::now();
        }
    }
}
