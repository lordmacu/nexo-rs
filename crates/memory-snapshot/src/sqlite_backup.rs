//! Online SQLite backup via the engine's native `VACUUM INTO`.
//!
//! `VACUUM INTO 'path'` (SQLite 3.27+) writes a defragmented, atomic
//! snapshot of the source database into `path`. The source remains
//! readable + writable while it runs (WAL-mode safe), and the result
//! is a self-contained file with no journal/WAL siblings.
//!
//! This is the equivalent of the `sqlite3 .backup` shell command used by
//! `scripts/nexo-backup.sh` (Phase 36.1) but as a pure SQL query, so we
//! can drive it through the workspace's `sqlx` pool without a second
//! SQLite client dependency.

use std::path::{Path, PathBuf};

use sqlx::sqlite::SqlitePoolOptions;

/// Take a point-in-time snapshot of the SQLite database at `src` into a
/// fresh file at `dst`. Returns the size in bytes of the resulting file.
///
/// Pre-conditions:
/// - `src` exists and is a SQLite database.
/// - `dst` parent directory exists.
/// - `dst` does **not** exist; `VACUUM INTO` refuses to overwrite.
pub async fn backup_db(src: &Path, dst: &Path) -> Result<u64, sqlx::Error> {
    if dst.exists() {
        return Err(sqlx::Error::Configuration(
            format!(
                "snapshot destination already exists: {} (VACUUM INTO refuses to overwrite)",
                dst.display()
            )
            .into(),
        ));
    }

    let url = format!("sqlite:{}?mode=ro", src.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;

    let dst_str = dst
        .to_str()
        .ok_or_else(|| sqlx::Error::Configuration("dst path is not valid UTF-8".into()))?;

    // VACUUM INTO does not accept positional parameters in any SQLite
    // client; the path must be inlined as a literal. Single-quote escape
    // by doubling embedded quotes — defensive even though `dst` is
    // operator-controlled and never user input.
    let escaped = dst_str.replace('\'', "''");
    let sql = format!("VACUUM INTO '{escaped}'");
    sqlx::query(&sql).execute(&pool).await?;

    pool.close().await;

    let size = std::fs::metadata(dst)
        .map_err(|e| sqlx::Error::Configuration(format!("metadata({}): {e}", dst.display()).into()))?
        .len();
    Ok(size)
}

/// Convenience wrapper that picks `<dst_dir>/<name>.sqlite` and returns
/// the chosen path alongside the size.
pub async fn backup_named(
    src: &Path,
    dst_dir: &Path,
    name: &str,
) -> Result<(PathBuf, u64), sqlx::Error> {
    let dst = dst_dir.join(format!("{name}.sqlite"));
    let size = backup_db(src, &dst).await?;
    Ok((dst, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, Connection};
    use std::str::FromStr;

    async fn seed_db(path: &Path, rows: i64) {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
            .unwrap()
            .create_if_missing(true);
        let mut conn = opts.connect().await.unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .execute(&mut conn)
            .await
            .unwrap();
        for i in 0..rows {
            sqlx::query("INSERT INTO t (id, v) VALUES (?, ?)")
                .bind(i)
                .bind(format!("row-{i}"))
                .execute(&mut conn)
                .await
                .unwrap();
        }
        conn.close().await.unwrap();
    }

    async fn count_rows(db: &Path) -> i64 {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=ro", db.display()))
            .unwrap();
        let mut conn = opts.connect().await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();
        n
    }

    #[tokio::test]
    async fn round_trip_preserves_row_count() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.sqlite");
        seed_db(&src, 100).await;

        let dst = tmp.path().join("snap.sqlite");
        let size = backup_db(&src, &dst).await.unwrap();
        assert!(size > 0);
        assert!(dst.exists());
        assert_eq!(count_rows(&dst).await, 100);
    }

    #[tokio::test]
    async fn round_trip_empty_db_is_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.sqlite");
        seed_db(&src, 0).await;

        let dst = tmp.path().join("snap.sqlite");
        backup_db(&src, &dst).await.unwrap();
        assert_eq!(count_rows(&dst).await, 0);
    }

    #[tokio::test]
    async fn refuses_existing_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.sqlite");
        seed_db(&src, 1).await;

        let dst = tmp.path().join("snap.sqlite");
        std::fs::write(&dst, b"existing").unwrap();
        let err = backup_db(&src, &dst).await.unwrap_err();
        assert!(format!("{err}").contains("already exists"));
    }

    #[tokio::test]
    async fn backup_named_picks_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src.sqlite");
        seed_db(&src, 5).await;

        let dst_dir = tmp.path().join("out");
        std::fs::create_dir(&dst_dir).unwrap();
        let (path, size) = backup_named(&src, &dst_dir, "long_term").await.unwrap();
        assert_eq!(path, dst_dir.join("long_term.sqlite"));
        assert!(size > 0);
        assert_eq!(count_rows(&path).await, 5);
    }
}
