//! Folder jobs outlive their network sessions; writable working images survive exit.
use amikvm_core::{
    Error, Result,
    media::{
        folder::{Change, Mapping, Plan, Progress},
        scsi::Kind,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    id: Uuid,
    server_id: Uuid,
    slot: u8,
    root: PathBuf,
    image: PathBuf,
    size_mib: u32,
    readonly: bool,
    mapping: Option<Mapping>,
}

#[derive(Clone)]
pub struct Snapshot {
    pub id: Uuid,
    pub server_id: Uuid,
    pub slot: u8,
    pub root: String,
    pub image: String,
    pub readonly: bool,
    pub phase: &'static str,
    pub stage: String,
    pub files: usize,
    pub bytes: u64,
    pub total_bytes: u64,
    pub changes: Vec<Change>,
    pub conflicts: Vec<String>,
    pub message: Option<String>,
}
struct Data {
    record: Record,
    snapshot: Snapshot,
    plan: Option<Plan>,
}
struct Job {
    data: Mutex<Data>,
    operation: Arc<AsyncMutex<()>>,
    cancel: Arc<AtomicBool>,
    media: Mutex<Option<Arc<crate::media::Manager>>>,
}
impl Job {
    fn new(record: Record, phase: &'static str) -> Self {
        Self {
            data: Mutex::new(Data {
                snapshot: Snapshot {
                    id: record.id,
                    server_id: record.server_id,
                    slot: record.slot,
                    root: record.root.to_string_lossy().into_owned(),
                    image: record.image.to_string_lossy().into_owned(),
                    readonly: record.readonly,
                    phase,
                    stage: String::new(),
                    files: 0,
                    bytes: 0,
                    total_bytes: 0,
                    changes: Vec::new(),
                    conflicts: Vec::new(),
                    message: None,
                },
                record,
                plan: None,
            }),
            operation: Arc::new(AsyncMutex::new(())),
            cancel: Arc::new(AtomicBool::new(false)),
            media: Mutex::new(None),
        }
    }
    fn update(&self, app: &AppHandle, phase: &'static str, message: Option<String>) {
        let mut data = self.data.lock().unwrap();
        data.snapshot.phase = phase;
        data.snapshot.message = message;
        crate::diagnostics::changed(
            app,
            amikvm_core::diagnostics::Category::Folder,
            Some(data.snapshot.server_id),
            &format!("folder-{}", data.snapshot.id),
            "文件夹任务状态已改变",
            serde_json::json!({"id":data.snapshot.id,"phase":phase,"message":data.snapshot.message}),
            if phase == "error" {
                amikvm_core::diagnostics::Level::Error
            } else {
                amikvm_core::diagnostics::Level::Info
            },
        );
        drop(data);
        let _ = app.emit("ui-changed", ());
    }
    fn disconnected(&self, app: &AppHandle, message: Option<String>) {
        let mut data = self.data.lock().unwrap();
        if data.snapshot.phase == "connected" {
            data.snapshot.phase = "pending";
            data.snapshot.message = message;
            drop(data);
            let _ = app.emit("ui-changed", ());
        }
    }
    fn progress(self: &Arc<Self>, app: &AppHandle) -> impl Fn(Progress) + Send + 'static {
        let job = self.clone();
        let app = app.clone();
        let last = Mutex::new(Instant::now());
        move |progress| {
            let mut data = job.data.lock().unwrap();
            data.snapshot.stage = progress.stage.into();
            data.snapshot.files = progress.files;
            data.snapshot.bytes = progress.bytes;
            data.snapshot.total_bytes = progress.total_bytes;
            drop(data);
            let mut last = last.lock().unwrap();
            if last.elapsed().as_millis() >= 100 {
                *last = Instant::now();
                let _ = app.emit("ui-changed", ());
            }
        }
    }
}

