//! Optional on-disk reuse of the developed native scene.
//!
//! An editing session renders the same photo many times while only tone and
//! colour settings change. Everything up to noise reduction depends on the
//! sensor data and a handful of settings, so that scene-linear frame can be
//! kept between invocations and finishing started from it. The stored pixels
//! are the exact float bits development produced: a hit and a miss render the
//! same picture.
//!
//! The library never caches on its own; the CLI opts in with [`configure`].

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use crate::arw::RawImage;
use crate::image::Image;

const MAGIC: &[u8; 8] = b"RSSCENE1";
const HEADER: usize = MAGIC.len() + 16 + 16;
/// Development of smaller frames is quicker than reading them back is worth.
const MIN_PIXELS: usize = 1_000_000;
/// Default bound on stored scenes: about seven 33 MP frames.
const DEFAULT_LIMIT_MB: u64 = 3000;
/// Scenes nobody has rendered for this long are removed on the next store.
const MAX_AGE: Duration = Duration::from_secs(3 * 24 * 3600);

#[derive(Clone, Debug)]
pub struct Config {
    pub dir: PathBuf,
    pub limit_bytes: u64,
    /// Distinguishes builds: stored pixels are only valid for the code that made them.
    pub build: [u64; 2],
}

static CONFIG: OnceLock<Option<Config>> = OnceLock::new();

/// Enables or disables scene reuse for this process. Only the first call counts.
pub fn configure(config: Option<Config>) {
    let _ = CONFIG.set(config);
}

fn config() -> Option<&'static Config> {
    CONFIG.get().and_then(Option::as_ref)
}

impl Config {
    /// `RAWSMITH_CACHE_MB=0` disables reuse; `RAWSMITH_CACHE_DIR` moves it.
    /// The build identity is the running executable's content, so a rebuilt
    /// binary never reads another build's pixels.
    pub fn from_env() -> Option<Config> {
        let limit_mb = match std::env::var("RAWSMITH_CACHE_MB") {
            Ok(text) => text.trim().parse::<u64>().ok()?,
            Err(_) => DEFAULT_LIMIT_MB,
        };
        if limit_mb == 0 {
            return None;
        }
        let dir = match std::env::var_os("RAWSMITH_CACHE_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => default_dir()?,
        };
        let exe = fs::read(std::env::current_exe().ok()?).ok()?;
        Some(Config { dir, limit_bytes: limit_mb.saturating_mul(1_000_000), build: hash_bytes(&exe) })
    }
}

fn default_dir() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(target_os = "macos") {
        Some(var("HOME")?.join("Library/Caches/rawsmith"))
    } else if cfg!(windows) {
        Some(var("LOCALAPPDATA")?.join("rawsmith").join("cache"))
    } else {
        Some(var("XDG_CACHE_HOME").or_else(|| Some(var("HOME")?.join(".cache")))?.join("rawsmith"))
    }
}

/// Two independent 64-bit multiply-xorshift lanes. This guards against
/// accidental collisions between photos and settings, not a hostile file.
#[derive(Clone, Copy)]
struct Hash([u64; 2]);

