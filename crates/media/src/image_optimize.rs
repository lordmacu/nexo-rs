//! On-upload image optimisation. The operator drops a phone
//! screenshot or a print-resolution logo into a microapp; we
//! decode, downscale to a reasonable dimension, strip metadata
//! (EXIF / colour profiles), and re-encode. The result is what
//! the public route serves, so every consumer pays the
//! optimised bytes — not the original 4 MB JPEG.
//!
//! Constraints we respect:
//! - **GIF** is left as-is. Animated GIFs lose their frames on
//!   a single-image re-encode and the `image` crate's GIF
//!   encoder doesn't preserve animation. The caller's upload
//!   size cap is the only guard.
//! - **PNG** keeps PNG (transparency).
//! - **JPEG** keeps JPEG re-encoded at quality 85 — visually
//!   indistinguishable, ~50% smaller on photographic input.
//! - **WebP** is decoded then re-encoded as PNG because the
//!   `image` crate's WebP encoder is lossy-only and PNG is
//!   universally supported. The returned `mime` reflects the
//!   chosen output.
//!
//! Max dimension is 1200 px on the longer side — 2× the 600 px
//! email canvas (the original consumer of this helper) and
//! retina-friendly for any in-app render.

use std::io::Cursor;

use image::{imageops::FilterType, ImageFormat};

/// Cap on the longer dimension. `1200` = 2× the 600px email
/// canvas (retina-friendly). Anything larger downscales with
/// Lanczos3 (good enough for photos and logos).
pub const MAX_DIMENSION: u32 = 1200;

/// JPEG re-encode quality. 85 is the conventional "looks
/// identical to the eye, half the size" sweet spot.
pub const JPEG_QUALITY: u8 = 85;

/// What the optimiser produced. The MIME may differ from the
/// input (e.g. WebP → PNG).
#[derive(Debug, Clone)]
pub struct Optimized {
    pub bytes: Vec<u8>,
    pub mime: String,
    /// Width × height of the encoded image, post-resize. Useful
    /// for the audit log so the operator can confirm the cap
    /// kicked in.
    pub width: u32,
    pub height: u32,
}

/// Errors the upload handler maps to `400 image_decode_failed`
/// or `500 image_encode_failed`.
#[derive(Debug, thiserror::Error)]
pub enum OptimizeError {
    #[error("decode failed: {0}")]
    Decode(image::ImageError),
    #[error("encode failed: {0}")]
    Encode(image::ImageError),
}

/// Optimise the bytes for the declared MIME. GIF passes
/// through (animation preservation > size win); everything
/// else round-trips through `image::DynamicImage` so EXIF +
/// embedded thumbnails get dropped.
pub fn optimize(bytes: &[u8], mime: &str) -> Result<Optimized, OptimizeError> {
    if mime == "image/gif" {
        // Pass-through: re-encoding strips animation.
        return Ok(Optimized {
            bytes: bytes.to_vec(),
            mime: mime.to_string(),
            width: 0,
            height: 0,
        });
    }
    let img = image::load_from_memory(bytes).map_err(OptimizeError::Decode)?;
    let (w, h) = (img.width(), img.height());
    // Only resize when at least one side exceeds the cap;
    // avoid pointlessly re-sampling a 200×200 logo.
    let resized = if w > MAX_DIMENSION || h > MAX_DIMENSION {
        img.resize(MAX_DIMENSION, MAX_DIMENSION, FilterType::Lanczos3)
    } else {
        img
    };
    let (out_w, out_h) = (resized.width(), resized.height());

    // Choose output codec by input MIME. WebP → PNG because
    // the `image` 0.25 WebP encoder is lossy-only.
    let (out_mime, format) = match mime {
        "image/png" => ("image/png", ImageFormat::Png),
        "image/jpeg" => ("image/jpeg", ImageFormat::Jpeg),
        "image/webp" => ("image/png", ImageFormat::Png),
        // Whitelist-validated upstream — anything else is a
        // bug; encode as PNG for safety.
        _ => ("image/png", ImageFormat::Png),
    };

    let mut out = Cursor::new(Vec::<u8>::with_capacity(bytes.len() / 2));
    if matches!(format, ImageFormat::Jpeg) {
        // Quality knob lives on the JPEG encoder directly; the
        // generic `write_to` path uses the default 75 which is
        // visibly worse on logos.
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut out, JPEG_QUALITY,
        );
        enc.encode_image(&resized).map_err(OptimizeError::Encode)?;
    } else {
        resized.write_to(&mut out, format).map_err(OptimizeError::Encode)?;
    }
    Ok(Optimized {
        bytes: out.into_inner(),
        mime: out_mime.to_string(),
        width: out_w,
        height: out_h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2000×1500 solid-colour PNG round-trips through the
    /// optimiser, comes out ≤ 1200 on the longer side, and is
    /// smaller than the input.
    #[test]
    fn png_oversize_downscales_to_cap() {
        let img = image::RgbImage::from_pixel(2000, 1500, image::Rgb([200, 100, 50]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, ImageFormat::Png)
            .unwrap();
        let input = buf.into_inner();

        let out = optimize(&input, "image/png").unwrap();
        assert_eq!(out.mime, "image/png");
        assert!(out.width <= MAX_DIMENSION && out.height <= MAX_DIMENSION);
        assert!(out.width.max(out.height) == MAX_DIMENSION);
        assert!(out.bytes.len() < input.len());
    }

    /// Small image isn't resized (cap doesn't trigger).
    #[test]
    fn small_image_kept_at_native_size() {
        let img = image::RgbImage::from_pixel(200, 100, image::Rgb([0, 0, 0]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, ImageFormat::Png)
            .unwrap();
        let out = optimize(&buf.into_inner(), "image/png").unwrap();
        assert_eq!(out.width, 200);
        assert_eq!(out.height, 100);
    }

    /// JPEG encodes at quality 85 — verify by re-decoding the
    /// output and confirming dimensions match.
    #[test]
    fn jpeg_round_trips_at_quality_85() {
        let img = image::RgbImage::from_pixel(800, 600, image::Rgb([10, 200, 50]));
        let mut buf = Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 95)
            .encode_image(&image::DynamicImage::ImageRgb8(img))
            .unwrap();
        let input = buf.into_inner();
        let out = optimize(&input, "image/jpeg").unwrap();
        assert_eq!(out.mime, "image/jpeg");
        let decoded = image::load_from_memory(&out.bytes).unwrap();
        assert_eq!(decoded.width(), 800);
        assert_eq!(decoded.height(), 600);
    }

    /// GIF passes through unchanged so animation is preserved.
    #[test]
    fn gif_passes_through_unchanged() {
        // Fake 1-byte payload — optimiser shouldn't even try
        // to decode for GIF.
        let bytes = b"GIF89a-fake-bytes".to_vec();
        let out = optimize(&bytes, "image/gif").unwrap();
        assert_eq!(out.bytes, bytes);
        assert_eq!(out.mime, "image/gif");
    }

    /// Garbage input → Decode error, not panic.
    #[test]
    fn garbage_returns_decode_error() {
        let err = optimize(b"not an image", "image/png").unwrap_err();
        assert!(matches!(err, OptimizeError::Decode(_)));
    }
}
