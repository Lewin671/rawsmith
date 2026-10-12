//! rawsmith: decode camera RAW files and develop them into finished pictures
//! from a small, reproducible JSON recipe.

// Indexing three colour channels by number is clearer than iterator chains here.
#![allow(clippy::needless_range_loop)]

pub mod adjust;
pub mod arw;
pub mod auto;
pub mod color;
pub mod demosaic;
pub mod error;
pub mod image;
pub mod lens;
pub mod output;
pub mod params;
pub mod pipeline;
pub mod stats;
pub mod synthetic;
pub mod tiff;
pub mod tone;

/// Development-only override of an internal constant, read from the
/// `RAWSMITH_TUNE` environment variable (`name=value,name=value`).
pub fn tune(name: &str, default: f32) -> f32 {
    std::env::var("RAWSMITH_TUNE")
        .ok()
        .and_then(|all| all.split(',').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')?.parse().ok()))
        .unwrap_or(default)
}
