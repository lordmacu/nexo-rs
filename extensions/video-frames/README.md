# Video Frames Extension (Rust)

Stdio extension that wraps `ffmpeg`/`ffprobe` to extract frames and audio
from local video files. Designed to pair with `openai-whisper` for
audio-to-text pipelines and with any LLM vision pass for frame analysis.

## Tools

- `status` — ffmpeg + ffprobe versions, sandbox root, limits
- `probe` — JSON metadata (duration, streams, format) from `ffprobe`
- `extract_frames` — sample N evenly-spaced or fps-based JPG frames
- `extract_audio` — extract audio to MP3 (default) or WAV (for whisper)

## Requires

- `ffmpeg` and `ffprobe` on `PATH` (declared in `plugin.toml` `requires.bins`)
- Not pure Rust — this is a thin subprocess wrapper

## Sandbox

All output paths (`output_dir` for frames, `output_path` for audio) must
lie under `VIDEO_FRAMES_OUTPUT_ROOT` (default: system temp dir). Paths
outside are rejected with `-32034` `IoError`. Prevents path traversal
from LLM-generated arguments.

## Reliability

- Timeout per subprocess: 600s default (override via
  `VIDEO_FRAMES_TIMEOUT_SECS`, hard cap 3600s)
- Watchdog thread sends `SIGKILL` on timeout (Unix)
- stderr captured and surfaced on non-zero exit
- Input size cap: 500 MB

## Typed errors

| Code | Meaning |
|------|---------|
| -32030 | ffmpeg/ffprobe binary missing on PATH |
| -32031 | subprocess spawn failed |
| -32032 | ffmpeg exited non-zero (stderr in message) |
| -32033 | subprocess exceeded timeout |
| -32034 | io / sandbox error |
| -32602 | bad input (missing file, bad codec, bad fps, oversize) |

## Build & test

```bash
cargo build --release --manifest-path extensions/video-frames/Cargo.toml
cargo test           --manifest-path extensions/video-frames/Cargo.toml
```

14 tests total: 4 unit (bin resolution, sandbox enforcement) + 10 integration
(status, probe, frame extraction, mp3/wav audio, missing input, out-of-sandbox
rejected, bad codec, out-of-range fps, unknown tool). Integration tests
synthesize a 2-second red clip with a 440 Hz sine at runtime — no fixtures
checked in.

## Pipeline examples

### Video → transcript (Whisper)

```
1. video-frames.extract_audio {
     path: "/tmp/meeting.mp4",
     output_path: "/tmp/wf/meeting.wav",
     codec: "wav", mono: true, sample_rate: 16000
   }
2. openai-whisper.transcribe_file {
     file_path: "/tmp/wf/meeting.wav",
     response_format: "verbose_json",
     language: "es"
   }
3. summarize.summarize_text { text: <transcript>, length: "long" }
```

### Video → key frames

```
1. video-frames.probe { path } → duration
2. video-frames.extract_frames { path, output_dir, count: 12 }
3. (operator or future vision-enabled LLM consumes the JPG paths)
```
