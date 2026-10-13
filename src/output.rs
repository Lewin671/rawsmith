//! Final encoding: quantisation with dither and JPEG / PNG file output.
//! Files carry no camera metadata (no EXIF, GPS or serial numbers).

use std::path::Path;

use rayon::prelude::*;

use crate::error::{io_at, Error, Result};
use crate::image::Image;

// Compact ICC Profiles, CC0-1.0; provenance and licence in assets/profiles/.
const SRGB_PROFILE: &[u8] = include_bytes!("../assets/profiles/sRGB-v2-magic.icc");

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
/// Returns whether an existing destination name was replaced (including links).
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    use std::io::Write;
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", std::process::id()));
    let staging = std::path::PathBuf::from(name);
    // `create_new` refuses an existing name without following it. If it
    // fails, whatever has that name is not ours and is left alone.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|e| io_at("stage output at", &staging, e))?;
    let written = (|| {
        file.write_all(bytes).map_err(|e| io_at("write staged output", &staging, e))?;
        let replaced = destination_exists(path)?;
        std::fs::rename(&staging, path).map_err(|e| io_at("replace output", path, e))?;
        Ok(replaced)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    written
}

fn destination_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_at("inspect destination", path, e)),
    }
}

struct Temporary {
    path: std::path::PathBuf,
    remove_on_drop: bool,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn temporary_name(path: &Path) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".rawsmith.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    name.into()
}

fn reserve_temporary<T>(path: &Path, create: impl Fn(&Path) -> std::io::Result<T>) -> std::io::Result<(Temporary, T)> {
    for _ in 0..100 {
        let candidate = temporary_name(path);
        match create(&candidate) {
            Ok(value) => return Ok((Temporary { path: candidate, remove_on_drop: true }, value)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_at("reserve temporary file beside", path, e)),
        }
    }
    Err(io_at(
        "reserve temporary file beside",
        path,
        std::io::Error::new(std::io::ErrorKind::AlreadyExists, "temporary output names are occupied"),
    ))
}

fn stage(path: &Path, bytes: &[u8]) -> std::io::Result<Temporary> {
    use std::io::Write;
    let (temporary, mut file) =
        reserve_temporary(path, |candidate| std::fs::OpenOptions::new().write(true).create_new(true).open(candidate))?;
    // Keep the create_new handle: reopening the name could follow a swapped-in link.
    file.write_all(bytes).map_err(|e| io_at("write staged output", &temporary.path, e))?;
    Ok(temporary)
}

fn backup(path: &Path) -> std::io::Result<Option<Temporary>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_at("inspect destination", path, e)),
    };
    if metadata.is_file() {
        return reserve_temporary(path, |candidate| std::fs::hard_link(path, candidate))
            .map(|(temporary, ())| Some(temporary));
    }
    if metadata.is_symlink() {
        let target = std::fs::read_link(path).map_err(|e| io_at("read symbolic link", path, e))?;
        #[cfg(unix)]
        return reserve_temporary(path, |candidate| std::os::unix::fs::symlink(&target, candidate))
            .map(|(temporary, ())| Some(temporary));
        #[cfg(windows)]
        return reserve_temporary(path, |candidate| std::os::windows::fs::symlink_file(&target, candidate))
            .map(|(temporary, ())| Some(temporary));
        #[cfg(not(any(unix, windows)))]
        return Err(io_at(
            "back up symbolic link",
            path,
            std::io::Error::new(std::io::ErrorKind::Unsupported, "symbolic links are unsupported"),
        ));
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is not a file", path.display())))
}

