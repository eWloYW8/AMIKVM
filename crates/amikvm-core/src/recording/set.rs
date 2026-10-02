use super::Recorder;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

/// The reference's normalize checkbox also selects how blank frames are handled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    #[default]
    Normalized,
    NativeSegments,
}
impl Policy {
    pub fn value(self) -> &'static str {
        match self {
            Self::Normalized => "normalized",
            Self::NativeSegments => "native_segments",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Normalized => "统一为 1024×768",
            Self::NativeSegments => "原分辨率，变化时分文件",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Normalized => "画面拉伸为 1024×768，记录无信号画面。",
            Self::NativeSegments => "保留原分辨率；跳过无信号时间，尺寸变化时保存为新 MP4。",
        }
    }
}

/// Immutable pixels may be reused for many timer ticks without resampling.
#[derive(Clone)]
pub struct Frame {
    width: u32,
    height: u32,
    bytes: Arc<Vec<u8>>,
    offset: usize,
}
impl Frame {
    pub fn new(width: u32, height: u32, rgba: Arc<Vec<u8>>) -> Result<Self> {
        Self::validate(width, height, rgba, 0)
    }
    pub fn from_packet(packet: Arc<Vec<u8>>) -> Result<Self> {
        if packet.len() < 12 {
            return Err(Error::Invalid("录制画面缺少尺寸和序号".into()));
        }
        let width = u32::from_le_bytes(packet[..4].try_into().unwrap());
        let height = u32::from_le_bytes(packet[4..8].try_into().unwrap());
        Self::validate(width, height, packet, 12)
    }
    fn validate(width: u32, height: u32, bytes: Arc<Vec<u8>>, offset: usize) -> Result<Self> {
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || bytes.len() != offset + width as usize * height as usize * 4
        {
            return Err(Error::Invalid("录制画面尺寸或像素长度无效".into()));
        }
        Ok(Self {
            width,
            height,
            bytes,
            offset,
        })
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn rgba(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }
    fn same_pixels(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.offset == other.offset
            && Arc::ptr_eq(&self.bytes, &other.bytes)
    }
}

struct Segment {
    recorder: Recorder,
    width: u32,
    height: u32,
    started: u64,
}

pub struct RecordingSet {
    path: PathBuf,
    policy: Policy,
    segment: Option<Segment>,
    paths: Vec<PathBuf>,
    next_index: u64,
    previous: Option<(u64, bool)>,
    written_ms: u64,
    skipped_ms: u64,
    cached_frame: Option<Frame>,
    normalized: Vec<u8>,
    blank: Vec<u8>,
    finished: bool,
}
impl RecordingSet {
    pub fn new(path: PathBuf, policy: Policy) -> Self {
        Self {
            path,
            policy,
            segment: None,
            paths: Vec::new(),
            next_index: 1,
            previous: None,
            written_ms: 0,
            skipped_ms: 0,
            cached_frame: None,
            normalized: Vec::new(),
            blank: no_signal_pixels(),
            finished: false,
        }
    }
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
    pub fn written_ms(&self) -> u64 {
        self.written_ms
    }
    pub fn skipped_ms(&self) -> u64 {
        self.skipped_ms
    }

    fn observe(&mut self, milliseconds: u64, signal: bool) -> Result<()> {
        if let Some((previous, previous_signal)) = self.previous {
            let delta = milliseconds
                .checked_sub(previous)
                .ok_or_else(|| Error::Invalid("录制时钟不能倒退".into()))?;
            if self.policy == Policy::NativeSegments && !previous_signal {
                self.skipped_ms += delta;
            } else {
                self.written_ms += delta;
            }
        } else if milliseconds != 0 {
            return Err(Error::Invalid("录制必须从零毫秒开始".into()));
        }
        self.previous = Some((milliseconds, signal));
        Ok(())
    }

