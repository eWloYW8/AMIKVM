use amikvm_core::video::{Cursor, Decoder};
use std::{collections::HashMap, sync::Arc};
use tauri::ipc::{Channel, Response};

#[derive(Default)]
pub struct Video {
    latest: Option<Arc<Vec<u8>>>,
    channels: HashMap<u32, Channel<Response>>,
    sequence: u32,
    signal: bool,
    reference: Option<amikvm_core::input::mouse::Point>,
    pub recording: Option<crate::recording::Recording>,
}

impl Video {
    pub fn publish(&mut self, decoder: &Decoder, cursor: &Cursor) {
        self.publish_pixels(decoder.width, decoder.height, &decoder.rgba, Some(cursor));
    }
    pub fn update_cursor(&mut self, decoder: &Decoder, cursor: &Cursor) {
        if self.signal {
            self.publish(decoder, cursor);
        }
    }
    pub fn no_signal(&mut self) {
        self.signal = false;
        if let Some(recording) = &self.recording {
            recording.frame(None);
        }
    }

    pub fn publish_pixels(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
        cursor: Option<&Cursor>,
    ) {
        self.signal = true;
        self.sequence = self.sequence.wrapping_add(1);
        let mut bytes = Vec::with_capacity(12 + rgba.len());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(rgba);
        if let Some(cursor) = cursor {
            cursor.overlay(width, height, &mut bytes[12..]);
        }
        let frame = Arc::new(bytes);
        if let Some(recording) = &self.recording {
            recording.frame(amikvm_core::recording::Frame::from_packet(frame.clone()).ok());
        }
        self.latest = Some(frame);
        self.display();
    }

    /// The local reference is display-only; exports and recording retain the BMC frame.
    pub fn reference(&mut self, point: Option<amikvm_core::input::mouse::Point>) {
        if self.reference != point {
            self.reference = point;
            self.display();
        }
    }
    fn display_frame(&self) -> Option<Vec<u8>> {
        let mut bytes = self.latest.as_ref()?.as_ref().clone();
        if let Some(p) = self
            .reference
            .filter(|p| p.width > 0 && p.height > 0 && self.signal)
        {
            let width = u32::from_le_bytes(bytes[..4].try_into().ok()?);
            let height = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
            let x = (p.x * f64::from(width) / f64::from(p.width)).round() as i32;
            let y = (p.y * f64::from(height) / f64::from(p.height)).round() as i32;
            for offset in -6..=6 {
                for (px, py) in [(x + offset, y), (x, y + offset)] {
                    if px >= 0 && py >= 0 && (px as u32) < width && (py as u32) < height {
                        let i = 12 + (py as usize * width as usize + px as usize) * 4;
                        if let Some(pixel) = bytes.get_mut(i..i + 4) {
                            pixel.copy_from_slice(&[255, 64, 64, 255]);
                        }
                    }
                }
            }
        }
        Some(bytes)
    }
    fn display(&mut self) {
        if let Some(bytes) = self.display_frame() {
            self.channels
                .retain(|_, channel| channel.send(Response::new(bytes.clone())).is_ok());
        }
    }

    pub fn subscribe(&mut self, channel: Channel<Response>) -> Result<u32, String> {
        let id = channel.id();
        if let Some(frame) = self.display_frame() {
            channel
                .send(Response::new(frame))
                .map_err(|e| e.to_string())?;
        }
        self.channels.insert(id, channel);
        Ok(id)
    }
    pub fn unsubscribe(&mut self, id: u32) {
        self.channels.remove(&id);
    }
    pub fn close(&mut self) {
        self.channels.clear();
    }
    pub fn clear_frame(&mut self) {
        self.no_signal();
        self.reference = None;
        self.latest = None;
    }
    pub fn latest(&self) -> Option<Arc<Vec<u8>>> {
        self.latest.clone()
    }
    pub fn recording_frame(&self) -> amikvm_core::Result<Option<amikvm_core::recording::Frame>> {
        if !self.signal {
            return Ok(None);
        }
        self.latest
            .clone()
            .map(amikvm_core::recording::Frame::from_packet)
            .transpose()
    }
}
