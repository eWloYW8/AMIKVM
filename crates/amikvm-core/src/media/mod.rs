//! IUSB media packets and native block-image SCSI servicing.
pub mod cache;
pub mod device;
pub mod folder;
mod nrg;
pub mod redirect;
pub mod scsi;
pub mod session;
use crate::{Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const MAX_TRANSFER: usize = 16 * 1024 * 1024;

pub struct Packet {
    pub header: [u8; 32],
    pub body: Vec<u8>,
}
impl Packet {
    pub fn opcode(&self) -> Result<u8> {
        self.body
            .get(9)
            .copied()
            .ok_or_else(|| Error::Protocol("Truncated IUSB SCSI command".into()))
    }
    pub fn instance(&self) -> u8 {
        self.header[23]
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.body.len() > MAX_TRANSFER {
            return Err(Error::Invalid("Media packet exceeds transfer limit".into()));
        }
        let mut header = self.header;
        header[11] = 0;
        header[12..16].copy_from_slice(&(self.body.len() as u32).to_le_bytes());
        // JViewer's IUSBSCSI.writePacket rewrites the checksum after the payload.
        header[11] = header
            .iter()
            .chain(self.body.iter())
            .fold(0_u8, |sum, b| sum.wrapping_add(*b))
            .wrapping_neg();
        let mut bytes = Vec::with_capacity(32 + self.body.len());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&self.body);
        Ok(bytes)
    }
    pub fn command(opcode: u8, instance: u8, data: &[u8]) -> Self {
        let mut header = [0; 32];
        header[..8].copy_from_slice(b"IUSB    ");
        header[8] = 1;
        header[10] = 32;
        header[17] = 5;
        header[18] = 1;
        header[19] = 128;
        header[23] = instance;
        let mut body = vec![0; 30];
        body[9] = opcode;
        body.extend_from_slice(data);
        Self { header, body }
    }
    pub fn authenticate(
        token: &str,
        instance: u8,
        hard_disk: bool,
        usb: bool,
        boost: bool,
    ) -> Result<Self> {
        if token.len() > 95 {
            return Err(Error::Invalid(
                "Media session token exceeds protocol limit".into(),
            ));
        }
        let mut packet = Self::command(242, instance, &[]);
        packet.body.resize(128, 0);
        packet.body[29] = u8::from(boost);
        // Authentication data begins at absolute byte 62, after the reserved byte at 61.
        packet.body[30] = 0;
        packet.body[31..31 + token.len()].copy_from_slice(token.as_bytes());
        if hard_disk {
            packet.header[22] = if usb { 128 } else { 0 };
        }
        Ok(packet)
    }
    pub fn device_info(name: &str, instance: u8) -> Result<Self> {
        if name.len() > 255 {
            return Err(Error::Invalid("Media name exceeds 255 bytes".into()));
        }
        let mut data = vec![0; 260];
        data[..4].copy_from_slice(&2_u32.to_le_bytes());
        data[4..4 + name.len()].copy_from_slice(name.as_bytes());
        Ok(Self::command(248, instance, &data))
    }
}

pub async fn read<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Packet> {
    let mut header = [0; 32];
    stream.read_exact(&mut header).await?;
    // PacketMaster.receivePacket always reads a fixed 32-byte media header,
    // and IUSBHeader.read validates only its signature. Some BMCs leave the
    // version and header-length fields zero, including in successful ACKs.
    if &header[..8] != b"IUSB    " {
        return Err(Error::Protocol("Invalid IUSB media header".into()));
    }
    let length = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
    if length < 29 || length > MAX_TRANSFER {
        return Err(Error::Protocol("Invalid IUSB media packet length".into()));
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    Ok(Packet { header, body })
}
