//! Per-web-session BMC recording operations. Shutdown waits for download unlock.
use amikvm_core::{auth::WebSession, recordings::Entry};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, watch};

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub phase: String,
    pub file: Option<String>,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub saved_path: Option<String>,
    pub error: Option<String>,
    pub loaded: bool,
}

pub struct Manager {
    web: Arc<WebSession>,
    app: AppHandle,
    snapshot: Mutex<Snapshot>,
    operation: Arc<AsyncMutex<()>>,
    cancel: Mutex<Option<watch::Sender<bool>>>,
    closing: AtomicBool,
}

impl Manager {
    pub fn new(web: Arc<WebSession>, app: AppHandle) -> Self {
        Self {
            web,
            app,
            snapshot: Mutex::new(Snapshot {
                phase: "idle".into(),
                ..Default::default()
            }),
            operation: Arc::new(AsyncMutex::new(())),
            cancel: Mutex::new(None),
            closing: AtomicBool::new(false),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }
    fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            f(&mut snapshot);
            crate::diagnostics::changed(
                &self.app,
                amikvm_core::diagnostics::Category::Recording,
                Some(self.web.server.id),
                "bmc",
                "BMC 录像任务状态已改变",
                serde_json::json!({"phase":snapshot.phase,"file":snapshot.file,"error":snapshot.error,"saved":snapshot.saved_path,"entries":snapshot.entries.len()}),
                if snapshot.error.is_some() {
                    amikvm_core::diagnostics::Level::Error
                } else {
                    amikvm_core::diagnostics::Level::Info
                },
            );
        }
        let _ = self.app.emit("ui-changed", ());
    }
    fn begin(&self, phase: &str) -> Result<(OwnedMutexGuard<()>, watch::Receiver<bool>), String> {
        let guard = self
            .operation
            .clone()
            .try_lock_owned()
            .map_err(|_| "正在处理 BMC 录像，请等待当前操作结束")?;
        if self.closing.load(Ordering::Acquire) {
            return Err("服务器连接已关闭".into());
        }
        let (cancel, receiver) = watch::channel(false);
        {
            let mut job = self.cancel.lock().map_err(|_| "录像取消状态不可用")?;
            if self.closing.load(Ordering::Acquire) {
                return Err("服务器连接已关闭".into());
            }
            *job = Some(cancel);
        }
        self.update(|s| {
            s.phase = phase.into();
            s.error = None;
            s.downloaded = 0;
            s.total = None;
            s.saved_path = None;
            s.file = None;
        });
        Ok((guard, receiver))
    }
    fn finish(&self, error: Option<String>) {
        if let Ok(mut cancel) = self.cancel.lock() {
            *cancel = None;
        }
        self.update(|s| {
            s.phase = "idle".into();
            s.error = error;
        });
    }
    pub fn refresh(self: &Arc<Self>) -> Result<(), String> {
        let (guard, mut cancel) = self.begin("catalog")?;
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = async { if !*cancel.borrow() { let _ = cancel.changed().await; } } => Err("录像列表刷新已取消".into()),
                result = manager.web.recording_catalog() => result.map_err(|e| e.to_string()),
            };
            let error = match result {
                Ok(entries) => {
                    manager.update(|s| {
                        s.entries = entries;
                        s.loaded = true;
                    });
                    None
                }
                Err(error) => Some(error),
            };
            manager.finish(error);
            drop(guard);
        });
        Ok(())
    }
    pub fn entry(&self, file: &str) -> Result<Entry, String> {
        self.snapshot()
            .entries
            .into_iter()
            .find(|e| e.file == file)
            .ok_or_else(|| "录像不在当前服务器列表中，请刷新后重试".into())
    }
    pub fn download(
        self: &Arc<Self>,
        file: String,
        path: PathBuf,
        temporary: Option<tempfile::TempDir>,
        playback: Arc<crate::playback::Manager>,
    ) -> Result<(), String> {
        self.entry(&file)?;
        let (guard, cancel) = self.begin("download")?;
        self.update(|s| s.file = Some(file.clone()));
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let last = Mutex::new(Instant::now() - Duration::from_secs(1));
            let result = manager
                .web
                .download_recording(&file, &path, cancel.clone(), |downloaded, total| {
                    if let Ok(mut snapshot) = manager.snapshot.lock() {
                        snapshot.downloaded = downloaded;
                        snapshot.total = total;
                    }
                    if let Ok(mut last) = last.lock() {
                        if last.elapsed() >= Duration::from_millis(100) {
                            let _ = manager.app.emit("ui-changed", ());
                            *last = Instant::now();
                        }
                    }
                })
                .await
                .map_err(|e| e.to_string());
            let result = match result {
                Ok(())
                    if temporary.is_some()
                        && !*cancel.borrow()
                        && !manager.closing.load(Ordering::Acquire) =>
                {
                    match playback.open(manager.app.clone(), path, temporary).await {
                        Ok(()) => crate::ui::select_playback(&manager.app).await,
                        Err(error) => Err(error),
                    }
                }
                Ok(()) => {
                    if temporary.is_none() {
                        manager
                            .update(|s| s.saved_path = Some(path.to_string_lossy().into_owned()));
                    }
                    Ok(())
                }
                Err(error) => Err(error),
            };
            manager.finish(result.err());
            drop(guard);
        });
        Ok(())
    }
    pub fn cancel(&self) {
        if let Ok(cancel) = self.cancel.lock() {
            if let Some(cancel) = cancel.as_ref() {
                let _ = cancel.send(true);
            }
        }
    }
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.cancel();
        let _operation = self.operation.lock().await;
    }
}
