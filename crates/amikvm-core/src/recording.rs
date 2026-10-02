//! H.264 encoding and MP4 muxing are native and do not launch external programs.
use crate::{Error, Result};
use bytes::Bytes;
use mp4::{AvcConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig};
use openh264::{
    OpenH264API,
    encoder::{Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType},
    formats::{RgbaSliceU8, YUVBuffer},
};
use std::{
    fs::{File, OpenOptions},
    path::Path,
};
mod set;
pub use set::{Frame, Policy, RecordingSet};

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("MP4 recording: {error}"))
}

pub struct Recorder {
    writer: Mp4Writer<File>,
    encoder: Encoder,
    width: usize,
    height: usize,
    sample_count: u64,
    track_added: bool,
    pixels: Vec<u8>,
    pending: Option<Mp4Sample>,
}

impl Recorder {
    pub fn new(path: &Path, width: u32, height: u32) -> Result<Self> {
        Self::open(path, width, height, false)
    }
    pub(crate) fn new_segment(path: &Path, width: u32, height: u32) -> Result<Self> {
        Self::open(path, width, height, true)
    }
    fn open(path: &Path, width: u32, height: u32, exclusive: bool) -> Result<Self> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(Error::Invalid("Invalid recording dimensions".into()));
        }
        let width = (width as usize).div_ceil(2) * 2;
        let height = (height as usize).div_ceil(2) * 2;
        let mut options = OpenOptions::new();
        options.write(true).read(true).truncate(false);
        if exclusive {
            options.create_new(true);
        } else {
            options.create(true);
        }
        let file = options.open(path)?;
        // Readers and writable virtual media also honor this lock. Acquire it
        // before truncating, so opening an in-use recording never destroys it.
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|_| Error::Invalid("录像文件正在回放或被其他进程占用".into()))?;
        file.set_len(0)?;
        let config = Mp4Config {
            major_brand: "isom".parse().unwrap(),
            minor_version: 512,
            compatible_brands: vec![
                "isom".parse().unwrap(),
                "iso2".parse().unwrap(),
                "avc1".parse().unwrap(),
                "mp41".parse().unwrap(),
            ],
            timescale: 1000,
        };
        let writer = Mp4Writer::write_start(file, &config).map_err(failure)?;
        let config = EncoderConfig::new()
            .max_frame_rate(FrameRate::from_hz(25.0))
            .skip_frames(false)
            .adaptive_quantization(false)
            .background_detection(false)
            .rate_control_mode(RateControlMode::Off)
            .usage_type(UsageType::ScreenContentRealTime);
        let encoder =
            Encoder::with_api_config(OpenH264API::from_source(), config).map_err(failure)?;
        Ok(Self {
            writer,
            encoder,
            width,
            height,
            sample_count: 0,
            track_added: false,
            pixels: vec![0; width * height * 4],
            pending: None,
        })
    }

    /// Record at a fixed canvas size. Resolution changes remain visible without invalidating AVC metadata.
    pub fn frame(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        self.frame_at(self.sample_count * 40, width, height, rgba)
    }

    /// Milliseconds on the recording timeline, with deliberate pauses removed by the caller.
    pub fn frame_at(
        &mut self,
        milliseconds: u64,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<()> {
        if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
            return Err(Error::Invalid("Invalid recording frame".into()));
        }
        if self.width == width as usize && self.height == height as usize {
            self.pixels.copy_from_slice(rgba);
        } else {
            let ratio = (self.width as f64 / width as f64).min(self.height as f64 / height as f64);
            let target_width = (width as f64 * ratio).round().max(1.0) as usize;
            let target_height = (height as f64 * ratio).round().max(1.0) as usize;
            let left = (self.width - target_width) / 2;
            let top = (self.height - target_height) / 2;
            self.pixels.fill(0);
            for y in 0..target_height {
                for x in 0..target_width {
                    let source = ((y * height as usize / target_height) * width as usize
                        + x * width as usize / target_width)
                        * 4;
                    let target = ((y + top) * self.width + x + left) * 4;
                    self.pixels[target..target + 4].copy_from_slice(&rgba[source..source + 4]);
                }
            }
        }
        let yuv =
            YUVBuffer::from_rgb_source(RgbaSliceU8::new(&self.pixels, (self.width, self.height)));
        let encoded = self.encoder.encode(&yuv).map_err(failure)?;
        let mut sample = vec![];
        let mut sps = vec![];
        let mut pps = vec![];
        let mut keyframe = false;
        for layer_index in 0..encoded.num_layers() {
            let layer = encoded
                .layer(layer_index)
                .ok_or_else(|| failure("Missing encoder layer"))?;
            for index in 0..layer.nal_count() {
                let unit = layer
                    .nal_unit(index)
                    .ok_or_else(|| failure("Missing NAL unit"))?;
                let unit = unit
                    .strip_prefix(&[0, 0, 0, 1])
                    .or_else(|| unit.strip_prefix(&[0, 0, 1]))
                    .ok_or_else(|| failure("Invalid NAL start code"))?;
                let kind = unit
                    .first()
                    .copied()
                    .ok_or_else(|| failure("Empty NAL unit"))?
                    & 31;
                match kind {
                    7 => sps = unit.to_vec(),
                    8 => pps = unit.to_vec(),
                    5 => keyframe = true,
                    _ => {}
                }
                sample.extend_from_slice(&(unit.len() as u32).to_be_bytes());
                sample.extend_from_slice(unit);
            }
        }
        if !self.track_added {
            if sps.len() < 4 || pps.is_empty() {
                return Err(failure("Encoder did not produce AVC parameters"));
            }
            let track = TrackConfig::from(AvcConfig {
                width: self.width as u16,
                height: self.height as u16,
                seq_param_set: sps,
                pic_param_set: pps,
            });
            self.writer.add_track(&track).map_err(failure)?;
            self.track_added = true;
        }
        if sample.is_empty() {
            return Err(failure("Encoder unexpectedly skipped a frame"));
        }
        if let Some(previous) = self.pending.as_mut() {
            previous.duration = u32::try_from(
                milliseconds
                    .checked_sub(previous.start_time)
                    .filter(|v| *v > 0)
                    .ok_or_else(|| failure("Frame timestamps must increase"))?,
            )
            .map_err(|_| failure("Frame duration exceeds MP4 limits"))?;
            self.writer.write_sample(1, previous).map_err(failure)?;
        } else if milliseconds != 0 {
            return Err(failure("First frame must begin at zero"));
        }
        self.pending = Some(Mp4Sample {
            start_time: milliseconds,
            duration: 40,
            rendering_offset: 0,
            is_sync: keyframe,
            bytes: Bytes::from(sample),
        });
        self.sample_count += 1;
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        let end = self.pending.as_ref().map_or(0, |s| s.start_time + 40);
        self.finish_at(end)
    }
    pub fn finish_at(mut self, milliseconds: u64) -> Result<()> {
        if !self.track_added {
            return Err(failure("Recording has no frames"));
        }
        if let Some(mut final_sample) = self.pending.take() {
            final_sample.duration =
                u32::try_from(milliseconds.saturating_sub(final_sample.start_time).max(1))
                    .map_err(|_| failure("Final frame duration exceeds MP4 limits"))?;
            self.writer
                .write_sample(1, &final_sample)
                .map_err(failure)?;
        }
        self.writer.write_end().map_err(failure)?;
        self.writer.into_writer().sync_all()?;
        Ok(())
    }
}
