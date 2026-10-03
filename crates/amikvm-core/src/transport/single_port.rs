//! Rejection handling traced to SinglePortKVM.ao/kO in the original JAR.
use crate::{Error, Result, error::SinglePortRejection};
use std::time::Duration;
use tokio::{io::AsyncReadExt, time::timeout};

const MAX_CONFIRMATION: usize = 512;

fn rejection(response: &[u8], complete: bool) -> Option<SinglePortRejection> {
    let offset = response.windows(5).position(|bytes| bytes == b"ERROR")?;
    let code = complete
        .then(|| {
            let rest = response[offset + 5..].strip_prefix(b":")?;
            // ao stops at a literal backslash, not at a Java newline escape.
            let end = rest
                .iter()
                .position(|byte| matches!(byte, b'\\' | b'\r' | b'\n'))
                .unwrap_or(rest.len());
            let digits = rest[..end].trim_ascii();
            if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            digits.iter().try_fold(0u16, |code, digit| {
                code.checked_mul(10)?.checked_add(u16::from(digit - b'0'))
            })
        })
        .flatten();
    Some(SinglePortRejection { code })
}

pub(super) async fn confirmation<R: tokio::io::AsyncRead + Unpin>(stream: &mut R) -> Result<()> {
    let mut response = Vec::new();
    let result = timeout(Duration::from_secs(10), async {
        loop {
            let byte = match stream.read_u8().await {
                Ok(byte) => byte,
                Err(io_error) => {
                    // A closed rejection remains terminal. Do not retry a web
                    // session merely because its gateway closed the socket.
                    // EOF completes a numeric code; other IO errors do not.
                    let complete = io_error.kind() == std::io::ErrorKind::UnexpectedEof;
                    if let Some(error) = rejection(&response, complete) {
                        return Err(error.into());
                    }
                    return Err(io_error.into());
                }
            };
            response.push(byte);
            if byte == b'\n' || byte == b'\\' && rejection(&response, false).is_some() {
                if let Some(error) = rejection(&response, true) {
                    return Err(error.into());
                }
                // Success currently uses a single LF. The original kO used
                // InputStream.available(); its full success framing is still
                // unresolved. Do not read ahead into a following binary packet.
                return Ok(());
            }
            if response.len() == MAX_CONFIRMATION {
                if let Some(error) = rejection(&response, false) {
                    return Err(error.into());
                }
                return Err(Error::Protocol(
                    "Single-port confirmation exceeds limit".into(),
                ));
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| {
        Err(rejection(&response, false)
            .map(Error::from)
            .unwrap_or(Error::Timeout("Single-port handshake")))
    })
}
