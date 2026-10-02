//! Darwin IOCDMediaBSDClient translates these ioctls to optical drive commands.
//! The kernel returns the actual MMC data, including multiple tracks/sessions.
use crate::media::MAX_TRANSFER;
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::FileExt},
};

// LP64 dk_cd_read_{toc,disc_info,track_info}_t share the same length/pointer
// layout. The command-specific prefix and all reserved fields are zeroed.
#[repr(C)]
struct Request {
    prefix: [u8; 14],
    length: u16,
    buffer: *mut u8,
}
const _: () = {
    assert!(std::mem::size_of::<Request>() == 24);
    assert!(std::mem::offset_of!(Request, length) == 14);
    assert!(std::mem::offset_of!(Request, buffer) == 16);
};

fn sense(error: io::Error) -> [u8; 3] {
    match error.raw_os_error() {
        Some(libc::ENXIO | libc::ENODEV) => [2, 0x3a, 0],
        Some(libc::EBUSY) => [2, 0x04, 1],
        Some(libc::EINVAL) => [5, 0x24, 0],
        Some(libc::ENOTTY | libc::EOPNOTSUPP | libc::ENOSYS) => [5, 0x20, 0],
        Some(libc::EIO) => [3, 0x11, 0],
        _ => [4, 0x44, 0],
    }
}

