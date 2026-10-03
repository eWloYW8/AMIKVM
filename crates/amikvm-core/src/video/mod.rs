//! Native AST video decoding. No Java, JNI, or proprietary decoder is loaded.
pub mod capture;
pub mod config;
pub mod remote_capture;
mod tables;
use crate::{Error, Result};
use tables::*;

const MAX_PIXELS: usize = 16_777_216;

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub source_width: u16,
    pub source_height: u16,
    pub width: u16,
    pub height: u16,
    pub quality: u8,
    pub second_quality: u8,
    pub chroma_as_luma: bool,
    pub yuv420: bool,
    pub payload_length: u32,
}

impl Header {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 86 {
            return Err(Error::Protocol("Truncated AST video header".into()));
        }
        let short = |i| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let header = Self {
            source_width: short(4),
            source_height: short(6),
            width: short(13),
            height: short(15),
            quality: bytes[44],
            chroma_as_luma: bytes[45] != 0,
            second_quality: bytes[47],
            yuv420: bytes[55] != 0,
            payload_length: u32::from_le_bytes(bytes[69..73].try_into().unwrap()),
        };
        if header.source_width == 0
            || header.source_height == 0
            || header.source_width as usize * header.source_height as usize > MAX_PIXELS
            || header.width == 0
            || header.height == 0
            || header.width as usize * header.height as usize > MAX_PIXELS
            || header.quality > 7
            || header.second_quality > 7
        {
            return Err(Error::Protocol(
                "Invalid AST video geometry or quality".into(),
            ));
        }
        Ok(header)
    }
}

/// AST consumes the most significant bit of each little-endian 32-bit word first.
struct Bits<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Bits<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn remaining(&self) -> usize {
        self.bytes.len().div_ceil(4) * 32 - self.position
    }
    fn peek(&self, count: usize) -> Result<u32> {
        if count > 24 || count > self.remaining() {
            return Err(Error::Protocol("Truncated AST bitstream".into()));
        }
        if count == 0 {
            return Ok(0);
        }
        let offset = self.position / 32 * 4;
        let word = |start| {
            let mut b = [0; 4];
            for (i, value) in b.iter_mut().enumerate() {
                *value = self.bytes.get(start + i).copied().unwrap_or(0);
            }
            u32::from_le_bytes(b) as u64
        };
        let bits = (word(offset) << 32) | word(offset + 4);
        Ok(((bits >> (64 - self.position % 32 - count)) & ((1_u64 << count) - 1)) as u32)
    }
    fn read(&mut self, count: usize) -> Result<u32> {
        let value = self.peek(count)?;
        self.position += count;
        Ok(value)
    }
    fn signed(&mut self, count: u8) -> Result<i32> {
        if count == 0 {
            return Ok(0);
        }
        let value = self.read(count as usize)? as i32;
        Ok(if value < 1 << (count - 1) {
            value + 1 - (1 << count)
        } else {
            value
        })
    }
}

struct Huffman {
    codes: Vec<(u32, u8, u8)>,
    lookup: Vec<u16>,
}
impl Huffman {
    fn new(counts: &[u8; 17], values: &[u8]) -> Self {
        let mut codes = Vec::with_capacity(values.len());
        let mut code = 0;
        let mut index = 0;
        for (length, count) in counts.iter().enumerate().skip(1) {
            for _ in 0..*count {
                codes.push((code, length as u8, values[index]));
                index += 1;
                code += 1;
            }
            code <<= 1;
        }
        let mut lookup = vec![0; 65536];
        for (code, length, value) in &codes {
            let start = (*code << (16 - *length)) as usize;
            let end = start + (1 << (16 - *length));
            lookup[start..end].fill((*value as u16) << 8 | *length as u16);
        }
        Self { codes, lookup }
    }
    fn decode(&self, bits: &mut Bits<'_>) -> Result<u8> {
        if bits.remaining() >= 16 {
            let entry = self.lookup[bits.peek(16)? as usize];
            if entry & 255 != 0 {
                bits.position += (entry & 255) as usize;
                return Ok((entry >> 8) as u8);
            }
        }
        let mut value = 0;
        for length in 1..=16 {
            value = (value << 1) | bits.read(1)?;
            if let Some((_, _, symbol)) = self
                .codes
                .iter()
                .find(|(code, n, _)| *n == length && *code == value)
            {
                return Ok(*symbol);
            }
        }
        Err(Error::Protocol("Invalid AST Huffman code".into()))
    }
}