impl Hash {
    const fn new() -> Self {
        Hash([0x9E37_79B9_7F4A_7C15, 0xC2B2_AE3D_27D4_EB4F])
    }
    #[inline]
    fn word(&mut self, w: u64) {
        let [a, b] = &mut self.0;
        *a = (*a ^ w).wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        *a ^= *a >> 32;
        *b = (b.rotate_left(27) ^ w).wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        *b ^= *b >> 29;
    }
    fn bytes(&mut self, bytes: &[u8]) {
        self.word(bytes.len() as u64);
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.word(u64::from_le_bytes(*w));
        }
        let mut last = [0u8; 8];
        last[..rest.len()].copy_from_slice(rest);
        self.word(u64::from_le_bytes(last));
    }
    fn floats(&mut self, values: &[f32]) {
        self.word(values.len() as u64);
        for v in values {
            self.word(v.to_bits() as u64);
        }
    }
    fn samples(&mut self, values: &[i16]) {
        self.word(values.len() as u64);
        for v in values {
            self.word(*v as u16 as u64);
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> [u64; 2] {
    let mut hash = Hash::new();
    hash.bytes(bytes);
    hash.0
}

/// Sensor samples are hashed in fixed blocks so the result does not depend on
/// the thread count.
fn hash_mosaic(data: &[u16]) -> [u64; 2] {
    let blocks: Vec<[u64; 2]> = data
        .par_chunks(1 << 18)
        .map(|block| {
            let mut hash = Hash::new();
            let (words, rest) = block.as_chunks::<4>();
            for w in words {
                hash.word(w[0] as u64 | (w[1] as u64) << 16 | (w[2] as u64) << 32 | (w[3] as u64) << 48);
            }
            for &v in rest {
                hash.word(v as u64);
            }
            hash.0
        })
        .collect();
    let mut hash = Hash::new();
    hash.word(data.len() as u64);
    for [a, b] in blocks {
        hash.word(a);
        hash.word(b);
    }
    hash.0
}

/// Everything native scene development reads, before orientation, rotation
/// and crop. `None` when reuse is disabled.
pub(crate) struct Inputs<'a> {
    pub raw: &'a RawImage,
    pub gains: [f32; 3],
    pub camera_to_working: [[f32; 3]; 3],
    pub lens_corrections: bool,
    pub defringe: f64,
    pub denoise: f64,
}

pub(crate) type Key = [u8; 16];

pub(crate) fn key(inputs: &Inputs) -> Option<Key> {
    let config = config()?;
    let raw = inputs.raw;
    if raw.crop.width * raw.crop.height < MIN_PIXELS {
        return None;
    }
    let mut hash = Hash::new();
    hash.word(config.build[0]);
    hash.word(config.build[1]);
    let [a, b] = hash_mosaic(&raw.data);
    hash.word(a);
    hash.word(b);
    for v in [raw.width, raw.height, raw.crop.x, raw.crop.y, raw.crop.width, raw.crop.height] {
        hash.word(v as u64);
    }
    hash.bytes(&raw.cfa);
    hash.floats(&raw.black);
    hash.floats(&[raw.white]);
    hash.floats(&inputs.gains);
    hash.floats(inputs.camera_to_working.as_flattened());
    hash.word(inputs.defringe.to_bits());
    hash.word(inputs.denoise.to_bits());
    match raw.lens.as_ref().filter(|_| inputs.lens_corrections) {
        Some(lens) => {
            hash.word(1);
            for table in [&lens.distortion, &lens.ca_red, &lens.ca_blue, &lens.vignetting] {
                hash.samples(table);
            }
        }
        None => hash.word(0),
    }
    let mut key = [0u8; 16];
    key[..8].copy_from_slice(&hash.0[0].to_le_bytes());
    key[8..].copy_from_slice(&hash.0[1].to_le_bytes());
    Some(key)
}

fn path(dir: &Path, key: &Key) -> PathBuf {
    let name: String = key.iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{name}.scene"))
}

// f32 has no padding and every bit pattern is a valid value, so pixels can
// be read and written as plain bytes without copying 400 MB through a buffer.
fn pixel_bytes(data: &[[f32; 3]]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast(), std::mem::size_of_val(data)) }
}

fn pixel_bytes_mut(data: &mut [[f32; 3]]) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr().cast(), std::mem::size_of_val(data)) }
}

/// A stored scene for `key`, or `None` on any mismatch or I/O problem.
pub(crate) fn load(key: &Key) -> Option<Image> {
    load_from(&config()?.dir, key)
}

fn load_from(dir: &Path, key: &Key) -> Option<Image> {
    let file = path(dir, key);
    let mut input = File::open(&file).ok()?;
    let mut header = [0u8; HEADER];
    input.read_exact(&mut header).ok()?;
    let (magic, rest) = header.split_at(MAGIC.len());
    let (stored_key, dims) = rest.split_at(16);
    let width = u64::from_le_bytes(dims[..8].try_into().unwrap());
    let height = u64::from_le_bytes(dims[8..].try_into().unwrap());
    let pixels = width.checked_mul(height)?;
    let expected = pixels.checked_mul(12)?.checked_add(HEADER as u64)?;
    if magic != MAGIC || stored_key != key || input.metadata().ok()?.len() != expected {
        return None;
    }
    let mut image = Image::new(usize::try_from(width).ok()?, usize::try_from(height).ok()?);
    input.read_exact(pixel_bytes_mut(&mut image.data)).ok()?;
    // Mark the scene as recently used; eviction removes the oldest first.
    if let Ok(touch) = File::options().write(true).open(&file) {
        let _ = touch.set_modified(SystemTime::now());
    }
    Some(image)
}

/// Stores a scene and trims the directory to its limit. Failures leave the
/// render unaffected: reuse is an optimisation, never a requirement.
pub(crate) fn store(key: &Key, image: &Image) -> bool {
    config().is_some_and(|config| store_in(&config.dir, config.limit_bytes, key, image))
}

fn store_in(dir: &Path, limit: u64, key: &Key, image: &Image) -> bool {
    let size = (HEADER + std::mem::size_of_val(image.data.as_slice())) as u64;
    if size > limit || fs::create_dir_all(dir).is_err() {
        return false;
    }
    let target = path(dir, key);
    let staged = target.with_extension(format!("tmp{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut out = File::create(&staged)?;
        out.write_all(MAGIC)?;
        out.write_all(key)?;
        out.write_all(&(image.width as u64).to_le_bytes())?;
        out.write_all(&(image.height as u64).to_le_bytes())?;
        out.write_all(pixel_bytes(&image.data))?;
        drop(out);
        fs::rename(&staged, &target)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&staged);
        return false;
    }
    trim(dir, limit);
    true
}

