//! Phase 11.2 follow-up — integration: plugin.toml watcher logs changes.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nexo_extensions::{spawn_extensions_watcher, KnownPluginSnapshot};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);
impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> MakeWriter<'a> for SharedBuf {
    type Writer = SharedBuf;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn write_plugin_toml(dir: &std::path::Path, id: &str, extra: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("plugin.toml");
    let body = format!(
        "[plugin]\nid = \"{id}\"\nversion = \"0.1.0\"\n\n[capabilities]\ntools = [\"foo\"]\n\n[transport]\nkind = \"stdio\"\ncommand = \"./{id}\"\n{extra}"
    );
    std::fs::write(&p, body).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn watcher_logs_manifest_change() {
    let buf = SharedBuf::default();
    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(EnvFilter::new("warn"))
        .with_writer(buf.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let ext_dir = dir.path().join("ext-a");
    write_plugin_toml(&ext_dir, "ext-a", "");

    // Boot snapshot includes the current file.
    let mut snapshot = KnownPluginSnapshot::new();
    snapshot.insert("ext-a", ext_dir.join("plugin.toml"));

    let shutdown = CancellationToken::new();
    spawn_extensions_watcher(
        vec![dir.path().to_path_buf()],
        snapshot,
        Duration::from_millis(100),
        shutdown.clone(),
    );
    // Let the watcher register.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Edit manifest (change version).
    write_plugin_toml(&ext_dir, "ext-a", "# change\n");
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Add new extension. Create the subdir first so notify registers the
    // descendant inode before we write, then write the file. Allow an
    // extra debounce window to catch the new-file event.
    let ext_b = dir.path().join("ext-b");
    std::fs::create_dir_all(&ext_b).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    write_plugin_toml(&ext_b, "ext-b", "");
    tokio::time::sleep(Duration::from_millis(800)).await;

    shutdown.cancel();

    let captured = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        captured.contains("extension manifest changed"),
        "missing change event:\n{captured}"
    );
    assert!(
        captured.contains("new extension detected"),
        "missing new event:\n{captured}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn watcher_logs_invalid_toml() {
    let buf = SharedBuf::default();
    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(EnvFilter::new("error"))
        .with_writer(buf.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = tempfile::tempdir().unwrap();
    let ext_dir = dir.path().join("broken");
    std::fs::create_dir_all(&ext_dir).unwrap();

    let shutdown = CancellationToken::new();
    spawn_extensions_watcher(
        vec![dir.path().to_path_buf()],
        KnownPluginSnapshot::new(),
        Duration::from_millis(100),
        shutdown.clone(),
    );
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Write an invalid TOML.
    std::fs::write(ext_dir.join("plugin.toml"), b"[plugin\nid = broken").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    shutdown.cancel();

    let captured = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(
        captured.contains("invalid plugin.toml"),
        "missing error event:\n{captured}"
    );
}