pub struct Decoder {
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    yuv: Vec<[u8; 3]>,
    dc_tables: [Huffman; 2],
    ac_tables: [Huffman; 2],
    dc: [i32; 3],
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            source_width: 0,
            source_height: 0,
            width: 0,
            height: 0,
            rgba: vec![],
            yuv: vec![],
            dc_tables: [
                Huffman::new(&DC_LUMA_BITS, &DC_LUMA_VALUES),
                Huffman::new(&DC_CHROMA_BITS, &DC_CHROMA_VALUES),
            ],
            ac_tables: [
                Huffman::new(&AC_LUMA_BITS, &AC_LUMA_VALUES),
                Huffman::new(&AC_CHROMA_BITS, &AC_CHROMA_VALUES),
            ],
            dc: [0; 3],
        }
    }
}

impl Decoder {
    pub fn decode(&mut self, frame: &[u8]) -> Result<bool> {
        // VideoHeader.m returns immediately for a header-only update, before
        // reading geometry/quality. Those fields may be unset in an empty frame.
        if frame.len() == 86 {
            return Ok(false);
        }
        let header = Header::parse(frame)?;
        let payload = &frame[86..];
        if header.payload_length as usize > payload.len() {
            return Err(Error::Protocol("Incomplete AST compressed frame".into()));
        }
        let payload = if header.payload_length == 0 {
            payload
        } else {
            &payload[..header.payload_length as usize]
        };
        // The compressed block grid and presented raster use MH/output geometry
        // (SOCFrameHdr.oy and decoder oK, verified in the original bytecode).
        // Keep our buffers bounded by that output raster. The original uses
        // MG/source stride for pixel writes despite presenting MH dimensions;
        // do not reproduce its potential out-of-bounds writes when they differ.
        if self.width != header.width as u32
            || self.height != header.height as u32
            || self.source_width != header.source_width as u32
            || self.source_height != header.source_height as u32
        {
            self.source_width = header.source_width as u32;
            self.source_height = header.source_height as u32;
            self.width = header.width as u32;
            self.height = header.height as u32;
            self.rgba = vec![0; self.width as usize * self.height as usize * 4];
            for pixel in self.rgba.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
            self.yuv = vec![[0, 128, 128]; self.width as usize * self.height as usize];
        }
        self.dc = [0; 3];
        let block = if header.yuv420 { 16 } else { 8 };
        let columns = self.width.div_ceil(block);
        let rows = self.height.div_ceil(block);
        let max_blocks = columns as usize * rows as usize * 4;
        let quant = [
            quant(&LUMA[header.quality as usize]),
            quant(if header.chroma_as_luma {
                &LUMA[header.quality as usize]
            } else {
                &CHROMA[header.quality as usize]
            }),
            quant(&LUMA[header.second_quality as usize]),
            quant(if header.chroma_as_luma {
                &LUMA[header.second_quality as usize]
            } else {
                &CHROMA[header.second_quality as usize]
            }),
        ];
        let mut palette = [0x008080_u32, 0xff8080, 0x808080, 0xc08080];
        let mut bits = Bits::new(payload);
        let (mut x, mut y) = (0, 0);
        for _ in 0..max_blocks {
            if bits.remaining() < 4 {
                return Ok(true);
            }
            let kind = bits.read(4)?;
            if kind == 9 {
                return Ok(true);
            }
            if kind & 8 != 0 {
                x = bits.read(8)?;
                y = bits.read(8)?;
            }
            if x >= columns || y >= rows {
                return Err(Error::Protocol("AST block is outside the frame".into()));
            }
            match kind & 7 {
                0 | 4 => {
                    let q = if kind & 7 == 4 { 2 } else { 0 };
                    let planes = self.jpeg(&mut bits, header.yuv420, &quant[q], &quant[q + 1])?;
                    self.paint(x, y, header.yuv420, &planes, false);
                }
                2 => {
                    if header.yuv420 {
                        return Err(Error::Protocol(
                            "AST differential pass in YUV420 mode".into(),
                        ));
                    }
                    let planes = self.jpeg(&mut bits, false, &quant[2], &quant[3])?;
                    self.paint(x, y, false, &planes, true);
                }
                5..=7 => {
                    if header.yuv420 {
                        return Err(Error::Protocol("AST VQ blocks require YUV444 mode".into()));
                    }
                    let count = 1 << ((kind & 7) - 5);
                    let depth = (kind & 7) - 5;
                    let mut indices = [0; 4];
                    for index in indices.iter_mut().take(count as usize) {
                        let update = bits.read(1)? != 0;
                        *index = bits.read(2)? as usize;
                        if update {
                            palette[*index] = bits.read(24)?;
                        }
                    }
                    let mut planes = vec![[0; 64]; 3];
                    for i in 0..64 {
                        let color = palette[indices[bits.read(depth as usize)? as usize]];
                        planes[0][i] = (color >> 16) as u8;
                        planes[1][i] = (color >> 8) as u8;
                        planes[2][i] = color as u8;
                    }
                    self.paint(x, y, false, &planes, false);
                }
                _ => {
                    return Err(Error::Protocol(format!(
                        "Unsupported AST block type {kind}"
                    )));
                }
            }
            x += 1;
            if x >= columns {
                x = 0;
                y += 1;
                if y >= rows {
                    y = 0;
                }
            }
        }
        Err(Error::Protocol(
            "AST block count exceeds frame bounds".into(),
        ))
    }

