//! JViewer KMCrypt uses token bytes, Blowfish ECB and zero-filled report blocks.
use crate::{Error, Result};
use blowfish::{
    Blowfish,
    cipher::{Block, BlockCipherEncrypt, KeyInit, zeroize::Zeroizing},
};

pub struct Cipher(Blowfish);

impl Cipher {
    pub fn from_token(token: &str) -> Result<Self> {
        if token.is_empty() || token.len() > 56 {
            return Err(Error::Invalid(
                "当前 KVM token 无法初始化原版输入加密".into(),
            ));
        }
        // Original KMCrypt pads short tokens to 16 bytes and keeps longer tokens whole.
        let mut key = Zeroizing::new(token.as_bytes().to_vec());
        if key.len() < 16 {
            key.resize(16, 0);
        }
        Blowfish::new_from_slice(&key)
            .map(Self)
            .map_err(|_| Error::Invalid("无法初始化键盘和鼠标加密".into()))
    }

    pub fn encrypt_report(&self, report: &[u8]) -> Result<[u8; 8]> {
        if ![4, 6, 8].contains(&report.len()) {
            return Err(Error::Invalid("Invalid HID report length".into()));
        }
        let mut block = Block::<Blowfish>::default();
        block[..report.len()].copy_from_slice(report);
        self.0.encrypt_block(&mut block);
        Ok(block.into())
    }
}
