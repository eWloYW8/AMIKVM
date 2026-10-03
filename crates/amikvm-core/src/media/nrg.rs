//! Nero chunk metadata is big endian; disk contents are not converted in place.
//! Format facts cross-checked against libcdio's NRG reader and nrg.h.
use crate::{Error, Result};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
};

#[derive(Clone)]
pub struct Track {
    pub number: u8,
    pub lba: u64,
    pub blocks: u64,
    pub offset: u64,
    pub sector_size: u16,
    pub data_offset: Option<u16>,
}
fn invalid() -> Error {
    Error::Invalid("Invalid or unsupported NRG track metadata".into())
}
fn u32be(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}
fn u64be(b: &[u8]) -> u64 {
    u64::from_be_bytes(b[..8].try_into().unwrap())
}
fn bcd(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 15)
}
fn sector(mode: u8, size: u16) -> Result<Option<u16>> {
    match (mode, size) {
        (0 | 2, 2048) => Ok(Some(0)),
        (3 | 6 | 0x20, 2336) => Ok(Some(8)),
        (0 | 5, 2352 | 2448) => Ok(Some(16)),
        (2 | 3 | 6 | 0x20, 2352 | 2448) => Ok(Some(24)),
        (7, 2352 | 2448) => Ok(None),
        _ => Err(invalid()),
    }
}
pub fn tracks(file: &mut File) -> Result<Vec<Track>> {
    match nero_tracks(file) {
        Ok(tracks) => Ok(tracks),
        Err(Error::Invalid(message)) => {
            // JViewer accepts ISO9660/UDF sector images under the .nrg suffix
            // without Nero chunks. Keep genuine Nero track geometry when known.
            let length = file.metadata()?.len();
            for (sector, offset, signature) in [
                (16_u64, 1_u64, b"CD001".as_slice()),
                (35, 217, b"*OSTA UDF Compliant".as_slice()),
            ] {
                if length < (sector + 1) * 2048 {
                    continue;
                }
                file.seek(SeekFrom::Start(sector * 2048 + offset))?;
                let mut bytes = vec![0; signature.len()];
                file.read_exact(&mut bytes)?;
                if bytes == signature {
                    return Ok(vec![Track {
                        number: 1,
                        lba: 0,
                        blocks: length / 2048,
                        offset: 0,
                        sector_size: 2048,
                        data_offset: Some(0),
                    }]);
                }
            }
            Err(Error::Invalid(message))
        }
        Err(error) => Err(error),
    }
}

