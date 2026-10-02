//! Independent IUSB media connections; file access never runs on the UI thread.
use super::{
    Packet, read,
    scsi::{Image, Kind},
};
use crate::{Error, Result, auth::WebSession};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, sync::watch, time::timeout};
use uuid::Uuid;

#[derive(Clone)]
pub enum Source {
    Image(PathBuf),
    Device(super::device::Device),
}
impl Source {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::Image(path) => path,
            Self::Device(device) => &device.path,
        }
    }
    pub fn groups(&self) -> Vec<String> {
        match self {
            Self::Image(_) => vec![],
            Self::Device(device) => device.groups.clone(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub id: Uuid,
    pub kind: Kind,
    pub slot: u8,
    pub instance: u8,
    pub source: String,
    pub readonly: bool,
    pub boost: bool,
    pub phase: String,
    pub message: Option<String>,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub requests: u64,
    pub capacity: u64,
    pub cache: Option<super::cache::Stats>,
    pub physical: bool,
    pub device_groups: Vec<String>,
}
impl Status {
    pub fn pending(
        kind: Kind,
        slot: u8,
        path: &std::path::Path,
        readonly: bool,
        boost: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            kind,
            slot,
            instance: slot,
            source: path.to_string_lossy().into_owned(),
            readonly: readonly || kind == Kind::Cdrom,
            boost,
            phase: "connecting".into(),
            message: None,
            bytes_read: 0,
            bytes_written: 0,
            requests: 0,
            capacity: 0,
            cache: None,
            physical: false,
            device_groups: vec![],
        }
    }
    pub fn active(&self) -> bool {
        matches!(self.phase.as_str(), "connecting" | "connected")
    }
}

pub struct Redirector {
    pub status: watch::Receiver<Status>,
    cancel: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
}
impl Redirector {
    pub async fn start(
        web: Arc<WebSession>,
        path: PathBuf,
        status: Status,
        usb: bool,
    ) -> Result<Self> {
        Self::start_source(web, Source::Image(path), status, usb).await
    }
    pub async fn start_source(
        web: Arc<WebSession>,
        source: Source,
        mut status: Status,
        usb: bool,
    ) -> Result<Self> {
        if web.config.privileges & 2 == 0 {
            return Err(Error::Authentication(
                "This account has no virtual media privilege".into(),
            ));
        }
        let cd = status.kind == Kind::Cdrom;
        let enabled = if cd {
            web.config.cd_enabled
        } else {
            web.config.hd_enabled
        };
        let count = if cd {
            web.config.cd_instances
        } else {
            web.config.hd_instances
        };
        if !enabled || status.slot >= count {
            return Err(Error::Invalid(
                "Virtual media instance is unavailable".into(),
            ));
        }
        let kind = status.kind;
        let readonly = status.readonly;
        status.physical = matches!(source, Source::Device(_));
        status.device_groups = source.groups();
        let image = tokio::task::spawn_blocking(move || match source {
            Source::Image(path) => Image::open(&path, kind, readonly),
            Source::Device(device) => Image::open_device(&device, kind, readonly),
        })
        .await
        .map_err(|e| Error::Protocol(e.to_string()))??;
        status.readonly = image.readonly;
        status.capacity = image.blocks * image.block_size as u64;
        let image = Arc::new(Mutex::new(image));
        let port = if web.config.single_port {
            Some(web.config.kvm_port)
        } else if cd {
            web.config.cd_port
        } else {
            web.config.hd_port
        }
        .ok_or_else(|| Error::Protocol("BMC did not advertise a media port".into()))?;
        let mut connection = web
            .open_channel(
                port,
                web.config.media_secure,
                if cd { "CDMEDIA" } else { "HDMEDIA" },
            )
            .await?;
        let auth = Packet::authenticate(&web.token, status.slot, !cd, usb, status.boost && cd)?
            .encode()?;
        let name = std::path::Path::new(&status.source)
            .file_name()
            .map(|name| name.to_string_lossy())
            // A Windows device namespace/GUID path can consist entirely of a
            // prefix component, so file_name() alone would report no name.
            .unwrap_or_else(|| {
                std::borrow::Cow::Borrowed(
                    status
                        .source
                        .rsplit(['\\', '/'])
                        .find(|v| !v.is_empty())
                        .unwrap_or_default(),
                )
            });
        let info = Packet::device_info(&name, status.slot)?.encode()?;
        timeout(Duration::from_secs(10), async {
            connection.stream.write_all(&auth).await?;
            connection.stream.write_all(&info).await?;
            connection.stream.flush().await
        })
        .await
        .map_err(|_| Error::Timeout("Media authentication send"))??;
        let ack = timeout(Duration::from_secs(15), read(&mut connection.stream))
            .await
            .map_err(|_| Error::Timeout("Media authentication"))??;
        if ack.opcode()? != 241 {
            return Err(Error::Protocol(
                "Expected media redirection acknowledgement".into(),
            ));
        }
        let code = ack
            .body
            .get(30)
            .copied()
            .ok_or_else(|| Error::Protocol("Missing media authentication status".into()))?;
        if !(code == 1 || cd && [27, 28].contains(&code)) {
            let other = ack
                .body
                .get(31..)
                .map(|v| {
                    String::from_utf8_lossy(v)
                        .trim_matches(['\0', ' '])
                        .to_owned()
                })
                .unwrap_or_default();
            return Err(Error::Authentication(match code {
                3 => "BMC rejected the media session token".into(),
                5 => "Virtual media permission denied".into(),
                8 => "BMC media session limit reached".into(),
                13 => "BMC virtual media license expired".into(),
                _ if !other.is_empty() => format!("Media instance is in use by {other}"),
                _ => format!("BMC rejected virtual media ({code})"),
            }));
        }
        status.instance = ack.instance();
        status.boost = cd && code == 27;
        status.phase = "connected".into();
        let physical = image.lock().unwrap().physical();
        let (device_tx, mut device_rx) = watch::channel(Some(status.capacity));
        let (status_tx, status_rx) = watch::channel(status);
        let (cancel, mut cancel_rx) = watch::channel(false);
        let monitor = if physical {
            let image = image.clone();
            let mut cancel = cancel.subscribe();
            Some(tokio::spawn(async move {
                loop {
                    tokio::select! {biased; _=cancel.changed()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
                    let image = image.clone();
                    let check = tokio::task::spawn_blocking(move || {
                        image.try_lock().ok().map(|mut image| image.poll_device())
                    })
                    .await;
                    if let Ok(Some(value)) = check {
                        device_tx.send_if_modified(|old| {
                            if *old == value {
                                false
                            } else {
                                *old = value;
                                true
                            }
                        });
                        if value.is_none() {
                            break;
                        }
                    }
                }
            }))
        } else {
            None
        };
        let monitor_cancel = cancel.clone();
        let (finish_tx, finished) = watch::channel(false);
        let (mut reader, mut writer) = tokio::io::split(connection.stream);
        tokio::spawn(async move {
            let mut state = status_tx.borrow().clone();
            let mut last_notification = Instant::now();
            let mut stop_reason = "disconnected";
            let mut heartbeat = tokio::time::interval(Duration::from_millis(500));
            let mut last_received = Instant::now();
            let mut last_sent = Instant::now();
            let mut published_requests = 0;
            let result: Result<()> = async {
                loop {
                    let reading = timeout(Duration::from_secs(90), read(&mut reader));
                    tokio::pin!(reading);
                    let request = loop {
                        tokio::select! {
                            _ = cancel_rx.changed() => break None,
                            changed=device_rx.changed(),if physical => {
                                if *cancel_rx.borrow(){break None;}
                                let capacity=*device_rx.borrow_and_update();
                                if capacity.is_none(){stop_reason="removed";break None;}
                                if changed.is_err(){return Err(Error::Protocol("Physical device monitor stopped".into()));}
                                state.capacity=capacity.unwrap();status_tx.send_replace(state.clone());
                            }
                            request = &mut reading => break Some(request.map_err(|_| Error::Timeout("Virtual media response"))??),
                            _ = heartbeat.tick() => {
                                let mut cache_changed = false;
                                if let Ok(image) = image.try_lock() {
                                    let cache = image.cache_stats();
                                    cache_changed = state.cache != cache;
                                    state.cache = cache;
                                }
                                if state.requests != published_requests || cache_changed {
                                    status_tx.send_replace(state.clone());
                                    published_requests=state.requests;
                                    last_notification=Instant::now();
                                }
                                if last_sent.elapsed()>=Duration::from_secs(60) || last_received.elapsed()>=Duration::from_secs(60) {
                                    let bytes=Packet::command(243,state.instance,&[]).encode()?;
                                    timeout(Duration::from_secs(15),writer.write_all(&bytes)).await.map_err(|_|Error::Timeout("Media keepalive"))??;
                                    last_sent=Instant::now();
                                }
                            }
                        }
                    };
                    let Some(request) = request else { break; };
                    last_received=Instant::now();
                    let opcode = request.opcode()?;
                    if opcode == 246 || opcode == 247 {
                        stop_reason = if opcode == 246 { "terminated" } else { "service_restart" };
                        break;
                    }
                    let request_size = request.body.len();
                    let request_image = image.clone();
                    let (mut response, ejected, cache) = tokio::task::spawn_blocking(move || -> Result<_> {
                        let mut image = request_image.lock().map_err(|_| Error::Protocol("Media image unavailable".into()))?;
                        let response = image.respond(&request)?;
                        Ok((response, image.ejected(), image.cache_stats()))
                    }).await.map_err(|e| Error::Protocol(e.to_string()))??;
                    state.cache = cache;
                    if opcode == 244 {
                        response.body.resize(31,0);
                        response.body[30]=if usb {128} else {0};
                    }
                    let data_length = u32::from_le_bytes(response.body[25..29].try_into().unwrap()) as u64;
                    let successful = response.body[21] == 0;
                    timeout(Duration::from_secs(15), writer.write_all(&response.encode()?)).await
                        .map_err(|_| Error::Timeout("Media response send"))??;
                    last_sent=Instant::now();
                    state.requests += 1;
                    if successful && matches!(opcode, 0x08 | 0x28 | 0xa8 | 0xbe | 0xb9) { state.bytes_read += data_length; }
                    if successful && matches!(opcode, 0x0a | 0x2a | 0xaa) {
                        state.bytes_written += request_size.saturating_sub(29) as u64;
                    }
                    if ejected { stop_reason = "ejected"; break; }
                    if last_notification.elapsed() >= Duration::from_millis(500) {
                        status_tx.send_replace(state.clone()); published_requests=state.requests; last_notification = Instant::now();
                    }
                }
                Ok(())
            }.await;
            // Complete any native poll before releasing device handles/volume locks.
            if let Some(monitor) = monitor {
                let _ = monitor_cancel.send(true);
                let _ = monitor.await;
            }
            let disconnect = Packet::command(247, state.instance, &[]).encode();
            if let Ok(bytes) = disconnect {
                let _ = timeout(Duration::from_secs(2), writer.write_all(&bytes)).await;
            }
            let _ = timeout(Duration::from_secs(2), writer.shutdown()).await;
            let flush = tokio::task::spawn_blocking(move || {
                let mut image = image
                    .lock()
                    .map_err(|_| Error::Protocol("Media image unavailable".into()))?;
                let cache = image.stop_cache();
                image.flush()?;
                Ok::<_, Error>(cache)
            })
            .await;
            let cleanup = flush
                .map_err(|e| Error::Protocol(e.to_string()))
                .and_then(|v| v);
            if let Ok(cache) = &cleanup {
                state.cache = *cache;
            }
            let result = result.and_then(|_| cleanup.map(|_| ()));
            match result {
                Ok(()) => state.phase = stop_reason.into(),
                Err(error) => {
                    state.phase = "error".into();
                    state.message = Some(error.to_string());
                }
            }
            status_tx.send_replace(state);
            let _ = finish_tx.send(true);
        });
        Ok(Self {
            status: status_rx,
            cancel,
            finished,
        })
    }
    pub async fn stop(&self) {
        let _ = self.cancel.send(true);
        let mut finished = self.finished.clone();
        while !*finished.borrow_and_update() {
            if finished.changed().await.is_err() {
                break;
            }
        }
    }
}
impl Drop for Redirector {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
    }
}
