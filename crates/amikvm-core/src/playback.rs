//! Native readers for BMC AST recordings and AMIKVM H.264/MP4 recordings.
//! Compressed packets remain bounded; decoding and playback timing live in Rust.
use crate::{Error, Result, video::Header};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

const MAX_AST_FRAME: usize = 4_718_592;
const MAX_AVC_FRAME: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Metadata {
    pub format: &'static str,
    pub duration_ms: Option<u64>,
}

pub enum Data {
    Ast(Vec<u8>),
    Avc(Vec<u8>),
    NoSignal,
    Marker,
    /// JViewer's AST command handler ignores this older bitmap/position format.
    /// Consume the complete record without applying an invented cursor encoding.
    LegacyCursor,
}

pub struct Packet {
    pub timestamp_ms: u64,
    pub data: Data,
}

pub struct Reader {
    pub metadata: Metadata,
    source: Source,
}

enum Source {
    Ast {
        file: BufReader<File>,
        previous: Option<u32>,
        elapsed: u64,
    },
    Mp4(Box<AvcReader>),
}

struct AvcReader {
    file: mp4::Mp4Reader<BufReader<File>>,
    track: u32,
    timescale: u32,
    count: u32,
    sample: u32,
    length_size: usize,
    parameters: Vec<u8>,
    first_time: Option<u64>,
}

