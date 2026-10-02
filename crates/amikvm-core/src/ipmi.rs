//! IPMI over IVTP and the OEM boot-flags dialog in the reference JAR.
//! Kind 49 puts the request identifier in the body, not in the status field.
use crate::{Error, Result, protocol::Control};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub const BOOT_REQUEST_ID: u8 = 126;
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const HISTORY_LIMIT: usize = 1000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub request_id: u8,
    pub completion_code: u16,
    pub data: Vec<u8>,
}
impl Response {
    pub fn parse(completion_code: u16, body: &[u8]) -> Result<Self> {
        let (&request_id, data) = body
            .split_first()
            .ok_or_else(|| Error::Protocol("IPMI 响应缺少请求编号".into()))?;
        if data.len() > 65_536 {
            return Err(Error::Protocol("IPMI 响应超过长度上限".into()));
        }
        Ok(Self {
            request_id,
            completion_code,
            data: data.to_vec(),
        })
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn ascii(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| {
            if (32..=126).contains(byte) {
                char::from(*byte)
            } else {
                '.'
            }
        })
        .collect()
}
pub fn parse_command(format: &str, value: &str) -> Result<Vec<u8>> {
    let bytes = match format {
        "hex" => return crate::input::parse_hex(value),
        "ascii" if value.is_ascii() => value.as_bytes().to_vec(),
        "ascii" => {
            return Err(Error::Invalid(
                "IPMI ASCII 输入只接受 ASCII 字符；其他字节请用十六进制输入".into(),
            ));
        }
        _ => return Err(Error::Invalid("IPMI 输入格式无效".into())),
    };
    validate_command(&bytes)?;
    Ok(bytes)
}
fn validate_command(command: &[u8]) -> Result<()> {
    if !(2..=1024).contains(&command.len()) {
        return Err(Error::Invalid("IPMI 命令须包含 2–1024 个字节".into()));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootDevice {
    NoChange,
    Pxe,
    CdDvd,
    HardDiskUsb,
    BiosSetup,
}
impl BootDevice {
    pub fn value(self) -> &'static str {
        match self {
            Self::NoChange => "no_change",
            Self::Pxe => "pxe",
            Self::CdDvd => "cd_dvd",
            Self::HardDiskUsb => "hard_disk_usb",
            Self::BiosSetup => "bios_setup",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::NoChange => "不更改启动设备",
            Self::Pxe => "PXE 网络启动",
            Self::CdDvd => "CD/DVD 光驱",
            Self::HardDiskUsb => "硬盘 / USB",
            Self::BiosSetup => "BIOS 设置",
        }
    }
    fn code(self) -> u8 {
        match self {
            Self::NoChange => 0x00,
            Self::Pxe => 0x04,
            Self::CdDvd => 0x14,
            Self::HardDiskUsb => 0x08,
            Self::BiosSetup => 0x18,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootOptions {
    pub device: Option<BootDevice>,
    pub device_code: u8,
    pub next_boot_only: bool,
    pub valid: bool,
    pub uefi: bool,
    // Preserve unrelated flags when changing only the controls in the dialog.
    #[serde(skip)]
    flags: [u8; 5],
}
impl BootOptions {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 7 || data[0] & 0x0f != 1 || data[1] != 5 {
            return Err(Error::Protocol(
                "启动选项响应缺少有效的版本、参数 5 或五字节启动标志".into(),
            ));
        }
        let flags: [u8; 5] = data[2..7].try_into().unwrap();
        let device_code = flags[1] & 0x3c;
        let device = match device_code {
            0x00 => Some(BootDevice::NoChange),
            0x04 => Some(BootDevice::Pxe),
            0x14 => Some(BootDevice::CdDvd),
            0x08 => Some(BootDevice::HardDiskUsb),
            0x18 => Some(BootDevice::BiosSetup),
            _ => None,
        };
        Ok(Self {
            device,
            device_code,
            next_boot_only: flags[0] & 0x40 == 0,
            valid: flags[0] & 0x80 != 0,
            uefi: flags[0] & 0x20 != 0,
            flags,
        })
    }
    pub fn with_selection(&self, device: BootDevice, next_boot_only: bool) -> Self {
        let mut options = self.clone();
        options.device = Some(device);
        options.device_code = device.code();
        options.next_boot_only = next_boot_only;
        options.valid = true;
        options.flags[0] = (options.flags[0] & !0xc0) | if next_boot_only { 0x80 } else { 0xc0 };
        options.flags[1] = (options.flags[1] & !0x3c) | device.code();
        options
    }
    pub fn write_command(&self) -> Vec<u8> {
        let mut command = vec![0x00, 0x08, 0x05];
        command.extend_from_slice(&self.flags);
        command
    }
    fn matches(&self, desired: &Self) -> bool {
        self.flags == desired.flags
    }
}
pub fn boot_read_command() -> Vec<u8> {
    vec![0x00, 0x09, 0x05, 0x00, 0x00]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Raw,
    BootRead,
    BootWrite,
    BootConfirm,
    Unmatched,
}
impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Raw => "原始命令",
            Self::BootRead => "读取启动选项",
            Self::BootWrite => "应用启动选项",
            Self::BootConfirm => "确认启动选项",
            Self::Unmatched => "未匹配响应",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Pending,
    Complete,
    Error,
    TimedOut,
    Disconnected,
    Unmatched,
    LateResponse,
}
impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "等待响应",
            Self::Complete => "已完成",
            Self::Error => "失败",
            Self::TimedOut => "响应超时",
            Self::Disconnected => "连接已关闭",
            Self::Unmatched => "未匹配",
            Self::LateResponse => "超时后收到响应",
        }
    }
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub sequence: u64,
    pub request_id: u8,
    pub operation: Operation,
    pub command: Vec<u8>,
    pub response: Option<Response>,
    pub phase: Phase,
    pub message: Option<String>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootPhase {
    #[default]
    Idle,
    Reading,
    Writing,
    Confirming,
    Ready,
    Error,
}
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootState {
    pub phase: BootPhase,
    pub options: Option<BootOptions>,
    pub revision: u64,
    pub message: Option<String>,
}
impl BootState {
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            BootPhase::Reading | BootPhase::Writing | BootPhase::Confirming
        )
    }
}
#[derive(Debug, Clone)]
enum Purpose {
    Raw,
    BootRead,
    BootWrite(BootOptions),
    BootConfirm(BootOptions),
}
impl Purpose {
    fn operation(&self) -> Operation {
        match self {
            Self::Raw => Operation::Raw,
            Self::BootRead => Operation::BootRead,
            Self::BootWrite(_) => Operation::BootWrite,
            Self::BootConfirm(_) => Operation::BootConfirm,
        }
    }
}
#[derive(Debug, Clone)]
struct Pending {
    sequence: u64,
    started: Instant,
    purpose: Purpose,
}
#[derive(Debug, Clone)]
pub struct Request {
    pub sequence: u64,
    pub request_id: u8,
    pub command: Vec<u8>,
}
impl Request {
    pub fn encode(&self) -> Result<Vec<u8>> {
        Control::Ipmi {
            command: self.command.clone(),
            request_id: self.request_id,
        }
        .encode()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub records: Vec<Record>,
    pub boot: BootState,
    pub response_error: Option<String>,
    #[serde(skip)]
    pending: BTreeMap<u8, Pending>,
    #[serde(skip)]
    retired: u128,
    #[serde(skip)]
    next_id: u8,
    #[serde(skip)]
    sequence: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            boot: BootState::default(),
            response_error: None,
            pending: BTreeMap::new(),
            retired: 0,
            next_id: 1,
            sequence: 0,
        }
    }
}
impl State {
    fn insert(
        &mut self,
        request_id: u8,
        command: Vec<u8>,
        purpose: Purpose,
        now: Instant,
    ) -> Request {
        self.sequence += 1;
        let sequence = self.sequence;
        self.push_record(Record {
            sequence,
            request_id,
            operation: purpose.operation(),
            command: command.clone(),
            response: None,
            phase: Phase::Pending,
            message: None,
        });
        self.pending.insert(
            request_id,
            Pending {
                sequence,
                started: now,
                purpose,
            },
        );
        self.response_error = None;
        Request {
            sequence,
            request_id,
            command,
        }
    }
    fn push_record(&mut self, record: Record) {
        self.records.push(record);
        if self.records.len() > HISTORY_LIMIT {
            if let Some(index) = self.records.iter().position(|r| r.phase != Phase::Pending) {
                self.records.remove(index);
            }
        }
    }
    pub fn begin_raw(&mut self, command: Vec<u8>, now: Instant) -> Result<Request> {
        validate_command(&command)?;
        for _ in 0..125 {
            let id = self.next_id;
            self.next_id = if id == 125 { 1 } else { id + 1 };
            if !self.pending.contains_key(&id) && self.retired & (1_u128 << id) == 0 {
                return Ok(self.insert(id, command, Purpose::Raw, now));
            }
        }
        Err(Error::Invalid(
            "没有可用的 IPMI 请求编号；请等待响应或重新连接后重试".into(),
        ))
    }
    fn check_boot_available(&self) -> Result<()> {
        if self.pending.contains_key(&BOOT_REQUEST_ID) {
            return Err(Error::Invalid("启动选项操作正在进行".into()));
        }
        if self.retired & (1_u128 << BOOT_REQUEST_ID) != 0 {
            return Err(Error::Invalid(
                "启动选项响应曾超时，请重新连接后重试".into(),
            ));
        }
        Ok(())
    }
    pub fn begin_boot_read(&mut self, now: Instant) -> Result<Request> {
        self.check_boot_available()?;
        self.boot.phase = BootPhase::Reading;
        self.boot.options = None;
        self.boot.message = None;
        Ok(self.insert(BOOT_REQUEST_ID, boot_read_command(), Purpose::BootRead, now))
    }
    pub fn begin_boot_write(
        &mut self,
        device: BootDevice,
        next_boot_only: bool,
        now: Instant,
    ) -> Result<Request> {
        self.check_boot_available()?;
        if self.boot.phase != BootPhase::Ready {
            return Err(Error::Invalid("请先成功读取服务器启动选项".into()));
        }
        let options = self
            .boot
            .options
            .as_ref()
            .ok_or_else(|| Error::Invalid("请先读取启动选项".into()))?
            .with_selection(device, next_boot_only);
        self.boot.phase = BootPhase::Writing;
        self.boot.message = None;
        Ok(self.insert(
            BOOT_REQUEST_ID,
            options.write_command(),
            Purpose::BootWrite(options),
            now,
        ))
    }
    pub fn receive(&mut self, response: Response, now: Instant) -> Option<Request> {
        let id = response.request_id;
        let pending = self.pending.remove(&id);
        let Some(pending) = pending else {
            if id < 128 && self.retired & (1_u128 << id) != 0 {
                if let Some(record) = self
                    .records
                    .iter_mut()
                    .rev()
                    .find(|r| r.request_id == id && r.phase == Phase::TimedOut)
                {
                    record.response = Some(response);
                    record.phase = Phase::LateResponse;
                    record.message = Some("响应晚于超时期限，未关联到新请求".into());
                    return None;
                }
            }
            self.sequence += 1;
            self.push_record(Record {
                sequence: self.sequence,
                request_id: id,
                operation: Operation::Unmatched,
                command: Vec::new(),
                response: Some(response),
                phase: Phase::Unmatched,
                message: Some("没有对应的待处理请求".into()),
            });
            return None;
        };
        let success = response.completion_code == 0;
        let error = (!success).then(|| format!("IPMI 完成码：0x{:04X}", response.completion_code));
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.sequence == pending.sequence)
        {
            record.phase = if success {
                Phase::Complete
            } else {
                Phase::Error
            };
            record.response = Some(response.clone());
            record.message = error.clone();
        }
        if matches!(pending.purpose, Purpose::Raw) {
            return None;
        }
        if let Some(error) = error {
            self.boot.phase = BootPhase::Error;
            self.boot.message = Some(error);
            return None;
        }
        match pending.purpose {
            Purpose::BootWrite(desired) => {
                self.boot.phase = BootPhase::Confirming;
                Some(self.insert(
                    BOOT_REQUEST_ID,
                    boot_read_command(),
                    Purpose::BootConfirm(desired),
                    now,
                ))
            }
            Purpose::BootRead | Purpose::BootConfirm(_) => {
                match BootOptions::parse(&response.data) {
                    Ok(options) => {
                        self.boot.phase = BootPhase::Ready;
                        self.boot.revision += 1;
                        self.boot.message = match pending.purpose {
                            Purpose::BootConfirm(desired) if !options.matches(&desired) => {
                                self.boot.phase = BootPhase::Error;
                                Some(
                                    "BMC 重新读取的启动选项与请求不一致；显示的是服务器返回的设置"
                                        .into(),
                                )
                            }
                            Purpose::BootConfirm(_) => {
                                Some("启动选项已应用，并已重新读取确认".into())
                            }
                            _ => None,
                        };
                        self.boot.options = Some(options);
                    }
                    Err(error) => {
                        self.boot.phase = BootPhase::Error;
                        self.boot.message = Some(error.to_string());
                        if let Some(record) = self
                            .records
                            .iter_mut()
                            .find(|r| r.sequence == pending.sequence)
                        {
                            record.phase = Phase::Error;
                            record.message = Some(error.to_string());
                        }
                    }
                }
                None
            }
            Purpose::Raw => None,
        }
    }
    pub fn send_failed(&mut self, request: &Request, message: String) {
        if self
            .pending
            .get(&request.request_id)
            .is_some_and(|p| p.sequence == request.sequence)
        {
            let pending = self.pending.remove(&request.request_id).unwrap();
            if let Some(record) = self
                .records
                .iter_mut()
                .find(|r| r.sequence == pending.sequence)
            {
                record.phase = Phase::Error;
                record.message = Some(message.clone());
            }
            if !matches!(pending.purpose, Purpose::Raw) {
                self.boot.phase = BootPhase::Error;
                self.boot.message = Some(message);
            }
        }
    }
    pub fn expire(&mut self, now: Instant) -> bool {
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| now.saturating_duration_since(p.started) >= RESPONSE_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            let pending = self.pending.remove(id).unwrap();
            self.retired |= 1_u128 << id;
            if let Some(record) = self
                .records
                .iter_mut()
                .find(|r| r.sequence == pending.sequence)
            {
                record.phase = Phase::TimedOut;
                record.message = Some("服务器在 30 秒内未返回 IPMI 响应".into());
            }
            if !matches!(pending.purpose, Purpose::Raw) {
                self.boot.phase = BootPhase::Error;
                self.boot.message = Some("启动选项响应超时；请重新连接后重试".into());
            }
        }
        !ids.is_empty()
    }
    pub fn close(&mut self) {
        for (_, pending) in std::mem::take(&mut self.pending) {
            if let Some(record) = self
                .records
                .iter_mut()
                .find(|r| r.sequence == pending.sequence)
            {
                record.phase = Phase::Disconnected;
                record.message = Some("连接已关闭，未取得响应".into());
            }
            if !matches!(pending.purpose, Purpose::Raw) {
                self.boot.phase = BootPhase::Error;
                self.boot.message = Some("连接已关闭，未确认启动选项".into());
            }
        }
    }
    pub fn clear_completed(&mut self) {
        self.records.retain(|r| r.phase == Phase::Pending);
        self.response_error = None;
    }
}
