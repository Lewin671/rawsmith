//! rawsmith: decode camera RAW files and develop them into finished pictures
//! from a small, reproducible JSON recipe.

// Indexing three colour channels by number is clearer than iterator chains here.
#![allow(clippy::needless_range_loop)]

pub mod adjust;
pub mod arw;
pub mod auto;
pub mod color;
pub mod compare;
pub mod demosaic;
pub mod error;
pub mod fringe;
pub mod image;
pub mod lens;
pub mod output;
pub mod params;
pub mod pipeline;
pub mod stats;
pub mod synthetic;
pub mod tiff;
pub mod tone;
