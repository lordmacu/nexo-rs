# docs-site/

Standalone landing page served at the root of the GitHub Pages
deployment (`https://lordmacu.github.io/nexo-rs/`). The mdBook docs
mount one level deeper at `/docs/`; the workflow at
`.github/workflows/docs.yml` assembles `_site/` by copying
`docs-site/.` to the root and `docs/book/.` to `/docs/`.

## Files

- `index.html` — hero + features + OpenClaw comparison + 5-min
  quickstart + channel matrix. Tailwind via CDN, no build step.
- `assets/logo.svg` — placeholder logo. **Replace this single
  file** to swap branding across the nav bar, the hero spot, the
  browser favicon, and the OpenGraph card image — every reference
  in `index.html` points here.

## Replacing the logo

1. Export your final logo as an SVG (preferred) or PNG (fallback).
   Square aspect ratio works best because the nav + favicon + hero
   reuse the same file at three sizes (32 px / 96 px / 128 px).
2. Drop the file in as `docs-site/assets/logo.svg`. Overwrite the
   placeholder.
3. If you ship PNG instead of SVG, also update the
   `<link rel="icon" type="image/svg+xml" ...>` line in
   `index.html` to use `image/png`.
4. Commit + push; GitHub Actions redeploys GH Pages on every
   push that touches `docs-site/**`.

## Editing the hero copy

Hero headline + tagline + install snippet live in `index.html` —
search for `<!-- Hero -->`. The badge above the title (`v0.1.5 ·
4 SDK languages shipped`) is also there; bump it on every release
so visitors see live status.

## Mobile preview

The page is responsive (Tailwind breakpoints `md:` and `lg:`).
Pull it up on a phone before publishing copy changes — the hero
font sizes, the channel-matrix grid, and the install snippet's
horizontal scroll all flex differently below 768 px.