/// Keeps the most recently used scenes within `limit`, and removes expired
/// scenes and staging files abandoned by interrupted runs.
fn trim(dir: &Path, limit: u64) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    let mut scenes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let age = meta.modified().ok().and_then(|at| now.duration_since(at).ok()).unwrap_or_default();
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
        if extension == "scene" && age <= MAX_AGE {
            scenes.push((age, meta.len(), path));
        } else if extension == "scene" || (extension.starts_with("tmp") && age > Duration::from_secs(3600)) {
            let _ = fs::remove_file(path);
        }
    }
    scenes.sort_by_key(|(age, ..)| *age);
    let mut total = 0u64;
    for (_, len, path) in scenes {
        total = total.saturating_add(len);
        if total > limit {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rawsmith-cache-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn sample(width: usize, height: usize, seed: f32) -> Image {
        let mut image = Image::new(width, height);
        for (i, p) in image.data.iter_mut().enumerate() {
            *p = [seed + i as f32, -0.0, f32::from_bits(0x7fc0_1234 + i as u32)];
        }
        image
    }

    #[test]
    fn stored_scene_returns_the_same_bits() {
        let dir = scratch("bits");
        let image = sample(7, 5, 0.25);
        assert!(store_in(&dir, 1 << 20, &[3; 16], &image));
        let loaded = load_from(&dir, &[3; 16]).unwrap();
        assert_eq!((loaded.width, loaded.height), (7, 5));
        assert_eq!(pixel_bytes(&loaded.data), pixel_bytes(&image.data));
        assert!(load_from(&dir, &[4; 16]).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn damaged_or_mislabelled_files_are_ignored() {
        let dir = scratch("damaged");
        let image = sample(4, 4, 1.0);
        assert!(store_in(&dir, 1 << 20, &[1; 16], &image));
        let file = path(&dir, &[1; 16]);
        let bytes = fs::read(&file).unwrap();
        // Truncated pixels, a foreign key under this name, and a wrong magic.
        fs::write(&file, &bytes[..bytes.len() - 1]).unwrap();
        assert!(load_from(&dir, &[1; 16]).is_none());
        fs::write(path(&dir, &[2; 16]), &bytes).unwrap();
        assert!(load_from(&dir, &[2; 16]).is_none());
        let mut wrong = bytes.clone();
        wrong[0] ^= 1;
        fs::write(&file, &wrong).unwrap();
        assert!(load_from(&dir, &[1; 16]).is_none());
        // Dimensions that overflow or disagree with the file length.
        let mut huge = bytes;
        huge[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&file, &huge).unwrap();
        assert!(load_from(&dir, &[1; 16]).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_limit_keeps_the_most_recently_used_scenes() {
        let dir = scratch("limit");
        let image = sample(8, 8, 2.0);
        let size = (HEADER + 8 * 8 * 12) as u64;
        let at = |seconds: u64| SystemTime::now() - Duration::from_secs(seconds);
        for (key, age) in [([1u8; 16], 300), ([2; 16], 200), ([3; 16], 100)] {
            assert!(store_in(&dir, size * 3, &key, &image));
            File::options().write(true).open(path(&dir, &key)).unwrap().set_modified(at(age)).unwrap();
        }
        // Reading the oldest makes it the newest; the next store evicts key 2.
        assert!(load_from(&dir, &[1; 16]).is_some());
        assert!(store_in(&dir, size * 3, &[4; 16], &image));
        assert!(load_from(&dir, &[2; 16]).is_none());
        for key in [[1u8; 16], [3; 16], [4; 16]] {
            assert!(load_from(&dir, &key).is_some(), "{key:?}");
        }
        // Expired scenes go regardless of room; a scene larger than the limit is not stored.
        File::options().write(true).open(path(&dir, &[3; 16])).unwrap().set_modified(at(4 * 24 * 3600)).unwrap();
        assert!(store_in(&dir, size * 9, &[5; 16], &image));
        assert!(load_from(&dir, &[3; 16]).is_none());
        assert!(!store_in(&dir, size - 1, &[6; 16], &image));
        assert!(load_from(&dir, &[6; 16]).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn mosaic_hash_is_independent_of_thread_count_and_sees_every_sample() {
        let data: Vec<u16> = (0..(1 << 19) + 7).map(|i| (i * 31 % 16384) as u16).collect();
        let reference = hash_mosaic(&data);
        for threads in [1, 3] {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            assert_eq!(pool.install(|| hash_mosaic(&data)), reference);
        }
        for index in [0, 5, 1 << 18, data.len() - 1] {
            let mut changed = data.clone();
            changed[index] ^= 1;
            assert_ne!(hash_mosaic(&changed), reference, "sample {index}");
        }
        assert_ne!(hash_mosaic(&data[..data.len() - 1]), reference);
    }
}
