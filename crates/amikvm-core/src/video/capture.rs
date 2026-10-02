//! Encode a captured remote framebuffer without a platform image library.
use crate::{Error, Result};

pub fn jpeg(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let pixels = width as u64 * height as u64;
    if width == 0
        || height == 0
        || width > u16::MAX as u32
        || height > u16::MAX as u32
        || pixels > super::MAX_PIXELS as u64
        || rgba.len() as u64 != pixels * 4
    {
        return Err(Error::Invalid(
            "Invalid screenshot dimensions or pixels".into(),
        ));
    }
    let mut rgb = Vec::with_capacity(pixels as usize * 3);
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    let mut encoded = Vec::new();
    jpeg_encoder::Encoder::new(&mut encoded, 90)
        .encode(
            &rgb,
            width as u16,
            height as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|e| Error::Invalid(format!("JPEG encoding failed: {e}")))?;
    Ok(encoded)
}
