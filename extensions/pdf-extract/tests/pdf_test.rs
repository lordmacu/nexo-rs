use serde_json::json;

use pdf_extract_ext::tools;

const HELLO_PDF: &[u8] = include_bytes!("fixtures/hello.pdf");

fn write_fixture(bytes: &[u8], ext: &str) -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("tempdir").into_path();
    let p = dir.join(format!("sample.{ext}"));
    std::fs::write(&p, bytes).expect("write fixture");
    p
}

#[test]
fn status_ok() {
    let out = tools::dispatch("status", &json!({})).expect("ok");
    assert_eq!(out["ok"], true);
    assert_eq!(out["provider"], "pdf-extract (rust)");
    assert_eq!(out["tools"][1], "extract_text");
    assert!(out["limits"]["max_file_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn extract_text_returns_document_content() {
    let path = write_fixture(HELLO_PDF, "pdf");
    let out = tools::dispatch(
        "extract_text",
        &json!({ "path": path.to_string_lossy() }),
    )
    .expect("ok");
    let text = out["text"].as_str().expect("text");
    assert!(
        text.contains("Hola mundo"),
        "expected 'Hola mundo' in extracted text, got: {text}"
    );
    assert_eq!(out["truncated"], false);
    assert!(out["char_count"].as_u64().unwrap() > 0);
}

#[test]
fn max_chars_truncates() {
    let path = write_fixture(HELLO_PDF, "pdf");
    let out = tools::dispatch(
        "extract_text",
        &json!({
            "path": path.to_string_lossy(),
            "max_chars": 5
        }),
    )
    .expect("ok");
    assert_eq!(out["truncated"], true);
    let text = out["text"].as_str().unwrap();
    assert_eq!(text.chars().count(), 5);
}

#[test]
fn missing_file_is_bad_input() {
    let err = tools::dispatch(
        "extract_text",
        &json!({ "path": "/definitely/not/a/real/file.pdf" }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
    assert!(err.message.contains("cannot stat"));
}

#[test]
fn empty_path_rejected() {
    let err = tools::dispatch("extract_text", &json!({ "path": "   " })).unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
fn max_chars_zero_rejected() {
    let path = write_fixture(HELLO_PDF, "pdf");
    let err = tools::dispatch(
        "extract_text",
        &json!({ "path": path.to_string_lossy(), "max_chars": 0 }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32602);
}

#[test]
fn non_pdf_file_surfaces_provider_error() {
    // A garbage/text file is not a PDF; pdf-extract should report a
    // parsing failure which we map to -32006.
    let path = write_fixture(b"this is not a pdf at all, just plain text", "pdf");
    let err = tools::dispatch(
        "extract_text",
        &json!({ "path": path.to_string_lossy() }),
    )
    .unwrap_err();
    assert_eq!(err.code, -32006, "unexpected err: {err:?}");
}

#[test]
fn dispatch_unknown_tool_returns_method_not_found() {
    let err = tools::dispatch("not_a_tool", &json!({})).unwrap_err();
    assert_eq!(err.code, -32601);
}
