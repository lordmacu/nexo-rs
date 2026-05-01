//! Bundle codec primitives: SHA-256 streaming, tar+zstd packing,
//! optional age encryption.
//!
//! These modules stay decoupled from the snapshot orchestration so they
//! can be tested in isolation and reused by future bundle formats.

pub mod sha256_stream;
pub mod tar_zst;

#[cfg(feature = "snapshot-encryption")]
#[cfg_attr(docsrs, doc(cfg(feature = "snapshot-encryption")))]
pub mod age_codec;