    /// The clock includes blank intervals, while deliberate pauses are removed by the caller.
    pub fn frame_at(&mut self, milliseconds: u64, frame: Option<Frame>) -> Result<()> {
        if self.finished {
            return Err(Error::Invalid("录制已结束".into()));
        }
        self.observe(milliseconds, frame.is_some())?;
        if frame.is_none() && self.policy == Policy::NativeSegments {
            return Ok(());
        }
        let (width, height) = if self.policy == Policy::Normalized {
            (1024, 768)
        } else {
            let frame = frame.as_ref().unwrap();
            (frame.width, frame.height)
        };
        if self
            .segment
            .as_ref()
            .is_some_and(|s| s.width != width || s.height != height)
        {
            self.finish_segment()?;
        }
        if self.segment.is_none() {
            let (path, recorder) = if self.paths.is_empty() {
                (self.path.clone(), Recorder::new(&self.path, width, height)?)
            } else {
                self.next_segment(width, height)?
            };
            self.segment = Some(Segment {
                recorder,
                width,
                height,
                started: self.written_ms,
            });
            self.paths.push(path);
        }
        let pixels = match (&frame, self.policy) {
            (Some(frame), Policy::NativeSegments) => frame.rgba(),
            (None, Policy::Normalized) => &self.blank,
            (Some(frame), Policy::Normalized) if frame.width == 1024 && frame.height == 768 => {
                frame.rgba()
            }
            (Some(frame), Policy::Normalized) => {
                if !self
                    .cached_frame
                    .as_ref()
                    .is_some_and(|old| old.same_pixels(frame))
                {
                    let input = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
                        frame.width,
                        frame.height,
                        frame.rgba(),
                    )
                    .ok_or_else(|| Error::Invalid("录制画面像素长度无效".into()))?;
                    self.normalized = image::imageops::resize(
                        &input,
                        1024,
                        768,
                        image::imageops::FilterType::CatmullRom,
                    )
                    .into_raw();
                    self.cached_frame = Some(frame.clone());
                }
                &self.normalized
            }
            _ => unreachable!(),
        };
        let segment = self.segment.as_mut().unwrap();
        segment
            .recorder
            .frame_at(self.written_ms - segment.started, width, height, pixels)
    }
    fn next_segment(&mut self, width: u32, height: u32) -> Result<(PathBuf, Recorder)> {
        loop {
            let stem = self
                .path
                .file_stem()
                .ok_or_else(|| Error::Invalid("录制文件名无效".into()))?;
            let mut name = stem.to_os_string();
            name.push(format!("_{}.mp4", self.next_index));
            self.next_index = self
                .next_index
                .checked_add(1)
                .ok_or_else(|| Error::Invalid("录制分段编号已耗尽".into()))?;
            let path = self.path.with_file_name(name);
            match Recorder::new_segment(&path, width, height) {
                Ok(recorder) => return Ok((path, recorder)),
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
    }
    fn finish_segment(&mut self) -> Result<()> {
        if let Some(segment) = self.segment.take() {
            segment
                .recorder
                .finish_at(self.written_ms - segment.started)?;
        }
        Ok(())
    }
    pub fn finish_at(&mut self, milliseconds: u64) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let signal = self.previous.is_some_and(|(_, signal)| signal);
        self.observe(milliseconds, signal)?;
        self.finished = true;
        self.finish_segment()?;
        if self.paths.is_empty() {
            return Err(Error::Invalid(
                "录制期间没有视频信号，未生成 MP4 文件".into(),
            ));
        }
        Ok(())
    }
}

/// A Rust-generated blank frame, independent of the reference JPEG and Java UI.
fn no_signal_pixels() -> Vec<u8> {
    let mut rgba = vec![0; 1024 * 768 * 4];
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    // Five-column, seven-row glyphs for "NO SIGNAL".
    let glyphs: [[u8; 7]; 9] = [
        [17, 25, 21, 19, 17, 17, 17],
        [14, 17, 17, 17, 17, 17, 14],
        [0; 7],
        [15, 16, 16, 14, 1, 1, 30],
        [31, 4, 4, 4, 4, 4, 31],
        [14, 17, 16, 23, 17, 17, 14],
        [17, 25, 21, 19, 17, 17, 17],
        [14, 17, 17, 31, 17, 17, 17],
        [16, 16, 16, 16, 16, 16, 31],
    ];
    let scale = 6;
    let left = (1024 - (glyphs.len() * 6 - 1) * scale) / 2;
    let top = (768 - 7 * scale) / 2;
    for (index, glyph) in glyphs.iter().enumerate() {
        for (y, row) in glyph.iter().enumerate() {
            for x in 0..5 {
                if row & (1 << (4 - x)) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let at = ((top + y * scale + dy) * 1024
                                + left
                                + (index * 6 + x) * scale
                                + dx)
                                * 4;
                            rgba[at..at + 3].fill(160);
                        }
                    }
                }
            }
        }
    }
    rgba
}
