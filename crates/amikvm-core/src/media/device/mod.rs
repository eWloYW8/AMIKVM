//! Local devices are discovered and reopened by identity, never treated as image files.
use super::{MAX_TRANSFER, scsi::Kind};
use crate::{Error, Result};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs::File,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub path: PathBuf,
    pub label: String,
    pub kind: Kind,
    pub capacity: u64,
    pub block_size: u32,
    pub readonly: bool,
    pub removable: bool,
    pub mounted: bool,
    pub groups: Vec<String>,
    pub identity: String,
}
pub fn list() -> Result<Vec<Device>> {
    let mut devices = platform::list()?;
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(devices)
}

pub struct Opened {
    pub file: File,
    pub device: Device,
    pub(crate) locks: Vec<File>,
    pub(crate) changed: bool,
    pub(crate) lease: Lease,
}
fn leases() -> &'static Mutex<HashSet<String>> {
    static LEASES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    LEASES.get_or_init(Default::default)
}
/// One physical medium has one owner across all server sessions in this process.
pub(crate) struct Lease {
    groups: Vec<String>,
}
impl Lease {
    fn reserve(device: &Device) -> Result<Self> {
        let mut active = leases()
            .lock()
            .map_err(|_| Error::Invalid("实体设备已被其他连接占用".into()))?;
        if device.groups.is_empty() || device.groups.iter().any(|g| active.contains(g)) {
            return Err(Error::Invalid("实体设备已被其他连接占用".into()));
        }
        active.extend(device.groups.iter().cloned());
        Ok(Self {
            groups: device.groups.clone(),
        })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut active) = leases().lock() {
            for group in &self.groups {
                active.remove(group);
            }
        }
    }
}
pub fn open(expected: &Device, kind: Kind, readonly: bool) -> Result<Opened> {
    let devices = list()?;
    let current = devices
        .iter()
        .find(|d| {
            d.id == expected.id
                && d.identity == expected.identity
                && d.path == expected.path
                && d.groups == expected.groups
        })
        .ok_or_else(|| Error::Invalid("实体设备已经移除或身份发生变化，请刷新列表".into()))?;
    if current.kind != kind {
        return Err(Error::Invalid("实体设备类型与所选介质不一致".into()));
    }
    if !readonly && (current.readonly || current.kind == Kind::Cdrom) {
        return Err(Error::Invalid("实体设备不支持可写连接".into()));
    }
    if !readonly
        && !cfg!(target_os = "windows")
        && devices.iter().any(|d| overlaps(d, current) && d.mounted)
    {
        return Err(Error::Invalid(
            "请先卸载所选设备上的本地文件系统，再以可写方式连接".into(),
        ));
    }
    let lease = Lease::reserve(current)?;
    let (file, locks) = platform::open(current, readonly)?;
    let mut device = current.clone();
    let (length, sector, changed) = platform::geometry(&file, kind)?;
    if !(512..=65536).contains(&sector)
        || !sector.is_power_of_two()
        || length % u64::from(sector) != 0
    {
        return Err(Error::Invalid("实体设备报告了无效的扇区大小或容量".into()));
    }
    if kind == Kind::Cdrom && !passthrough_supported() && sector != 2048 {
        return Err(Error::Invalid("该光盘的扇区格式暂不支持直接读取".into()));
    }
    device.capacity = length;
    device.block_size = sector;
    if length == 0 && kind != Kind::Cdrom {
        return Err(Error::Invalid("实体设备中没有可用介质".into()));
    }
    Ok(Opened {
        file,
        device,
        locks,
        changed,
        lease,
    })
}
pub fn overlaps(a: &Device, b: &Device) -> bool {
    a.groups.iter().any(|g| b.groups.contains(g))
}
/// Also checked while idle so unplugging does not wait for another BMC command.
pub fn present(file: &File, device: &Device) -> bool {
    platform::present(file, device)
}
pub fn geometry(file: &File, kind: Kind) -> Result<(u64, u32, bool)> {
    platform::geometry(file, kind)
}
pub fn passthrough_supported() -> bool {
    cfg!(any(target_os = "linux", target_os = "windows"))
}
pub fn optical_supported(opcode: u8) -> bool {
    passthrough_supported()
        || cfg!(target_os = "macos")
            && matches!(
                opcode,
                0x00 | 0x08
                    | 0x25
                    | 0x28
                    | 0x42
                    | 0x43
                    | 0x51
                    | 0x52
                    | 0xa4
                    | 0xa8
                    | 0xad
                    | 0xb9
                    | 0xbb
                    | 0xbe
            )
}
pub fn read(file: &mut File, offset: u64, bytes: usize) -> std::io::Result<Vec<u8>> {
    #[cfg(target_os = "windows")]
    {
        platform::read(file, offset, bytes)
    }
    #[cfg(not(target_os = "windows"))]
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut data = vec![0; bytes];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut data)?;
        Ok(data)
    }
}
pub fn write(file: &mut File, offset: u64, data: &[u8]) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        platform::write(file, offset, data)
    }
    #[cfg(not(target_os = "windows"))]
    {
        use std::io::{Seek, SeekFrom, Write};
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)
    }
}