#[repr(C)]
struct ReadRequest {
    offset: u64,
    area: u8,
    kind: u8,
    reserved: [u8; 10],
    length: u32,
    buffer: *mut u8,
}
const _: () = {
    assert!(std::mem::size_of::<ReadRequest>() == 32);
    assert!(std::mem::offset_of!(ReadRequest, length) == 20);
    assert!(std::mem::offset_of!(ReadRequest, buffer) == 24);
};
fn ioctl<T>(file: &File, command: libc::c_ulong, request: &mut T) -> io::Result<()> {
    if unsafe { libc::ioctl(file.as_raw_fd(), command, request) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
/// CD nodes can expose raw 2352-byte blocks; the logical data device uses
/// 2048 bytes per block. READ CD independently selects raw/audio areas.
pub(super) fn geometry(file: &File) -> io::Result<(u64, u32)> {
    let mut sector = 0u32;
    let mut count = 0u64;
    let result =
        ioctl(file, 0x40046418, &mut sector).and_then(|_| ioctl(file, 0x40086419, &mut count));
    if let Err(error) = result {
        return if matches!(error.raw_os_error(), Some(libc::ENXIO | libc::ENODEV)) {
            Ok((0, 2048))
        } else {
            Err(error)
        };
    }
    if !matches!(sector, 2048 | 2352) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid optical block size",
        ));
    }
    Ok((
        count.checked_mul(2048).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Optical capacity overflow")
        })?,
        2048,
    ))
}
fn read_native(
    file: &File,
    lba: u64,
    count: usize,
    stride: usize,
    area: u8,
    kind: u8,
) -> Result<Vec<u8>, [u8; 3]> {
    let bytes = count
        .checked_mul(stride)
        .filter(|&n| n <= MAX_TRANSFER - 30)
        .ok_or([5, 0x24, 0])?;
    if bytes == 0 {
        return Ok(vec![]);
    }
    let mut data = vec![0; bytes];
    let mut request = ReadRequest {
        // The Darwin driver divides byteStart by the complete selected frame
        // size, including error flags and subchannel bytes.
        offset: lba.checked_mul(stride as u64).ok_or([5, 0x24, 0])?,
        area,
        kind,
        reserved: [0; 10],
        length: bytes as u32,
        buffer: data.as_mut_ptr(),
    };
    ioctl(file, 0xc0206460, &mut request).map_err(sense)?;
    if request.length as usize > bytes || !(request.length as usize).is_multiple_of(stride) {
        return Err([4, 0x44, 0]);
    }
    data.truncate(request.length as usize);
    Ok(data)
}
fn read_data(file: &File, cdb: &[u8], bytes: usize) -> Result<Vec<u8>, [u8; 3]> {
    let (lba, count) = match cdb[0] {
        0x08 => (
            u32::from_be_bytes([0, cdb[1] & 31, cdb[2], cdb[3]]),
            if cdb[4] == 0 { 256 } else { u32::from(cdb[4]) },
        ),
        0x28 => (
            u32::from_be_bytes(cdb[2..6].try_into().unwrap()),
            u32::from(u16::from_be_bytes(cdb[7..9].try_into().unwrap())),
        ),
        _ => (
            u32::from_be_bytes(cdb[2..6].try_into().unwrap()),
            u32::from_be_bytes(cdb[6..10].try_into().unwrap()),
        ),
    };
    if (count as usize).checked_mul(2048) != Some(bytes) {
        return Err([5, 0x24, 0]);
    }
    match read_native(file, u64::from(lba), count as usize, 2048, 0x10, 2) {
        Err([5, 0x20, 0]) => {
            // DVD/BD nodes can lack the CD-specific ioctl. Only a cooked
            // 2048-byte node can use positional reads: a raw CD node would
            // otherwise return framing bytes at the wrong sector offsets.
            let mut native_sector = 0u32;
            ioctl(file, 0x40046418, &mut native_sector).map_err(sense)?;
            if native_sector != 2048 {
                return Err([5, 0x20, 0]);
            }
            let (capacity, _) = geometry(file).map_err(sense)?;
            let offset = u64::from(lba) * 2048;
            if offset
                .checked_add(bytes as u64)
                .is_none_or(|end| end > capacity)
            {
                return Err([5, 0x21, 0]);
            }
            let mut data = vec![0; bytes];
            file.read_exact_at(&mut data, offset).map_err(sense)?;
            Ok(data)
        }
        value => value,
    }
}
fn msf(bytes: &[u8]) -> Result<u64, [u8; 3]> {
    if bytes[1] >= 60 || bytes[2] >= 75 {
        return Err([5, 0x24, 0]);
    }
    Ok((u64::from(bytes[0]) * 60 + u64::from(bytes[1])) * 75 + u64::from(bytes[2]))
}
fn read_cd(file: &File, cdb: &[u8], capacity: usize) -> Result<Vec<u8>, [u8; 3]> {
    let expected = (cdb[1] >> 2) & 7;
    let c2 = (cdb[9] >> 1) & 3;
    let sub = cdb[10] & 7;
    // BSD exposes 294 C2 bytes and raw P-W or formatted Q. MMC block-error
    // bytes can be derived from C2, but formatted R-W is unavailable here.
    if expected > 5
        || cdb[1] & 0xe3 != 0
        || cdb[9] & 1 != 0
        || cdb[10] & 0xf8 != 0
        || c2 > 2
        || !matches!(sub, 0..=2)
    {
        return Err([5, 0x24, 0]);
    }
    let (lba, count) = if cdb[0] == 0xb9 {
        let start = msf(&cdb[3..6])?;
        let end = msf(&cdb[6..9])?;
        (
            start.checked_sub(150).ok_or([5, 0x21, 0])?,
            end.checked_sub(start).ok_or([5, 0x24, 0])?,
        )
    } else {
        (
            u64::from(u32::from_be_bytes(cdb[2..6].try_into().unwrap())),
            u64::from(u32::from_be_bytes([0, cdb[6], cdb[7], cdb[8]])),
        )
    };
    if count.checked_mul(2744) != Some(capacity as u64) || capacity > MAX_TRANSFER - 30 {
        return Err([5, 0x24, 0]);
    }
    if count == 0 || cdb[9] & 0xfe == 0 && sub == 0 {
        return Ok(vec![]);
    }
    let stride = 2352
        + if c2 != 0 { 294 } else { 0 }
        + match sub {
            1 => 96,
            2 => 16,
            _ => 0,
        };
    let area = 0xf8
        | if c2 != 0 { 2 } else { 0 }
        | match sub {
            1 => 1,
            2 => 4,
            _ => 0,
        };
    let raw = read_native(file, lba, count as usize, stride, area, expected)?;
    let mut data = Vec::with_capacity(raw.len());
    for frame in raw.chunks_exact(stride) {
        let kind = if expected == 1 {
            1
        } else if frame[..12] == [0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0] {
            match frame[15] {
                1 => 2,
                2 if frame[16..20] != frame[20..24] => 3,
                2 if frame[18] & 0x20 == 0 => 4,
                2 => 5,
                _ => return Err([3, 0x11, 0]),
            }
        } else {
            1
        };
        if expected != 0 && expected != kind {
            return Err([5, 0x64, 0]);
        }
        let main = cdb[9] & 0xf8;
        // MMC READ CD field combinations (table 153): EDC/ECC requires
        // user data, sync requires the header, and XA user data with a
        // header also requires its subheader. CD-DA has special semantics:
        // every defined nonzero main-channel selection returns all audio.
        if matches!(main, 0x08 | 0x80 | 0x88)
            || kind != 1
                && (main & 8 != 0 && main & 0x10 == 0
                    || main & 0x80 != 0 && main & 0x20 == 0
                    || matches!(kind, 4 | 5) && main & 0x30 == 0x30 && main & 0x40 == 0)
        {
            return Err([5, 0x24, 0]);
        }
        let mut add = |selected: bool, range: std::ops::Range<usize>| {
            if selected {
                data.extend_from_slice(&frame[range]);
            }
        };
        if kind != 1 {
            add(cdb[9] & 0x80 != 0, 0..12);
            add(cdb[9] & 0x20 != 0, 12..16);
            if matches!(kind, 4 | 5) {
                add(cdb[9] & 0x40 != 0, 16..24);
            }
        }
        let user = match kind {
            1 => 0..2352,
            2 => 16..2064,
            3 => 16..2352,
            4 => 24..2072,
            _ => 24..2352,
        };
        add(
            if kind == 1 {
                main != 0
            } else {
                main & 0x10 != 0
            },
            user,
        );
        match kind {
            2 => add(cdb[9] & 8 != 0, 2064..2352),
            4 => add(cdb[9] & 8 != 0, 2072..2352),
            _ => {}
        }
        if c2 == 2 {
            // The first byte is the OR of the actual C2 pointers, followed
            // by an undefined pad byte and the 294-byte C2 bitmap.
            data.push(frame[2352..2646].iter().fold(0, |bits, &next| bits | next));
            data.push(0);
        }
        data.extend_from_slice(&frame[2352..]);
    }
    Ok(data)
}
fn metadata(
    file: &File,
    prefix: [u8; 14],
    command: libc::c_ulong,
    bytes: usize,
) -> Result<Vec<u8>, [u8; 3]> {
    if bytes > u16::MAX as usize {
        return Err([5, 0x24, 0]);
    }
    let invalidate_key = command == 0x80186481 && prefix[0] == 0x3f;
    if bytes == 0 && !invalidate_key {
        return Ok(vec![]);
    }
    let mut data = vec![0; bytes];
    let mut request = Request {
        prefix,
        length: bytes as u16,
        buffer: if bytes == 0 {
            std::ptr::null_mut()
        } else {
            data.as_mut_ptr()
        },
    };
    ioctl(file, command, &mut request).map_err(sense)?;
    if request.length as usize > data.len() {
        return Err([4, 0x44, 0]);
    }
    data.truncate(request.length as usize);
    if matches!(command, 0x80186480 | 0x80186481) && data.len() >= 2 {
        let actual = u16::from_be_bytes([data[0], data[1]]) as usize + 2;
        data.truncate(actual);
    }
    Ok(data)
}
fn subchannel(file: &File, cdb: &[u8], bytes: usize) -> Result<Vec<u8>, [u8; 3]> {
    if cdb[2] & 0x40 == 0
        || !matches!(cdb[3], 2 | 3)
        || bytes != u16::from_be_bytes([cdb[7], cdb[8]]) as usize
    {
        return Err([5, 0x24, 0]);
    }
    if bytes == 0 {
        return Ok(vec![]);
    }
    let mut request = [0u8; 16];
    let (command, length) = if cdb[3] == 2 {
        (0xc0106462, 13)
    } else {
        if !(1..=99).contains(&cdb[6]) {
            return Err([5, 0x24, 0]);
        }
        request[13] = cdb[6];
        (0xc0106461, 12)
    };
    ioctl(file, command, &mut request).map_err(sense)?;
    // Audio status is unavailable through BSD, reported as MMC unsupported (0),
    // rather than inventing a playback state. Identifiers are actual drive data.
    let mut data = vec![0; 24];
    data[3] = 20;
    data[4] = cdb[3];
    if cdb[3] == 3 {
        data[6] = cdb[6];
    }
    if request[0] != 0 {
        data[8] = 0x80;
        data[9..9 + length].copy_from_slice(&request[..length]);
    }
    data.truncate(bytes);
    Ok(data)
}
fn dvd_toc(file: &File, cdb: &[u8], format: u8, bytes: usize) -> Result<Vec<u8>, [u8; 3]> {
    // Confirm DVD through its actual structure, never from capacity alone.
    let structure = metadata(file, [0; 14], 0x80186480, 20)?;
    if structure.len() < 20 {
        return Err([4, 0x44, 0]);
    }
    let (capacity, _) = geometry(file).map_err(sense)?;
    let blocks = capacity / 2048;
    if blocks == 0 {
        return Err([2, 0x3a, 0]);
    }
    let address = |lba: u64| -> Result<[u8; 4], [u8; 3]> {
        if cdb[1] & 2 == 0 {
            return Ok(u32::try_from(lba).map_err(|_| [5, 0x24, 0])?.to_be_bytes());
        }
        let n = lba.checked_add(150).ok_or([5, 0x24, 0])?;
        Ok([
            0,
            u8::try_from(n / 4500).map_err(|_| [5, 0x24, 0])?,
            ((n % 4500) / 75) as u8,
            (n % 75) as u8,
        ])
    };
    let mut data = vec![0, 0, 1, 1];
    if format == 1 || cdb[6] <= 1 {
        data.extend_from_slice(&[0, 0x14, 1, 0]);
        data.extend_from_slice(&address(0)?);
    } else if cdb[6] != 0xaa {
        return Err([5, 0x24, 0]);
    }
    if format == 0 {
        data.extend_from_slice(&[0, 0x14, 0xaa, 0]);
        data.extend_from_slice(&address(blocks)?);
    }
    let length = (data.len() - 2) as u16;
    data[..2].copy_from_slice(&length.to_be_bytes());
    data.truncate(bytes);
    Ok(data)
}