/// Replaces two distinct destinations, restoring the first if committing the
/// second fails. Both files are staged before either destination changes.
/// This handles ordinary I/O failures; the two renames are not jointly atomic
/// and do not provide recovery after process termination or power loss.
/// Returns whether the first destination (the image) was replaced.
pub fn replace_pair(first: (&Path, &[u8]), second: (&Path, &[u8])) -> std::io::Result<bool> {
    let resolved_name = |path: &Path| {
        let absolute = std::path::absolute(path).ok()?;
        Some(std::fs::canonicalize(absolute.parent()?).ok()?.join(absolute.file_name()?))
    };
    if first.0 == second.0 || resolved_name(first.0).is_some_and(|name| Some(name) == resolved_name(second.0)) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("paired outputs {} and {} must have different names", first.0.display(), second.0.display()),
        ));
    }
    let first_stage = stage(first.0, first.1)?;
    let second_stage = stage(second.0, second.1)?;
    let mut previous = backup(first.0)?;
    let replaced = previous.is_some();
    std::fs::rename(&first_stage.path, first.0).map_err(|e| io_at("commit output", first.0, e))?;
    if let Err(commit_error) = std::fs::rename(&second_stage.path, second.0) {
        let restored = match &previous {
            Some(previous) => std::fs::rename(&previous.path, first.0),
            None => std::fs::remove_file(first.0),
        };
        if let Err(restore_error) = restored {
            let recovery = match &mut previous {
                Some(previous) => {
                    previous.remove_on_drop = false;
                    format!("old output remains at {}", previous.path.display())
                }
                None => "there was no previous output to back up".into(),
            };
            return Err(std::io::Error::new(
                commit_error.kind(),
                format!(
                    "cannot commit {}: {commit_error}; cannot restore {}: {restore_error}; {recovery}",
                    second.0.display(),
                    first.0.display()
                ),
            ));
        }
        return Err(std::io::Error::new(
            commit_error.kind(),
            format!("cannot commit {}: {commit_error}; {} restored", second.0.display(), first.0.display()),
        ));
    }
    Ok(replaced)
}

/// Encodes `image` (sRGB-encoded, 0..1). The path extension selects the
/// format: `.jpg`/`.jpeg` (8-bit, `quality` 1..100) or `.png` (16-bit, lossless).
pub fn encode(path: &Path, image: &Image, quality: u8) -> Result<Vec<u8>> {
    encode_with_color_space(path, image, quality, true)
}

/// Comparison rows may contain unconverted camera pixels whose space is not sRGB.
/// In that case, omit the output profile instead of making a false declaration.
pub fn encode_with_color_space(path: &Path, image: &Image, quality: u8, srgb: bool) -> Result<Vec<u8>> {
    let _stage = crate::profile::Stage::new("encode");
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
            if srgb {
                encoder.add_icc_profile(SRGB_PROFILE).map_err(|e| failed(&e))?;
            }
            encoder
                .encode(&to_rgb8(image), w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
                .map_err(|e| failed(&e))?;
            Ok(bytes)
        }
        "png" => {
            let mut bytes = Vec::new();
            let mut encoder = png::Encoder::new(&mut bytes, w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Sixteen);
            if srgb {
                encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            }
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().map_err(|e| failed(&e))?;
            writer.write_image_data(&to_rgb16_be(image)).map_err(|e| failed(&e))?;
            writer.finish().map_err(|e| failed(&e))?;
            Ok(bytes)
        }
        _ => Err(Error::Params(format!("unsupported output format '.{extension}'; use .jpg or .png"))),
    }
}

