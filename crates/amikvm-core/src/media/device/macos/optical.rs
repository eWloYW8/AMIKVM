//! Darwin IOCDMediaBSDClient translates these ioctls to optical drive commands.
//! The kernel returns the actual MMC data, including multiple tracks/sessions.
use std::{fs::File, io, os::fd::AsRawFd};

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

pub(super) fn execute(file: &File, cdb: &[u8], bytes: usize) -> Result<Vec<u8>, [u8; 3]> {
    if cdb.len() < 10 || bytes > u16::MAX as usize {
        return Err([5, 0x24, 0]);
    }
    if cdb[0] == 0xbb {
        if cdb.len() != 12 || cdb[1] & 3 != 0 || cdb[4..6] != [0xff, 0xff] {
            return Err([5, 0x24, 0]);
        }
        let speed = u16::from_be_bytes([cdb[2], cdb[3]]);
        // DKIOCCDSETSPEED uses kB/s, the same unit as MMC SET CD SPEED.
        if unsafe { libc::ioctl(file.as_raw_fd(), 0x80026463, &speed) } < 0 {
            return Err(sense(io::Error::last_os_error()));
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
        _ => return Err([5, 0x20, 0]),
    };
    if bytes != u16::from_be_bytes([cdb[7], cdb[8]]) as usize {
        return Err([5, 0x24, 0]);
    }
    if bytes == 0 {
        return Ok(vec![]);
    }
    let mut data = vec![0; bytes];
    let mut request = Request {
        prefix,
        length: bytes as u16,
        buffer: data.as_mut_ptr(),
    };
    // The request and buffer remain live and unaliased throughout the syscall.
    if unsafe { libc::ioctl(file.as_raw_fd(), command, &mut request) } < 0 {
        return Err(sense(io::Error::last_os_error()));
    }
    if usize::from(request.length) > data.len() {
        return Err([4, 0x44, 0]);
    }
    data.truncate(request.length as usize);
    Ok(data)
}
