//! Final encoding: quantisation with dither and JPEG / PNG file output.
//! Files carry no camera metadata (no EXIF, GPS or serial numbers).

use std::path::Path;

use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::image::Image;

/// Deterministic per-sample noise in -0.5..0.5 with a triangular
/// distribution; it breaks up banding in smooth gradients such as skies.
#[inline]
fn dither(index: usize) -> f32 {
    let hash = |mut v: u32| {
        v ^= v >> 16;
        v = v.wrapping_mul(0x7feb_352d);
        v ^= v >> 15;
        v = v.wrapping_mul(0x846c_a68b);
        v ^= v >> 16;
        v as f32 / u32::MAX as f32
    };
    (hash(index as u32) + hash((index as u32) ^ 0x9e37_79b9) - 1.0) * 0.5
}

/// Quantises an sRGB-encoded 0..1 image to 8 bits per channel.
pub fn to_rgb8(image: &Image) -> Vec<u8> {
    let mut out = vec![0u8; image.data.len() * 3];
    out.par_chunks_mut(3).zip(&image.data).enumerate().for_each(|(i, (o, p))| {
        for c in 0..3 {
            o[c] = (p[c] * 255.0 + 0.5 + dither(i * 3 + c)).clamp(0.0, 255.0) as u8;
        }
    });
    out
}

fn to_rgb16_be(image: &Image) -> Vec<u8> {
    let mut out = vec![0u8; image.data.len() * 6];
    out.par_chunks_mut(6).zip(&image.data).for_each(|(o, p)| {
        for c in 0..3 {
            let v = (p[c] * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16;
            o[c * 2..c * 2 + 2].copy_from_slice(&v.to_be_bytes());
        }
    });
    out
}

/// Writes `image` (sRGB-encoded, 0..1) to `path`. The extension selects the
/// format: `.jpg`/`.jpeg` (8-bit, `quality` 1..100) or `.png` (16-bit, lossless).
pub fn write(path: &Path, image: &Image, quality: u8) -> Result<()> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let (w, h) = (image.width, image.height);
    let failed =
        |e: &dyn std::fmt::Display| Error::Io(std::io::Error::other(format!("cannot write {}: {e}", path.display())));
    match extension.as_str() {
        "jpg" | "jpeg" => {
            if w > u16::MAX as usize || h > u16::MAX as usize {
                return Err(Error::Params("image is too large for JPEG; use --size or PNG".into()));
            }
            let mut encoder = jpeg_encoder::Encoder::new_file(path, quality.clamp(1, 100)).map_err(|e| failed(&e))?;
            // Full chroma resolution at high quality, as cameras and editors do.
            let sampling =
                if quality >= 90 { jpeg_encoder::SamplingFactor::F_1_1 } else { jpeg_encoder::SamplingFactor::F_2_2 };
            encoder.set_sampling_factor(sampling);
            encoder.set_optimized_huffman_tables(true);
            encoder.encode(&to_rgb8(image), w as u16, h as u16, jpeg_encoder::ColorType::Rgb).map_err(|e| failed(&e))
        }
        "png" => {
            let file = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| failed(&e))?);
            let mut encoder = png::Encoder::new(file, w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Sixteen);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().map_err(|e| failed(&e))?;
            writer.write_image_data(&to_rgb16_be(image)).map_err(|e| failed(&e))
        }
        _ => Err(Error::Params(format!("unsupported output format '.{extension}'; use .jpg or .png"))),
    }
}
