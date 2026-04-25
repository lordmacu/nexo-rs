//! Item 5: when client_id / client_secret files change on disk, the
//! GoogleAuthClient picks up the new values without a daemon restart.

use std::path::PathBuf;
use std::time::Duration;

use nexo_plugin_google::{GoogleAuthClient, GoogleAuthConfig, SecretSources};

fn write(p: &PathBuf, body: &str) {
    std::fs::write(p, body).unwrap();
}

#[tokio::test]
async fn refresh_when_files_change() {
    let dir = tempfile::tempdir().unwrap();
    let cid_path = dir.path().join("cid.txt");
    let cs_path = dir.path().join("cs.txt");
    write(&cid_path, "old-client-id");
    write(&cs_path, "old-secret");

    let cfg = GoogleAuthConfig {
        client_id: "old-client-id".into(),
        client_secret: "old-secret".into(),
        scopes: vec![],
        token_file: dir.path().join("tok.json").to_string_lossy().into_owned(),
        redirect_port: 0,
    };
    let client = GoogleAuthClient::new_with_sources(
        cfg,
        dir.path(),
        Some(SecretSources {
            client_id_path: cid_path.clone(),
            client_secret_path: cs_path.clone(),
        }),
    );

    assert_eq!(client.config().client_id, "old-client-id");
    assert_eq!(client.config().client_secret, "old-secret");

    // mtime resolution on some filesystems is 1s — sleep before the
    // rewrite so the new mtime is strictly greater.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    write(&cid_path, "new-client-id");
    write(&cs_path, "new-secret");

    client.refresh_secrets_if_changed().await.unwrap();

    assert_eq!(client.config().client_id, "new-client-id");
    assert_eq!(client.config().client_secret, "new-secret");
}

#[tokio::test]
async fn refresh_no_op_when_files_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let cid_path = dir.path().join("cid.txt");
    let cs_path = dir.path().join("cs.txt");
    write(&cid_path, "stable-id");
    write(&cs_path, "stable-secret");

    let cfg = GoogleAuthConfig {
        client_id: "stable-id".into(),
        client_secret: "stable-secret".into(),
        scopes: vec![],
        token_file: dir.path().join("tok.json").to_string_lossy().into_owned(),
        redirect_port: 0,
    };
    let client = GoogleAuthClient::new_with_sources(
        cfg,
        dir.path(),
        Some(SecretSources {
            client_id_path: cid_path,
            client_secret_path: cs_path,
        }),
    );

    let before = client.config();
    client.refresh_secrets_if_changed().await.unwrap();
    let after = client.config();
    // Same Arc pointer — no swap happened.
    assert!(std::sync::Arc::ptr_eq(&before, &after));
}

#[tokio::test]
async fn refresh_no_op_without_sources() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = GoogleAuthConfig {
        client_id: "x".into(),
        client_secret: "y".into(),
        scopes: vec![],
        token_file: dir.path().join("tok.json").to_string_lossy().into_owned(),
        redirect_port: 0,
    };
    let client = GoogleAuthClient::new(cfg, dir.path());
    // Should not error and not change anything.
    client.refresh_secrets_if_changed().await.unwrap();
    assert_eq!(client.config().client_id, "x");
}