    fn coefficients(&mut self, bits: &mut Bits<'_>, component: usize) -> Result<[i32; 64]> {
        let table = usize::from(component != 0);
        let mut coefficients = [0; 64];
        let size = self.dc_tables[table].decode(bits)?;
        if size > 11 {
            return Err(Error::Protocol("AST DC coefficient exceeds range".into()));
        }
        self.dc[component] = (self.dc[component] + bits.signed(size)?) as i16 as i32;
        coefficients[0] = self.dc[component];
        let mut index = 1;
        while index < 64 {
            let code = self.ac_tables[table].decode(bits)?;
            let zeros = (code >> 4) as usize;
            let size = code & 15;
            if size == 0 {
                if zeros == 15 {
                    index += 16;
                    continue;
                }
                break;
            }
            index += zeros;
            if index >= 64 || size > 10 {
                return Err(Error::Protocol(
                    "AST AC coefficient exceeds block bounds".into(),
                ));
            }
            coefficients[ZIGZAG[index] as usize] = bits.signed(size)?;
            index += 1;
        }
        Ok(coefficients)
    }

    fn jpeg(
        &mut self,
        bits: &mut Bits<'_>,
        yuv420: bool,
        luma: &[i32; 64],
        chroma: &[i32; 64],
    ) -> Result<Vec<[u8; 64]>> {
        let mut planes = Vec::with_capacity(if yuv420 { 6 } else { 3 });
        for _ in 0..if yuv420 { 4 } else { 1 } {
            planes.push(idct(&self.coefficients(bits, 0)?, luma));
        }
        planes.push(idct(&self.coefficients(bits, 1)?, chroma));
        planes.push(idct(&self.coefficients(bits, 2)?, chroma));
        Ok(planes)
    }

