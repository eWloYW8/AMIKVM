//! Rejection handling traced to SinglePortKVM.ao/kO in the original JAR.
use crate::{Error, Result, error::SinglePortRejection};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    time::timeout,
};

const MAX_CONFIRMATION: usize = 512;
const MAX_HTTP_CONFIRMATION: usize = 16 * 1024;

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

/// Return whether the gateway used an HTTP response, possibly C-string terminated.
pub(super) async fn confirmation<R: AsyncRead + Unpin>(stream: &mut R) -> Result<bool> {
    let mut response = Vec::new();
    let result = timeout(Duration::from_secs(10), async {
        let mut http = false;
        let mut line_start = 0;
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
                let line = &response[line_start..];
                if line_start == 0 && line.starts_with(b"HTTP/") {
                    let status = line
                        .split(u8::is_ascii_whitespace)
                        .filter(|part| !part.is_empty())
                        .nth(1)
                        .filter(|code| code.len() == 3 && code.iter().all(u8::is_ascii_digit))
                        .map(|code| {
                            u16::from(code[0] - b'0') * 100
                                + u16::from(code[1] - b'0') * 10
                                + u16::from(code[2] - b'0')
                        })
                        .ok_or_else(|| Error::Protocol("Invalid single-port HTTP status".into()))?;
                    if !(200..300).contains(&status) {
                        return Err(Error::Protocol(format!(
                            "Single-port gateway returned HTTP {status}"
                        )));
                    }
                    http = true;
                } else if !http || matches!(line, b"\r\n" | b"\n") {
                    // Legacy gateways use a single LF. HTTP gateways end their
                    // headers with an empty line. Read neither binary IVTP bytes
                    // nor only the status line, leaving a CRLF as a false header.
                    return Ok(http);
                }
                line_start = response.len();
            }
            if response.len()
                == if http {
                    MAX_HTTP_CONFIRMATION
                } else {
                    MAX_CONFIRMATION
                }
            {
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

/// Some AMI gateways append a NUL to their HTTP header block. Consume that
/// optional terminator on the first protocol read, preserving a non-NUL byte.
/// This also works when TLS delivers the terminator in a separate read and does
/// not wait for protocol data before the caller can send its authentication.
pub(super) struct HttpStream {
    pub stream: super::Socket,
    pub first_read: bool,
}

impl AsyncRead for HttpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if this.first_read {
            let mut first = [0];
            let mut first_buf = ReadBuf::new(&mut first);
            match this.stream.as_mut().poll_read(cx, &mut first_buf) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {}
            }
            this.first_read = false;
            if first_buf.filled().is_empty() {
                return Poll::Ready(Ok(()));
            }
            if first[0] != 0 {
                buf.put_slice(&first);
                return Poll::Ready(Ok(()));
            }
        }
        this.stream.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for HttpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.get_mut().stream.as_mut().poll_write(cx, bytes)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().stream.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().stream.as_mut().poll_shutdown(cx)
    }
}
