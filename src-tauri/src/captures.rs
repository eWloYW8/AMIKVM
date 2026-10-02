//! Transient BMC preview connections and read-only captured images.
use amikvm_core::{auth::WebSession, video::remote_capture::Kind};
use serde::Serialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex as AsyncMutex, watch};
use uuid::Uuid;

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: Uuid,
    pub kind: Option<Kind>,
    pub busy: bool,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub error: Option<String>,
    pub saved_path: Option<String>,
}

pub struct Manager {
    pub id: Uuid,
    pub video: Arc<Mutex<crate::video::Video>>,
    web: Arc<WebSession>,
    app: AppHandle,
    snapshot: Mutex<Snapshot>,
    operation: Arc<AsyncMutex<()>>,
    cancel: Mutex<Option<watch::Sender<bool>>>,
    closing: AtomicBool,
}

impl Manager {
    pub fn new(web: Arc<WebSession>, app: AppHandle) -> Self {
        let id = Uuid::new_v4();
        Self {
            id,
            video: Arc::new(Mutex::new(crate::video::Video::default())),
            web,
            app,
            snapshot: Mutex::new(Snapshot {
                id,
                kind: None,
                busy: false,
                width: 0,
                height: 0,
                source_width: 0,
                source_height: 0,
                error: None,
                saved_path: None,
            }),
            operation: Arc::new(AsyncMutex::new(())),
            cancel: Mutex::new(None),
            closing: AtomicBool::new(false),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| Snapshot {
                id: self.id,
                error: Some("捕获状态不可用".into()),
                ..Default::default()
            })
    }
    fn update(&self, f: impl FnOnce(&mut Snapshot)) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            f(&mut snapshot);
            crate::diagnostics::changed(
                &self.app,
                amikvm_core::diagnostics::Category::Capture,
                Some(self.web.server.id),
                "capture",
                "画面捕获状态已改变",
                serde_json::json!({"busy":snapshot.busy,"kind":snapshot.kind,"size":[snapshot.width,snapshot.height],"error":snapshot.error,"saved":snapshot.saved_path}),
                if snapshot.error.is_some() {
                    amikvm_core::diagnostics::Level::Error
                } else {
                    amikvm_core::diagnostics::Level::Info
                },
            );
        }
        let _ = self.app.emit("ui-changed", ());
    }
    pub fn refresh(self: &Arc<Self>, kind: Kind) -> Result<(), String> {
        let guard = self
            .operation
            .clone()
            .try_lock_owned()
            .map_err(|_| "正在抓取 BMC 画面，请等待当前操作结束")?;
        let (cancel, receiver) = watch::channel(false);
        {
            let mut job = self.cancel.lock().map_err(|_| "画面取消状态不可用")?;
            if self.closing.load(Ordering::Acquire) {
                return Err("服务器连接已关闭".into());
            }
            *job = Some(cancel);
        }
        self.video
            .lock()
            .map_err(|_| "捕获画面不可用")?
            .clear_frame();
        self.update(|s| {
            s.kind = Some(kind);
            s.busy = true;
            s.error = None;
            s.saved_path = None;
            s.width = 0;
            s.height = 0;
            s.source_width = 0;
            s.source_height = 0;
        });
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = manager.web.capture_image(kind, receiver).await;
            match result {
                Ok(frame) => {
                    if let Ok(mut video) = manager.video.lock() {
                        video.publish_pixels(frame.width, frame.height, &frame.rgba, None);
                    }
                    manager.update(|s| {
                        s.width = frame.width;
                        s.height = frame.height;
                        s.source_width = frame.source_width;
                        s.source_height = frame.source_height;
                    });
                }
                Err(error) => manager.update(|s| s.error = Some(error.to_string())),
            }
            if let Ok(mut cancel) = manager.cancel.lock() {
                *cancel = None;
            }
            manager.update(|s| s.busy = false);
            drop(guard);
        });
        Ok(())
    }
    pub fn cancel(&self) {
        if let Ok(cancel) = self.cancel.lock() {
            if let Some(cancel) = &*cancel {
                let _ = cancel.send(true);
            }
        }
    }
    pub fn saved(&self, path: String) {
        self.update(|s| s.saved_path = Some(path));
    }
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.cancel();
        let _guard = self.operation.lock().await;
        if let Ok(mut video) = self.video.lock() {
            video.close();
        }
    }
}