    fn paint(&mut self, x: u32, y: u32, yuv420: bool, planes: &[[u8; 64]], differential: bool) {
        let size = if yuv420 { 16 } else { 8 };
        for row in 0..size {
            for col in 0..size {
                let px = x * size + col;
                let py = y * size + row;
                if px >= self.width || py >= self.height {
                    continue;
                }
                let values = if yuv420 {
                    [
                        planes[(row / 8 * 2 + col / 8) as usize]
                            [((row % 8) * 8 + col % 8) as usize],
                        planes[4][(row / 2 * 8 + col / 2) as usize],
                        planes[5][(row / 2 * 8 + col / 2) as usize],
                    ]
                } else {
                    let i = (row * 8 + col) as usize;
                    [planes[0][i], planes[1][i], planes[2][i]]
                };
                let index = (py * self.width + px) as usize;
                let values = if differential {
                    std::array::from_fn(|i| {
                        (self.yuv[index][i] as i32 + values[i] as i32 - 128).clamp(0, 255) as u8
                    })
                } else {
                    values
                };
                // AST pass 2 refines the last pass-1/VQ samples. Its result
                // changes the presented pixels, never that reference: the
                // next refinement is again relative to the same base.
                if !differential {
                    self.yuv[index] = values;
                }
                let yy = fixed(1.164, values[0] as i32 - 16);
                let b = (yy + fixed(2.015625, values[1] as i32 - 128)).clamp(0, 255) as u8;
                let g = (yy
                    + fixed(-0.390625, values[1] as i32 - 128)
                    + fixed(-0.8125, values[2] as i32 - 128))
                .clamp(0, 255) as u8;
                let r = (yy + fixed(1.597656, values[2] as i32 - 128)).clamp(0, 255) as u8;
                self.rgba[index * 4..index * 4 + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
}

fn fixed(factor: f64, value: i32) -> i32 {
    (((factor.abs() * 65536.0 + 0.5) as i32 * if factor < 0.0 { -value } else { value }) + 32768)
        >> 16
}

fn quant(table: &[i16; 64]) -> [i32; 64] {
    let scale = [
        1.0_f32, 1.3870399, 1.306563, 1.1758755, 1.0, 0.78569496, 0.5411961, 0.27589938,
    ];
    // Preserve the reference's signed table-byte and integer AAN rounding semantics.
    std::array::from_fn(|i| ((table[i].max(1) as f32 * scale[i / 8] * scale[i % 8]) as i32) * 65536)
}

fn aan(v: [i32; 8]) -> [i32; 8] {
    let mul = |x: i32, k: i32| x.wrapping_mul(k) >> 8;
    let t11 = v[0] + v[4];
    let t12 = v[0] - v[4];
    let t13 = v[2] + v[6];
    let t14 = mul(v[2] - v[6], 362) - t13;
    let t15 = t11 + t13;
    let t16 = t11 - t13;
    let t17 = t12 + t14;
    let t18 = t12 - t14;
    let t23 = v[5] + v[3];
    let t24 = v[5] - v[3];
    let t25 = v[1] + v[7];
    let t26 = v[1] - v[7];
    let t27 = t25 + t23;
    let t28 = mul(t25 - t23, 362);
    let t29 = mul(t24 + t26, 473);
    let t30 = mul(t26, 277) - t29;
    let t31 = mul(t24, -669) + t29 - t27;
    let t32 = t28 - t31;
    let t33 = t30 + t32;
    [
        t15 + t27,
        t17 + t31,
        t18 + t32,
        t16 - t33,
        t16 + t33,
        t18 - t32,
        t17 - t31,
        t15 - t27,
    ]
}

fn idct(coefficients: &[i32; 64], quant: &[i32; 64]) -> [u8; 64] {
    let mut workspace = [0; 64];
    for col in 0..8 {
        let v = std::array::from_fn(|row| {
            ((coefficients[row * 8 + col] as i64 * quant[row * 8 + col] as i64) as i32) >> 16
        });
        let transformed = aan(v);
        for row in 0..8 {
            workspace[row * 8 + col] = transformed[row];
        }
    }
    let mut out = [0; 64];
    for row in 0..8 {
        let v = aan(workspace[row * 8..row * 8 + 8].try_into().unwrap());
        for col in 0..8 {
            // Reference range-limit table handles signed values in a wrapped 10-bit domain.
            let wrapped = ((v[col] >> 3) & 1023) as usize;
            out[row * 8 + col] = match wrapped {
                0..=127 => (wrapped + 128) as u8,
                128..=510 => 255,
                511..=895 => 0,
                _ => (wrapped - 896) as u8,
            };
        }
    }
    out
}

#[derive(Default)]
pub struct Cursor {
    kind: u8,
    pub x: i16,
    pub y: i16,
    offset_x: u16,
    offset_y: u16,
    pixels: Vec<u16>,
}
impl Cursor {
    pub fn update(&mut self, body: &[u8]) -> Result<()> {
        // HardwareCursorReader resets to HeaderReader for an empty update.
        // Leave the last cursor intact instead of ending the video session.
        if body.is_empty() {
            return Ok(());
        }
        if body.len() < 13 || (body.len() != 13 && body.len() != 13 + 8192) {
            return Err(Error::Protocol("Invalid AST hardware cursor packet".into()));
        }
        let offset_x = u16::from_le_bytes(body[9..11].try_into().unwrap());
        let offset_y = u16::from_le_bytes(body[11..13].try_into().unwrap());
        if offset_x > 64 || offset_y > 64 {
            return Err(Error::Protocol("Invalid AST cursor offset".into()));
        }
        self.kind = body[0];
        self.x = i16::from_le_bytes(body[5..7].try_into().unwrap());
        self.y = i16::from_le_bytes(body[7..9].try_into().unwrap());
        self.offset_x = offset_x;
        self.offset_y = offset_y;
        if body.len() > 13 {
            self.pixels = body[13..]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
        }
        Ok(())
    }
    /// Overlay on a copy. The decoder's background and differential state stay intact.
    pub fn overlay(&self, width: u32, height: u32, rgba: &mut [u8]) {
        // HardwareCursor.oN logs unknown formats and skips drawing, keeping
        // the KVM connection alive for the remaining video/input streams.
        if self.pixels.len() != 4096 || self.kind > 1 {
            return;
        }
        for row in 0..64 - self.offset_y {
            for col in 0..64 - self.offset_x {
                let x = self.x as i32 + col as i32;
                let y = self.y as i32 + row as i32;
                if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                    continue;
                }
                let pixel = self.pixels
                    [(row + self.offset_y) as usize * 64 + (col + self.offset_x) as usize];
                // Both cursor formats carry RGB444. Repeat each nibble to
                // normalize 0..15 to the full 8-bit display range, 0..255.
                let rgb = [
                    ((pixel >> 8) & 15) as u8 * 17,
                    ((pixel >> 4) & 15) as u8 * 17,
                    (pixel & 15) as u8 * 17,
                ];
                let index = (y as usize * width as usize + x as usize) * 4;
                if self.kind == 0 {
                    if pixel & 0x8000 == 0 {
                        rgba[index..index + 3].copy_from_slice(&rgb);
                    } else if pixel & 0x4000 != 0 {
                        for b in &mut rgba[index..index + 3] {
                            *b = !*b;
                        }
                    }
                } else {
                    let alpha = (pixel >> 12) as u32;
                    for channel in 0..3 {
                        let color = u32::from(rgb[channel]);
                        rgba[index + channel] = ((rgba[index + channel] as u32 * (15 - alpha)
                            + color * alpha)
                            / 15) as u8;
                    }
                }
            }
        }
    }
}
