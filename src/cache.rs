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

const MAGIC: &[u8; 8] = b"RSSCENE2";
/// Magic, key, width and height, then a checksum of the pixel bytes.
const HEADER: usize = MAGIC.len() + 16 + 16 + 16;
/// Development of smaller frames is quicker than reading them back is worth.
const MIN_PIXELS: usize = 1_000_000;
/// Default bound on stored scenes: about fifteen 33 MP frames, so a batch
/// edited photo by photo or pass by pass keeps every frame it returns to.
const DEFAULT_LIMIT_MB: u64 = 6000;
/// Scenes nobody has rendered for this long are removed on the next store.
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

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

/// The per-user temporary directory: assistants often run sandboxed with
/// only the working and temporary directories writable, and the system
/// clears it without anyone having to manage a cache.
fn default_dir() -> Option<PathBuf> {
    let user = ["USER", "USERNAME"].iter().find_map(|name| std::env::var(name).ok()).unwrap_or_default();
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect();
    Some(std::env::temp_dir().join(if user.is_empty() {
        "rawsmith-scenes".into()
    } else {
        format!("rawsmith-scenes-{user}")
    }))
}

/// Creates the directory private to this user, and refuses one that someone
/// else owns or can write: a shared temporary directory must not let another
/// account substitute pixels. Nothing is written until the directory itself
/// has passed, and the ownership probe is created exclusively, so a name
/// planted there, such as a link to a photo, is never opened or truncated.
fn private_dir(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
        let Ok(meta) = fs::symlink_metadata(dir) else { return false };
        if !meta.is_dir() || meta.mode() & 0o022 != 0 {
            return false;
        }
        // std has no way to ask for the process's user; a file it creates has it.
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let probe = dir.join(format!("owner-{}-{nanos}", std::process::id()));
        let Ok(file) = File::options().write(true).create_new(true).open(&probe) else { return false };
        let owner = file.metadata().map(|meta| meta.uid());
        drop(file);
        let _ = fs::remove_file(&probe);
        owner.is_ok_and(|owner| owner == meta.uid())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir).is_ok()
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

/// Checksum of stored pixels, in fixed blocks so it does not depend on the
/// thread count. A frame whose bytes changed on disk is developed again
/// instead of being rendered.
fn hash_pixels(bytes: &[u8]) -> [u8; 16] {
    let blocks: Vec<[u64; 2]> = bytes.par_chunks(1 << 20).map(hash_bytes).collect();
    let mut hash = Hash::new();
    hash.word(bytes.len() as u64);
    for [a, b] in blocks {
        hash.word(a);
        hash.word(b);
    }
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&hash.0[0].to_le_bytes());
    out[8..].copy_from_slice(&hash.0[1].to_le_bytes());
    out
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
    if !dir.is_dir() || !private_dir(dir) {
        return None;
    }
    let file = path(dir, key);
    let mut input = File::open(&file).ok()?;
    let mut header = [0u8; HEADER];
    input.read_exact(&mut header).ok()?;
    let (magic, rest) = header.split_at(MAGIC.len());
    let (stored_key, rest) = rest.split_at(16);
    let (dims, checksum) = rest.split_at(16);
    let width = u64::from_le_bytes(dims[..8].try_into().unwrap());
    let height = u64::from_le_bytes(dims[8..].try_into().unwrap());
    let pixels = width.checked_mul(height)?;
    let expected = pixels.checked_mul(12)?.checked_add(HEADER as u64)?;
    if magic != MAGIC || stored_key != key || input.metadata().ok()?.len() != expected {
        return None;
    }
    let mut image = Image::new(usize::try_from(width).ok()?, usize::try_from(height).ok()?);
    input.read_exact(pixel_bytes_mut(&mut image.data)).ok()?;
    if hash_pixels(pixel_bytes(&image.data)) != checksum {
        return None;
    }
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
    if size > limit || !private_dir(dir) {
        return false;
    }
    let target = path(dir, key);
    let staged = target.with_extension(format!("tmp{}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        // Never write through a name that already exists.
        let mut out = File::options().write(true).create_new(true).open(&staged)?;
        out.write_all(MAGIC)?;
        out.write_all(key)?;
        out.write_all(&(image.width as u64).to_le_bytes())?;
        out.write_all(&(image.height as u64).to_le_bytes())?;
        out.write_all(&hash_pixels(pixel_bytes(&image.data)))?;
        out.write_all(pixel_bytes(&image.data))?;
        drop(out);
        fs::rename(&staged, &target)
    })();
    if let Err(error) = written {
        // An existing name was not written by this call and is not this call's to remove.
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            let _ = fs::remove_file(&staged);
        }
        return false;
    }
    trim(dir, limit);
    true
}

