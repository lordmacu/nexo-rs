# Spotify Extension (Rust)

Spotify Web API client with blocking reqwest + ext_common breaker.
Reinterpretation of OpenClaw's `spotify-player` skill, which wraps a
TUI CLI (`spogo`/`spotify_player`). This extension hits the Web API
directly — better fit for a headless server.

## Tools

- `status`
- `now_playing`
- `search` (query + types + limit)
- `play` (uri?, device_id?)
- `pause`, `next`, `previous`

## Auth

`SPOTIFY_ACCESS_TOKEN` env var with a user-scoped access token. Token
refresh is the operator's responsibility (Spotify OAuth 2.0 PKCE).

## Error codes

- -32041 missing token
- -32011 unauthorized (401)
- -32012 forbidden (403)
- -32001 not found (404)
- -32013 rate limited (429) with retry-after in message
- -32070 NO_ACTIVE_DEVICE (detected from error body)
- -32005 timeout · -32003 transport · -32004 circuit open

## Tests

12 integration tests via wiremock (now_playing shapes, 204/no-device,
search, 401/429/NO_ACTIVE_DEVICE detection, URI validation, missing token).

## Env

- `SPOTIFY_ACCESS_TOKEN` required
- `SPOTIFY_API_URL` default `https://api.spotify.com/v1`
- `SPOTIFY_HTTP_TIMEOUT_SECS` default 15
