//! KVMClient 19/22/23 and JVAPP validation/duplicate-client handling.
use crate::{Result, error::VideoSessionError};

/// The original accepts both old one-byte replies and a second session-ID byte.
/// OEM trailing fields are ignored, just as KVMClient ignores them.
pub fn validation(body: &[u8], requested: bool) -> Result<Option<u8>> {
    let code = body
        .first()
        .copied()
        .ok_or(VideoSessionError::MissingValidationStatus)?;
    if code != 1 {
        return Err(VideoSessionError::Validation(code).into());
    }
    if !requested {
        return Err(VideoSessionError::UnexpectedValidation.into());
    }
    Ok(body.get(1).copied())
}

/// The normal transport advertises existing clients as fixed 48-byte MAC slots.
/// SinglePortKVM skips the local duplicate check in the original viewer.
pub fn hello(status: u16, body: &[u8], single_port: bool, local_macs: &[[u8; 6]]) -> Result<bool> {
    if status != 0 && status != 2 {
        return Err(VideoSessionError::UnsupportedSoc(status).into());
    }
    let first = body.is_empty();
    if first || single_port {
        return Ok(first);
    }
    // KVMClient uses bodyLength / 48 and inspects complete MAC slots only.
    // A trailing OEM extension does not invalidate the client list.
    if body
        .chunks_exact(48)
        .any(|slot| mac(slot).is_some_and(|mac| mac != [0; 6] && local_macs.contains(&mac)))
    {
        return Err(VideoSessionError::SessionLimit(1).into());
    }
    Ok(false)
}

fn mac(bytes: &[u8]) -> Option<[u8; 6]> {
    // Java String.trim removes NUL padding and characters <= ASCII space.
    let start = bytes.iter().position(|byte| *byte > b' ')?;
    let end = bytes.iter().rposition(|byte| *byte > b' ')? + 1;
    let bytes = &bytes[start..end];
    if bytes.len() != 17 || !matches!(bytes[2], b'-' | b':') {
        return None;
    }
    let mut result = [0; 6];
    for (i, pair) in bytes.split(|byte| *byte == bytes[2]).enumerate() {
        if pair.len() != 2 || i >= result.len() {
            return None;
        }
        let hex = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        };
        result[i] = hex(pair[0])? << 4 | hex(pair[1])?;
    }
    Some(result)
}
