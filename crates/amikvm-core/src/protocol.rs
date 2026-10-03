//! IVTP framing and IUSB input reports. Source: e/j.java, c/j.java and c/l.java.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const HEADER_LEN: usize = 8;
pub const MAX_PACKET: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Header {
    pub kind: u16,
    pub length: u32,
    pub status: u16,
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Protocol("Truncated IVTP header".into()));
        }
        let header = Self {
            kind: u16::from_le_bytes([bytes[0], bytes[1]]),
            length: u32::from_le_bytes(bytes[2..6].try_into().unwrap()),
            status: u16::from_le_bytes([bytes[6], bytes[7]]),
        };
        if header.length as usize > MAX_PACKET {
            return Err(Error::Protocol("IVTP payload exceeds limit".into()));
        }
        Ok(header)
    }

    pub fn bytes(self) -> [u8; HEADER_LEN] {
        let mut bytes = [0; HEADER_LEN];
        bytes[..2].copy_from_slice(&self.kind.to_le_bytes());
        bytes[2..6].copy_from_slice(&self.length.to_le_bytes());
        bytes[6..].copy_from_slice(&self.status.to_le_bytes());
        bytes
    }
}

pub fn packet(kind: u16, status: u16, body: &[u8]) -> Result<Vec<u8>> {
    if body.len() > MAX_PACKET {
        return Err(Error::Invalid("Packet payload exceeds limit".into()));
    }
    let mut bytes = Vec::with_capacity(HEADER_LEN + body.len());
    bytes.extend(
        Header {
            kind,
            length: body.len() as u32,
            status,
        }
        .bytes(),
    );
    bytes.extend_from_slice(body);
    Ok(bytes)
}

pub fn command(kind: u16, status: u16) -> Vec<u8> {
    Header {
        kind,
        length: 0,
        status,
    }
    .bytes()
    .to_vec()
}

/// JViewer hX() advertises 446 but sends 446 bytes *including* the header.
/// This authentication-specific convention was checked against JAR bytecode.
pub fn authenticate(
    token: &str,
    local_ip: &str,
    local_name: &str,
    mac: &str,
    bmc_host: &str,
) -> Result<Vec<u8>> {
    let mut bytes = vec![0; 446];
    bytes[..8].copy_from_slice(
        &Header {
            kind: 18,
            length: 446,
            status: 0,
        }
        .bytes(),
    );
    for (start, end, value) in [
        (9, 138, token),
        (138, 203, local_ip),
        (203, 332, local_name),
        (332, 381, mac),
        (381, 446, bmc_host),
    ] {
        if value.len() >= end - start {
            return Err(Error::Invalid("Session identity field is too long".into()));
        }
        bytes[start..start + value.len()].copy_from_slice(value.as_bytes());
    }
    Ok(bytes)
}

