use super::*;
use plist::Value;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{self, Read},
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn diskutil(args: &[&str], deadline: Instant) -> Result<Value> {
    let mut child = Command::new("/usr/sbin/diskutil")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Protocol("Missing diskutil output".into()))?;
    let reading = std::thread::spawn(move || {
        let mut bytes = vec![];
        stdout
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(Error::Timeout("Local device discovery"));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error.into());
            }
        }
    };
    let bytes = reading
        .join()
        .map_err(|_| Error::Protocol("Device discovery reader failed".into()))??;
    if !status?.success() {
        return Err(Error::Invalid("无法读取本地设备信息".into()));
    }
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(Error::Protocol(
            "Device discovery output is too large".into(),
        ));
    }
    Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| Error::Protocol(format!("Invalid device property list: {e}")))
}
fn string(value: &Value, key: &str) -> String {
    value
        .as_dictionary()
        .and_then(|d| d.get(key))
        .and_then(Value::as_string)
        .unwrap_or_default()
        .to_owned()
}
fn boolean(value: &Value, key: &str) -> Option<bool> {
    value.as_dictionary()?.get(key)?.as_boolean()
}
fn number(value: &Value, key: &str) -> Option<u64> {
    value.as_dictionary()?.get(key)?.as_unsigned_integer()
}
fn valid_name(value: &str) -> bool {
    value.strip_prefix("disk").is_some_and(|s| {
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_digit() || c == 's')
            && s.chars().next().unwrap().is_ascii_digit()
    })
}
fn whole(name: &str) -> String {
    let rest = name.strip_prefix("disk").unwrap_or(name);
    format!(
        "disk{}",
        rest.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
    )
}
fn collect(
    value: &Value,
    ids: &mut BTreeSet<String>,
    stores: &mut HashMap<String, BTreeSet<String>>,
) {
    match value {
        Value::Dictionary(dict) => {
            let id = string(value, "DeviceIdentifier");
            if valid_name(&id) {
                ids.insert(id.clone());
            }
            if let Some(Value::Array(physical)) = dict.get("APFSPhysicalStores") {
                let roots: BTreeSet<_> = physical
                    .iter()
                    .map(|v| string(v, "DeviceIdentifier"))
                    .filter(|s| valid_name(s))
                    .map(|s| whole(&s))
                    .collect();
                if !roots.is_empty() && valid_name(&id) {
                    stores.insert(whole(&id), roots);
                }
            }
            let container = string(value, "APFSContainerReference");
            if valid_name(&container) && valid_name(&id) {
                stores
                    .entry(whole(&container))
                    .or_default()
                    .insert(whole(&id));
            }
            for child in dict.values() {
                collect(child, ids, stores);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect(value, ids, stores);
            }
        }
        Value::String(name) if valid_name(name) => {
            ids.insert(name.clone());
        }
        _ => {}
    }
}
fn roots(
    name: &str,
    stores: &HashMap<String, BTreeSet<String>>,
    visited: &mut BTreeSet<String>,
) -> BTreeSet<String> {
    let name = whole(name);
    if !visited.insert(name.clone()) {
        return BTreeSet::new();
    }
    let mut result = BTreeSet::from([name.clone()]);
    if let Some(physical) = stores.get(&name) {
        for disk in physical {
            result.extend(roots(disk, stores, visited));
        }
    }
    result
}
fn node_identity(metadata: &fs::Metadata) -> String {
    format!(
        ":node:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.rdev()
    )
}
pub(super) fn list() -> Result<Vec<Device>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let listing = diskutil(
        &["list", "-plist"],
        deadline.min(Instant::now() + Duration::from_secs(10)),
    )?;
    let mut ids = BTreeSet::new();
    let mut stores = HashMap::new();
    collect(&listing, &mut ids, &mut stores);
    let mut devices = vec![];
    let mut uncertain = false;
    for id in ids {
        if Instant::now() >= deadline {
            return Err(Error::Timeout("Local device discovery"));
        }
        let info = match diskutil(
            &["info", "-plist", &id],
            deadline.min(Instant::now() + Duration::from_secs(5)),
        ) {
            Ok(v) => v,
            Err(_) => {
                uncertain = true;
                continue;
            }
        };
        let path = PathBuf::from(string(&info, "DeviceNode"));
        if path != Path::new("/dev").join(&id) {
            uncertain = true;
            continue;
        }
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_block_device() && !metadata.file_type().is_char_device() {
            continue;
        }
        let protocol = string(&info, "BusProtocol");
        let media = string(&info, "MediaType");
        let optical = protocol.to_ascii_lowercase().contains("atapi")
            || ["CD", "DVD", "BD"].iter().any(|s| media.starts_with(s));
        let kind = if optical { Kind::Cdrom } else { Kind::HardDisk };
        let label = string(&info, "MediaName");
        let readonly = kind == Kind::Cdrom
            || boolean(&info, "Writable") == Some(false)
            || boolean(&info, "ReadOnlyMedia") == Some(true)
            || boolean(&info, "ReadOnlyVolume") == Some(true);
        let block_size = number(&info, "DeviceBlockSize")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(if optical { 2048 } else { 512 });
        let capacity = number(&info, "TotalSize")
            .or_else(|| number(&info, "DiskSize"))
            .unwrap_or(0);
        let identity = format!(
            "{id}:{}:{}:{}:{}{}",
            string(&info, "MediaUUID"),
            string(&info, "DiskUUID"),
            string(&info, "VolumeUUID"),
            string(&info, "IORegistryEntryName"),
            node_identity(&metadata)
        );
        devices.push(Device {
            id: id.clone(),
            path,
            label: if label.is_empty() {
                id.clone()
            } else {
                format!("{label} · {id}")
            },
            kind,
            capacity,
            block_size,
            readonly,
            removable: boolean(&info, "RemovableMedia").unwrap_or(false)
                || boolean(&info, "Ejectable").unwrap_or(false),
            mounted: boolean(&info, "Mounted").unwrap_or(false)
                || !string(&info, "MountPoint").is_empty(),
            groups: roots(&id, &stores, &mut BTreeSet::new())
                .into_iter()
                .collect(),
            identity,
        });
    }
    // A missing volume's mount/alias state must never authorize raw writes.
    if uncertain {
        for device in &mut devices {
            device.readonly = true;
        }
    }
    Ok(devices)
}
pub(super) fn open(device: &Device, readonly: bool) -> Result<(File, Vec<File>)> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(!readonly)
        .custom_flags(
            libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | libc::O_NOFOLLOW
                | if readonly {
                    libc::O_SHLOCK
                } else {
                    libc::O_EXLOCK
                },
        )
        .open(&device.path)?;
    let metadata = file.metadata()?;
    if (!metadata.file_type().is_block_device() && !metadata.file_type().is_char_device())
        || !device.identity.ends_with(&node_identity(&metadata))
    {
        return Err(Error::Invalid("实体设备身份已改变".into()));
    }
    Ok((file, vec![]))
}
pub(super) fn geometry(file: &File, _kind: Kind) -> Result<(u64, u32, bool)> {
    let mut sector = 0u32;
    let mut count = 0u64;
    // Darwin disk.h DKIOCGETBLOCKSIZE and DKIOCGETBLOCKCOUNT use fixed-width
    // output buffers; these do not depend on host pointer size.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x40046418, &mut sector) } < 0
        || unsafe { libc::ioctl(file.as_raw_fd(), 0x40086419, &mut count) } < 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok((
        count
            .checked_mul(u64::from(sector))
            .ok_or_else(|| Error::Protocol("Device capacity overflow".into()))?,
        sector,
        false,
    ))
}
pub(super) fn present(file: &File, device: &Device) -> bool {
    let (Ok(current), Ok(open)) = (fs::metadata(&device.path), file.metadata()) else {
        return false;
    };
    current.rdev() == open.rdev()
        && current.ino() == open.ino()
        && device.identity.ends_with(&node_identity(&current))
}
pub(super) fn optical(
    _file: &File,
    _cdb: &[u8],
    _bytes: usize,
) -> std::result::Result<Vec<u8>, [u8; 3]> {
    Err([5, 0x20, 0])
}
