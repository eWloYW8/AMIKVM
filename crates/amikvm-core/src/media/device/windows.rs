use super::*;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    mem::{offset_of, size_of},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::{FileSystem::*, IscsiDisc::*},
    System::{IO::DeviceIoControl, Ioctl::*, WindowsProgramming::DRIVE_REMOVABLE},
};
fn handle(file: &File) -> HANDLE {
    file.as_raw_handle() as HANDLE
}
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
fn native_open(path: &Path, access: u32, sharing: u32) -> Result<File> {
    let name = wide(path.as_os_str());
    // OPEN_EXISTING never creates a device or file. The returned handle has one owner.
    let value = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            sharing,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if value == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_handle(value.cast()) })
}
const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
fn ioctl(file: &File, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize> {
    let mut written = 0;
    let valid = unsafe {
        DeviceIoControl(
            handle(file),
            code,
            if input.is_empty() {
                std::ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if output.is_empty() {
                std::ptr::null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            output.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    if valid == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if written as usize > output.len() {
        return Err(Error::Protocol("Invalid device response size".into()));
    }
    Ok(written as usize)
}
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn property(file: &File, property: u32) -> Result<Vec<u8>> {
    let mut input = [0u8; 12];
    input[..4].copy_from_slice(&property.to_le_bytes());
    let mut output = vec![0; 65536];
    let length = ioctl(file, IOCTL_STORAGE_QUERY_PROPERTY, &input, &mut output)?;
    if length < 8 || u32_at(&output, 4) as usize > length {
        return Err(Error::Protocol("Truncated storage property".into()));
    }
    output.truncate(u32_at(&output, 4) as usize);
    Ok(output)
}
fn descriptor(file: &File) -> (String, bool) {
    let Ok(output) = property(file, StorageDeviceProperty as u32) else {
        return (String::new(), false);
    };
    if output.len() < 36 {
        return (String::new(), false);
    }
    let parts: Vec<_> = [12, 16, 20, 24]
        .into_iter()
        .filter_map(|offset| {
            let at = u32_at(&output, offset) as usize;
            (at >= 36 && at < output.len()).then(|| {
                String::from_utf8_lossy(output[at..].split(|b| *b == 0).next().unwrap())
                    .trim()
                    .to_owned()
            })
        })
        .filter(|s| !s.is_empty())
        .collect();
    (parts.join(" · "), output[10] != 0)
}
fn disk_groups(file: &File) -> Result<Vec<String>> {
    let mut output = vec![0; 65536];
    let length = ioctl(file, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], &mut output)?;
    if length < 8 {
        return Err(Error::Protocol("Truncated volume disk extents".into()));
    }
    let count = u32_at(&output, 0) as usize;
    if count == 0 || count > (length - 8) / 24 {
        return Err(Error::Protocol("Invalid volume disk extents".into()));
    }
    let mut groups: Vec<_> = (0..count)
        .map(|i| format!("windows:7:{}", u32_at(&output, 8 + i * 24)))
        .collect();
    groups.sort();
    groups.dedup();
    Ok(groups)
}
fn identity(file: &File, path: &Path) -> Result<(String, Vec<String>)> {
    let mut number = [0u8; 12];
    let (number_id, mut groups) =
        match ioctl(file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut number) {
            Ok(12) => {
                let group = format!("windows:{}:{}", u32_at(&number, 0), u32_at(&number, 4));
                (format!("{group}:{}", u32_at(&number, 8)), vec![group])
            }
            _ => {
                let groups = disk_groups(file)?;
                (groups.join(";"), groups)
            }
        };
    if let Ok(extents) = disk_groups(file) {
        groups = extents;
    }
    let (description, _) = descriptor(file);
    let unique = property(file, StorageDeviceIdProperty as u32)
        .map(|v| format!("{:x}", Sha256::digest(v)))
        .unwrap_or_default();
    Ok((
        format!("{number_id}:{}:{description}:{unique}", path.display()),
        groups,
    ))
}
struct Volumes(HANDLE);
impl Drop for Volumes {
    fn drop(&mut self) {
        unsafe {
            FindVolumeClose(self.0);
        }
    }
}
fn volume_paths() -> Result<Vec<(PathBuf, String, bool)>> {
    let mut name = vec![0u16; 1024];
    let search = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if search == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            Ok(vec![])
        } else {
            Err(error.into())
        };
    }
    let search = Volumes(search);
    let mut output = vec![];
    loop {
        let end = name
            .iter()
            .position(|v| *v == 0)
            .ok_or_else(|| Error::Protocol("Unterminated volume name".into()))?;
        let volume = String::from_utf16_lossy(&name[..end]);
        let mut mounts = vec![0u16; 512];
        loop {
            let mut needed = 0;
            if unsafe {
                GetVolumePathNamesForVolumeNameW(
                    name.as_ptr(),
                    mounts.as_mut_ptr(),
                    mounts.len() as u32,
                    &mut needed,
                )
            } != 0
            {
                break;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_MORE_DATA as i32)
                || needed > 1024 * 1024
                || needed as usize <= mounts.len()
            {
                return Err(error.into());
            }
            mounts.resize(needed as usize, 0);
        }
        let points: Vec<_> = mounts
            .split(|v| *v == 0)
            .filter(|v| !v.is_empty())
            .map(String::from_utf16_lossy)
            .collect();
        let label = if points.is_empty() {
            volume.clone()
        } else {
            points.join(" · ")
        };
        output.push((
            PathBuf::from(volume.trim_end_matches('\\')),
            label,
            !points.is_empty(),
        ));
        name.fill(0);
        if unsafe { FindNextVolumeW(search.0, name.as_mut_ptr(), name.len() as u32) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error.into());
            }
            break;
        }
    }
    Ok(output)
}
pub(super) fn list() -> Result<Vec<Device>> {
    let mut names = vec![0u16; 32768];
    let length = loop {
        let n =
            unsafe { QueryDosDeviceW(std::ptr::null(), names.as_mut_ptr(), names.len() as u32) };
        if n > 0 {
            break n as usize;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || names.len() >= 1024 * 1024
        {
            return Err(error.into());
        }
        names.resize(names.len() * 2, 0);
    };
    let mut paths = vec![];
    for name in names[..length].split(|v| *v == 0).filter(|s| !s.is_empty()) {
        let name = String::from_utf16_lossy(name);
        let physical = name
            .strip_prefix("PhysicalDrive")
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()));
        let cd = name
            .strip_prefix("CdRom")
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()));
        if physical || cd {
            paths.push((
                PathBuf::from(format!(r"\\.\{name}")),
                name,
                if cd { Kind::Cdrom } else { Kind::HardDisk },
                false,
            ));
        }
    }
    for (path, label, mounted) in volume_paths()? {
        paths.push((path, label, Kind::HardDisk, mounted));
    }
    // Floppy devices are not guaranteed to appear in volume GUID enumeration.
    let drives = unsafe { GetLogicalDrives() };
    for index in 0..2u8 {
        if drives & (1 << index) == 0 {
            continue;
        }
        let letter = b'A' + index;
        let root = [u16::from(letter), 58, 92, 0];
        if unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOVABLE {
            paths.push((
                PathBuf::from(format!(r"\\.\{}:", letter as char)),
                format!("{}:", letter as char),
                Kind::Floppy,
                true,
            ));
        }
    }
    let mut devices = vec![];
    let mut uncertain = false;
    for (path, label, mut kind, mounted) in paths {
        let Ok(file) = native_open(&path, 0, SHARING) else {
            uncertain = true;
            continue;
        };
        let Ok((identity, groups)) = identity(&file, &path) else {
            uncertain = true;
            continue;
        };
        // Skip duplicate optical volumes; the CdRom device already names the drive.
        if groups.iter().any(|g| g.starts_with("windows:2:")) {
            if kind != Kind::Cdrom {
                continue;
            }
            kind = Kind::Cdrom;
        }
        let (description, removable) = descriptor(&file);
        let (capacity, block_size, _) = geometry(&file, kind).unwrap_or((
            0,
            if kind == Kind::Cdrom { 2048 } else { 512 },
            false,
        ));
        let readonly =
            kind == Kind::Cdrom || ioctl(&file, IOCTL_DISK_IS_WRITABLE, &[], &mut []).is_err();
        devices.push(Device {
            id: path.to_string_lossy().into_owned(),
            path,
            label: if description.is_empty() {
                label
            } else {
                format!("{label} · {description}")
            },
            kind,
            capacity,
            block_size,
            readonly,
            removable,
            mounted,
            groups,
            identity,
            media_generation: generation(&file, kind),
        });
    }
    // Unknown mounted volumes make writable whole-disk exclusion unprovable.
    if uncertain {
        for device in &mut devices {
            device.readonly = true;
        }
    }
    Ok(devices)
}
fn lock_volume(file: &File) -> Result<()> {
    ioctl(file, FSCTL_LOCK_VOLUME, &[], &mut [])
        .map(|_| ())
        .map_err(|_| Error::Invalid("无法独占锁定实体设备，请关闭使用该设备的程序".into()))
}
pub(super) fn open(device: &Device, readonly: bool) -> Result<(File, Vec<File>)> {
    let whole = device
        .path
        .to_string_lossy()
        .starts_with(r"\\.\PhysicalDrive");
    let mut locks = vec![];
    if !readonly && whole {
        // Hold locks on every volume intersecting this disk, including GUID-only
        // volumes and multi-disk dynamic volumes, for the entire redirect lifetime.
        for (path, _, _) in volume_paths()? {
            let metadata = native_open(&path, 0, SHARING)?;
            let groups = identity(&metadata, &path)?.1;
            if groups.iter().any(|v| device.groups.contains(v)) {
                drop(metadata);
                let volume = native_open(&path, GENERIC_READ | GENERIC_WRITE, SHARING)?;
                lock_volume(&volume)?;
                locks.push(volume);
            }
        }
    }
    // Windows optical passthrough requires RW handle access; the core never
    // forwards writing CDBs to an optical device.
    let access = GENERIC_READ
        | if !readonly || device.kind == Kind::Cdrom {
            GENERIC_WRITE
        } else {
            0
        };
    let file = native_open(
        &device.path,
        access,
        if !readonly && whole { 0 } else { SHARING },
    )?;
    if identity(&file, &device.path)?.0 != device.identity {
        return Err(Error::Invalid("实体设备身份已改变".into()));
    }
    if !readonly && !whole {
        lock_volume(&file)?;
    }
    Ok((file, locks))
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
            Ok(v) if v.len() == 8 => {
                let blocks = u32::from_be_bytes(v[..4].try_into().unwrap()) as u64 + 1;
                let sector = u32::from_be_bytes(v[4..].try_into().unwrap());
                Ok((blocks.saturating_mul(u64::from(sector)), sector, changed))
            }
            Err([2, 0x3a, _]) => Ok((0, 2048, changed)),
            _ => {
                let mut geometry = vec![0; 512];
                if ioctl(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geometry)? < 32 {
                    return Err(Error::Protocol("Truncated device geometry".into()));
                }
                Ok((
                    u64::from_le_bytes(geometry[24..32].try_into().unwrap()),
                    2048,
                    changed,
                ))
            }
        };
    }
    let mut geometry = vec![0; 512];
    if ioctl(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], &mut geometry)? < 32 {
        return Err(Error::Protocol("Truncated device geometry".into()));
    }
    // Physical geometry describes the containing disk; volume capacity is separate.
    let mut length = [0u8; 8];
    if ioctl(file, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut length)? != 8 {
        return Err(Error::Protocol("Truncated device length response".into()));
    }
    Ok((u64::from_le_bytes(length), u32_at(&geometry, 20), false))
}
pub(super) fn present(file: &File, device: &Device) -> bool {
    let Ok(current) = native_open(&device.path, 0, SHARING) else {
        // A writable whole disk has a non-sharing handle; inspect that handle.
        return identity(file, &device.path).is_ok_and(|v| v.0 == device.identity);
    };
    identity(&current, &device.path).is_ok_and(|v| v.0 == device.identity)
}
fn generation(file: &File, kind: Kind) -> Option<u64> {
    if kind == Kind::Cdrom {
        // VERIFY can consume optical unit attention; SPTD already handles it.
        return None;
    }
    let mut count = [0u8; 4];
    (ioctl(file, IOCTL_STORAGE_CHECK_VERIFY2, &[], &mut count).ok()? == 4)
        .then(|| u64::from(u32::from_le_bytes(count)))
}
pub(super) fn media_generation(file: &File, device: &Device) -> Option<u64> {
    generation(file, device.kind)
}
struct Buffer(*mut u8, std::alloc::Layout);
impl Buffer {
    fn new(file: &File, bytes: usize) -> io::Result<Self> {
        let alignment = property(file, StorageAdapterProperty as u32)
            .ok()
            .filter(|p| p.len() >= 20)
            .map_or(65536, |p| {
                (u32_at(&p, 16) as usize)
                    .checked_add(1)
                    .unwrap_or(0)
                    .max(65536)
            });
        let layout = std::alloc::Layout::from_size_align(bytes.max(1), alignment)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid adapter alignment"))?;
        let value = unsafe { std::alloc::alloc_zeroed(layout) };
        if value.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "Device transfer allocation failed",
            ));
        }
        Ok(Self(value, layout))
    }
    fn slice(&self, bytes: usize) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.0, bytes) }
    }
    fn slice_mut(&mut self, bytes: usize) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.0, bytes) }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        unsafe {
            std::alloc::dealloc(self.0, self.1);
        }
    }
}
pub(super) fn read(file: &mut File, offset: u64, bytes: usize) -> io::Result<Vec<u8>> {
    let mut buffer = Buffer::new(file, bytes)?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buffer.slice_mut(bytes))?;
    Ok(buffer.slice(bytes).to_vec())
}
pub(super) fn write(file: &mut File, offset: u64, data: &[u8]) -> io::Result<()> {
    let mut buffer = Buffer::new(file, data.len())?;
    buffer.slice_mut(data.len()).copy_from_slice(data);
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(buffer.slice(data.len()))
}
#[repr(C)]
struct Request {
    pass: SCSI_PASS_THROUGH_DIRECT,
    sense: [u8; 64],
}
pub(super) fn optical(
    file: &File,
    cdb: &[u8],
    bytes: usize,
) -> std::result::Result<Vec<u8>, [u8; 3]> {
    if cdb.len() > 16 {
        return Err([5, 0x24, 0]);
    }
    let buffer = Buffer::new(file, bytes).map_err(|_| [4, 0x44, 0])?;
    let mut request = Request {
        pass: SCSI_PASS_THROUGH_DIRECT::default(),
        sense: [0; 64],
    };
    request.pass.Length = size_of::<SCSI_PASS_THROUGH_DIRECT>() as u16;
    request.pass.CdbLength = cdb.len() as u8;
    request.pass.SenseInfoLength = 64;
    request.pass.DataIn = if bytes == 0 {
        SCSI_IOCTL_DATA_UNSPECIFIED as u8
    } else {
        SCSI_IOCTL_DATA_IN as u8
    };
    request.pass.DataTransferLength = bytes as u32;
    request.pass.TimeOutValue = 15;
    request.pass.DataBuffer = buffer.0.cast();
    request.pass.SenseInfoOffset = offset_of!(Request, sense) as u32;
    request.pass.Cdb[..cdb.len()].copy_from_slice(cdb);
    let mut written = 0;
    let pointer = (&mut request as *mut Request).cast();
    if unsafe {
        DeviceIoControl(
            handle(file),
            IOCTL_SCSI_PASS_THROUGH_DIRECT,
            pointer,
            size_of::<Request>() as u32,
            pointer,
            size_of::<Request>() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(match io::Error::last_os_error().raw_os_error() {
            Some(v) if v == ERROR_NOT_READY as i32 || v == ERROR_DEVICE_NOT_CONNECTED as i32 => {
                [2, 0x3a, 0]
            }
            Some(v) if v == ERROR_ACCESS_DENIED as i32 => [5, 0x20, 0],
            _ => [4, 0x44, 0],
        });
    }
    if written < offset_of!(Request, sense) as u32 || request.pass.SenseInfoLength > 64 {
        return Err([4, 0x44, 0]);
    }
    if request.pass.ScsiStatus != 0 {
        let available = (written as usize)
            .saturating_sub(offset_of!(Request, sense))
            .min(request.pass.SenseInfoLength as usize);
        return Err(super::sense(&request.sense[..available]));
    }
    if request.pass.DataTransferLength as usize > bytes {
        return Err([4, 0x44, 0]);
    }
    Ok(buffer
        .slice(request.pass.DataTransferLength as usize)
        .to_vec())
}