fn nero_tracks(file: &mut File) -> Result<Vec<Track>> {
    let length = file.metadata()?.len();
    if length < 12 {
        return Err(invalid());
    }
    file.seek(SeekFrom::End(-12))?;
    let mut footer = [0; 12];
    file.read_exact(&mut footer)?;
    let (offset, footer_bytes) = if &footer[..4] == b"NER5" {
        (u64be(&footer[4..]), 12)
    } else if &footer[4..8] == b"NERO" {
        (u32be(&footer[8..]) as u64, 8)
    } else {
        return Err(invalid());
    };
    let bytes = length
        .checked_sub(footer_bytes)
        .and_then(|n| n.checked_sub(offset))
        .ok_or_else(invalid)?;
    if bytes < 8 || bytes > 1024 * 1024 {
        return Err(invalid());
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut metadata = vec![0; bytes as usize];
    file.read_exact(&mut metadata)?;
    let mut chunks = Vec::new();
    let mut cursor = 0;
    while cursor < metadata.len() {
        let header = metadata.get(cursor..cursor + 8).ok_or_else(invalid)?;
        let end = cursor
            .checked_add(8 + u32be(&header[4..]) as usize)
            .ok_or_else(invalid)?;
        let data = metadata.get(cursor + 8..end).ok_or_else(invalid)?;
        chunks.push((&header[..4], data));
        cursor = end;
        if &header[..4] == b"END!" {
            break;
        }
    }
    let mut cues = HashMap::new();
    for (name, data) in &chunks {
        if *name != b"CUEX" {
            continue;
        }
        if data.len() % 8 != 0 {
            return Err(invalid());
        }
        for cue in data.chunks_exact(8) {
            let lba = u32be(&cue[4..]) as i32;
            if cue[2] == 1 && cue[1] != 0xaa && lba >= 0 {
                cues.insert(bcd(cue[1]), lba as u64);
            }
        }
    }
    let mut result = vec![];
    for (name, data) in &chunks {
        if *name == b"DAOX" || *name == b"DAOI" {
            if data.len() < 22 {
                return Err(invalid());
            }
            let wide = *name == b"DAOX";
            let entry_size = if wide { 42 } else { 30 };
            if (data.len() - 22) % entry_size != 0 {
                return Err(invalid());
            }
            let mut next_lba = 0;
            for (index, entry) in data[22..].chunks_exact(entry_size).enumerate() {
                let size = u16::from_be_bytes(entry[12..14].try_into().unwrap());
                let (index0, index1, end) = if wide {
                    (
                        u64be(&entry[18..]),
                        u64be(&entry[26..]),
                        u64be(&entry[34..]),
                    )
                } else {
                    (
                        u32be(&entry[18..]) as u64,
                        u32be(&entry[22..]) as u64,
                        u32be(&entry[26..]) as u64,
                    )
                };
                if size == 0
                    || index1 < index0
                    || end <= index1
                    || end > offset
                    || (end - index1) % size as u64 != 0
                {
                    return Err(invalid());
                }
                let number = data[20].checked_add(index as u8).ok_or_else(invalid)?;
                let lba = cues.get(&number).copied().unwrap_or(next_lba);
                let blocks = (end - index1) / size as u64;
                result.push(Track {
                    number,
                    lba,
                    blocks,
                    offset: index1,
                    sector_size: size,
                    data_offset: sector(entry[14], size)?,
                });
                next_lba = lba.checked_add(blocks).ok_or_else(invalid)?;
            }
            break;
        }
    }
    if result.is_empty() {
        for (name, data) in &chunks {
            if *name != b"ETN2" && *name != b"ETNF" {
                continue;
            }
            let wide = *name == b"ETN2";
            let entry_size = if wide { 32 } else { 20 };
            if data.len() % entry_size != 0 {
                return Err(invalid());
            }
            for (index, entry) in data.chunks_exact(entry_size).enumerate() {
                let (start, bytes, mode, lba) = if wide {
                    (
                        u64be(entry),
                        u64be(&entry[8..]),
                        u32be(&entry[16..]),
                        u32be(&entry[20..]),
                    )
                } else {
                    (
                        u32be(entry) as u64,
                        u32be(&entry[4..]) as u64,
                        u32be(&entry[8..]),
                        u32be(&entry[12..]),
                    )
                };
                let size = match mode {
                    0 | 2 => 2048,
                    3 | 6 | 0x20 => 2336,
                    5 | 7 => 2352,
                    _ => return Err(invalid()),
                };
                if bytes == 0
                    || bytes % size as u64 != 0
                    || start.checked_add(bytes).is_none_or(|end| end > offset)
                {
                    return Err(invalid());
                }
                result.push(Track {
                    number: (index + 1) as u8,
                    lba: lba as u64 + index as u64 * 150,
                    blocks: bytes / size as u64,
                    offset: start,
                    sector_size: size,
                    data_offset: sector(mode as u8, size)?,
                });
            }
            break;
        }
    }
    if result.is_empty() || result.len() > 99 {
        return Err(invalid());
    }
    result.sort_by_key(|t| t.lba);
    for pair in result.windows(2) {
        if pair[0].lba + pair[0].blocks > pair[1].lba {
            return Err(invalid());
        }
    }
    Ok(result)
}