fn mp4_error(error: impl std::fmt::Display) -> Error {
    Error::Protocol(format!("MP4: {error}"))
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid("请选择普通录像文件".into()));
        }
        fs2::FileExt::try_lock_shared(&file)
            .map_err(|_| Error::Invalid("录像文件正在写入或被其他进程占用".into()))?;
        let size = file.metadata()?.len();
        let mut magic = [0; 8];
        let mut read = 0;
        while read < magic.len() {
            let count = file.read(&mut magic[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        if read < 5 {
            return Err(Error::Invalid("录像文件为空或报头不完整".into()));
        }
        file.seek(SeekFrom::Start(0))?;
        if &magic[4..8] == b"ftyp" {
            let mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size).map_err(mp4_error)?;
            let track = mp4
                .tracks()
                .values()
                .find(|track| track.trak.mdia.minf.stbl.stsd.avc1.is_some())
                .ok_or_else(|| Error::Invalid("MP4 中没有 H.264 视频轨道".into()))?;
            let timescale = track.timescale();
            if timescale == 0 {
                return Err(mp4_error("Invalid video timescale"));
            }
            let avc = &track.trak.mdia.minf.stbl.stsd.avc1.as_ref().unwrap().avcc;
            let length_size = usize::from((avc.length_size_minus_one & 3) + 1);
            if length_size == 3 {
                return Err(mp4_error("Invalid NAL length size"));
            }
            let mut parameters = Vec::new();
            for nal in avc
                .sequence_parameter_sets
                .iter()
                .chain(avc.picture_parameter_sets.iter())
            {
                if nal.bytes.is_empty() {
                    return Err(mp4_error("Empty H.264 parameter set"));
                }
                parameters.extend_from_slice(&[0, 0, 0, 1]);
                parameters.extend_from_slice(&nal.bytes);
            }
            if parameters.is_empty() {
                return Err(mp4_error("Missing H.264 parameter sets"));
            }
            let id = track.track_id();
            let count = mp4.sample_count(id).map_err(mp4_error)?;
            if count == 0 {
                return Err(Error::Invalid("MP4 中没有视频帧".into()));
            }
            let duration_ms = u64::try_from(
                u128::from(track.trak.mdia.mdhd.duration) * 1000 / u128::from(timescale),
            )
            .map_err(|_| mp4_error("Video duration overflow"))?;
            Ok(Self {
                metadata: Metadata {
                    format: "H.264 / MP4",
                    duration_ms: Some(duration_ms),
                },
                source: Source::Mp4(Box::new(AvcReader {
                    file: mp4,
                    track: id,
                    timescale,
                    count,
                    sample: 1,
                    length_size,
                    parameters,
                    first_time: None,
                })),
            })
        } else {
            let duration_ms = if &magic[..4] == b"len=" {
                let mut header = [0; 20];
                file.read_exact(&mut header)?;
                let duration = std::str::from_utf8(&header[4..])
                    .ok()
                    .and_then(|s| {
                        s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
                            .parse::<f64>()
                            .ok()
                    })
                    .filter(|v| v.is_finite() && *v >= 0.0 && *v * 1000.0 < u64::MAX as f64)
                    .ok_or_else(|| Error::Protocol("Invalid AST recording duration".into()))?;
                Some((duration * 1000.0).round() as u64)
            } else {
                if !matches!(magic[4], 0x55 | 0x66 | 0x77 | 0xaa) {
                    return Err(Error::Invalid("文件不是 BMC AST 或 H.264/MP4 录像".into()));
                }
                None
            };
            Ok(Self {
                metadata: Metadata {
                    format: "BMC / AST",
                    duration_ms,
                },
                source: Source::Ast {
                    file: BufReader::new(file),
                    previous: None,
                    elapsed: 0,
                },
            })
        }
    }

    pub fn read_next(&mut self) -> Result<Option<Packet>> {
        match &mut self.source {
            Source::Ast {
                file,
                previous,
                elapsed,
            } => {
                let mut first = [0];
                if file.read(&mut first)? == 0 {
                    return Ok(None);
                }
                let mut prefix = [0; 5];
                prefix[0] = first[0];
                file.read_exact(&mut prefix[1..])?;
                let time = u32::from_le_bytes(prefix[..4].try_into().unwrap());
                if prefix[4] == 0xaa {
                    return Ok(Some(Packet {
                        timestamp_ms: *elapsed,
                        data: Data::Marker,
                    }));
                }
                if let Some(before) = previous {
                    let delta = time.wrapping_sub(*before);
                    if delta > i32::MAX as u32 {
                        return Err(Error::Protocol(
                            "AST recording timestamps run backwards".into(),
                        ));
                    }
                    *elapsed = elapsed
                        .checked_add(u64::from(delta))
                        .ok_or_else(|| Error::Protocol("AST timestamp overflow".into()))?;
                }
                *previous = Some(time);
                let data = match prefix[4] {
                    0xaa => Data::Marker,
                    0x66 => Data::NoSignal,
                    0x77 => {
                        let mut header = [0; 8];
                        file.read_exact(&mut header)?;
                        if u16::from_le_bytes(header[..2].try_into().unwrap()) != 4097 {
                            return Err(Error::Protocol("Invalid legacy cursor record".into()));
                        }
                        let mut cursor = [0; 1024 + 16];
                        file.read_exact(&mut cursor)?;
                        Data::LegacyCursor
                    }
                    0x55 => {
                        let mut header = [0; 86];
                        file.read_exact(&mut header)?;
                        let length = Header::parse(&header)?.payload_length as usize;
                        if length == 0 || length > MAX_AST_FRAME {
                            return Err(Error::Protocol("Invalid AST recording frame size".into()));
                        }
                        let mut frame = Vec::with_capacity(86 + length);
                        frame.extend_from_slice(&header);
                        frame.resize(86 + length, 0);
                        file.read_exact(&mut frame[86..])?;
                        Data::Ast(frame)
                    }
                    flag => {
                        return Err(Error::Protocol(format!(
                            "Unknown AST recording flag 0x{flag:02x}"
                        )));
                    }
                };
                Ok(Some(Packet {
                    timestamp_ms: *elapsed,
                    data,
                }))
            }
            Source::Mp4(avc) => avc.read_next(),
        }
    }
}

