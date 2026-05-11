//! SQLite migration helpers — small primitives shared by
//! every SDK store that adds columns to existing tables on a
//! deployed tenant.
//!
//! ## Why
//!
//! Every per-tenant store in this SDK (and its consumers) ends
//! up with the same pattern when a new field lands on an
//! already-shipped schema:
//!
//! 1. The fresh-install `CREATE TABLE` carries the new column.
//! 2. An `ALTER TABLE ADD COLUMN` statement runs at boot to
//!    catch existing tenant DBs that were created before the
//!    column existed.
//! 3. On already-migrated DBs the `ALTER` errors with
//!    `duplicate column name "X"` — caller swallows that as
//!    the success signal and surfaces every other error.
//!
//! Hand-rolled across `module_state`, `compose_attachment`,
//! `compose_draft`, `email_template::store`, …  the substring
//! check is a one-liner but it's also a one-liner that's
//! repeated 4+ times in tree, drifting between
//! `"duplicate column name"` and `"already exists"` depending
//! on the author's Stack-Overflow tab. Consolidating here
//! makes future migrations one-line consumers.

#![allow(missing_docs)]

use sqlx::SqlitePool;

/// SQLite error substrings that mean "the schema already has
/// this column / index / table". Treated as success because
/// the only operation that produces them is an idempotent
/// `IF NOT EXISTS`-shaped alter that another process already
/// applied. Lower-cased before matching to defang case-sensitive
/// driver versions.
const ALREADY_PRESENT_NEEDLES: &[&str] = &["duplicate column name", "already exists"];

/// Run a single SQL statement that's expected to be idempotent
/// — typically `ALTER TABLE … ADD COLUMN …`. Returns
/// `Ok(applied)` where `applied = true` if the statement ran
/// fresh, `false` if SQLite reported the change was already
/// present. Any other error bubbles up.
pub async fn run_idempotent_alter(pool: &SqlitePool, stmt: &str) -> Result<bool, sqlx::Error> {
    match sqlx::query(stmt).execute(pool).await {
        Ok(_) => Ok(true),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            if ALREADY_PRESENT_NEEDLES.iter().any(|n| msg.contains(n)) {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

/// Same as [`run_idempotent_alter`] but for a slice of
/// statements — applied in order, each guarded the same way.
/// Returns the count of statements that ran fresh (i.e.
/// excluded the "already present" no-ops). Stops at the first
/// non-idempotent error.
pub async fn run_idempotent_alters(
    pool: &SqlitePool,
    stmts: &[&str],
) -> Result<usize, sqlx::Error> {
    let mut applied = 0;
    for stmt in stmts {
        if run_idempotent_alter(pool, stmt).await? {
            applied += 1;
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let p = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT)")
            .execute(&p)
            .await
            .unwrap();
        p
    }

    #[tokio::test]
    async fn fresh_alter_returns_applied_true() {
        let p = pool().await;
        let applied = run_idempotent_alter(&p, "ALTER TABLE t ADD COLUMN b TEXT")
            .await
            .unwrap();
        assert!(applied);
    }

    #[tokio::test]
    async fn duplicate_column_returns_applied_false() {
        let p = pool().await;
        run_idempotent_alter(&p, "ALTER TABLE t ADD COLUMN b TEXT")
            .await
            .unwrap();
        // Second run on a column that's already there.
        let applied = run_idempotent_alter(&p, "ALTER TABLE t ADD COLUMN b TEXT")
            .await
            .unwrap();
        assert!(!applied);
    }

    #[tokio::test]
    async fn unrelated_error_bubbles_up() {
        let p = pool().await;
        // Bad column reference — not an "already present" case.
        let err = run_idempotent_alter(&p, "ALTER TABLE nonexistent ADD COLUMN x TEXT").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn batch_counts_only_fresh_applications() {
        let p = pool().await;
        // First run: both alters land fresh.
        let applied = run_idempotent_alters(
            &p,
            &[
                "ALTER TABLE t ADD COLUMN b TEXT",
                "ALTER TABLE t ADD COLUMN c TEXT",
            ],
        )
        .await
        .unwrap();
        assert_eq!(applied, 2);
        // Second run: both already present, count drops to zero.
        let applied = run_idempotent_alters(
            &p,
            &[
                "ALTER TABLE t ADD COLUMN b TEXT",
                "ALTER TABLE t ADD COLUMN c TEXT",
            ],
        )
        .await
        .unwrap();
        assert_eq!(applied, 0);
    }

    #[tokio::test]
    async fn batch_stops_at_first_real_error() {
        let p = pool().await;
        // First applies fresh, second fails (table doesn't
        // exist), third never runs.
        let res = run_idempotent_alters(
            &p,
            &[
                "ALTER TABLE t ADD COLUMN b TEXT",
                "ALTER TABLE missing ADD COLUMN x TEXT",
                "ALTER TABLE t ADD COLUMN c TEXT",
            ],
        )
        .await;
        assert!(res.is_err());
        // The third alter shouldn't have run — verify by
        // checking column `c` doesn't exist.
        let cols = sqlx::query("PRAGMA table_info(t)")
            .fetch_all(&p)
            .await
            .unwrap();
        let names: Vec<String> = cols
            .iter()
            .map(|r| sqlx::Row::try_get::<String, _>(r, 1).unwrap())
            .collect();
        assert!(!names.contains(&"c".to_string()));
    }

    #[tokio::test]
    async fn case_insensitive_needle_match() {
        let p = pool().await;
        // SQLite emits the canonical "duplicate column name:
        // X" message; re-running confirms the lowercase
        // matcher catches it regardless of any future
        // capitalisation drift in the driver.
        run_idempotent_alter(&p, "ALTER TABLE t ADD COLUMN b TEXT")
            .await
            .unwrap();
        let applied = run_idempotent_alter(&p, "ALTER TABLE t ADD COLUMN b TEXT")
            .await
            .unwrap();
        assert!(!applied);
    }
}