pub struct Options {
    pub server_id: Uuid,
    pub slot: u8,
    pub root: PathBuf,
    pub image: PathBuf,
    pub size_mib: u32,
    pub readonly: bool,
}
pub struct Manager {
    directory: PathBuf,
    jobs: Mutex<HashMap<Uuid, Arc<Job>>>,
    closing: AtomicBool,
}
impl Manager {
    pub fn open(directory: PathBuf) -> Result<Self> {
        fs::create_dir_all(&directory)?;
        let mut jobs = HashMap::new();
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            let record: Record = serde_json::from_slice(&fs::read(entry.path())?)?;
            let phase = if record.mapping.is_some() {
                "pending"
            } else {
                "error"
            };
            let id = record.id;
            jobs.insert(id, Arc::new(Job::new(record, phase)));
        }
        Ok(Self {
            directory,
            jobs: Mutex::new(jobs),
            closing: AtomicBool::new(false),
        })
    }
    pub fn snapshots(&self) -> Vec<Snapshot> {
        let mut values: Vec<_> = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .map(|j| j.data.lock().unwrap().snapshot.clone())
            .collect();
        values.sort_by_key(|v| v.id);
        values
    }
    fn save(&self, record: &Record) -> Result<()> {
        let target = self.directory.join(format!("{}.json", record.id));
        let temp = self.directory.join(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec(record)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    fn job(&self, id: Uuid) -> Result<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Invalid("文件夹映射不存在".into()))
    }
    pub fn contains_server(&self, server_id: Uuid) -> bool {
        self.snapshots().iter().any(|s| s.server_id == server_id)
    }
    pub async fn create(
        self: &Arc<Self>,
        app: AppHandle,
        media: Arc<crate::media::Manager>,
        mut options: Options,
    ) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::Invalid("应用正在关闭".into()));
        }
        let root = options.root.clone();
        let image = options.image.clone();
        let (root, image) = tokio::task::spawn_blocking(move || -> Result<_> {
            let parent = image
                .parent()
                .ok_or_else(|| Error::Invalid("镜像路径无效".into()))?
                .canonicalize()
                .map_err(|e| {
                    Error::Invalid(format!(
                        "工作镜像的目录无法打开（{}）：{e}",
                        image.display()
                    ))
                })?;
            Ok((
                root.canonicalize().map_err(|e| {
                    Error::Invalid(format!("所选文件夹无法打开（{}）：{e}", root.display()))
                })?,
                parent.join(
                    image
                        .file_name()
                        .ok_or_else(|| Error::Invalid("镜像名称无效".into()))?,
                ),
            ))
        })
        .await
        .map_err(|e| Error::Protocol(e.to_string()))??;
        options.root = root;
        options.image = image;
        let record = Record {
            id: Uuid::new_v4(),
            server_id: options.server_id,
            slot: options.slot,
            root: options.root,
            image: options.image,
            size_mib: options.size_mib,
            readonly: options.readonly,
            mapping: None,
        };
        let job = Arc::new(Job::new(record.clone(), "creating"));
        let operation = job.operation.clone().lock_owned().await;
        {
            let mut jobs = self.jobs.lock().unwrap();
            for existing in jobs.values() {
                let data = existing.data.lock().unwrap();
                let old = &data.record;
                if record.root.starts_with(&old.root)
                    || old.root.starts_with(&record.root)
                    || record.image == old.image
                    || record.image.starts_with(&old.root)
                    || old.image.starts_with(&record.root)
                    || (old.server_id == record.server_id
                        && old.slot == record.slot
                        && matches!(data.snapshot.phase, "creating" | "connected"))
                {
                    return Err(Error::Invalid(
                        "文件夹、工作镜像或介质实例已被另一个映射占用".into(),
                    ));
                }
            }
            self.save(&record)?;
            jobs.insert(record.id, job.clone());
        }
        *job.media.lock().unwrap() = Some(media.clone());
        job.update(&app, "creating", None);
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let record = job.data.lock().unwrap().record.clone();
            let worker_job = job.clone();
            let worker_manager = manager.clone();
            let progress = job.progress(&app);
            let created = tokio::task::spawn_blocking(move || -> Result<Mapping> {
                let mapping = Mapping::create(
                    record.root.clone(),
                    record.image.clone(),
                    record.size_mib,
                    record.readonly,
                    &worker_job.cancel,
                    progress,
                )?;
                let mut record = record;
                record.mapping = Some(mapping.clone());
                worker_manager.save(&record)?;
                Ok(mapping)
            })
            .await
            .map_err(|e| Error::Protocol(e.to_string()))
            .and_then(|r| r);
            let mapping = match created {
                Ok(mapping) => mapping,
                Err(error) => {
                    job.update(&app, "error", Some(error.to_string()));
                    return;
                }
            };
            let slot = {
                let mut data = job.data.lock().unwrap();
                data.record.mapping = Some(mapping.clone());
                data.record.slot
            };
            if job.cancel.load(Ordering::Acquire) || manager.closing.load(Ordering::Acquire) {
                job.update(&app, "pending", Some("工作镜像已保留，尚未连接".into()));
                return;
            }
            if let Err(error) = media
                .start(
                    Kind::HardDisk,
                    slot,
                    mapping.image,
                    mapping.readonly,
                    true,
                    false,
                )
                .await
            {
                job.update(&app, "pending", Some(error.to_string()));
                return;
            }
            let Some(mut status) = media.watch(Kind::HardDisk, slot).await else {
                job.update(&app, "pending", Some("介质连接状态不可用".into()));
                return;
            };
            if job.cancel.load(Ordering::Acquire) || manager.closing.load(Ordering::Acquire) {
                let image = job.data.lock().unwrap().record.image.clone();
                media.stop_source(Kind::HardDisk, slot, &image).await;
                job.update(&app, "pending", Some("操作已取消，工作镜像已保留".into()));
                return;
            }
            job.update(&app, "connected", None);
            drop(operation);
            loop {
                let value = status.borrow_and_update().clone();
                if !value.active() {
                    job.disconnected(&app, value.message);
                    break;
                }
                if status.changed().await.is_err() {
                    job.disconnected(&app, None);
                    break;
                }
            }
        });
        Ok(())
    }
    async fn stop_media(job: &Arc<Job>) {
        let media = job.media.lock().unwrap().clone();
        let (slot, image) = {
            let data = job.data.lock().unwrap();
            (data.record.slot, data.record.image.clone())
        };
        if let Some(media) = media {
            media.stop_source(Kind::HardDisk, slot, &image).await;
        }
    }
    pub async fn prepare(self: &Arc<Self>, app: &AppHandle, id: Uuid) -> Result<()> {
        let job = self.job(id)?;
        let _operation = job
            .operation
            .try_lock()
            .map_err(|_| Error::Invalid("文件夹操作正在进行".into()))?;
        job.cancel.store(false, Ordering::Release);
        Self::stop_media(&job).await;
        let mapping = {
            let mut data = job.data.lock().unwrap();
            data.plan.take();
            data.record.mapping.clone().ok_or_else(|| {
                Error::Invalid("工作镜像没有完成生成，请移除此映射后重新连接".into())
            })?
        };
        job.update(app, "reading", None);
        let progress = job.progress(app);
        let cancel = job.cancel.clone();
        let result = tokio::task::spawn_blocking(move || mapping.prepare(&cancel, progress))
            .await
            .map_err(|e| Error::Protocol(e.to_string()))
            .and_then(|r| r);
        match result {
            Ok(plan) => {
                let mut data = job.data.lock().unwrap();
                data.snapshot.changes = plan.changes.clone();
                data.snapshot.conflicts = plan.conflicts.clone();
                data.plan = Some(plan);
                drop(data);
                job.update(app, "preview", None);
                Ok(())
            }
            Err(error) => {
                job.update(app, "pending", Some(error.to_string()));
                Err(error)
            }
        }
    }
    pub async fn apply(self: &Arc<Self>, app: &AppHandle, id: Uuid, overwrite: bool) -> Result<()> {
        let job = self.job(id)?;
        let _operation = job
            .operation
            .try_lock()
            .map_err(|_| Error::Invalid("文件夹操作正在进行".into()))?;
        job.cancel.store(false, Ordering::Release);
        let (mapping, plan) = {
            let mut data = job.data.lock().unwrap();
            (
                data.record
                    .mapping
                    .clone()
                    .ok_or_else(|| Error::Invalid("映射不可用".into()))?,
                data.plan
                    .take()
                    .ok_or_else(|| Error::Invalid("请先预览文件夹修改".into()))?,
            )
        };
        job.update(app, "applying", None);
        let progress = job.progress(app);
        let cancel = job.cancel.clone();
        let worker_mapping = mapping.clone();
        let result = tokio::task::spawn_blocking(move || {
            worker_mapping.apply(plan, overwrite, &cancel, progress)
        })
        .await
        .map_err(|e| Error::Protocol(e.to_string()))
        .and_then(|r| r);
        match result {
            Ok(()) => self.remove(app, &job, mapping).await,
            Err(error) => {
                job.update(app, "pending", Some(error.to_string()));
                Err(error)
            }
        }
    }
    pub fn cancel(&self, id: Uuid) -> Result<()> {
        self.job(id)?.cancel.store(true, Ordering::Release);
        Ok(())
    }
    pub async fn discard(self: &Arc<Self>, app: &AppHandle, id: Uuid) -> Result<()> {
        let job = self.job(id)?;
        let _operation = job
            .operation
            .try_lock()
            .map_err(|_| Error::Invalid("请先取消正在进行的文件夹操作".into()))?;
        Self::stop_media(&job).await;
        let mapping = {
            let mut data = job.data.lock().unwrap();
            data.plan.take();
            data.record.mapping.clone()
        };
        if let Some(mapping) = mapping {
            self.remove(app, &job, mapping).await
        } else {
            self.remove_record(app, id)
        }
    }
    async fn remove(&self, app: &AppHandle, job: &Arc<Job>, mapping: Mapping) -> Result<()> {
        let result = tokio::task::spawn_blocking(move || mapping.discard())
            .await
            .map_err(|e| Error::Protocol(e.to_string()))
            .and_then(|r| r);
        if let Err(error) = result {
            job.update(app, "pending", Some(error.to_string()));
            return Err(error);
        }
        let id = job.data.lock().unwrap().record.id;
        self.remove_record(app, id)
    }
    fn remove_record(&self, app: &AppHandle, id: Uuid) -> Result<()> {
        fs::remove_file(self.directory.join(format!("{id}.json")))?;
        self.jobs.lock().unwrap().remove(&id);
        let _ = app.emit("ui-changed", ());
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        let jobs: Vec<_> = self.jobs.lock().unwrap().values().cloned().collect();
        for job in &jobs {
            job.cancel.store(true, Ordering::Release);
        }
        for job in jobs {
            let _operation = job.operation.lock().await;
        }
    }
}
