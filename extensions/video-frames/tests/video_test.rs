//! End-to-end tests for the `video-frames` extension.
//!
//! Requires `ffmpeg` on PATH. Every test generates its own 2-second
//! synthetic clip with a sine-wave audio track so no fixtures are
//! checked into the repo.

use std::path::PathBuf;

use serde_json::json;
use serial_test::serial;

use video_frames_ext::tools;

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| if o.status.success() { Some(()) } else { None })
        .is_some()
}

fn make_sandbox() -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix("vframes-test-")
        .tempdir()
        .expect("tempdir")
        .into_path();
    std::env::set_var("VIDEO_FRAMES_OUTPUT_ROOT", &dir);
    dir
}

fn synth_video(dir: &PathBuf, name: &str, secs: u32) -> PathBuf {
    let out = dir.join(name);
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=red:s=160x120:d={secs}:r=10"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:duration={secs}"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn ffmpeg");
    assert!(
        status.status.success(),
        "fixture synth failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    out
}

#[test]
#[serial]
fn status_reports_ffmpeg_versions() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not on PATH; skipping");
        return;
    }
    let out = tools::dispatch("status", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert!(out["ffmpeg_version"].as_str().unwrap().contains("ffmpeg"));
    assert!(out["ffprobe_version"].as_str().unwrap().contains("ffprobe"));
}

#[test]
#[serial]
fn probe_returns_duration_and_streams() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = make_sandbox();
    let video = synth_video(&sandbox, "probe.mp4", 2);
    let out = tools::dispatch(
        "probe",
        &json!({ "path": video.to_string_lossy() }),
    )
    .expect("ok");
    let dur = out["duration_secs"].as_f64().expect("duration");
    assert!(dur >= 1.5 && dur <= 2.5, "unexpected duration: {dur}");
    // At least one video stream.
    let streams = out["streams"].as_array().expect("streams");
    assert!(streams.iter().any(|s| s["codec_type"] == "video"));
    // And one audio stream.
    assert!(streams.iter().any(|s| s["codec_type"] == "audio"));
}

#[test]
#[serial]
fn extract_frames_writes_files_under_sandbox() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = make_sandbox();
    let video = synth_video(&sandbox, "frames.mp4", 2);
    let out_dir = sandbox.join("frames");
    std::fs::create_dir_all(&out_dir).unwrap();

    let out = tools::dispatch(
        "extract_frames",
        &json!({
            "path": video.to_string_lossy(),
            "output_dir": out_dir.to_string_lossy(),
            "count": 4
        }),
    )
    .expect("ok");
    let written = out["count_written"].as_u64().expect("count_written");
    assert!(written >= 1 && written <= 4, "unexpected frame count: {written}");
    let frames = out["frames"].as_array().expect("frames");
    for f in frames {
        let p = std::path::Path::new(f.as_str().unwrap());
        assert!(p.is_file(), "frame missing: {}", p.display());
        assert!(p.starts_with(&sandbox), "frame outside sandbox: {}", p.display());
    }
}

#[test]
#[serial]
fn extract_audio_writes_mono_wav_for_whisper() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = make_sandbox();
    let video = synth_video(&sandbox, "audio.mp4", 2);
    let out_path = sandbox.join("audio.wav");

    let out = tools::dispatch(
        "extract_audio",
        &json!({
            "path": video.to_string_lossy(),
            "output_path": out_path.to_string_lossy(),
            "codec": "wav",
            "mono": true,
            "sample_rate": 16000
        }),
    )
    .expect("ok");
    assert_eq!(out["codec"], "wav");
    assert_eq!(out["mono"], true);
    assert_eq!(out["sample_rate"], 16000);
    assert!(out["bytes"].as_u64().unwrap() > 1000);
    assert!(std::path::Path::new(&out["output_path"].as_str().unwrap()).is_file());
}

#[test]
#[serial]
fn extract_audio_mp3_default() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = make_sandbox();
    let video = synth_video(&sandbox, "audio.mp4", 2);
    let out_path = sandbox.join("audio.mp3");
    let out = tools::dispatch(
        "extract_audio",
        &json!({
            "path": video.to_string_lossy(),
            "output_path": out_path.to_string_lossy(),
        }),
    )
    .expect("ok");
    assert_eq!(out["codec"], "mp3");
}

#[test]
#[serial]
fn missing_input_returns_bad_input() {
    if !ffmpeg_available() {
        return;
    }
    let _ = make_sandbox();
    let err = tools::dispatch(
        "probe",
        &json!({ "path": "/does/not/exist.mp4" }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("cannot stat"));
}

#[test]
#[serial]
fn output_dir_outside_sandbox_rejected() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = make_sandbox();
    let video = synth_video(&sandbox, "f.mp4", 2);
    // Attempt to write frames to /tmp/outside — outside our per-test sandbox.
    let bad_dir = std::env::temp_dir().join(format!(
        "outside-{}-{}",
        std::process::id(),
        chrono_like_ts()
    ));
    std::fs::create_dir_all(&bad_dir).unwrap();
    let err = tools::dispatch(
        "extract_frames",
        &json!({
            "path": video.to_string_lossy(),
            "output_dir": bad_dir.to_string_lossy(),
            "count": 2
        }),
    )
    .unwrap_err();
    // sandbox_output_dir maps to IoError → code -32034
    assert_eq!(err.code, -32034, "unexpected err: {err:?}");
    std::fs::remove_dir_all(&bad_dir).ok();
}

#[test]
#[serial]
fn bad_codec_rejected_locally() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = make_sandbox();
    let video = synth_video(&sandbox, "f.mp4", 2);
    let err = tools::dispatch(
        "extract_audio",
        &json!({
            "path": video.to_string_lossy(),
            "output_path": sandbox.join("x.ogg").to_string_lossy(),
            "codec": "ogg"
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn fps_out_of_range_rejected() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = make_sandbox();
    let video = synth_video(&sandbox, "f.mp4", 2);
    let err = tools::dispatch(
        "extract_frames",
        &json!({
            "path": video.to_string_lossy(),
            "output_dir": sandbox.join("frames").to_string_lossy(),
            "fps": 120.0
        }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
#[serial]
fn unknown_tool_returns_method_not_found() {
    let err = tools::dispatch("not_real", &json!({})).unwrap_err();
    assert_eq!(err.code, -32601);
}

fn chrono_like_ts() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
