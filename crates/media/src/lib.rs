//! Media-asset pipelines for nexo microapps.
//!
//! Currently exposes one function: [`optimize`] — the
//! upload-side image guard that decodes operator-supplied
//! bytes, caps dimensions, strips metadata (EXIF + colour
//! profiles), and re-encodes at an email-sane quality.
//!
//! Originally inline in the marketing extension's email-
//! template upload handler. Lifted so any microapp that
//! accepts operator image uploads shares the same receive-side
//! treatment without copy-pasting the codec choices.

pub mod image_optimize;

pub use image_optimize::{optimize, OptimizeError, Optimized, JPEG_QUALITY, MAX_DIMENSION};
