use crate::session::Snapshot;
use amikvm_core::{
    Error, Result,
    auth::WebSession,
    media::{
        redirect::{Redirector, Status},
        scsi::Kind,
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex as AsyncMutex, watch};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Slot {
    cd: bool,
    number: u8,
}
impl Slot {
    fn new(kind: Kind, number: u8) -> Self {
        Self {
            cd: kind == Kind::Cdrom,
            number,
        }
    }
}
struct Pending {
    path: PathBuf,
    finished: watch::Receiver<()>,
}
#[derive(Default)]
struct Registry {
    handles: HashMap<Slot, Arc<Redirector>>,
    pending: HashMap<Slot, Pending>,
}
pub struct Manager {
    web: Mutex<Arc<WebSession>>,
    reconfiguring: AtomicBool,
    configuration: AsyncMutex<()>,
    registry: AsyncMutex<Registry>,
    snapshot: Arc<Mutex<Snapshot>>,
    app: AppHandle,
    parent_cancel: watch::Receiver<bool>,
    closed: AtomicBool,
    generation: watch::Sender<u64>,
}
impl Manager {
    pub fn new(
        web: Arc<WebSession>,
        snapshot: Arc<Mutex<Snapshot>>,
        app: AppHandle,
        parent_cancel: watch::Receiver<bool>,
    ) -> Self {
        Self {
            web: Mutex::new(web),
            reconfiguring: AtomicBool::new(false),
            configuration: AsyncMutex::new(()),
            registry: AsyncMutex::new(Registry::default()),
            snapshot,
            app,
            parent_cancel,
            closed: AtomicBool::new(false),
            generation: watch::channel(0).0,
        }
    }
    fn update(&self, value: Status) {
        update(&self.app, &self.snapshot, value);
    }
    pub async fn start(
        &self,
        kind: Kind,
        number: u8,
        path: PathBuf,
        readonly: bool,
        usb: bool,
        boost: bool,
    ) -> Result<()> {
        let mut generation_rx = self.generation.subscribe();
        let generation = *generation_rx.borrow_and_update();
        if (self.closed.load(Ordering::Acquire) || self.reconfiguring.load(Ordering::Acquire))
            || *self.parent_cancel.borrow()
        {
            return Err(Error::Invalid(
                if self.reconfiguring.load(Ordering::Acquire) {
                    "虚拟介质配置正在更新，请稍后重试".into()
                } else {
                    "Session is closing".into()
                },
            ));
        }
        {
            let snapshot = self
                .snapshot
                .lock()
                .map_err(|_| Error::Invalid("Session unavailable".into()))?;
            if snapshot.phase != "connected" {
                return Err(Error::Invalid("会话未连接，无法启动介质重定向".into()));
            }
            if snapshot.video_connected && !snapshot.can_control {
                return Err(Error::Authentication("仅查看会话不能启动介质重定向".into()));
            }
        }
        let source = path.clone();
        let path = tokio::task::spawn_blocking(move || source.canonicalize())
            .await
            .map_err(|e| Error::Protocol(e.to_string()))??;
        let slot = Slot::new(kind, number);
        // A dropped sender tells stop_active that this start has finished
        // dropping its connection (or stopped any worker it created).
        let (_completion, finished) = watch::channel(());
        {
            let mut registry = self.registry.lock().await;
            if (self.closed.load(Ordering::Acquire) || self.reconfiguring.load(Ordering::Acquire))
                || *self.parent_cancel.borrow()
                || *self.generation.borrow() != generation
            {
                return Err(Error::Invalid(
                    "介质连接已在配置更新、权限切换或关闭过程中取消".into(),
                ));
            }
            if registry.pending.contains_key(&slot)
                || registry
                    .handles
                    .get(&slot)
                    .is_some_and(|h| h.status.borrow().active())
            {
                return Err(Error::Invalid(
                    "This media instance is already active".into(),
                ));
            }
            let mut paths: HashSet<PathBuf> =
                registry.pending.values().map(|p| p.path.clone()).collect();
            for handle in registry.handles.values() {
                let status = handle.status.borrow();
                if status.active() {
                    paths.insert(PathBuf::from(&status.source));
                }
            }
            if paths.contains(&path) {
                return Err(Error::Invalid(
                    "This image is already being redirected".into(),
                ));
            }
            registry.pending.insert(
                slot,
                Pending {
                    path: path.clone(),
                    finished,
                },
            );
        }
        let pending = Status::pending(kind, number, &path, readonly, boost);
        self.update(pending.clone());
        let web = self.web.lock().unwrap().clone();
        let mut parent_cancel = self.parent_cancel.clone();
        let result = tokio::select! {
            biased;
            _ = parent_cancel.changed() => Err(Error::Invalid("Session is closing".into())),
            _ = generation_rx.changed() => Err(Error::Invalid("介质连接已在配置更新、权限切换或关闭过程中取消".into())),
            result = Redirector::start(web, path, pending.clone(), usb) => result,
        };
        let mut registry = self.registry.lock().await;
        let redirector = match result {
            Ok(handle) => Arc::new(handle),
            Err(error) => {
                registry.pending.remove(&slot);
                let mut status = pending;
                status.phase = "error".into();
                status.message = Some(error.to_string());
                self.update(status);
                return Err(error);
            }
        };
        if (self.closed.load(Ordering::Acquire) || self.reconfiguring.load(Ordering::Acquire))
            || *self.parent_cancel.borrow()
            || *self.generation.borrow() != generation
            || self
                .snapshot
                .lock()
                .is_ok_and(|s| s.video_connected && !s.can_control)
        {
            drop(registry);
            redirector.stop().await;
            self.update(redirector.status.borrow().clone());
            self.registry.lock().await.pending.remove(&slot);
            return Err(Error::Invalid(
                "介质连接已在配置更新、权限切换或关闭过程中取消".into(),
            ));
        }
        let mut status = redirector.status.clone();
        self.update(status.borrow().clone());
        registry.pending.remove(&slot);
        registry.handles.insert(slot, redirector);
        drop(registry);
        let snapshot = self.snapshot.clone();
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            while status.changed().await.is_ok() {
                let value = status.borrow_and_update().clone();
                let done = !value.active();
                update(&app, &snapshot, value);
                if done {
                    break;
                }
            }
        });
        Ok(())
    }
    pub async fn stop(&self, kind: Kind, number: u8) -> Result<()> {
        let handle = self
            .registry
            .lock()
            .await
            .handles
            .get(&Slot::new(kind, number))
            .cloned()
            .ok_or_else(|| Error::Invalid("Media instance is not connected".into()))?;
        handle.stop().await;
        self.update(handle.status.borrow().clone());
        Ok(())
    }
    pub async fn watch(&self, kind: Kind, number: u8) -> Option<watch::Receiver<Status>> {
        self.registry
            .lock()
            .await
            .handles
            .get(&Slot::new(kind, number))
            .map(|handle| handle.status.clone())
    }
    pub async fn stop_source(&self, kind: Kind, number: u8, path: &std::path::Path) {
        let handle = self
            .registry
            .lock()
            .await
            .handles
            .get(&Slot::new(kind, number))
            .cloned();
        if let Some(handle) = handle.filter(|h| h.status.borrow().source == path.to_string_lossy())
        {
            handle.stop().await;
            self.update(handle.status.borrow().clone());
        }
    }
    pub async fn stop_all(&self) {
        self.closed.store(true, Ordering::Release);
        self.stop_active().await;
    }
    pub async fn reconfigure(&self, config: amikvm_core::auth::SessionConfig) {
        let _operation = self.configuration.lock().await;
        self.reconfiguring.store(true, Ordering::Release);
        self.stop_active().await;
        let mut web = self.web.lock().unwrap();
        *web = Arc::new(web.with_config(config));
        self.reconfiguring.store(false, Ordering::Release);
    }
    // Ending current redirects must allow new redirects after control is regained.
    pub async fn stop_active(&self) {
        let (handles, pending) = {
            let registry = self.registry.lock().await;
            // Invalidate and collect under the same lock used by start. A new
            // generation may connect after this operation releases the lock.
            self.generation.send_modify(|v| *v = v.wrapping_add(1));
            (
                registry.handles.values().cloned().collect::<Vec<_>>(),
                registry
                    .pending
                    .values()
                    .map(|p| p.finished.clone())
                    .collect::<Vec<_>>(),
            )
        };
        let mut tasks = tokio::task::JoinSet::new();
        for handle in handles {
            let app = self.app.clone();
            let snapshot = self.snapshot.clone();
            tasks.spawn(async move {
                handle.stop().await;
                update(&app, &snapshot, handle.status.borrow().clone());
            });
        }
        while tasks.join_next().await.is_some() {}
        for mut finished in pending {
            let _ = finished.changed().await;
        }
    }
}
fn update(app: &AppHandle, snapshot: &Arc<Mutex<Snapshot>>, value: Status) {
    if let Ok(mut snapshot) = snapshot.lock() {
        let slot = Slot::new(value.kind, value.slot);
        if let Some(previous) = snapshot
            .media
            .iter_mut()
            .find(|s| Slot::new(s.kind, s.slot) == slot)
        {
            // An observer for an older connection must not overwrite a restarted slot.
            if previous.id != value.id && value.phase != "connecting" {
                return;
            }
            *previous = value;
        } else {
            snapshot.media.push(value);
        }
        crate::diagnostics::session(&app, &snapshot);
        let _ = app.emit("session-state", snapshot.clone());
    }
}