impl AvcReader {
    fn read_next(&mut self) -> Result<Option<Packet>> {
        if self.sample > self.count {
            return Ok(None);
        }
        let sample = self
            .file
            .read_sample(self.track, self.sample)
            .map_err(mp4_error)?
            .ok_or_else(|| mp4_error("Missing video sample"))?;
        if sample.bytes.is_empty() || sample.bytes.len() > MAX_AVC_FRAME {
            return Err(mp4_error("Invalid H.264 sample size"));
        }
        // AMIKVM produces low-latency H.264 without B frames. Don't silently play
        // other MP4 files on the wrong presentation timeline.
        if sample.rendering_offset != 0 {
            return Err(Error::Invalid("暂不支持含重排帧的外部 MP4 录像".into()));
        }
        let first = *self.first_time.get_or_insert(sample.start_time);
        let timestamp_ms = sample
            .start_time
            .checked_sub(first)
            .and_then(|v| u64::try_from(u128::from(v) * 1000 / u128::from(self.timescale)).ok())
            .ok_or_else(|| mp4_error("Invalid sample timestamp"))?;
        let mut data = Vec::with_capacity(sample.bytes.len() + self.parameters.len());
        if self.sample == 1 {
            data.extend_from_slice(&self.parameters);
        }
        let mut at = 0;
        while at < sample.bytes.len() {
            let prefix = sample
                .bytes
                .get(at..at + self.length_size)
                .ok_or_else(|| mp4_error("Truncated NAL length"))?;
            let length = prefix
                .iter()
                .fold(0_usize, |length, b| (length << 8) | usize::from(*b));
            at += self.length_size;
            if length == 0 {
                return Err(mp4_error("Empty NAL unit"));
            }
            let nal = sample
                .bytes
                .get(at..at + length)
                .ok_or_else(|| mp4_error("Truncated NAL unit"))?;
            data.extend_from_slice(&[0, 0, 0, 1]);
            data.extend_from_slice(nal);
            at += length;
        }
        self.sample += 1;
        Ok(Some(Packet {
            timestamp_ms,
            data: Data::Avc(data),
        }))
    }
}

/// Stateful AST/H.264 decoder, shared by native playback workers and direct checks.
#[derive(Default)]
pub struct Decoder {
    ast: crate::video::Decoder,
    avc: Option<openh264::decoder::Decoder>,
}

pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

impl Decoder {
    pub fn decode(&mut self, data: &Data) -> Result<Option<Frame<'_>>> {
        match data {
            Data::Ast(bytes) => {
                if self.ast.decode(bytes)? {
                    Ok(Some(Frame {
                        width: self.ast.width,
                        height: self.ast.height,
                        rgba: &self.ast.rgba,
                    }))
                } else {
                    Ok(None)
                }
            }
            Data::Avc(bytes) => {
                use openh264::formats::YUVSource;
                if self.avc.is_none() {
                    self.avc = Some(openh264::decoder::Decoder::new().map_err(mp4_error)?);
                }
                let decoded = self
                    .avc
                    .as_mut()
                    .unwrap()
                    .decode(bytes)
                    .map_err(mp4_error)?;
                if let Some(decoded) = decoded {
                    let (width, height) = decoded.dimensions();
                    let pixels = width
                        .checked_mul(height)
                        .filter(|&size| size > 0 && size <= 16_777_216)
                        .ok_or_else(|| mp4_error("Invalid decoded image size"))?;
                    self.ast.rgba.resize(pixels * 4, 0);
                    self.ast.width = width as u32;
                    self.ast.height = height as u32;
                    decoded.write_rgba8(&mut self.ast.rgba);
                    Ok(Some(Frame {
                        width: width as u32,
                        height: height as u32,
                        rgba: &self.ast.rgba,
                    }))
                } else {
                    Ok(None)
                }
            }
            Data::NoSignal => {
                self.ast = Default::default();
                Ok(None)
            }
            Data::Marker | Data::LegacyCursor => Ok(None),
        }
    }

    pub fn latest(&self) -> Option<Frame<'_>> {
        (!self.ast.rgba.is_empty()).then_some(Frame {
            width: self.ast.width,
            height: self.ast.height,
            rgba: &self.ast.rgba,
        })
    }
}