/// Optical commands have explicit bounded input lengths; writes are never forwarded.
pub fn optical_transfer(cdb: &[u8], sector: u32) -> std::result::Result<(usize, usize), [u8; 3]> {
    if cdb.len() < 12 {
        return Err([5, 0x24, 0]);
    }
    let be16 = |at| u16::from_be_bytes([cdb[at], cdb[at + 1]]) as u64;
    let be32 = |at| u32::from_be_bytes(cdb[at..at + 4].try_into().unwrap()) as u64;
    let (cdb_length, bytes) = match cdb[0] {
        0x00 | 0x1b | 0x1e => (6, 0),
        0x03 | 0x12 | 0x1a => (6, cdb[4] as u64),
        0x08 => (
            6,
            u64::from(if cdb[4] == 0 { 256 } else { u32::from(cdb[4]) }) * u64::from(sector),
        ),
        0x25 => (10, 8),
        0x28 => (10, be16(7) * u64::from(sector)),
        0xa8 => (12, be32(6) * u64::from(sector)),
        // READ CD can return audio/raw sectors, C2 information and subchannels.
        // Reserve the largest permitted frame; SG/SPTD reports the actual bytes.
        0xbe => (
            12,
            u64::from(u32::from_be_bytes([0, cdb[6], cdb[7], cdb[8]])) * 2744,
        ),
        0xb9 => {
            let frames =
                |at: usize| -> std::result::Result<u64, [u8; 3]> {
                    if cdb[at + 1] >= 60 || cdb[at + 2] >= 75 {
                        return Err([5, 0x24, 0]);
                    }
                    Ok((u64::from(cdb[at]) * 60 + u64::from(cdb[at + 1])) * 75
                        + u64::from(cdb[at + 2]))
                };
            (
                12,
                frames(6)?.checked_sub(frames(3)?).ok_or([5, 0x24, 0])? * 2744,
            )
        }
        0x23 | 0x42 | 0x43 | 0x44 | 0x46 | 0x4a | 0x51 | 0x52 | 0x5a => (10, be16(7)),
        0xad => (12, be16(8)),
        0xbd => (12, be16(8)),
        0xa4 => (12, be16(8)),
        // Selection of read speed changes drive behavior but cannot write media.
        0xbb => (12, 0),
        0x45 | 0x47 | 0x48 | 0x4b => (10, 0),
        0xa5 => (12, 0),
        0x35 | 0x2f => (10, 0),
        0x0a | 0x15 | 0x2a | 0x55 | 0xaa | 0x04 | 0x2e | 0xae => return Err([7, 0x27, 0]),
        _ => return Err([5, 0x20, 0]),
    };
    if bytes > (MAX_TRANSFER - 30) as u64 {
        return Err([5, 0x24, 0]);
    }
    Ok((cdb_length, bytes as usize))
}
pub(super) fn sense(bytes: &[u8]) -> [u8; 3] {
    match bytes.first().map(|b| b & 0x7f) {
        Some(0x70 | 0x71) if bytes.len() >= 14 => [bytes[2] & 15, bytes[12], bytes[13]],
        Some(0x72 | 0x73) if bytes.len() >= 4 => [bytes[1] & 15, bytes[2], bytes[3]],
        _ => [4, 0x44, 0],
    }
}
pub fn optical(file: &File, cdb: &[u8], sector: u32) -> std::result::Result<Vec<u8>, [u8; 3]> {
    let (length, bytes) = optical_transfer(cdb, sector)?;
    platform::optical(file, &cdb[..length], bytes)
}
