//! Item 6: inline-credential paths must never be echoed verbatim in
//! error messages — the synthetic `inline:<raw client_id>` marker
//! could leak the OAuth client id into operator logs.

use std::path::PathBuf;

use nexo_auth::error::{display_path, BuildError, CredentialError};

#[test]
fn display_path_redacts_inline_prefix() {
    let p = PathBuf::from(
        "inline:706186208439-38enpqmsp4o8om1ujb8ka5t5l4oji58o.apps.googleusercontent.com",
    );
    let rendered = display_path(&p);
    assert_eq!(rendered, "<inline credential>");
    assert!(!rendered.contains("706186208439"));
}

#[test]
fn display_path_passes_real_paths_through() {
    let p = PathBuf::from("./secrets/google/ana_client_id.txt");
    assert_eq!(display_path(&p), "./secrets/google/ana_client_id.txt");
}

#[test]
fn file_missing_error_redacts_inline() {
    let err = CredentialError::FileMissing {
        path: PathBuf::from("inline:706186208439-secret-stuff"),
    };
    let msg = err.to_string();
    assert!(msg.contains("<inline credential>"), "got: {msg}");
    assert!(!msg.contains("706186208439"));
}

#[test]
fn duplicate_path_build_error_redacts_inline() {
    let err = BuildError::DuplicatePath {
        path: PathBuf::from("inline:706186208439-secret"),
        a_channel: nexo_auth::handle::GOOGLE,
        a_instance: "ana".into(),
        b_channel: nexo_auth::handle::GOOGLE,
        b_instance: "kate".into(),
    };
    let msg = err.to_string();
    assert!(msg.contains("<inline credential>"));
    assert!(!msg.contains("706186208439"));
}