/// Whether a directory entry is one of this cache's own files: the name it
/// would have generated and starting with its magic. The directory may be
/// one the user chose and may hold other things; nothing else is ever
/// removed, not even an empty file under such a name.
fn own_file(path: &Path) -> Option<bool> {
    let stem = path.file_stem()?.to_str()?;
    let extension = path.extension()?.to_str()?;
    let staging =
        extension.strip_prefix("tmp").is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
    if stem.len() != 32
        || !stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        || !(extension == "scene" || staging)
    {
        return None;
    }
    let mut magic = [0u8; MAGIC.len()];
    // Earlier formats of this cache are its own too.
    File::open(path).and_then(|mut file| file.read_exact(&mut magic)).ok()?;
    (magic.starts_with(b"RSSCENE")).then_some(staging)
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
        if !meta.is_file() {
            continue;
        }
        let Some(staging) = own_file(&path) else { continue };
        let age = meta.modified().ok().and_then(|at| now.duration_since(at).ok()).unwrap_or_default();
        if !staging && age <= MAX_AGE {
            scenes.push((age, meta.len(), path));
        } else if !staging || age > Duration::from_secs(3600) {
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
    fn changed_pixel_bytes_are_developed_again() {
        let dir = scratch("checksum");
        let image = sample(6, 6, 3.0);
        assert!(store_in(&dir, 1 << 20, &[7; 16], &image));
        let file = path(&dir, &[7; 16]);
        let bytes = fs::read(&file).unwrap();
        for index in [HEADER, HEADER + 100, bytes.len() - 1] {
            let mut flipped = bytes.clone();
            flipped[index] ^= 0x40;
            fs::write(&file, &flipped).unwrap();
            assert!(load_from(&dir, &[7; 16]).is_none(), "byte {index}");
        }
        fs::write(&file, &bytes).unwrap();
        assert!(load_from(&dir, &[7; 16]).is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn trimming_removes_only_files_this_cache_wrote() {
        let dir = scratch("foreign");
        let image = sample(8, 8, 2.0);
        let size = (HEADER + 8 * 8 * 12) as u64;
        assert!(store_in(&dir, size * 9, &[1; 16], &image));
        let old = SystemTime::now() - Duration::from_secs(4 * 24 * 3600);
        let hex = "0123456789abcdef0123456789abcdef";
        // Someone else's files in a directory the user pointed the cache at,
        // including ones with this cache's extensions and name shape.
        let foreign = ["backup.tmp", "project.scene", "notes.tmp123", &format!("{hex}.scene"), &format!("{hex}.tmp77")];
        for name in foreign {
            fs::write(dir.join(name), b"not a stored scene").unwrap();
            File::options().write(true).open(dir.join(name)).unwrap().set_modified(old).unwrap();
        }
        fs::create_dir(dir.join(format!("{}.scene", "f".repeat(32)))).unwrap();
        // An empty file is nobody's in particular, whatever it is called.
        let empty = path(&dir, &[4; 16]).with_extension("tmp4243");
        fs::write(&empty, b"").unwrap();
        File::options().write(true).open(&empty).unwrap().set_modified(old).unwrap();
        // This cache's own leftovers: an expired scene and an abandoned staging file.
        let expired = path(&dir, &[2; 16]);
        fs::copy(path(&dir, &[1; 16]), &expired).unwrap();
        let abandoned = path(&dir, &[3; 16]).with_extension("tmp4242");
        fs::copy(path(&dir, &[1; 16]), &abandoned).unwrap();
        for file in [&expired, &abandoned] {
            File::options().write(true).open(file).unwrap().set_modified(old).unwrap();
        }
        // A limit below one scene evicts everything of its own.
        trim(&dir, size * 9);
        assert!(!expired.exists() && !abandoned.exists());
        assert!(load_from(&dir, &[1; 16]).is_some());
        trim(&dir, 1);
        assert!(!path(&dir, &[1; 16]).exists());
        assert_eq!(fs::read(&empty).unwrap(), b"");
        for name in foreign {
            assert_eq!(fs::read(dir.join(name)).unwrap(), b"not a stored scene", "{name}");
        }
        assert!(dir.join(format!("{}.scene", "f".repeat(32))).is_dir());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn planted_names_and_shared_directories_are_never_written_through() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("planted");
        let image = sample(4, 4, 1.0);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let victim = dir.join("photo.ARW");
        fs::write(&victim, b"irreplaceable").unwrap();
        // A link waiting under the staging name this process would use.
        let staged = path(&dir, &[5; 16]).with_extension(format!("tmp{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &staged).unwrap();
        assert!(!store_in(&dir, 1 << 20, &[5; 16], &image));
        assert_eq!(fs::read(&victim).unwrap(), b"irreplaceable");
        assert!(fs::symlink_metadata(&staged).unwrap().file_type().is_symlink());
        assert!(load_from(&dir, &[5; 16]).is_none());
        fs::remove_file(&staged).unwrap();
        assert!(store_in(&dir, 1 << 20, &[5; 16], &image));
        // A directory others can write is refused before anything is created in it.
        let shared = scratch("shared");
        fs::create_dir_all(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!store_in(&shared, 1 << 20, &[5; 16], &image));
        assert_eq!(fs::read_dir(&shared).unwrap().count(), 0);
        // A link in place of the directory is refused too.
        let link = scratch("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!store_in(&link, 1 << 20, &[6; 16], &image));
        assert!(!path(&dir, &[6; 16]).exists());
        let _ = fs::remove_file(link);
        let _ = fs::remove_dir_all(shared);
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