/// JViewer hY(): 374-byte body, with a 130-byte token field and the old session ID.
pub fn reconnect(
    token: &str,
    local_ip: &str,
    local_name: &str,
    mac: &str,
    session_id: u8,
) -> Result<Vec<u8>> {
    let mut bytes = vec![0; 382];
    bytes[..8].copy_from_slice(
        &Header {
            kind: 58,
            length: 374,
            status: 0,
        }
        .bytes(),
    );
    for (start, end, value) in [
        (8, 138, token),
        (138, 203, local_ip),
        (203, 332, local_name),
        (332, 381, mac),
    ] {
        if value.len() >= end - start || value.contains('\0') {
            return Err(Error::Invalid(
                "Session identity field is invalid or too long".into(),
            ));
        }
        bytes[start..start + value.len()].copy_from_slice(value.as_bytes());
    }
    bytes[381] = session_id;
    Ok(bytes)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Control {
    Pause,
    Resume,
    Refresh,
    Power { operation: PowerOperation },
    PowerStatus,
    MouseMode { mode: u16 },
    Bandwidth { bytes_per_second: u32 },
    DetectBandwidth,
    KeyboardLayout { layout: String },
    InputEncryption { enabled: bool },
    LockLeds { leds: u8 },
    HostDisplay { locked: bool },
    ActiveUsers,
    RequestControl,
    Ipmi { command: Vec<u8>, request_id: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerOperation {
    Off,
    On,
    Cycle,
    Reset,
    Shutdown,
}

impl Control {
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Pause => Ok(command(4, 0)),
            Self::Resume => Ok(command(6, 0)),
            Self::Refresh => {
                let mut bytes = command(4, 0);
                bytes.extend(command(6, 0));
                bytes.extend(command(20, 0));
                Ok(bytes)
            }
            Self::Power { operation } => Ok(command(
                35,
                match operation {
                    PowerOperation::Off => 0,
                    PowerOperation::On => 1,
                    PowerOperation::Cycle => 2,
                    PowerOperation::Reset => 3,
                    PowerOperation::Shutdown => 5,
                },
            )),
            Self::PowerStatus => Ok(command(34, 0)),
            Self::MouseMode { mode } if (1..=3).contains(mode) => Ok(command(28, *mode)),
            Self::MouseMode { .. } => Err(Error::Invalid("Invalid mouse mode".into())),
            Self::Bandwidth { bytes_per_second } => packet(2, 0, &bytes_per_second.to_le_bytes()),
            Self::DetectBandwidth => packet(16, 0, &13_107_200_u32.to_le_bytes()),
            Self::KeyboardLayout { layout }
                if layout == "AD"
                    || crate::input::layout::Layout::parse(layout).is_ok_and(|l| l.physical()) =>
            {
                packet(55, 0, layout.as_bytes())
            }
            Self::KeyboardLayout { .. } => Err(Error::Invalid("Invalid keyboard layout".into())),
            Self::InputEncryption { enabled } => Ok(command(if *enabled { 12 } else { 13 }, 0)),
            Self::LockLeds { .. } => Err(Error::Invalid(
                "Use HID lock-key events to change remote LEDs".into(),
            )),
            Self::HostDisplay { locked } => packet(51, 0, &[u8::from(*locked)]),
            Self::ActiveUsers => Ok(command(39, 0)),
            Self::RequestControl => Ok(command(50, 0)),
            Self::Ipmi {
                command,
                request_id,
            } => {
                if command.len() < 2 || command.len() > 1024 {
                    return Err(Error::Invalid(
                        "IPMI command must contain 2–1024 bytes".into(),
                    ));
                }
                let mut body = vec![*request_id];
                body.extend_from_slice(command);
                packet(48, 0, &body)
            }
        }
    }
}

/// IUSB has a 32-byte header with checksum over that header only.
pub fn input_report(sequence: u32, mouse: bool, report: &[u8]) -> Result<Vec<u8>> {
    input_report_with_cipher(sequence, mouse, report, None)
}

pub fn input_report_with_cipher(
    sequence: u32,
    mouse: bool,
    report: &[u8],
    cipher: Option<&crate::input::encryption::Cipher>,
) -> Result<Vec<u8>> {
    if (mouse && report.len() != 4 && report.len() != 6) || (!mouse && report.len() != 8) {
        return Err(Error::Invalid("Invalid HID report length".into()));
    }
    let mut body = vec![0_u8; 32];
    body[..8].copy_from_slice(b"IUSB    ");
    body[8] = 1;
    body[10] = 32;
    body[12..16].copy_from_slice(&((report.len() + 1) as u32).to_le_bytes());
    body[17] = if mouse { 49 } else { 48 };
    body[18] = if mouse { 32 } else { 16 };
    body[19] = 128;
    body[20] = 2;
    body[21] = u8::from(mouse);
    body[24..28].copy_from_slice(&sequence.to_le_bytes());
    body[11] = body
        .iter()
        .fold(0_u8, |sum, b| sum.wrapping_add(*b))
        .wrapping_neg();
    body.push(if mouse { report.len() as u8 } else { 8 });
    if let Some(cipher) = cipher {
        body.extend_from_slice(&cipher.encrypt_report(report)?);
    } else {
        body.extend_from_slice(report);
    }
    packet(1, if cipher.is_some() { 255 } else { 0 }, &body)
}

pub fn absolute_mouse(
    buttons: u8,
    x: f64,
    y: f64,
    width: u32,
    height: u32,
    wheel: i8,
) -> Result<[u8; 6]> {
    if width == 0 || height == 0 || !x.is_finite() || !y.is_finite() {
        return Err(Error::Invalid("Invalid mouse coordinates".into()));
    }
    let x = (x.clamp(0.0, width as f64) * 32767.0 / width as f64).round() as u16;
    let y = (y.clamp(0.0, height as f64) * 32767.0 / height as f64).round() as u16;
    Ok([
        buttons & 7,
        x as u8,
        (x >> 8) as u8,
        y as u8,
        (y >> 8) as u8,
        wheel as u8,
    ])
}

pub struct Fragments {
    data: Vec<u8>,
    next: u16,
}

impl Default for Fragments {
    fn default() -> Self {
        Self {
            data: Vec::new(),
            next: 0,
        }
    }
}

impl Fragments {
    pub fn push(&mut self, body: &[u8]) -> Result<Option<Vec<u8>>> {
        if body.len() < 2 {
            return Err(Error::Protocol("Truncated video fragment".into()));
        }
        let number = u16::from_le_bytes([body[0], body[1]]);
        let index = number & 0x7fff;
        if index == 0 {
            self.data.clear();
            self.next = 0;
        }
        if index != self.next {
            self.data.clear();
            self.next = 0;
            return Err(Error::Protocol("Video fragment sequence gap".into()));
        }
        if self.data.len() + body.len() - 2 > MAX_PACKET {
            return Err(Error::Protocol("Video frame exceeds limit".into()));
        }
        self.data.extend_from_slice(&body[2..]);
        self.next = self.next.wrapping_add(1);
        if number & 0x8000 != 0 {
            self.next = 0;
            Ok(Some(std::mem::take(&mut self.data)))
        } else {
            Ok(None)
        }
    }
}
