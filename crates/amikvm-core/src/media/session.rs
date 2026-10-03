use super::Packet;
use crate::{Result, error::MediaSessionError};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acknowledgement {
    pub instance: u8,
    pub boost: bool,
}

fn owner(packet: &Packet) -> Option<IpAddr> {
    // IUSBSCSI.readData uses exactly 45 bytes at body[31], not the reply tail.
    let field = packet.body.get(31..76)?;
    let value = field.split(|byte| *byte == 0).next()?;
    std::str::from_utf8(value).ok()?.trim().parse().ok()
}

pub fn acknowledge(packet: &Packet, cd: bool) -> Result<Acknowledgement> {
    let opcode = packet.body.get(9).copied();
    if opcode != Some(241) {
        return Err(MediaSessionError::UnexpectedAcknowledgement { opcode }.into());
    }
    let code = packet
        .body
        .get(30)
        .copied()
        .ok_or(MediaSessionError::MissingStatus)?;
    if code == 1 || cd && matches!(code, 27 | 28) {
        return Ok(Acknowledgement {
            instance: packet.instance(),
            boost: cd && code == 27,
        });
    }
    let reason = match code {
        3 => MediaSessionError::InvalidToken,
        5 => MediaSessionError::NoPrivilege,
        8 => MediaSessionError::SessionLimit {
            cd,
            owner: if cd { None } else { owner(packet) },
        },
        13 => MediaSessionError::LicenseRequired,
        _ => match owner(packet) {
            Some(ip)
                if ip == IpAddr::V4(Ipv4Addr::LOCALHOST)
                    || ip == IpAddr::V6(Ipv6Addr::LOCALHOST) =>
            {
                MediaSessionError::OccupiedLocal { code }
            }
            Some(owner) => MediaSessionError::Occupied { code, owner },
            None => MediaSessionError::Rejected { code },
        },
    };
    Err(reason.into())
}
