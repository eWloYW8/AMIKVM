use super::{MAX_TRANSFER, Packet};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Cdrom,
    HardDisk,
    Floppy,
}

pub struct Image {
    file: File,
    pub kind: Kind,
    pub readonly: bool,
    pub block_size: u32,
    pub blocks: u64,
    tracks: Vec<super::nrg::Track>,
    ejected: bool,
    prevent: bool,
    attention: bool,
    sense: [u8; 3],
    cache: Option<super::cache::ReadAhead>,
    cache_attempted: bool,
    physical: Option<super::device::Device>,
    _device_locks: Vec<File>,
    removed: bool,
    _device_lease: Option<super::device::Lease>,
}
impl Image {
    pub fn open(path: &Path, kind: Kind, readonly: bool) -> Result<Self> {
        let readonly = readonly || kind == Kind::Cdrom;
        let mut file = OpenOptions::new().read(true).write(!readonly).open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid(
                "镜像入口只接受普通文件，请从实体设备列表选择设备".into(),
            ));
        }
        if readonly {
            fs2::FileExt::try_lock_shared(&file)
        } else {
            fs2::FileExt::try_lock_exclusive(&file)
        }
        .map_err(|error| Error::Invalid(format!("Could not lock media image: {error}")))?;
        let length = file.metadata()?.len();
        let block_size = if kind == Kind::Cdrom { 2048 } else { 512 };
        let nrg = kind == Kind::Cdrom
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("nrg"));
        if !nrg && (length < block_size as u64 || length % block_size as u64 != 0) {
            return Err(Error::Invalid(
                "Image size must be a positive multiple of its sector size".into(),
            ));
        }
        let tracks = if nrg {
            super::nrg::tracks(&mut file)?
        } else if kind == Kind::Cdrom {
            vec![super::nrg::Track {
                number: 1,
                lba: 0,
                blocks: length / 2048,
                offset: 0,
                sector_size: 2048,
                data_offset: Some(0),
            }]
        } else {
            vec![]
        };
        let blocks = tracks
            .last()
            .map_or(length / block_size as u64, |t| t.lba + t.blocks);
        Ok(Self {
            file,
            kind,
            readonly,
            block_size,
            blocks,
            tracks,
            ejected: false,
            prevent: false,
            attention: true,
            sense: [0; 3],
            cache: None,
            cache_attempted: false,
            physical: None,
            _device_locks: vec![],
            removed: false,
            _device_lease: None,
        })
    }
    pub fn open_device(
        expected: &super::device::Device,
        kind: Kind,
        readonly: bool,
    ) -> Result<Self> {
        let opened = super::device::open(expected, kind, readonly || kind == Kind::Cdrom)?;
        let blocks = opened.device.capacity / u64::from(opened.device.block_size);
        let tracks = if kind == Kind::Cdrom {
            vec![super::nrg::Track {
                number: 1,
                lba: 0,
                blocks,
                offset: 0,
                sector_size: opened.device.block_size.try_into().unwrap_or(2048),
                data_offset: Some(0),
            }]
        } else {
            vec![]
        };
        Ok(Self {
            file: opened.file,
            kind,
            readonly: readonly || kind == Kind::Cdrom,
            block_size: opened.device.block_size,
            blocks,
            tracks,
            ejected: false,
            prevent: false,
            attention: opened.changed || kind != Kind::Cdrom,
            sense: [0; 3],
            cache: None,
            cache_attempted: true,
            physical: Some(opened.device),
            _device_locks: opened.locks,
            removed: false,
            _device_lease: Some(opened.lease),
        })
    }
    pub fn physical(&self) -> bool {
        self.physical.is_some()
    }
    /// Called only from a blocking worker, including while the BMC is idle.
    pub fn poll_device(&mut self) -> Option<u64> {
        let Some(device) = self.physical.as_ref() else {
            return Some(self.blocks * u64::from(self.block_size));
        };
        if !super::device::present(&self.file, device) {
            self.removed = true;
            return None;
        }
        if let Ok((length, sector, changed)) = super::device::geometry(&self.file, self.kind) {
            if (512..=65536).contains(&sector)
                && sector.is_power_of_two()
                && length % u64::from(sector) == 0
            {
                if length == 0 && self.kind != Kind::Cdrom {
                    self.removed = true;
                    return None;
                }
                if changed {
                    self.attention = true;
                }
                if self.blocks * u64::from(self.block_size) != length || self.block_size != sector {
                    self.blocks = length / u64::from(sector);
                    self.block_size = sector;
                    self.attention = true;
                    if let Some(track) = self.tracks.first_mut() {
                        track.blocks = self.blocks;
                        track.sector_size = sector.try_into().unwrap_or(2048);
                    }
                }
            }
        }
        Some(self.blocks * u64::from(self.block_size))
    }
    fn ready(&mut self) -> std::result::Result<(), [u8; 3]> {
        if self.ejected || self.removed || self.blocks == 0 {
            return Err([2, 0x3a, 0]);
        }
        if self.attention {
            self.attention = false;
            return Err([6, 0x28, 0]);
        }
        Ok(())
    }
    fn span(&self, lba: u64, count: u32) -> std::result::Result<(u64, usize), [u8; 3]> {
        let size = (count as u64)
            .checked_mul(self.block_size as u64)
            .ok_or([5, 0x24, 0])?;
        if size > (MAX_TRANSFER - 30) as u64 {
            return Err([5, 0x24, 0]);
        }
        if lba
            .checked_add(count as u64)
            .is_none_or(|end| end > self.blocks)
        {
            return Err([5, 0x21, 0]);
        }
        Ok((lba * self.block_size as u64, size as usize))
    }
    pub fn respond(&mut self, request: &Packet) -> Result<Packet> {
        if request.body.len() < 29 {
            return Err(Error::Protocol("Truncated media command".into()));
        }
        let mut response = Packet {
            header: request.header,
            body: request.body[..29].to_vec(),
        };
        response.header[19] = 128;
        response.body[21..25].fill(0);
        response.body[25..29].fill(0);
        let result = self.execute(&request.body[9..21], &request.body[29..]);
        match result {
            Ok(data) => {
                response.body[25..29].copy_from_slice(&(data.len() as u32).to_le_bytes());
                response.body.extend_from_slice(&data);
            }
            Err(sense) => {
                self.sense = sense;
                response.body[21] = 1;
                response.body[22..25].copy_from_slice(&sense);
            }
        }
        if response.body.len() < 30 {
            response.body.resize(30, 0);
        }
        Ok(response)
    }
    pub fn ejected(&self) -> bool {
        self.ejected
    }
    pub fn cache_stats(&self) -> Option<super::cache::Stats> {
        self.cache.as_ref().map(|cache| cache.stats())
    }
    pub fn stop_cache(&mut self) -> Option<super::cache::Stats> {
        self.cache.take().map(|cache| cache.stop())
    }
    pub fn flush(&self) -> Result<()> {
        if !self.readonly {
            self.file.sync_data()?;
        }
        Ok(())
    }
    fn read_cd(&mut self, lba: u64, count: u32) -> std::result::Result<Vec<u8>, [u8; 3]> {
        let (_, length) = self.span(lba, count)?;
        if count == 0 {
            return Ok(vec![]);
        }
        if !self.cache_attempted {
            self.cache_attempted = true;
            // A failed optimization must not disable a readable optical image.
            self.cache = super::cache::ReadAhead::new(&self.file, &self.tracks, self.blocks).ok();
        }
        if let Some(cache) = self.cache.as_mut() {
            return cache.read(lba, count);
        }
        let mut data = vec![0; length];
        let mut completed = 0;
        while completed < count as u64 {
            let position = lba + completed;
            let track = self
                .tracks
                .iter()
                .find(|t| position >= t.lba && position < t.lba + t.blocks)
                .ok_or([5, 0x21, 0])?;
            let data_offset = track.data_offset.ok_or([5, 0x64, 0])? as usize;
            let sectors = (count as u64 - completed).min(track.lba + track.blocks - position);
            let offset = track.offset + (position - track.lba) * track.sector_size as u64;
            self.file
                .seek(SeekFrom::Start(offset))
                .map_err(|_| [3, 0x11, 0])?;
            if track.sector_size == 2048 {
                let range = completed as usize * 2048..(completed + sectors) as usize * 2048;
                self.file
                    .read_exact(&mut data[range])
                    .map_err(|_| [3, 0x11, 0])?;
            } else {
                let mut sector = vec![0; track.sector_size as usize];
                for i in 0..sectors {
                    self.file
                        .read_exact(&mut sector)
                        .map_err(|_| [3, 0x11, 0])?;
                    // Mode 2 Form 2 has no 2048-byte logical block representation.
                    if matches!(data_offset, 8 | 24) && sector[data_offset - 6] & 32 != 0 {
                        return Err([5, 0x64, 0]);
                    }
                    let start = (completed + i) as usize * 2048;
                    data[start..start + 2048]
                        .copy_from_slice(&sector[data_offset..data_offset + 2048]);
                }
            }
            completed += sectors;
        }
        Ok(data)
    }
    fn address(lba: u64, msf: bool) -> [u8; 4] {
        if msf {
            let frames = lba + 150;
            [
                0,
                (frames / 4500) as u8,
                ((frames / 75) % 60) as u8,
                (frames % 75) as u8,
            ]
        } else {
            (lba.min(u32::MAX as u64) as u32).to_be_bytes()
        }
    }
    fn toc(&mut self, cdb: &[u8]) -> std::result::Result<Vec<u8>, [u8; 3]> {
        if self.kind != Kind::Cdrom {
            return Err([5, 0x20, 0]);
        }
        self.ready()?;
        let msf = cdb[1] & 2 != 0;
        let format = if cdb[2] & 15 == 0 {
            cdb[9] >> 6
        } else {
            cdb[2] & 15
        };
        let first = self.tracks.first().ok_or([2, 0x3a, 0])?.number;
        let last = self.tracks.last().unwrap().number;
        let mut data = vec![0, 0, first, last];
        match format {
            0 => {
                if cdb[6] > last && cdb[6] != 0xaa {
                    return Err([5, 0x24, 0]);
                }
                for t in self.tracks.iter().filter(|t| t.number >= cdb[6]) {
                    data.extend_from_slice(&[
                        0,
                        if t.data_offset.is_some() { 0x14 } else { 0x10 },
                        t.number,
                        0,
                    ]);
                    data.extend_from_slice(&Self::address(t.lba, msf));
                }
                data.extend_from_slice(&[0, 0x14, 0xaa, 0]);
                data.extend_from_slice(&Self::address(self.blocks, msf));
            }
            1 => {
                data[2] = 1;
                data[3] = 1;
                data.extend_from_slice(&[0, 0x14, first, 0]);
                data.extend_from_slice(&Self::address(self.tracks[0].lba, msf));
            }
            2 => {
                data[2] = 1;
                data[3] = 1;
                for (point, address) in [
                    (0xa0, [0, first, 0, 0]),
                    (0xa1, [0, last, 0, 0]),
                    (0xa2, Self::address(self.blocks, true)),
                ] {
                    data.extend_from_slice(&[
                        1, 0x14, 0, point, 0, 0, 0, 0, address[1], address[2], address[3],
                    ]);
                }
                for t in &self.tracks {
                    let address = Self::address(t.lba, true);
                    data.extend_from_slice(&[
                        1,
                        if t.data_offset.is_some() { 0x14 } else { 0x10 },
                        0,
                        t.number,
                        0,
                        0,
                        0,
                        0,
                        address[1],
                        address[2],
                        address[3],
                    ]);
                }
            }
            _ => return Err([5, 0x24, 0]),
        }
        let length = (data.len() - 2) as u16;
        data[..2].copy_from_slice(&length.to_be_bytes());
        data.truncate(u16::from_be_bytes(cdb[7..9].try_into().unwrap()) as usize);
        Ok(data)
    }
    fn mode_sense(&self, cdb: &[u8]) -> std::result::Result<Vec<u8>, [u8; 3]> {
        let ten = cdb[0] == 0x5a;
        let page = cdb[2] & 63;
        let changeable = cdb[2] >> 6 == 1;
        if cdb[2] >> 6 == 3 {
            return Err([5, 0x39, 0]);
        }
        if cdb[3] != 0 {
            return Err([5, 0x24, 0]);
        }
        let mut data = vec![0; if ten { 8 } else { 4 }];
        data[if ten { 3 } else { 2 }] = if self.readonly { 128 } else { 0 };
        if cdb[1] & 8 == 0 && self.kind != Kind::Cdrom {
            let size = self.block_size.to_be_bytes();
            let blocks = (self.blocks.min(0xff_ffff) as u32).to_be_bytes();
            data.extend_from_slice(&[
                0, blocks[1], blocks[2], blocks[3], 0, size[1], size[2], size[3],
            ]);
            data[if ten { 7 } else { 3 }] = 8;
        }
        let mut pages: Vec<Vec<u8>> = vec![
            vec![1, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![8, 10, 4, 0, 255, 255, 0, 0, 255, 255, 255, 255],
        ];
        if self.kind == Kind::Cdrom {
            let mut capabilities = vec![0; 28];
            capabilities[..2].copy_from_slice(&[0x2a, 26]);
            capabilities[2] = 8;
            capabilities[6] = 0x29;
            capabilities[8..10].copy_from_slice(&7060_u16.to_be_bytes());
            capabilities[12..14].copy_from_slice(&1024_u16.to_be_bytes());
            capabilities[14..16].copy_from_slice(&7060_u16.to_be_bytes());
            pages.push(capabilities);
        } else if self.kind == Kind::Floppy {
            let mut flexible = vec![0; 32];
            flexible[..2].copy_from_slice(&[5, 30]);
            flexible[2..4].copy_from_slice(&500_u16.to_be_bytes());
            flexible[4] = 2;
            flexible[5] = 18;
            flexible[6..8].copy_from_slice(&512_u16.to_be_bytes());
            flexible[8..10].copy_from_slice(&((self.blocks / 36) as u16).to_be_bytes());
            pages.push(flexible);
        }
        let mut valid = page == 0;
        for mut value in pages {
            if page == 63 || value[0] == page {
                valid = true;
                if changeable {
                    value[2..].fill(0);
                }
                data.extend_from_slice(&value);
            }
        }
        if !valid {
            return Err([5, 0x24, 0]);
        }
        if ten {
            let size = (data.len() - 2) as u16;
            data[..2].copy_from_slice(&size.to_be_bytes());
        } else {
            data[0] = (data.len() - 1) as u8;
        }
        data.truncate(if ten {
            u16::from_be_bytes(cdb[7..9].try_into().unwrap()) as usize
        } else {
            cdb[4] as usize
        });
        Ok(data)
    }
    fn configuration(&self, cdb: &[u8]) -> std::result::Result<Vec<u8>, [u8; 3]> {
        if self.kind != Kind::Cdrom {
            return Err([5, 0x20, 0]);
        }
        let profile: u16 = if self.blocks > 360_000 { 0x10 } else { 8 };
        let mut data = vec![0; 8];
        data[6..8].copy_from_slice(&profile.to_be_bytes());
        let start = u16::from_be_bytes(cdb[2..4].try_into().unwrap());
        let single = cdb[1] & 3 == 2;
        for (code, payload) in [
            (
                0_u16,
                vec![
                    0,
                    8,
                    u8::from(profile == 8),
                    0,
                    0,
                    16,
                    u8::from(profile == 16),
                    0,
                ],
            ),
            (1, vec![0, 0, 0, 7, 0, 0, 0, 0]),
            (3, vec![0x29, 0, 0, 0]),
            (0x10, vec![0, 0, 8, 0, 0, 1, 0, 0]),
            (if profile == 8 { 0x1e } else { 0x1f }, vec![0, 0, 0, 0]),
        ] {
            if code < start || single && code != start {
                continue;
            }
            data.extend_from_slice(&code.to_be_bytes());
            data.extend_from_slice(&[3, payload.len() as u8]);
            data.extend_from_slice(&payload);
        }
        let length = (data.len() - 4) as u32;
        data[..4].copy_from_slice(&length.to_be_bytes());
        data.truncate(u16::from_be_bytes(cdb[7..9].try_into().unwrap()) as usize);
        Ok(data)
    }
    fn execute(&mut self, cdb: &[u8], input: &[u8]) -> std::result::Result<Vec<u8>, [u8; 3]> {
        if self.removed {
            return Err([2, 0x3a, 0]);
        }
        if self.physical.is_some()
            && self.kind == Kind::Cdrom
            && super::device::optical_supported(cdb[0])
            && cdb[0] < 0xf0
        {
            if self.attention && matches!(cdb[0], 0x00 | 0x08 | 0x25 | 0x28 | 0xa8 | 0xbe | 0xb9) {
                self.ready()?;
            }
            let result = super::device::optical(&self.file, cdb, self.block_size);
            if result.is_ok() && cdb[0] == 0x1b && cdb[4] & 3 == 2 {
                self.ejected = true;
            }
            return result;
        }
        let be16 = |i| u16::from_be_bytes([cdb[i], cdb[i + 1]]) as u32;
        let be32 = |i| u32::from_be_bytes(cdb[i..i + 4].try_into().unwrap());
        let read_write = |code: u8| matches!(code, 0x08 | 0x0a | 0x28 | 0x2a | 0xa8 | 0xaa);
        if read_write(cdb[0]) {
            self.ready()?;
            let (lba, count) = match cdb[0] {
                0x08 | 0x0a => (
                    (((cdb[1] & 31) as u32) << 16) | ((cdb[2] as u32) << 8) | cdb[3] as u32,
                    if cdb[4] == 0 { 256 } else { cdb[4] as u32 },
                ),
                0x28 | 0x2a => (be32(2), be16(7)),
                _ => (be32(2), be32(6)),
            };
            let (position, length) = self.span(lba as u64, count)?;
            if matches!(cdb[0], 0x0a | 0x2a | 0xaa) {
                if self.readonly {
                    return Err([7, 0x27, 0]);
                }
                if input.len() < length {
                    return Err([5, 0x24, 0]);
                }
                if self.physical.is_some() {
                    super::device::write(&mut self.file, position, &input[..length])
                        .map_err(|_| [3, 0x0c, 2])?;
                } else {
                    self.file
                        .seek(SeekFrom::Start(position))
                        .map_err(|_| [3, 0x0c, 2])?;
                    self.file
                        .write_all(&input[..length])
                        .map_err(|_| [3, 0x0c, 2])?;
                }
                if cdb[0] != 0x0a && cdb[1] & 8 != 0 {
                    self.file.sync_data().map_err(|_| [3, 0x0c, 2])?;
                }
                return Ok(vec![]);
            }
            if self.kind == Kind::Cdrom {
                return self.read_cd(lba as u64, count);
            }
            if self.physical.is_some() {
                return super::device::read(&mut self.file, position, length)
                    .map_err(|_| [3, 0x11, 0]);
            }
            let mut data = vec![0; length];
            self.file
                .seek(SeekFrom::Start(position))
                .map_err(|_| [3, 0x11, 0])?;
            self.file.read_exact(&mut data).map_err(|_| [3, 0x11, 0])?;
            return Ok(data);
        }
        Ok(match cdb[0] {
            0x00 => {
                self.ready()?;
                vec![]
            }
            0x03 => {
                let mut data = vec![0; 18];
                data[0] = 0x70;
                data[2] = self.sense[0];
                data[7] = 10;
                data[12] = self.sense[1];
                data[13] = self.sense[2];
                self.sense = [0; 3];
                data.truncate(cdb[4] as usize);
                data
            }
            0x12 => {
                if cdb[1] & 1 != 0 {
                    match cdb[2] {
                        0 => vec![if self.kind == Kind::Cdrom { 5 } else { 0 }, 0, 0, 1, 0],
                        _ => return Err([5, 0x24, 0]),
                    }
                } else {
                    let mut data = vec![0; 36];
                    data[0] = if self.kind == Kind::Cdrom { 5 } else { 0 };
                    data[1] = 128;
                    data[2] = 2;
                    data[3] = 2;
                    data[4] = 31;
                    data[8..16].copy_from_slice(b"AMIKVM  ");
                    data[16..32].copy_from_slice(if self.kind == Kind::Cdrom {
                        b"Virtual CDROM   "
                    } else {
                        b"Virtual Disk    "
                    });
                    data[32..36].copy_from_slice(b"0100");
                    data.truncate(cdb[4] as usize);
                    data
                }
            }
            0x1b => {
                if cdb[4] & 2 != 0 {
                    if cdb[4] & 1 == 0 {
                        if self.prevent {
                            return Err([5, 0x53, 2]);
                        }
                        self.ejected = true;
                    } else {
                        self.ejected = false;
                        self.attention = true;
                    }
                    if let Some(cache) = &self.cache {
                        cache.clear();
                    }
                }
                vec![]
            }
            0x1e => {
                self.prevent = cdb[4] & 1 != 0;
                vec![]
            }
            0x25 => {
                self.ready()?;
                let mut data = vec![];
                data.extend_from_slice(
                    &((self.blocks - 1).min(u32::MAX as u64) as u32).to_be_bytes(),
                );
                data.extend_from_slice(&self.block_size.to_be_bytes());
                data
            }
            0x23 => {
                self.ready()?;
                let mut data = vec![0, 0, 0, 8];
                data.extend_from_slice(&(self.blocks.min(u32::MAX as u64) as u32).to_be_bytes());
                data.push(2);
                data.extend_from_slice(&self.block_size.to_be_bytes()[1..]);
                data.truncate(be16(7) as usize);
                data
            }
            0x1a | 0x5a => self.mode_sense(cdb)?,
            0x43 => self.toc(cdb)?,
            0x44 => {
                self.ready()?;
                if self.kind != Kind::Cdrom {
                    return Err([5, 0x20, 0]);
                }
                self.span(be32(2) as u64, 1)?;
                let mut data = vec![1, 0, 0, 0];
                data.extend_from_slice(&Self::address(be32(2) as u64, cdb[1] & 2 != 0));
                data.truncate(be16(7) as usize);
                data
            }
            0x46 => self.configuration(cdb)?,
            0x4a => {
                if self.kind != Kind::Cdrom || cdb[1] & 1 == 0 {
                    return Err([5, 0x24, 0]);
                }
                let mut data = if cdb[4] & 16 != 0 {
                    vec![0, 6, 4, 16, 0, if self.ejected { 1 } else { 2 }, 0, 0]
                } else {
                    vec![0, 2, 0x80, 16]
                };
                data.truncate(be16(7) as usize);
                data
            }
            0x35 => {
                self.file.sync_data().map_err(|_| [3, 0x0c, 2])?;
                vec![]
            }
            0x2f => {
                self.ready()?;
                self.span(be32(2) as u64, be16(7))?;
                vec![]
            }
            0xf3 | 0xf6 | 0xf7 => vec![],
            0xf4 => vec![0],
            _ => return Err([5, 0x20, 0]),
        })
    }
}
