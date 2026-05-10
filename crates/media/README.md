# nexo-media

Media-asset pipelines shared by nexo microapps that accept
operator-uploaded images.

## Surface

```rust
use nexo_media::{optimize, MAX_DIMENSION, JPEG_QUALITY};

let out = optimize(&bytes, "image/jpeg")?;
// out.bytes — re-encoded JPEG at quality 85
// out.mime  — "image/jpeg" (or "image/png" for WebP inputs)
// out.width / out.height — post-resize dimensions
```

## Behaviour

- **PNG** → PNG (transparency preserved).
- **JPEG** → JPEG re-encoded at quality 85.
- **GIF** → passes through (animation preserved — re-encode
  would lose frames).
- **WebP** → PNG (the `image` crate's WebP encoder is lossy-
  only; PNG keeps every recipient's mail client happy).

Any image whose longer side exceeds `MAX_DIMENSION` (1200 px)
gets downscaled with Lanczos3. EXIF + colour profiles drop in
the round-trip through `image::DynamicImage`.

## Why

Originally inline in the marketing extension's email-template
upload handler. Lifted because:
- Any microapp accepting operator image uploads needs the same
  receive-side guard against 4 MB phone screenshots.
- The 1200 px cap + JPEG q85 + EXIF strip are universal
  defaults — nothing email-specific.
- Splitting the heavy `image` codec dependency out of the
  marketing crate lets unrelated microapps consume the helper
  without dragging in the rest of the marketing surface.
