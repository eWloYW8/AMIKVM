use crate::session::Snapshot;
use amikvm_core::{
    Error, Result,
    recording::{Frame, Policy, RecordingSet},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

pub struct Recording {
    latest: Arc<Mutex<Option<Frame>>>,
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    timeline: Arc<Mutex<Timeline>>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}

struct Timeline {
    started: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
}
impl Timeline {
    fn milliseconds(&self) -> u64 {
        self.paused_at
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .saturating_sub(self.paused_total)
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
    fn pause(&mut self, pause: bool) {
        if pause {
            if self.paused_at.is_none() {
                self.paused_at = Some(Instant::now());
            }
        } else if let Some(start) = self.paused_at.take() {
            self.paused_total += start.elapsed();
        }
    }
}

impl Recording {
    pub fn start(
        path: PathBuf,
        frame: Option<Frame>,
        app: AppHandle,
        snapshot: Arc<Mutex<Snapshot>>,
        limit_seconds: u16,
        policy: Policy,
    ) -> Result<Self> {
        if !(1..=1800).contains(&limit_seconds) {
            return Err(Error::Invalid("录制时长须为 1–1800 秒".into()));
        }
        let limit_ms = u64::from(limit_seconds) * 1000;
        let latest = Arc::new(Mutex::new(frame));
        let stop = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        let worker_running = running.clone();
        let timeline = Arc::new(Mutex::new(Timeline {
            started: Instant::now(),
            paused_at: None,
            paused_total: Duration::ZERO,
        }));
        let worker_latest = latest.clone();
        let worker_stop = stop.clone();
        let worker_timeline = timeline.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("mp4-recorder".into())
            .spawn(move || {
                let frame = worker_latest
                    .lock()
                    .map_err(|_| Error::Invalid("Recording frame unavailable".into()))?
                    .clone();
                let mut recorder = RecordingSet::new(path.clone(), policy);
                {
                    if let Err(e) = recorder.frame_at(0, frame) {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return Err(e);
                    }
                    // Encoder/resampler startup is not part of the requested duration.
                    if let Ok(mut clock) = worker_timeline.lock() {
                        clock.started = Instant::now();
                    }
                    if let Ok(mut s) = snapshot.lock() {
                        s.recording = true;
                        s.recording_paused = false;
                        s.recording_path = Some(path.to_string_lossy().into_owned());
                        s.recording_elapsed_ms = 0;
                        s.recording_limit_seconds = limit_seconds;
                        s.recording_policy = policy;
                        s.recording_outputs = recorder
                            .paths()
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect();
                        s.recording_written_ms = 0;
                        s.recording_skipped_ms = 0;
                        s.recording_message = None;
                    }
                    let _ = ready_tx.send(Ok(()));
                }
                let result = (|| -> Result<()> {
                    let mut next = Instant::now() + Duration::from_millis(40);
                    let mut notified = Instant::now();
                    while !worker_stop.load(Ordering::Acquire) {
                        if let Some(delay) = next.checked_duration_since(Instant::now()) {
                            thread::sleep(delay);
                        }
                        if worker_stop.load(Ordering::Acquire) {
                            break;
                        }
                        let (timestamp, elapsed) = {
                            let timeline = worker_timeline.lock().map_err(|_| {
                                Error::Invalid("Recording clock unavailable".into())
                            })?;
                            let elapsed = timeline.milliseconds();
                            if elapsed >= limit_ms {
                                break;
                            }
                            let timestamp = if timeline.paused_at.is_some() {
                                None
                            } else {
                                Some(elapsed)
                            };
                            (timestamp, elapsed)
                        };
                        // Release the clock before publishing state. Pause takes
                        // the snapshot and then the clock in the opposite order.
                        if notified.elapsed() >= Duration::from_millis(250) {
                            if let Ok(mut s) = snapshot.lock() {
                                s.recording_elapsed_ms = elapsed;
                                s.recording_outputs = recorder
                                    .paths()
                                    .iter()
                                    .map(|p| p.to_string_lossy().into_owned())
                                    .collect();
                                s.recording_written_ms = recorder.written_ms();
                                s.recording_skipped_ms = recorder.skipped_ms();
                                crate::diagnostics::session(&app, &s);
                                let _ = app.emit("session-state", s.clone());
                            }
                            notified = Instant::now();
                        }
                        if let Some(timestamp) = timestamp {
                            let frame = worker_latest
                                .lock()
                                .map_err(|_| Error::Invalid("Recording frame unavailable".into()))?
                                .clone();
                            recorder.frame_at(timestamp, frame)?;
                        }
                        next += Duration::from_millis(40);
                        if next < Instant::now() {
                            next = Instant::now();
                        }
                    }
                    Ok(())
                })();
                let end = worker_timeline
                    .lock()
                    .map(|t| t.milliseconds())
                    .unwrap_or(0)
                    .min(limit_ms);
                let finished = recorder.finish_at(end);
                let result = result.and(finished);
                worker_running.store(false, Ordering::Release);
                if let Ok(mut s) = snapshot.lock() {
                    s.recording = false;
                    s.recording_paused = false;
                    s.recording_elapsed_ms = end;
                    s.recording_outputs = recorder
                        .paths()
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    s.recording_written_ms = recorder.written_ms();
                    s.recording_skipped_ms = recorder.skipped_ms();
                    s.recording_message = Some(match &result {
                        Err(e) => e.to_string(),
                        Ok(()) if end >= limit_ms => format!(
                            "已达到 {limit_seconds} 秒，录制已保存（{} 个文件）",
                            recorder.paths().len()
                        ),
                        Ok(()) => format!("录制已保存（{} 个文件）", recorder.paths().len()),
                    });
                    crate::diagnostics::session(&app, &s);
                    let _ = app.emit("session-state", s.clone());
                }
                result
            })?;
        ready_rx
            .recv()
            .map_err(|_| Error::Invalid("Recording worker did not start".into()))?
            .map_err(Error::Invalid)?;
        Ok(Self {
            latest,
            stop,
            running,
            timeline,
            worker: Some(worker),
        })
    }
    pub fn frame(&self, frame: Option<Frame>) {
        if let Ok(mut latest) = self.latest.lock() {
            *latest = frame;
        }
    }
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
    pub fn pause(&self, pause: bool) {
        if let Ok(mut timeline) = self.timeline.lock() {
            timeline.pause(pause);
        }
    }
    pub fn finish(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .map(|w| {
                w.join()
                    .map_err(|_| Error::Invalid("Recording worker failed".into()))?
            })
            .unwrap_or(Ok(()))
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