/// Encodes and replaces one image without opening an existing file for writing.
pub fn write(path: &Path, image: &Image, quality: u8) -> Result<()> {
    let bytes = encode(path, image, quality)?;
    replace(path, &bytes).map(|_| ()).map_err(Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PairFixture(std::path::PathBuf);

    impl PairFixture {
        fn new() -> Self {
            let dir = temporary_name(&std::env::temp_dir().join("rawsmith-pair"));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }

        fn assert_names(&self, expected: &[&str]) {
            let mut names: Vec<_> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            let mut expected: Vec<_> = expected.iter().map(|name| name.to_string()).collect();
            expected.sort();
            assert_eq!(names, expected, "temporary files must be cleaned up");
        }
    }

    impl Drop for PairFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pair_staging_failure_changes_neither_destination() {
        let f = PairFixture::new();
        let first = f.path("preview");
        let second = f.path("missing/recipe");
        std::fs::write(&first, b"old preview").unwrap();
        assert!(replace_pair((&first, b"new preview"), (&second, b"new recipe")).is_err());
        assert_eq!(std::fs::read(&first).unwrap(), b"old preview");
        assert!(!second.exists());
        f.assert_names(&["preview"]);
    }

    #[test]
    fn temporary_name_collision_preserves_the_foreign_file() {
        let f = PairFixture::new();
        let foreign = std::cell::RefCell::new(None);
        let (temporary, file) = reserve_temporary(&f.path("preview"), |candidate| {
            if foreign.borrow().is_none() {
                std::fs::write(candidate, b"not ours")?;
                *foreign.borrow_mut() = Some(candidate.to_owned());
            }
            std::fs::OpenOptions::new().write(true).create_new(true).open(candidate)
        })
        .unwrap();
        drop(file);
        drop(temporary);
        let foreign = foreign.into_inner().unwrap();
        assert_eq!(std::fs::read(&foreign).unwrap(), b"not ours");
        assert_eq!(std::fs::read_dir(&f.0).unwrap().count(), 1);
    }

    #[test]
    fn pair_second_commit_failure_restores_the_old_inode() {
        let f = PairFixture::new();
        let (first, second, original) = (f.path("preview"), f.path("recipe"), f.path("original"));
        std::fs::write(&original, b"old preview").unwrap();
        std::fs::hard_link(&original, &first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(second.join("keep"), b"untouched").unwrap();
        assert!(replace_pair((&first, b"new preview"), (&second, b"new recipe")).is_err());
        assert_eq!(std::fs::read(&first).unwrap(), b"old preview");
        assert_eq!(std::fs::read(&original).unwrap(), b"old preview");
        assert_eq!(std::fs::read(second.join("keep")).unwrap(), b"untouched");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (restored, original) = (std::fs::metadata(&first).unwrap(), std::fs::metadata(&original).unwrap());
            assert_eq!((restored.dev(), restored.ino()), (original.dev(), original.ino()));
        }
        f.assert_names(&["preview", "recipe", "original"]);
    }

    #[test]
    fn pair_second_commit_failure_removes_a_new_first_destination() {
        let f = PairFixture::new();
        let (first, second) = (f.path("preview"), f.path("recipe"));
        std::fs::create_dir(&second).unwrap();
        assert!(replace_pair((&first, b"new preview"), (&second, b"new recipe")).is_err());
        assert!(!first.exists());
        assert!(second.is_dir());
        f.assert_names(&["recipe"]);
    }

    #[test]
    fn pair_success_replaces_names_without_writing_through_hard_links() {
        let f = PairFixture::new();
        let (first, second, original) = (f.path("preview"), f.path("recipe"), f.path("original"));
        std::fs::write(&original, b"original").unwrap();
        std::fs::hard_link(&original, &first).unwrap();
        std::fs::hard_link(&original, &second).unwrap();
        replace_pair((&first, b"new preview"), (&second, b"new recipe")).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"new preview");
        assert_eq!(std::fs::read(&second).unwrap(), b"new recipe");
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
        f.assert_names(&["preview", "recipe", "original"]);
    }

    #[cfg(unix)]
    #[test]
    fn pair_preserves_symlink_targets_and_restores_dangling_links() {
        let f = PairFixture::new();
        let (first, second, original) = (f.path("preview"), f.path("recipe"), f.path("original"));
        std::fs::write(&original, b"original").unwrap();
        std::os::unix::fs::symlink("original", &first).unwrap();
        std::os::unix::fs::symlink("original", &second).unwrap();
        replace_pair((&first, b"new preview"), (&second, b"new recipe")).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"new preview");
        assert_eq!(std::fs::read(&second).unwrap(), b"new recipe");
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
        std::fs::remove_file(&second).unwrap();
        std::fs::create_dir(&second).unwrap();
        for target in ["original", "missing"] {
            std::fs::remove_file(&first).unwrap();
            std::os::unix::fs::symlink(target, &first).unwrap();
            assert!(replace_pair((&first, b"new preview"), (&second, b"new recipe")).is_err());
            assert_eq!(std::fs::read_link(&first).unwrap(), Path::new(target));
            assert_eq!(std::fs::read(&original).unwrap(), b"original");
            f.assert_names(&["preview", "recipe", "original"]);
        }
    }

    #[test]
    fn pair_rejects_two_names_for_the_same_destination() {
        let f = PairFixture::new();
        let first = f.path("preview");
        std::fs::write(&first, b"original").unwrap();
        std::fs::create_dir(f.path("sub")).unwrap();
        for second in [first.clone(), f.path("sub/../preview")] {
            assert!(replace_pair((&first, b"new preview"), (&second, b"new recipe")).is_err());
            assert_eq!(std::fs::read(&first).unwrap(), b"original");
            f.assert_names(&["preview", "sub"]);
        }
    }

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
    fn jpeg_embeds_only_srgb_metadata_without_changing_the_codestream_or_pixels() {
        let image = picture(33, 21);
        let profiled = encode(Path::new("synthetic.jpg"), &image, 92).unwrap();
        let mut baseline = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut baseline, 92);
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_1_1);
        encoder.set_optimized_huffman_tables(true);
        encoder.encode(&to_rgb8(&image), 33, 21, jpeg_encoder::ColorType::Rgb).unwrap();

        let mut without_profile = profiled[..2].to_vec();
        let mut at = 2;
        let mut profiles = 0;
        while profiled[at + 1] != 0xda {
            assert_eq!(profiled[at], 0xff);
            let marker = profiled[at + 1];
            let len = u16::from_be_bytes([profiled[at + 2], profiled[at + 3]]) as usize;
            let payload = &profiled[at + 4..at + 2 + len];
            if marker == 0xe2 {
                assert_eq!(&payload[..14], b"ICC_PROFILE\0\x01\x01");
                assert_eq!(&payload[14..], SRGB_PROFILE);
                profiles += 1;
            } else {
                // No EXIF, XMP, camera comments or other application metadata.
                assert!(!(0xe1..=0xef).contains(&marker) && marker != 0xfe);
                if marker == 0xe0 {
                    assert!(payload.starts_with(b"JFIF\0"));
                }
                without_profile.extend_from_slice(&profiled[at..at + 2 + len]);
            }
            at += 2 + len;
        }
        without_profile.extend_from_slice(&profiled[at..]);
        assert_eq!(profiles, 1);
        assert_eq!(without_profile, baseline, "only the ICC segment may change");
        let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(&profiled));
        let pixels = decoder.decode().unwrap();
        assert_eq!(decoder.icc_profile().unwrap(), SRGB_PROFILE);
        assert_eq!(pixels, jpeg_decoder::Decoder::new(std::io::Cursor::new(&baseline)).decode().unwrap());
        assert_eq!(SRGB_PROFILE.len(), 736);
        assert_eq!(&SRGB_PROFILE[8..12], &[2, 0x10, 0, 0], "ICC profile must be v2.1");
        assert_eq!(&SRGB_PROFILE[36..40], b"acsp");
        assert_eq!(&SRGB_PROFILE[16..20], b"RGB ");
        // Read the ICC v2 description tag: a filename alone cannot identify sRGB.
        let read_u32 = |bytes: &[u8]| u32::from_be_bytes(bytes.try_into().unwrap()) as usize;
        let tag_count = read_u32(&SRGB_PROFILE[128..132]);
        let description_entry = SRGB_PROFILE[132..132 + tag_count * 12]
            .as_chunks::<12>()
            .0
            .iter()
            .find(|entry| &entry[..4] == b"desc")
            .expect("ICC profile must have a description tag");
        let offset = read_u32(&description_entry[4..8]);
        let size = read_u32(&description_entry[8..12]);
        let description = &SRGB_PROFILE[offset..offset + size];
        assert_eq!(&description[..4], b"desc", "ICC v2 description type");
        let ascii_len = read_u32(&description[8..12]);
        assert_eq!(&description[12..12 + ascii_len], b"sRGB\0");
    }

    #[test]
    fn png_is_16_bit_and_exact() {
        let path = std::env::temp_dir().join(format!("rawsmith-png-{}.png", std::process::id()));
        let image = picture(33, 21);
        write(&path, &image, 90).unwrap();
        let mut reader = png::Decoder::new(std::fs::File::open(&path).unwrap()).read_info().unwrap();
        assert_eq!(reader.info().srgb, Some(png::SrgbRenderingIntent::Perceptual));
        let mut buffer = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buffer).unwrap();
        assert_eq!((info.width, info.height, info.bit_depth), (33, 21, png::BitDepth::Sixteen));
        assert_eq!(&buffer[..info.buffer_size()], to_rgb16_be(&image));
        let bytes = std::fs::read(&path).unwrap();
        let mut at = 8;
        while at < bytes.len() {
            let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = &bytes[at + 4..at + 8];
            assert!(
                [b"IHDR", b"sRGB", b"gAMA", b"cHRM", b"IDAT", b"IEND"].iter().any(|allowed| *allowed == kind),
                "unexpected PNG metadata: {kind:?}"
            );
            at += 12 + len;
        }
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
