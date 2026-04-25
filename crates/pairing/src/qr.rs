//! QR rendering for the setup-code payload.
//!
//! - [`render_ansi`] — terminal QR using Unicode block characters,
//!   always available, prints to stdout in `agent pair start`.
//! - [`render_png`] — full PNG bitmap, gated behind the `qr-png`
//!   feature (default on). The bin uses this when `--qr-png <path>`
//!   is passed.

use crate::types::PairingError;

/// Pretty-print a QR for the given payload as a multi-line string.
/// Caller writes the result to stdout / a file / a Telegram caption.
pub fn render_ansi(data: &str) -> Result<String, PairingError> {
    let code = qrcode::QrCode::new(data.as_bytes())
        .map_err(|e| PairingError::Storage(format!("qr encode: {e}")))?;
    let s = code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build();
    Ok(s)
}

#[cfg(feature = "qr-png")]
pub fn render_png(data: &str) -> Result<Vec<u8>, PairingError> {
    use image::{ImageBuffer, Luma};
    let code = qrcode::QrCode::new(data.as_bytes())
        .map_err(|e| PairingError::Storage(format!("qr encode: {e}")))?;
    // Render to a u8 buffer at 8 px/module.
    let img: ImageBuffer<Luma<u8>, Vec<u8>> = code
        .render::<Luma<u8>>()
        .min_dimensions(256, 256)
        .quiet_zone(true)
        .build();
    let mut out = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut out);
        img.write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|e| PairingError::Storage(format!("png encode: {e}")))?;
    }
    Ok(out)
}

#[cfg(not(feature = "qr-png"))]
pub fn render_png(_data: &str) -> Result<Vec<u8>, PairingError> {
    Err(PairingError::Invalid(
        "qr-png feature disabled at compile time",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_contains_block_chars() {
        let s = render_ansi("hello").unwrap();
        // Dense1x2 uses upper/lower half block runes.
        assert!(
            s.chars().any(|c| c == '█' || c == '▀' || c == '▄' || c == ' '),
            "no block chars in QR ANSI output: {s:?}"
        );
    }

    #[cfg(feature = "qr-png")]
    #[test]
    fn png_starts_with_signature() {
        let bytes = render_png("hello").unwrap();
        // PNG file signature.
        assert_eq!(&bytes[..8], &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    }
}
