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

/// Puts `bytes` at `path` by writing a new file beside it and renaming it
/// into place. Nothing that already exists is ever opened for writing: an
/// interrupted write leaves the old file intact, and a destination that is a
/// symlink or hard link to some other file (the RAW, say) is replaced as a
/// name while that other file's contents stay untouched.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", std::process::id()));
    let staging = std::path::PathBuf::from(name);
    // `create_new` refuses an existing name without following it. If it
    // fails, whatever has that name is not ours and is left alone.
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&staging)?;
    let written = file.write_all(bytes).and_then(|()| std::fs::rename(&staging, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    written
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
            let mut bytes = Vec::new();
            let mut encoder = jpeg_encoder::Encoder::new(&mut bytes, quality.clamp(1, 100));
            // Always full chroma resolution. Subsampled (4:2:0) files from this
            // encoder decode correctly in libjpeg and ImageIO, but an image
            // viewer used by AI assistants showed them scrambled, and a preview
            // the assistant cannot see correctly defeats the purpose.
            encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
            encoder.set_optimized_huffman_tables(true);
            encoder
                .encode(&to_rgb8(image), w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
                .map_err(|e| failed(&e))?;
            replace(path, &bytes).map_err(|e| failed(&e))
        }
        "png" => {
            let mut bytes = Vec::new();
            let mut encoder = png::Encoder::new(&mut bytes, w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Sixteen);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().map_err(|e| failed(&e))?;
            writer.write_image_data(&to_rgb16_be(image)).map_err(|e| failed(&e))?;
            writer.finish().map_err(|e| failed(&e))?;
            replace(path, &bytes).map_err(|e| failed(&e))
        }
        _ => Err(Error::Params(format!("unsupported output format '.{extension}'; use .jpg or .png"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A colourful picture with sharp edges and fine texture, sRGB-encoded.
    fn picture(width: usize, height: usize) -> Image {
        let mut image = Image::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
                let block = if (x / 16 + y / 16) % 2 == 0 { 0.15 } else { 0.0 };
                let grain = 0.1 * dither(y * width + x);
                image.data[y * width + x] =
                    [0.2 + 0.6 * u + grain, 0.7 - 0.5 * v + block + grain, 0.3 + 0.4 * u * v + grain];
            }
        }
        image
    }

    #[test]
    fn jpeg_round_trips_at_every_quality_and_odd_sizes() {
        let dir = std::env::temp_dir().join(format!("rawsmith-jpeg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (width, height) in [(160, 96), (163, 99), (17, 9), (900, 600), (1400, 933)] {
            let image = picture(width, height);
            for quality in [40, 75, 85, 89, 90, 95, 100] {
                let path = dir.join(format!("{width}x{height}-{quality}.jpg"));
                write(&path, &image, quality).unwrap();
                let mut decoder = jpeg_decoder::Decoder::new(std::fs::File::open(&path).unwrap());
                let pixels = decoder.decode().unwrap();
                let info = decoder.info().unwrap();
                assert_eq!((info.width as usize, info.height as usize), (width, height));
                let error: f32 = pixels
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(&image.data)
                    .map(|(got, want)| (0..3).map(|c| (got[c] as f32 / 255.0 - want[c]).abs()).sum::<f32>() / 3.0)
                    .sum::<f32>()
                    / image.data.len() as f32;
                // Lossy, but the picture must be the same picture.
                assert!(error < 0.03, "{width}x{height} at quality {quality}: mean error {error}");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replace_swaps_the_file_and_never_touches_a_squatter() {
        let path = std::env::temp_dir().join(format!("rawsmith-replace-{}.txt", std::process::id()));
        replace(&path, b"one").unwrap();
        replace(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        // Something else already has the staging name: fail and leave it alone.
        let mut squatter = path.clone().into_os_string();
        squatter.push(format!(".{}.tmp", std::process::id()));
        std::fs::write(&squatter, b"not ours").unwrap();
        assert!(replace(&path, b"three").is_err());
        assert_eq!(std::fs::read(&squatter).unwrap(), b"not ours");
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        std::fs::remove_file(&squatter).unwrap();
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn png_is_16_bit_and_exact() {
        let path = std::env::temp_dir().join(format!("rawsmith-png-{}.png", std::process::id()));
        let image = picture(33, 21);
        write(&path, &image, 90).unwrap();
        let mut reader = png::Decoder::new(std::fs::File::open(&path).unwrap()).read_info().unwrap();
        let mut buffer = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((info.width, info.height, info.bit_depth), (33, 21, png::BitDepth::Sixteen));
        for (got, want) in buffer.as_chunks::<6>().0.iter().zip(&image.data) {
            for c in 0..3 {
                let value = u16::from_be_bytes([got[c * 2], got[c * 2 + 1]]) as f32 / 65535.0;
                assert!((value - want[c]).abs() < 1e-4);
            }
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn dither_is_small_and_unbiased() {
        let values: Vec<f32> = (0..100_000).map(dither).collect();
        assert!(values.iter().all(|v| v.abs() <= 0.5));
        assert!((values.iter().sum::<f32>() / values.len() as f32).abs() < 0.01);
    }
}
