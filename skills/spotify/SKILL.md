---
name: Spotify
description: Control Spotify playback via Web API — now playing, search, play/pause/next/previous.
requires:
  bins: []
  env: [SPOTIFY_ACCESS_TOKEN]
---

# Spotify

Use para consultar qué está sonando, buscar música, y controlar la
reproducción en cualquier dispositivo conectado a Spotify Connect.
Llama a Spotify Web API directamente — no wrappea ningún CLI TUI.

## Use when

- "¿qué está sonando?"
- "pon la playlist X"
- "siguiente canción"
- "baja el volumen" (con `play/device_id` para cambiar target)

## Do not use when

- Tu dispositivo no está conectado a Spotify Connect → la API devuelve
  `NO_ACTIVE_DEVICE` (-32070). Abre Spotify en algún lado primero.
- Necesitas audio local synthesis (TTS, playback sin Spotify) → otra skill.
- Quieres editar biblioteca, crear playlists → posible con Web API pero
  fuera del scope v0.1.

## Auth

Requiere `SPOTIFY_ACCESS_TOKEN` (user scope). El refresh token flow lo
maneja el operador (Spotify OAuth 2.0 PKCE o Authorization Code). La
extension **no** maneja refresh automático — si el token expira, el
operador genera uno nuevo o implementa refresh externamente y lo
rota en env.

## Tools

### `status`
Token presence, endpoint. Use para verificar antes de mandar comandos.

### `now_playing`
Returns `{is_playing, progress_ms, track, artists, album, uri, device}`.
204/"no active device" → `{is_playing:false, reason:"no active device"}`.

### `search { query, types?, limit? }`
- `types` default `track`; acepta `track,artist,album,playlist,show,episode,audiobook`
- `limit` 1..50 (default 10)

Returns el JSON crudo de `/v1/search`. Útil para `play` subsiguiente.

### `play { uri?, device_id? }`
- Sin `uri`: resume.
- `spotify:track:...` toca esa pista.
- `spotify:album:...` / `spotify:playlist:...` / `spotify:artist:...` setea como `context_uri`.
- `device_id` opcional: fuerza dispositivo.

### `pause { device_id? }` · `next { device_id? }` · `previous { device_id? }`
Controles directos.

## Execution guidance

- Si `-32070 NO_ACTIVE_DEVICE`: mensaje clean al usuario ("abre Spotify en
  tu altavoz/teléfono y reintento").
- Si `-32011 unauthorized`: token expiró; operador debe rotar.
- Si `-32013 rate limited`: respeta `retry_after` que viene en el mensaje.
- Guarda `device.id` del `now_playing` para usarlo después (evita pasos
  extra preguntando devices disponibles).
