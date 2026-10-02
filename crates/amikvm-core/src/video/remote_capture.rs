//! WebPreview/BSOD .cap retrieval and native AST decoding.
//! Sources: WebPreviewer.bS/bT/bV, JVAPP.hZ and KVMClient case 27.
use crate::{Error, Result, auth::WebSession, protocol, transport};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{io::AsyncWriteExt, sync::watch};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Preview,
    Crash,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preview => "BMC 预览画面",
            Self::Crash => "蓝屏捕获画面",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Preview => "/capture/webPreview.cap",
            Self::Crash => "/bsod/crashscreen.cap",
        }
    }
}

pub struct Frame {
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn decode(bytes: &[u8]) -> Result<Frame> {
    let mut decoder = super::Decoder::default();
    if !decoder.decode(bytes)? {
        return Err(Error::Protocol("BMC capture contains no image data".into()));
    }
    Ok(Frame {
        source_width: decoder.source_width,
        source_height: decoder.source_height,
        width: decoder.width,
        height: decoder.height,
        rgba: decoder.rgba,
    })
}

async fn canceled(cancel: &mut watch::Receiver<bool>) {
    if !*cancel.borrow() {
        let _ = cancel.changed().await;
    }
}

impl WebSession {
    async fn open_preview(&self) -> Result<(transport::Connection, bool)> {
        if !self.token.is_empty() {
            let connection = self
                .open_channel(self.config.kvm_port, self.config.kvm_secure, "VIDEO")
                .await?;
            return Ok((connection, self.config.oem_features & 32 != 0));
        }
        let rest = self.config.api_mode == crate::server::ApiMode::Rest;
        let (adviser, media) = tokio::try_join!(
            self.get_fields(if rest {
                "/api/settings/media/adviser"
            } else {
                "/rpc/getadvisercfg.asp"
            }),
            self.get_fields(if rest {
                "/api/settings/media/instance"
            } else {
                "/rpc/getvmediacfg.asp"
            }),
        )?;
        let key = |r, l| if rest { r } else { l };
        let single_port =
            media.number(key("single_port_enabled", "V_SINGLE_PORT_ENABLED")) == Some(1);
        let port_key = if single_port {
            key("web_port", "V_STR_WEB_PORT")
        } else {
            key("kvm_port", "V_STR_KVM_PORT")
        };
        let port = adviser
            .number(port_key)
            .and_then(|n| u16::try_from(n).ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| Error::Protocol(format!("Invalid or missing {port_key}")))?;
        let secure = if single_port {
            self.server.https
        } else {
            adviser.number(key("secure_channel", "V_STR_SECURE_CHANNEL")) == Some(1)
        };
        let oem = if rest {
            media.number("oemFeature")
        } else {
            adviser.number("V_STR_OEM_FEATURE_STATUS")
        }
        .unwrap_or(0);
        let connection = self
            .open_channel_with_mode(port, secure, "VIDEO", single_port)
            .await?;
        Ok((connection, oem & 32 != 0))
    }

    pub async fn capture_image(
        &self,
        kind: Kind,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Frame> {
        if kind == Kind::Preview {
            self.generate_preview(&mut cancel).await?;
        }
        let bytes = tokio::select! {
            biased;
            _ = canceled(&mut cancel) => return Err(Error::Invalid("画面抓取已取消".into())),
            result = self.capture_bytes(kind) => result?,
        };
        // AST Huffman/IDCT work stays off the asynchronous network executor.
        let frame = tokio::task::spawn_blocking(move || decode(&bytes))
            .await
            .map_err(|e| Error::Protocol(format!("BMC capture decoder: {e}")))??;
        if *cancel.borrow() {
            return Err(Error::Invalid("画面抓取已取消".into()));
        }
        Ok(frame)
    }

    async fn capture_bytes(&self, kind: Kind) -> Result<Vec<u8>> {
        let mut response = self.request(Method::GET, kind.path()).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(if matches!(status.as_u16(), 401 | 403) {
                Error::Authentication(format!("BMC capture denied (HTTP {status})"))
            } else {
                Error::Protocol(format!("BMC capture unavailable (HTTP {status})"))
            });
        }
        let limit = protocol::MAX_PACKET + 86;
        let length = response.content_length();
        if length.is_some_and(|n| n > limit as u64 || n <= 86) {
            return Err(Error::Protocol("Invalid BMC capture length".into()));
        }
        let mut bytes = Vec::with_capacity(length.unwrap_or(4096).min(limit as u64) as usize);
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(Error::Protocol("BMC capture exceeds frame limit".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        if length.is_some_and(|n| n != bytes.len() as u64) {
            return Err(Error::Protocol("Truncated BMC capture response".into()));
        }
        Ok(bytes)
    }

    async fn generate_preview(&self, cancel: &mut watch::Receiver<bool>) -> Result<()> {
        let (mut connection, oem) = tokio::select! {
            biased;
            _ = canceled(cancel) => return Err(Error::Invalid("画面抓取已取消".into())),
            result = self.open_preview() => result?,
        };
        let result = tokio::select! {
            biased;
            _ = canceled(cancel) => Err(Error::Invalid("画面抓取已取消".into())),
            result = tokio::time::timeout(Duration::from_secs(10), async {
                let mut requested = false;
                for _ in 0..4096 {
                    let packet = transport::read_packet(&mut connection.stream).await?;
                    match packet.header.kind {
                        23 if !requested => {
                            if ![0, 2].contains(&packet.header.status) {
                                return Err(Error::Protocol("BMC uses a different video SoC".into()));
                            }
                            // Preview is its own IVTP operation, not a normal kind-18
                            // authenticated console. This is the original hZ flow.
                            if oem {
                                transport::write_packet(&mut connection.stream, &protocol::command(58, 0)).await?;
                            }
                            transport::write_packet(&mut connection.stream, &protocol::command(26, 0)).await?;
                            requested = true;
                        }
                        27 if requested => {
                            return match packet.body.first().copied().map(|b| b as i8) {
                                Some(0) => Ok(()),
                                Some(-1) => Err(Error::Protocol("BMC 无法捕获预览画面".into())),
                                Some(-5) => Err(Error::Protocol("主机已关机或休眠，无法捕获预览画面".into())),
                                Some(status) => Err(Error::Protocol(format!("BMC preview capture failed ({status})"))),
                                None => Err(Error::Protocol("Missing BMC preview status".into())),
                            };
                        }
                        8 | 22 => return Err(Error::Authentication("BMC rejected preview capture".into())),
                        _ => {}
                    }
                }
                Err(Error::Protocol("BMC preview sent too many control packets".into()))
            }) => match result {
                Ok(result) => result,
                Err(_) => Err(Error::Timeout("BMC preview capture")),
            },
        };
        // Complete this close even after user cancellation; HTTP retrieval begins
        // only after the transient preview connection has been released.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            connection
                .stream
                .write_all(&protocol::command(8, 0))
                .await?;
            connection.stream.shutdown().await
        })
        .await;
        result
    }
}
