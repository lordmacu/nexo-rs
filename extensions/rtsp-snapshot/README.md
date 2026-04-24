# RTSP Snapshot Extension (Rust)

ffmpeg-based stills + short-clip capture from IP cameras. Reinterpretation
of OpenClaw's `camsnap` (which wraps a proprietary `camsnap` binary).

## Tools

- `status`
- `snapshot` — single JPG frame
- `clip` — short MP4 (≤ 5 min)

## URL validation

Accepts only `rtsp://`, `rtsps://`, `http://`, `https://`. Explicitly rejects
`file://`, `concat:`, and anything else ffmpeg would otherwise consume.

## Sandbox

All output paths are forced under `RTSP_SNAPSHOT_OUTPUT_ROOT` (default system
temp). Prevents path traversal from LLM-generated args.

## Reliability

- ffmpeg subprocess with watchdog SIGKILL on timeout
- Default 60s / snapshot, 30s+duration / clip (max 600s)

## Error codes

-32030 bin missing · -32031 spawn · -32032 non-zero exit · -32033 timeout
· -32034 io/sandbox · -32602 bad input

## Tests

12 tests (5 unit + 7 integration). Live-camera test uses an unreachable
RTSP URL to exercise the failure path end-to-end.
