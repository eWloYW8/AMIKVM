use super::*;
use std::{
    fs, io,
    os::{
        fd::AsRawFd,
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

fn text(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
fn number(path: impl AsRef<Path>) -> u64 {
    text(path).parse().unwrap_or(0)
}
fn node_identity(metadata: &fs::Metadata) -> String {
    format!(
        ":node:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.rdev()
    )
}
fn backing_groups(
    location: &Path,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Vec<String> {
    let whole = if location.join("partition").exists() {
        location.parent().unwrap_or(location)
    } else {
        location
    };
    if !visited.insert(whole.to_owned()) {
        return vec![];
    }
    let mut groups = vec![whole.to_string_lossy().into_owned()];
    if let Ok(slaves) = fs::read_dir(whole.join("slaves")) {
        for slave in slaves.flatten() {
            if let Ok(target) = slave.path().canonicalize() {
                groups.extend(backing_groups(&target, visited));
            }
        }
    }
    groups.sort();
    groups.dedup();
    groups
}
// sysfs capacity is always in 512-byte units, regardless of logical sector size.
pub(super) fn list() -> Result<Vec<Device>> {
    scan(
        Path::new("/sys/class/block"),
        Path::new("/dev"),
        &fs::read_to_string("/proc/self/mountinfo")?,
    )
}
pub(crate) fn scan(root: &Path, nodes: &Path, mountinfo: &str) -> Result<Vec<Device>> {
    let mounted: std::collections::HashSet<_> = mountinfo
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .collect();
    let mut output = vec![];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let path = nodes.join(&name);
        if !path.exists() {
            continue;
        }
        let sys = entry.path();
        let Ok(location) = fs::canonicalize(&sys) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let partition = sys.join("partition").exists();
        let whole = if partition {
            location.parent().unwrap_or(&location).to_owned()
        } else {
            location.clone()
        };
        let device_number = text(sys.join("dev"));
        if device_number.is_empty() {
            continue;
        }
        let class = text(whole.join("device/type"));
        let name = name.to_string_lossy();
        let kind = if class == "5" || name.starts_with("sr") {
            Kind::Cdrom
        } else if name.starts_with("fd") {
            Kind::Floppy
        } else {
            Kind::HardDisk
        };
        let model = text(whole.join("device/model"));
        let serial = text(whole.join("device/serial"));
        let wwid = text(whole.join("device/wwid"));
        let sector = number(whole.join("queue/logical_block_size"));
        let block_size = if kind == Kind::Cdrom {
            2048
        } else {
            u32::try_from(sector)
                .ok()
                .filter(|v| *v >= 512)
                .unwrap_or(512)
        };
        let identity = format!(
            "{device_number}:{serial}:{wwid}:{}{}",
            location.display(),
            node_identity(&metadata)
        );
        output.push(Device {
            id: device_number.clone(),
            path,
            label: if model.is_empty() {
                name.into_owned()
            } else {
                format!("{model} · {name}")
            },
            kind,
            capacity: number(sys.join("size")).saturating_mul(512),
            block_size,
            readonly: kind == Kind::Cdrom || number(sys.join("ro")) != 0,
            removable: number(whole.join("removable")) != 0,
            mounted: mounted.contains(device_number.as_str()),
            groups: backing_groups(&location, &mut std::collections::HashSet::new()),
            identity,
        });
    }
    Ok(output)
}
pub(super) fn open(device: &Device, readonly: bool) -> Result<(File, Vec<File>)> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(!readonly)
        .custom_flags(
            libc::O_NONBLOCK
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | if readonly { 0 } else { libc::O_EXCL },
        )
        .open(&device.path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_block_device()
        || format!(
            "{}:{}",
            libc::major(metadata.rdev()),
            libc::minor(metadata.rdev())
        ) != device.id
        || !device.identity.ends_with(&node_identity(&metadata))
    {
        return Err(Error::Invalid("所选路径不再是原实体块设备".into()));
    }
    if readonly {
        fs2::FileExt::try_lock_shared(&file)
    } else {
        fs2::FileExt::try_lock_exclusive(&file)
    }
    .map_err(|_| Error::Invalid("实体设备已被其他连接占用".into()))?;
    Ok((file, vec![]))
}
pub(super) fn geometry(file: &File, kind: Kind) -> Result<(u64, u32, bool)> {
    if kind == Kind::Cdrom {
        let mut cdb = [0u8; 10];
        cdb[0] = 0x25;
        let mut value = optical(file, &cdb, 8);
        let changed = matches!(value, Err([6, _, _]));
        if changed {
            value = optical(file, &cdb, 8);
        }
        return match value {
            Ok(data) if data.len() == 8 => {
                let blocks = u64::from(u32::from_be_bytes(data[..4].try_into().unwrap())) + 1;
                let sector = u32::from_be_bytes(data[4..].try_into().unwrap());
                Ok((blocks * u64::from(sector), sector, changed))
            }
            Err([2, 0x3a, _]) => Ok((0, 2048, changed)),
            Err(sense) => Err(Error::Invalid(format!(
                "Optical capacity unavailable (sense {:02x}/{:02x}/{:02x})",
                sense[0], sense[1], sense[2]
            ))),
            _ => Err(Error::Protocol(
                "Truncated optical capacity response".into(),
            )),
        };
    }
    let mut length = 0u64;
    let mut sector = if kind == Kind::Cdrom { 2048 } else { 512u32 };
    // Both buffers match the Linux block ioctl ABI and outlive the synchronous call.
    let get_size = 0x80001272u64 | ((std::mem::size_of::<libc::size_t>() as u64) << 16);
    let result = unsafe { libc::ioctl(file.as_raw_fd(), get_size as libc::c_ulong, &mut length) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if kind == Kind::Cdrom && error.raw_os_error() == Some(libc::ENOMEDIUM) {
            return Ok((0, 2048, false));
        }
        return Err(error.into());
    }
    if kind != Kind::Cdrom
        && unsafe { libc::ioctl(file.as_raw_fd(), libc::BLKSSZGET, &mut sector) } < 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok((length, sector, false))
}
pub(super) fn present(file: &File, device: &Device) -> bool {
    let Ok(current) = fs::metadata(&device.path) else {
        return false;
    };
    let Ok(open) = file.metadata() else {
        return false;
    };
    current.file_type().is_block_device()
        && current.rdev() == open.rdev()
        && current.ino() == open.ino()
        && device.identity.ends_with(&node_identity(&open))
        && Path::new("/sys/class/block")
            .join(device.path.file_name().unwrap_or_default())
            .exists()
}
#[repr(C)]
struct Sg {
    interface: i32,
    direction: i32,
    cdb_length: u8,
    sense_length: u8,
    iovec: u16,
    length: u32,
    data: *mut libc::c_void,
    cdb: *mut u8,
    sense: *mut u8,
    timeout: u32,
    flags: u32,
    id: i32,
    user: *mut libc::c_void,
    status: u8,
    masked_status: u8,
    message: u8,
    sense_written: u8,
    host: u16,
    driver: u16,
    residual: i32,
    duration: u32,
    info: u32,
}
pub(super) fn optical(
    file: &File,
    cdb: &[u8],
    bytes: usize,
) -> std::result::Result<Vec<u8>, [u8; 3]> {
    let mut data = vec![0; bytes];
    let mut command = cdb.to_vec();
    let mut sense_buffer = [0u8; 64];
    let mut request = Sg {
        interface: i32::from(b'S'),
        direction: if bytes == 0 { -1 } else { -3 },
        cdb_length: command.len() as u8,
        sense_length: 64,
        iovec: 0,
        length: bytes as u32,
        data: data.as_mut_ptr().cast(),
        cdb: command.as_mut_ptr(),
        sense: sense_buffer.as_mut_ptr(),
        timeout: 15000,
        flags: 0,
        id: 0,
        user: std::ptr::null_mut(),
        status: 0,
        masked_status: 0,
        message: 0,
        sense_written: 0,
        host: 0,
        driver: 0,
        residual: 0,
        duration: 0,
        info: 0,
    };
    // SG_IO uses buffered IO; all CDB/data/sense allocations remain live until it returns.
    if unsafe { libc::ioctl(file.as_raw_fd(), 0x2285, &mut request) } < 0 {
        return Err(match io::Error::last_os_error().raw_os_error() {
            Some(libc::ENODEV | libc::ENXIO | libc::ENOMEDIUM) => [2, 0x3a, 0],
            Some(libc::EPERM | libc::EACCES) => [5, 0x20, 0],
            _ => [4, 0x44, 0],
        });
    }
    if request.sense_written > 0 {
        return Err(super::sense(
            &sense_buffer[..usize::from(request.sense_written).min(64)],
        ));
    }
    if request.status != 0 || request.host != 0 || request.driver != 0 {
        return Err([4, 0x44, 0]);
    }
    if request.residual < 0 || request.residual as usize > data.len() {
        return Err([4, 0x44, 0]);
    }
    data.truncate(data.len() - request.residual as usize);
    Ok(data)
}
