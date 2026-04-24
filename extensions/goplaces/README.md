# GoPlaces Extension (Rust)

Standalone Rust stdio extension for place search/details with provider routing:

- `google` (Google Places API)
- `openstreetmap` (Nominatim)
- `auto` (default): Google if `GOOGLE_PLACES_API_KEY` exists, otherwise OpenStreetMap

## Tools

- `status`
- `search_text`
- `place_details`

For OpenStreetMap results, `search_text` includes `lookup_id` (for example
`N12345`) that can be passed directly to `place_details`.

## Environment

- `GOOGLE_PLACES_API_KEY` optional (required only when provider is `google`)
- `GOOGLE_PLACES_BASE_URL` optional override
- `OPENSTREETMAP_BASE_URL` optional override for Nominatim-compatible endpoint

## Build

```bash
cargo build --release --manifest-path extensions/goplaces/Cargo.toml
```
