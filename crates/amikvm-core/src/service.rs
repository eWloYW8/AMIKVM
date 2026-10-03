//! Live IVTP service configuration (37), media configuration (38/56), and end reasons.
//! Layouts checked against ConfPkt/KVMClient in the original JAR.
use crate::{Error, Result, auth::SessionConfig};
use serde::Serialize;

const RECORD_BYTES: usize = 57;
const RECORD_COUNT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub name: String,
    pub enabled: bool,
    pub interface: String,
    pub port: i32,
    pub secure_port: i32,
    pub inactivity_seconds: i32,
    pub max_sessions: u8,
    pub reserved: u8,
    pub maximum_inactivity_seconds: i32,
    pub minimum_inactivity_seconds: i32,
}
fn invalid(message: &'static str) -> Error {
    Error::Protocol(message.into())
}
fn field(bytes: &[u8]) -> Result<String> {
    let bytes = &bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())];
    let text = std::str::from_utf8(bytes)
        .map_err(|_| invalid("Invalid service configuration text"))?
        .trim();
    if text.chars().any(char::is_control) {
        return Err(invalid("Invalid service configuration text"));
    }
    Ok(text.to_owned())
}
fn integer(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn port(value: i32) -> Option<u16> {
    u16::try_from(value).ok().filter(|v| *v > 0)
}
fn count(value: i32) -> Result<u8> {
    u8::try_from(value).map_err(|_| invalid("Invalid media instance count"))
}

pub fn services(bytes: &[u8]) -> Result<Vec<Service>> {
    if bytes.len() < RECORD_BYTES * RECORD_COUNT {
        return Err(invalid("Invalid service configuration packet length"));
    }
    bytes[..RECORD_BYTES * RECORD_COUNT]
        .chunks_exact(RECORD_BYTES)
        .map(|b| {
            let name = field(&b[..17])?;
            Ok(Service {
                name,
                enabled: b[17] != 0,
                interface: field(&b[18..35])?,
                port: integer(b, 35),
                secure_port: integer(b, 39),
                inactivity_seconds: integer(b, 43),
                max_sessions: b[47],
                reserved: b[48],
                maximum_inactivity_seconds: integer(b, 49),
                minimum_inactivity_seconds: integer(b, 53),
            })
        })
        .collect()
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub slot: usize,
    pub service: String,
    pub fields: Vec<&'static str>,
}
fn changes(before: &[Service], after: &[Service]) -> Vec<Change> {
    // The original five-slot table uses record position, so duplicate names and
    // empty/reserved slots must not be mistaken for a malformed packet.
    let mut result = vec![];
    for (slot, (old, new)) in before.iter().zip(after).enumerate() {
        let mut fields = vec![];
        for (changed, name) in [
            (old.name != new.name, "服务名称"),
            (old.enabled != new.enabled, "服务状态"),
            (old.interface != new.interface, "网络接口"),
            (old.port != new.port, "非加密端口"),
            (old.secure_port != new.secure_port, "加密端口"),
            (old.inactivity_seconds != new.inactivity_seconds, "空闲超时"),
            (old.max_sessions != new.max_sessions, "最大会话数"),
            (
                old.maximum_inactivity_seconds != new.maximum_inactivity_seconds,
                "最大空闲超时",
            ),
            (
                old.minimum_inactivity_seconds != new.minimum_inactivity_seconds,
                "最小空闲超时",
            ),
        ] {
            if changed {
                fields.push(name);
            }
        }
        if !fields.is_empty() {
            result.push(Change {
                slot,
                service: new.name.clone(),
                fields,
            });
        }
    }
    result
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaConfiguration {
    pub retry_count: Option<u32>,
    pub retry_interval: Option<u32>,
    pub mouse_mode: u8,
    pub host_display_control: bool,
    pub cd_instances: u8,
    pub hd_instances: u8,
    pub power_save_mode: u8,
    pub kvm_cd_instances: u8,
    pub kvm_hd_instances: u8,
}
impl MediaConfiguration {
    pub fn parse(bytes: &[u8], retry_fields: bool) -> Result<Self> {
        // KVMClient.nY chooses the prefix from OEM bit 32, not packet size.
        // Additional OEM fields may follow the 46-byte common configuration.
        let prefix = if retry_fields { 8 } else { 0 };
        if bytes.len() < prefix + 46 {
            return Err(invalid("Invalid media configuration packet length"));
        }
        let retry_count = if prefix == 8 {
            Some(
                u32::try_from(integer(bytes, 0))
                    .map_err(|_| invalid("Invalid reconnect count"))?
                    .min(100),
            )
        } else {
            None
        };
        let retry_interval = if prefix == 8 {
            Some(
                u32::try_from(integer(bytes, 4))
                    .map_err(|_| invalid("Invalid reconnect interval"))?
                    .min(3600),
            )
        } else {
            None
        };
        let bytes = &bytes[prefix..];
        Ok(Self {
            retry_count,
            retry_interval,
            mouse_mode: bytes[0],
            host_display_control: bytes[1] & 1 != 0,
            cd_instances: count(integer(bytes, 14))?,
            hd_instances: count(integer(bytes, 18))?,
            power_save_mode: count(integer(bytes, 34))?,
            kvm_cd_instances: count(integer(bytes, 38))?,
            kvm_hd_instances: count(integer(bytes, 42))?,
        })
    }
    pub fn apply(&self, config: &mut SessionConfig) -> bool {
        let before = (config.cd_instances, config.hd_instances);
        if let Some(count) = self.retry_count {
            config.retry_count = count;
        }
        if let Some(seconds) = self.retry_interval {
            config.retry_interval = seconds;
        }
        config.kvm_cd_instances = self.kvm_cd_instances;
        config.kvm_hd_instances = self.kvm_hd_instances;
        (config.cd_instances, config.hd_instances) = if config.oem_features & 128 != 0 {
            (self.kvm_cd_instances, self.kvm_hd_instances)
        } else {
            (self.cd_instances, self.hd_instances)
        };
        config.power_save_mode = self.power_save_mode;
        before != (config.cd_instances, config.hd_instances)
    }
}
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub services: Vec<Service>,
    pub media: Option<MediaConfiguration>,
    pub changes: Vec<Change>,
    pub revision: u64,
    pub notice: Option<&'static str>,
}
#[derive(Default)]
pub struct Effect {
    pub close: bool,
    pub media_changed: bool,
}
impl State {
    pub fn receive_services(&mut self, bytes: &[u8], config: &mut SessionConfig) -> Result<Effect> {
        let next = services(bytes)?;
        let changed = if self.services.is_empty() {
            vec![]
        } else {
            changes(&self.services, &next)
        };
        let close = changed.iter().any(|c| {
            c.service.eq_ignore_ascii_case("kvm")
                || c.service.eq_ignore_ascii_case("web")
                || self.services[c.slot].name.eq_ignore_ascii_case("kvm")
                || self.services[c.slot].name.eq_ignore_ascii_case("web")
        });
        let media_changed = changed.iter().any(|c| {
            c.service.eq_ignore_ascii_case("cd-media") || c.service.eq_ignore_ascii_case("hd-media")
        });
        if media_changed {
            for change in &changed {
                let service = &next[change.slot];
                let secure = if config.media_secure {
                    service.secure_port
                } else {
                    service.port
                };
                if service.name.eq_ignore_ascii_case("cd-media") {
                    config.cd_enabled = service.enabled;
                    if !config.single_port {
                        config.cd_port = port(secure);
                    }
                }
                if service.name.eq_ignore_ascii_case("hd-media") {
                    config.hd_enabled = service.enabled;
                    if !config.single_port {
                        config.hd_port = port(secure);
                    }
                }
            }
        }
        if next != self.services {
            self.revision = self.revision.saturating_add(1);
        }
        if !changed.is_empty() {
            self.changes = changed;
            self.notice = Some(if close {
                "KVM 或 Web 服务配置已改变，请重新连接服务器。"
            } else if media_changed {
                "虚拟介质服务配置已改变，活动重定向已停止。"
            } else {
                "服务器服务配置已改变。"
            });
        }
        self.services = next;
        Ok(Effect {
            close,
            media_changed,
        })
    }
    pub fn receive_media(&mut self, bytes: &[u8], config: &mut SessionConfig) -> Result<bool> {
        let next = MediaConfiguration::parse(bytes, config.oem_features & 32 != 0)?;
        let stop = next.apply(config);
        if self.media.as_ref() != Some(&next) {
            self.revision = self.revision.saturating_add(1);
        }
        if stop {
            self.notice = Some("虚拟介质实例配置已改变，活动重定向已停止。");
        }
        self.media = Some(next);
        Ok(stop)
    }
    pub fn receive_instances(&mut self, bytes: &[u8], config: &mut SessionConfig) -> Result<bool> {
        if bytes.len() < 8 {
            return Err(invalid("Invalid media instance packet length"));
        }
        let (cd, hd) = (count(integer(bytes, 0))?, count(integer(bytes, 4))?);
        let changed = (cd, hd) != (config.cd_instances, config.hd_instances);
        if changed {
            config.cd_instances = cd;
            config.hd_instances = hd;
            if config.oem_features & 128 != 0 {
                config.kvm_cd_instances = cd;
                config.kvm_hd_instances = hd;
            }
            self.revision = self.revision.saturating_add(1);
            self.notice = Some("虚拟介质实例配置已改变，活动重定向已停止。");
        }
        Ok(changed)
    }
}
pub fn end_reason(status: u16) -> Option<&'static str> {
    Some(match status {
        2 => "BMC 重定向服务要求关闭会话。",
        5 => "重定向服务配置已改变，请重新连接服务器。",
        7 => "BMC Web 会话已注销，远程控制台已断开。",
        8 => "KVM 许可证已过期，会话已关闭。",
        9 => "KVM 会话空闲超时，连接已关闭。",
        10 => "BMC 已终止远程控制台会话。",
        11 => "BMC Web 服务已重启，连接已关闭。",
        12 => "BMC 正在执行热复位，连接已关闭。",
        13 => "BMC 正在恢复出厂设置，连接已关闭。",
        14 => "已有 VNC 会话占用控制台，请关闭该会话后重新连接。",
        _ => return None,
    })
}
