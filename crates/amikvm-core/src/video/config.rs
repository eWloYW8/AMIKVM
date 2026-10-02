//! AST configuration command 4099/4100, from SOCApp and VideoEngineConfigs.
use crate::{Error, Result, protocol};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct EngineConfig {
    #[serde(skip)]
    bytes: [u8; 8],
    pub compression: u8,
    pub quality: u8,
}

pub enum Setting {
    Compression(u8),
    Quality(u8),
}

/// IVTP 52 states 2/3 retain visibility but disable changes, as in JViewer.
pub fn host_display_available(status: Option<u16>, configured: Option<bool>) -> bool {
    configured != Some(false) && matches!(status, Some(0 | 1))
}

impl EngineConfig {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 8 || bytes[6] > 3 || bytes[2] > 7 {
            return Err(Error::Protocol("Invalid AST engine configuration".into()));
        }
        Ok(Self {
            bytes: bytes.try_into().unwrap(),
            compression: bytes[6],
            quality: bytes[2],
        })
    }
    pub fn change(self, setting: Setting, host_display: Option<u16>) -> Result<Self> {
        let mut bytes = self.bytes;
        match setting {
            Setting::Compression(mode) if mode <= 3 => bytes[6] = mode,
            Setting::Quality(quality) if quality <= 7 => bytes[2] = quality,
            _ => {
                return Err(Error::Invalid(
                    "Invalid AST quality or compression mode".into(),
                ));
            }
        }
        // SOCApp keeps host output consistent with IVTP 52 when sending 4100.
        // Unknown/disabled states preserve the BMC's existing output byte.
        match host_display {
            Some(0) => bytes[7] = 1,
            Some(1) => bytes[7] = 0,
            _ => {}
        }
        Self::parse(&bytes)
    }
    pub fn packet(self) -> Result<Vec<u8>> {
        protocol::packet(4100, 0, &self.bytes)
    }
}