pub(super) fn execute(file: &File, cdb: &[u8], bytes: usize) -> Result<Vec<u8>, [u8; 3]> {
    let Some(&opcode) = cdb.first() else {
        return Err([5, 0x24, 0]);
    };
    let length = match opcode {
        0x00 | 0x08 => 6,
        0xa8 | 0xbe | 0xb9 | 0xad | 0xa4 | 0xbb => 12,
        _ => 10,
    };
    if cdb.len() < length || bytes > MAX_TRANSFER - 30 {
        return Err([5, 0x24, 0]);
    }
    match opcode {
        0x08 | 0x28 | 0xa8 => return read_data(file, cdb, bytes),
        0xbe | 0xb9 => return read_cd(file, cdb, bytes),
        0x00 | 0x25 => {
            let (capacity, sector) = geometry(file).map_err(sense)?;
            if capacity == 0 {
                return Err([2, 0x3a, 0]);
            }
            if opcode == 0x00 {
                return Ok(vec![]);
            }
            let mut data = ((capacity / u64::from(sector) - 1).min(u64::from(u32::MAX)) as u32)
                .to_be_bytes()
                .to_vec();
            data.extend_from_slice(&sector.to_be_bytes());
            return Ok(data);
        }
        0x42 => return subchannel(file, cdb, bytes),
        _ => {}
    }
    if cdb[0] == 0xbb {
        if cdb.len() != 12 || cdb[1] & 3 != 0 || cdb[4..6] != [0xff, 0xff] {
            return Err([5, 0x24, 0]);
        }
        let mut speed = u16::from_be_bytes([cdb[2], cdb[3]]);
        // DKIOCCDSETSPEED uses kB/s, the same unit as MMC SET CD SPEED.
        if let Err(error) = ioctl(file, 0x80026463, &mut speed) {
            let code = sense(error);
            if code != [5, 0x20, 0] {
                return Err(code);
            }
            ioctl(file, 0x80026483, &mut speed).map_err(sense)?;
        }
        return Ok(vec![]);
    }
    let mut prefix = [0; 14];
    let command = match cdb[0] {
        0x43 => {
            let format = if cdb[2] & 15 == 0 {
                cdb[9] >> 6
            } else {
                cdb[2] & 15
            };
            if format > 5 {
                return Err([5, 0x24, 0]);
            }
            prefix[0] = format;
            prefix[1] = (cdb[1] >> 1) & 1;
            prefix[7] = cdb[6];
            0xc0186464
        }
        0x51 => {
            // The BSD interface exposes standard disc information (data type 0).
            if cdb[1] & 7 != 0 {
                return Err([5, 0x24, 0]);
            }
            0xc0186465
        }
        0x52 => {
            let address_type = cdb[1] & 3;
            if address_type > 2 || cdb[1] & 4 != 0 {
                return Err([5, 0x24, 0]);
            }
            prefix[4..8]
                .copy_from_slice(&u32::from_be_bytes(cdb[2..6].try_into().unwrap()).to_ne_bytes());
            prefix[8] = address_type;
            0xc0186466
        }
        0xad => {
            if cdb[1] & 15 != 0 || cdb[10] & 63 != 0 {
                return Err([5, 0x24, 0]);
            }
            prefix[0] = cdb[7];
            prefix[4..8]
                .copy_from_slice(&u32::from_be_bytes(cdb[2..6].try_into().unwrap()).to_ne_bytes());
            prefix[8] = cdb[10] >> 6;
            prefix[9] = cdb[6];
            0x80186480
        }
        0xa4 => {
            prefix[0] = cdb[10] & 63;
            prefix[1] = cdb[7];
            prefix[2] = cdb[6];
            prefix[4..8]
                .copy_from_slice(&u32::from_be_bytes(cdb[2..6].try_into().unwrap()).to_ne_bytes());
            prefix[8] = cdb[10] >> 6;
            0x80186481
        }
        _ => return Err([5, 0x20, 0]),
    };
    let at = if matches!(opcode, 0xad | 0xa4) { 8 } else { 7 };
    if bytes != u16::from_be_bytes([cdb[at], cdb[at + 1]]) as usize {
        return Err([5, 0x24, 0]);
    }
    match metadata(file, prefix, command, bytes) {
        Err([5, 0x20, 0]) if opcode == 0x43 && prefix[0] <= 1 => {
            dvd_toc(file, cdb, prefix[0], bytes)
        }
        Err([5, 0x20, 0]) if matches!(opcode, 0x51 | 0x52) => metadata(
            file,
            prefix,
            if opcode == 0x51 {
                0xc0186484
            } else {
                0xc0186485
            },
            bytes,
        ),
        value => value,
    }
}
