---
name: RTSP Snapshot
description: Capture JPG frames or short MP4 clips from RTSP/HTTP IP cameras via ffmpeg.
requires:
  bins: [ffmpeg]
  env: []
---

# RTSP Snapshot

Use to grab stills or short clips from IP security cameras (RTSP, HTTP
HLS) so kate can report "qué está pasando en el patio" o guardar
evidencia. Todo output va bajo un sandbox dir (`RTSP_SNAPSHOT_OUTPUT_ROOT`,
default `$TMPDIR`).

## Use when

- "toma foto de la cámara del garaje"
- "graba 30s del timbre ahora"
- "hay movimiento? snap y me lo mandas"

## Do not use when

- La cámara no está en la LAN y no tiene URL RTSP/HTTP expuesta
- Necesitas streaming continuo o WebRTC
- Clips > 5 minutos (cap duro por diseño)

## Tools

### `status`
ffmpeg bin + sandbox + limits.

### `snapshot { url, output_path, transport?, width? }`
- `url` — `rtsp://user:pass@host/stream`, `rtsps://`, `http://`, `https://` (NO `file://`, NO `concat:`)
- `output_path` — debe estar dentro del sandbox root; `.jpg` recomendado
- `transport` — `tcp` (default, estable sobre NAT) o `udp`
- `width` — resize manteniendo aspecto

Returns `{url, output_path, bytes, transport}`.

### `clip { url, output_path, duration_secs, transport? }`
- `duration_secs` 1..=300
- Usa stream copy (sin re-encode) — extremadamente rápido

Returns `{url, output_path, bytes, duration_secs, transport}`.

## Execution guidance

- Prefiere TCP transport para evitar pérdida de paquetes sobre Wi-Fi/VPN
- Credenciales van inline en la URL (`rtsp://user:pass@host/...`); **guárdalas
  en 1Password** y léelas con `read_secret` cuando reveal esté on
- Output path fuera del sandbox → `-32034` IoError; ajusta operator config
- Cámara unreachable → `-32032` NonZeroExit con el stderr de ffmpeg dentro
- Para detección de movimiento fuera de scope — chain con tu pipeline
  vision-enabled LLM consumiendo los frames

## Pipelines

### Snapshot + vision

```
1. rtsp-snapshot.snapshot { url, output_path: "/sandbox/cam1.jpg" }
2. (futuro) vision-lm.describe { path: "/sandbox/cam1.jpg" }
```

### Clip + transcribe

```
1. rtsp-snapshot.clip { url, output_path: "/sandbox/ring.mp4", duration_secs: 30 }
2. video-frames.extract_audio { path, output_path: "/sandbox/ring.wav", codec:"wav", mono:true, sample_rate:16000 }
3. openai-whisper.transcribe_file { file_path: "/sandbox/ring.wav" }
```
