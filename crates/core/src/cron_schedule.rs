//! Phase 79.7 — `cron_create` / `cron_list` / `cron_delete` /
//! `cron_pause` / `cron_resume` storage layer.
//!
//! Persists LLM-time-scheduled cron entries to SQLite. Distinct
//! from Phase 7 Heartbeat (config-time only) and Phase 20
//! `agent_turn` poller (config-time only) — this is the only path
//! where the model itself mutates the schedule.
//!
//! Schedule shape:
//!   * 5-field POSIX cron expressions (also accepts a 6-field
//!     form with explicit seconds).
//!   * `recurring` + `durable` flags per entry.
//!   * Hard cap of 50 entries per binding (see [`MAX_PER_BINDING`]).
//!   * 60 s minimum interval guard at validate time.
//!
//! Phase 80.2-80.6 layered the jitter cluster on top of this
//! storage layer: `CronJitterConfig` lives in
//! [`nexo_config::types::cron_jitter`] and its helpers
//! ([`apply_recurring_jitter`], [`apply_one_shot_lead`],
//! [`jitter_frac_from_entry_id`]) are consumed by [`CronRunner`]
//! every tick.
//!
//! Current scope:
//!   * SQLite-backed store with idempotent `CREATE TABLE IF NOT
//!     EXISTS`.
//!   * Cron expression validated at insert time with 60s minimum
//!     interval guard.
//!   * Cap 50 entries per binding.
//!   * Runtime firing is shipped via `CronRunner` + `LlmCronDispatcher`.
//!   * One-shot retry durability is tracked in `failure_count`.

use chrono::{DateTime, TimeZone, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow},
    ConnectOptions, Row,
};
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

/// Hard cap per binding — keeps storage bounded and prevents a
/// runaway model from spamming the table.
pub const MAX_CRON_ENTRIES_PER_BINDING: usize = 50;

/// Minimum interval between fires. Anything finer pathologically
/// loads the daemon.
pub const MIN_CRON_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CronEntry {
    /// ULID-shaped id assigned at insert.
    pub id: String,
    /// Per-binding namespace. The runtime stamps this from
    /// `AgentContext.inbound_origin` — entries from a `whatsapp:ops`
    /// goal stay isolated from a `telegram:bot` goal.
    pub binding_id: String,
    /// Standard 5-field cron expression (M H DoM Mon DoW). Parsed
    /// at insert; storage retains the literal string so a future
    /// `cron_list` can show the operator what was scheduled.
    pub cron_expr: String,
    /// Prompt to enqueue when the entry fires. Plain string; the
    /// runtime hands it to the cron LLM dispatch machinery.
    pub prompt: String,
    /// Optional channel hint (`whatsapp:default`, `telegram:bot`).
    /// `None` = inherit binding's primary channel.
    pub channel: Option<String>,
    /// LLM provider pinned at schedule time. `None` on legacy rows
    /// created before Phase 79.7.B; dispatcher falls back to process
    /// default wiring in that case.
    #[serde(default)]
    pub model_provider: Option<String>,
    /// LLM model pinned at schedule time (paired with
    /// `model_provider`). `None` on legacy rows.
    #[serde(default)]
    pub model_name: Option<String>,
    /// Phase 79.7 outbound — optional recipient (channel-specific
    /// id: WhatsApp JID, Telegram chat_id, email address). When
    /// `Some`, the runtime's `LlmCronDispatcher` (with publisher
    /// wired) routes the model response to
    /// `plugin.outbound.{plugin}.{instance}` with `{to: recipient,
    /// text: response}`. `None` keeps the entry log-only.
    #[serde(default)]
    pub recipient: Option<String>,
    /// `true` (default) → fire on every cron match until deleted.
    /// `false` → fire once at the next match, then auto-delete.
    pub recurring: bool,
    /// Unix-seconds creation timestamp.
    pub created_at: i64,
    /// Computed at insert. Future runtime polls this.
    pub next_fire_at: i64,
    /// Unix-seconds of last fire. `None` until first fire.
    pub last_fired_at: Option<i64>,
    /// Consecutive dispatch failures for one-shot entries. Starts at
    /// 0, increments on each failed fire that is rescheduled via the
    /// retry policy, and resets to 0 on successful recurring advance.
    /// Exposed in `cron_list` so operators can inspect degraded jobs.
    #[serde(default)]
    pub failure_count: u32,
    /// Soft-disable flag — toggled by `cron_pause` / `cron_resume`. `true`
    /// keeps the entry in storage but skips firing.
    pub paused: bool,
    /// Phase 80.5 — exempt from `recurring_max_age_ms` auto-expiry.
    /// Built-in / always-on entries (assistant_mode initial cron,
    /// ops cleanups) carry `true` so a long uptime doesn't sweep
    /// them away. Default `false`.
    #[serde(default)]
    pub permanent: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CronStoreError {
    #[error("invalid cron expression `{0}`: {1}")]
    InvalidCron(String, String),
    #[error("interval below minimum 60s for cron `{0}` ({1})")]
    IntervalTooShort(String, &'static str),
    #[error("binding `{0}` already has {1} entries (max {2})")]
    BindingFull(String, usize, &'static str),
    #[error("cron entry `{0}` not found")]
    NotFound(String),
    #[error("sqlx: {0}")]
    Sql(#[from] sqlx::Error),
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS nexo_cron_entries (
        id              TEXT PRIMARY KEY,
        binding_id      TEXT NOT NULL,
        cron_expr       TEXT NOT NULL,
        prompt          TEXT NOT NULL,
        channel         TEXT,
        recurring       INTEGER NOT NULL,
        created_at      INTEGER NOT NULL,
        next_fire_at    INTEGER NOT NULL,
        last_fired_at   INTEGER,
        failure_count   INTEGER NOT NULL DEFAULT 0,
        paused          INTEGER NOT NULL DEFAULT 0,
        recipient       TEXT,
        model_provider  TEXT,
        model_name      TEXT
    )
";

const INDEX_BINDING: &str =
    "CREATE INDEX IF NOT EXISTS idx_nexo_cron_entries_binding ON nexo_cron_entries(binding_id)";
const INDEX_FIRE: &str =
    "CREATE INDEX IF NOT EXISTS idx_nexo_cron_entries_fire ON nexo_cron_entries(next_fire_at) WHERE paused = 0";

/// Validate the cron expression and return the next-fire timestamp
/// (unix seconds) at or after `from_unix`. Returns
/// `Err(InvalidCron)` when the expression is unparseable, and
/// `Err(IntervalTooShort)` when two consecutive fires would be
/// closer than `MIN_CRON_INTERVAL_SECS`.
pub fn next_fire_after(cron_expr: &str, from_unix: i64) -> Result<i64, CronStoreError> {
    // The `cron` crate uses 6-field expressions (with seconds).
    // Prepend "0 " so 5-field (classic Unix)
    // works. Existing 6-field expressions pass through unchanged.
    let parsed_expr = if cron_expr.split_whitespace().count() == 5 {
        format!("0 {cron_expr}")
    } else {
        cron_expr.to_string()
    };
    let schedule = Schedule::from_str(&parsed_expr)
        .map_err(|e| CronStoreError::InvalidCron(cron_expr.to_string(), e.to_string()))?;
    let from = Utc.timestamp_opt(from_unix, 0).single().ok_or_else(|| {
        CronStoreError::InvalidCron(cron_expr.to_string(), "bad timestamp".into())
    })?;
    let mut iter = schedule.after(&from);
    let first: DateTime<Utc> = iter.next().ok_or_else(|| {
        CronStoreError::InvalidCron(cron_expr.to_string(), "no future fire".into())
    })?;
    let second_opt = iter.next();
    if let Some(second) = second_opt {
        let delta = (second - first).num_seconds();
        if (delta as u64) < MIN_CRON_INTERVAL_SECS {
            return Err(CronStoreError::IntervalTooShort(
                cron_expr.to_string(),
                "interval < 60s",
            ));
        }
    }
    Ok(first.timestamp())
}

#[async_trait::async_trait]
pub trait CronStore: Send + Sync {
    async fn insert(&self, entry: &CronEntry) -> Result<(), CronStoreError>;
    async fn list_by_binding(&self, binding_id: &str) -> Result<Vec<CronEntry>, CronStoreError>;
    async fn count_by_binding(&self, binding_id: &str) -> Result<usize, CronStoreError>;
    async fn delete(&self, id: &str) -> Result<(), CronStoreError>;
    /// Fetch every non-paused entry due at or before `now`.
    /// `CronRunner` consumes this on each tick.
    async fn due_at(&self, now_unix: i64) -> Result<Vec<CronEntry>, CronStoreError>;
    /// Toggle the `paused` flag on a single entry. `paused = true`
    /// keeps the row in storage but `due_at` skips it.
    async fn set_paused(&self, id: &str, paused: bool) -> Result<(), CronStoreError>;
    /// Fetch a single entry by id. Returns `NotFound` when absent.
    async fn get(&self, id: &str) -> Result<CronEntry, CronStoreError>;
    /// Advance a recurring entry after a fire: bump
    /// `next_fire_at` to the next match and stamp
    /// `last_fired_at`. Caller decides between this and
    /// [`Self::delete`] based on the entry's `recurring` flag.
    async fn advance_after_fire(
        &self,
        id: &str,
        new_next_fire_at: i64,
        last_fired_at: i64,
    ) -> Result<(), CronStoreError>;
    /// Reschedule a failed one-shot fire for a retry attempt.
    /// Increments `failure_count`, stamps `last_fired_at`, and moves
    /// `next_fire_at` to `retry_next_fire_at`. Returns the new
    /// `failure_count` after increment.
    async fn schedule_one_shot_retry(
        &self,
        id: &str,
        retry_next_fire_at: i64,
        last_fired_at: i64,
    ) -> Result<u32, CronStoreError>;

    /// Phase 80.6 — boot helper. Atomically rewrite `next_fire_at`
    /// to `i64::MAX` for every entry whose stored `next_fire_at`
    /// is more than `skew_ms` milliseconds in the past. Returns the
    /// number of entries quarantined.
    ///
    /// Intent: when the daemon has been down longer than the user's
    /// expected schedule, the next tick would otherwise dispatch a
    /// stampede of "missed" fires. Setting `next_fire_at = MAX`
    /// keeps the row visible (operator can resume manually) without
    /// firing it immediately. `permanent: true` entries are exempt.
    async fn sweep_missed_entries(
        &self,
        now_unix: i64,
        skew_ms: i64,
    ) -> Result<usize, CronStoreError>;

    /// Phase 80.5 — auto-expire **recurring** entries older than
    /// `max_age_ms`. `permanent: true` entries are exempt regardless
    /// of age. Returns the number of entries deleted. `max_age_ms`
    /// of `0` is a no-op (operator opt-in).
    async fn sweep_expired_recurring(
        &self,
        now_unix: i64,
        max_age_ms: i64,
    ) -> Result<usize, CronStoreError>;
}

/// Apply ±`pct_pct/100` jitter to a future-fire timestamp. Used to
/// avoid thundering-herd when many bindings schedule at the same
/// `from_unix` is the reference "now" used to compute the spread —
/// jitter is applied in seconds and bounded so the result never
/// goes BEFORE `from_unix` (a negative jitter past `from_unix`
/// would re-fire immediately).
///
/// Phase 80.2-80.4 superseded by [`apply_recurring_jitter`] +
/// [`apply_one_shot_lead`] which take a `CronJitterConfig` + the
/// entry id so retries are deterministic. This single-`pct`
/// variant remains for the legacy `CronRunner::with_jitter_pct`
/// shim path.
pub fn apply_jitter(next_fire_at: i64, from_unix: i64, pct: u32) -> i64 {
    if pct == 0 {
        return next_fire_at;
    }
    let span = next_fire_at - from_unix;
    if span <= 0 {
        return next_fire_at;
    }
    let max_offset = ((span as i128) * (pct as i128) / 100).max(0) as i64;
    if max_offset == 0 {
        return next_fire_at;
    }
    // Cheap deterministic jitter — combines next_fire_at with a
    // process counter so consecutive calls don't produce the same
    // value. Not a CSPRNG; jitter is ops noise, not cryptographic.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mix = (next_fire_at as i128).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ (n as i128).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let mut signed: i64 = ((mix.unsigned_abs() % (2 * max_offset as u128 + 1)) as i64) - max_offset;
    // Clamp so the result never goes earlier than `from_unix + 1` —
    // we never want a jittered timestamp to fire instantly.
    if next_fire_at + signed <= from_unix {
        signed = from_unix + 1 - next_fire_at;
    }
    next_fire_at + signed
}

// ─────────────────────────────────────────────────────────────────────
// Phase 80.2-80.6 — config-driven jitter helpers
// ─────────────────────────────────────────────────────────────────────

use nexo_config::types::cron_jitter::CronJitterConfig;

/// Phase 80.3 — deterministic jitter fraction in `[0.0, 1.0)`
/// derived from the entry id. The first 8 hex chars of a UUIDv4
/// supply 32 bits of entropy normalised to a float; non-UUID-shaped
/// ids fall back to `0.0` (no jitter) so legacy entries don't
/// break.
pub fn jitter_frac_from_entry_id(entry_id: &str) -> f64 {
    let take: String = entry_id.chars().filter(|c| c.is_ascii_hexdigit()).take(8).collect();
    if take.len() < 8 {
        return 0.0;
    }
    match u32::from_str_radix(&take, 16) {
        Ok(n) => (n as f64) / (u32::MAX as f64 + 1.0),
        Err(_) => 0.0,
    }
}

/// Phase 80.2 + 80.3 + 80.4 — recurring forward jitter.
/// `t1 + min(frac * (t2 - t1), cap_ms)` where `frac` is derived
/// deterministically from `entry_id`. `t1` and `t2` are the next
/// two fire timestamps (unix seconds) from the cron expression.
/// Returns the new fire timestamp in unix seconds.
///
/// `from_unix` clamps so the result never goes BEFORE `from_unix`
/// — a jittered timestamp must remain in the future.
pub fn apply_recurring_jitter(
    next_fire_at_unix: i64,
    following_fire_at_unix: i64,
    from_unix: i64,
    entry_id: &str,
    cfg: &CronJitterConfig,
) -> i64 {
    if !cfg.enabled || cfg.recurring_frac <= 0.0 || cfg.recurring_cap_ms <= 0 {
        return next_fire_at_unix;
    }
    let interval_ms = (following_fire_at_unix - next_fire_at_unix) * 1000;
    if interval_ms <= 0 {
        return next_fire_at_unix;
    }
    let frac_window = (interval_ms as f64) * cfg.recurring_frac;
    let window_ms = (frac_window as i64).min(cfg.recurring_cap_ms);
    if window_ms <= 0 {
        return next_fire_at_unix;
    }
    let offset_ms = (jitter_frac_from_entry_id(entry_id) * window_ms as f64) as i64;
    let result_unix = next_fire_at_unix + offset_ms / 1000;
    if result_unix <= from_unix {
        from_unix + 1
    } else {
        result_unix
    }
}

/// Phase 80.4 — one-shot backward lead. The fire happens `lead`
/// seconds BEFORE `target_unix`, never after. Only applied when
/// the target's minute-of-hour passes the modulo gate
/// `cfg.one_shot_minute_mod`. `from_unix` clamps so the result is
/// never in the past.
///
/// Returns `target_unix` unchanged when the gate fails or jitter is
/// disabled — the caller fires at the original timestamp.
pub fn apply_one_shot_lead(
    target_unix: i64,
    from_unix: i64,
    entry_id: &str,
    cfg: &CronJitterConfig,
    target_minute: u32,
) -> i64 {
    if !cfg.enabled || cfg.one_shot_max_ms <= 0 {
        return target_unix;
    }
    // `mod 0` is the documented "never jitter one-shots" sentinel
    // — rather than panic on division, we return early.
    if cfg.one_shot_minute_mod == 0 {
        return target_unix;
    }
    if target_minute % cfg.one_shot_minute_mod != 0 {
        return target_unix;
    }
    let max_lead_ms = cfg.one_shot_max_ms;
    let lead_window_ms =
        (jitter_frac_from_entry_id(entry_id) * max_lead_ms as f64) as i64;
    let lead_ms = lead_window_ms.max(cfg.one_shot_floor_ms);
    let result_unix = target_unix - lead_ms / 1000;
    if result_unix <= from_unix {
        from_unix
    } else {
        result_unix
    }
}

pub struct SqliteCronStore {
    pool: SqlitePool,
}

impl SqliteCronStore {
    pub async fn open(path: &str) -> Result<Self, CronStoreError> {
        let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite:{path}"))
            .map_err(|e| CronStoreError::Sql(sqlx::Error::Configuration(Box::new(e))))?
            .create_if_missing(true)
            .disable_statement_logging();
        let pool = SqlitePoolOptions::new()
            .max_connections(if path == ":memory:" { 1 } else { 4 })
            .connect_with(opts)
            .await?;
        sqlx::query(SCHEMA).execute(&pool).await?;
        sqlx::query(INDEX_BINDING).execute(&pool).await?;
        sqlx::query(INDEX_FIRE).execute(&pool).await?;
        // Idempotent ALTER for DBs created before the recipient
        // column existed. Same pattern as Phase 71's plan_mode
        // column on agent_registry: tolerate "duplicate column"
        // errors so migrate() stays callable on every boot.
        let alter = sqlx::query("ALTER TABLE nexo_cron_entries ADD COLUMN recipient TEXT")
            .execute(&pool)
            .await;
        if let Err(e) = alter {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(CronStoreError::Sql(e));
            }
        }
        let alter_model_provider =
            sqlx::query("ALTER TABLE nexo_cron_entries ADD COLUMN model_provider TEXT")
                .execute(&pool)
                .await;
        if let Err(e) = alter_model_provider {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(CronStoreError::Sql(e));
            }
        }
        let alter_model_name =
            sqlx::query("ALTER TABLE nexo_cron_entries ADD COLUMN model_name TEXT")
                .execute(&pool)
                .await;
        if let Err(e) = alter_model_name {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(CronStoreError::Sql(e));
            }
        }
        let alter_failure_count = sqlx::query(
            "ALTER TABLE nexo_cron_entries ADD COLUMN failure_count INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&pool)
        .await;
        if let Err(e) = alter_failure_count {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(CronStoreError::Sql(e));
            }
        }
        let alter_permanent = sqlx::query(
            "ALTER TABLE nexo_cron_entries ADD COLUMN permanent INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&pool)
        .await;
        if let Err(e) = alter_permanent {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(CronStoreError::Sql(e));
            }
        }
        Ok(Self { pool })
    }

    pub async fn open_memory() -> Result<Self, CronStoreError> {
        Self::open(":memory:").await
    }

    /// CLI/admin helper: list every cron entry across bindings,
    /// sorted by next fire time.
    pub async fn list_all(&self) -> Result<Vec<CronEntry>, CronStoreError> {
        let rows = sqlx::query("SELECT * FROM nexo_cron_entries ORDER BY next_fire_at ASC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row_to_entry).collect()
    }
}

fn row_to_entry(row: &SqliteRow) -> Result<CronEntry, CronStoreError> {
    Ok(CronEntry {
        id: row.try_get("id")?,
        binding_id: row.try_get("binding_id")?,
        cron_expr: row.try_get("cron_expr")?,
        prompt: row.try_get("prompt")?,
        channel: row.try_get("channel")?,
        model_provider: row.try_get("model_provider").unwrap_or(None),
        model_name: row.try_get("model_name").unwrap_or(None),
        recurring: row.try_get::<i64, _>("recurring")? != 0,
        created_at: row.try_get("created_at")?,
        next_fire_at: row.try_get("next_fire_at")?,
        last_fired_at: row.try_get("last_fired_at")?,
        failure_count: row.try_get::<i64, _>("failure_count").unwrap_or(0).max(0) as u32,
        paused: row.try_get::<i64, _>("paused")? != 0,
        permanent: row.try_get::<i64, _>("permanent").unwrap_or(0) != 0,
        // The column was added by an idempotent ALTER, so older
        // DBs may still be missing it on the first read after
        // migration. Default to None when the row predates the
        // column.
        recipient: row.try_get("recipient").unwrap_or(None),
    })
}

#[async_trait::async_trait]
impl CronStore for SqliteCronStore {
    async fn insert(&self, entry: &CronEntry) -> Result<(), CronStoreError> {
        sqlx::query(
            "INSERT INTO nexo_cron_entries \
             (id, binding_id, cron_expr, prompt, channel, recurring, created_at, next_fire_at, last_fired_at, failure_count, paused, recipient, model_provider, model_name, permanent) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .bind(&entry.id)
        .bind(&entry.binding_id)
        .bind(&entry.cron_expr)
        .bind(&entry.prompt)
        .bind(entry.channel.as_deref())
        .bind(entry.recurring as i64)
        .bind(entry.created_at)
        .bind(entry.next_fire_at)
        .bind(entry.last_fired_at)
        .bind(entry.failure_count as i64)
        .bind(entry.paused as i64)
        .bind(entry.recipient.as_deref())
        .bind(entry.model_provider.as_deref())
        .bind(entry.model_name.as_deref())
        .bind(entry.permanent as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_by_binding(&self, binding_id: &str) -> Result<Vec<CronEntry>, CronStoreError> {
        let rows = sqlx::query(
            "SELECT * FROM nexo_cron_entries WHERE binding_id = ?1 ORDER BY next_fire_at ASC",
        )
        .bind(binding_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_entry).collect()
    }

    async fn count_by_binding(&self, binding_id: &str) -> Result<usize, CronStoreError> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM nexo_cron_entries WHERE binding_id = ?1")
            .bind(binding_id)
            .fetch_one(&self.pool)
            .await?;
        let n: i64 = row.try_get("n")?;
        Ok(n as usize)
    }

    async fn delete(&self, id: &str) -> Result<(), CronStoreError> {
        let res = sqlx::query("DELETE FROM nexo_cron_entries WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CronStoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn due_at(&self, now_unix: i64) -> Result<Vec<CronEntry>, CronStoreError> {
        let rows = sqlx::query(
            "SELECT * FROM nexo_cron_entries \
             WHERE paused = 0 AND next_fire_at <= ?1 \
             ORDER BY next_fire_at ASC",
        )
        .bind(now_unix)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_entry).collect()
    }

    async fn set_paused(&self, id: &str, paused: bool) -> Result<(), CronStoreError> {
        let res = sqlx::query("UPDATE nexo_cron_entries SET paused = ?1 WHERE id = ?2")
            .bind(paused as i64)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(CronStoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<CronEntry, CronStoreError> {
        let row = sqlx::query("SELECT * FROM nexo_cron_entries WHERE id = ?1 LIMIT 1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| CronStoreError::NotFound(id.to_string()))?;
        row_to_entry(&row)
    }

    async fn advance_after_fire(
        &self,
        id: &str,
        new_next_fire_at: i64,
        last_fired_at: i64,
    ) -> Result<(), CronStoreError> {
        let res = sqlx::query(
            "UPDATE nexo_cron_entries \
             SET next_fire_at = ?1, last_fired_at = ?2, failure_count = 0 \
             WHERE id = ?3",
        )
        .bind(new_next_fire_at)
        .bind(last_fired_at)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(CronStoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn schedule_one_shot_retry(
        &self,
        id: &str,
        retry_next_fire_at: i64,
        last_fired_at: i64,
    ) -> Result<u32, CronStoreError> {
        let mut tx = self.pool.begin().await?;
        let res = sqlx::query(
            "UPDATE nexo_cron_entries \
             SET next_fire_at = ?1, last_fired_at = ?2, failure_count = failure_count + 1 \
             WHERE id = ?3",
        )
        .bind(retry_next_fire_at)
        .bind(last_fired_at)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 0 {
            tx.rollback().await.ok();
            return Err(CronStoreError::NotFound(id.to_string()));
        }
        let row = sqlx::query("SELECT failure_count FROM nexo_cron_entries WHERE id = ?1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let failure_count = row.try_get::<i64, _>("failure_count").unwrap_or(0).max(0) as u32;
        tx.commit().await?;
        Ok(failure_count)
    }

    async fn sweep_missed_entries(
        &self,
        now_unix: i64,
        skew_ms: i64,
    ) -> Result<usize, CronStoreError> {
        if skew_ms <= 0 {
            return Ok(0);
        }
        let cutoff = now_unix.saturating_sub(skew_ms / 1000);
        let res = sqlx::query(
            "UPDATE nexo_cron_entries \
             SET next_fire_at = ?1 \
             WHERE next_fire_at < ?2 \
               AND paused = 0 \
               AND permanent = 0",
        )
        .bind(i64::MAX)
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() as usize)
    }

    async fn sweep_expired_recurring(
        &self,
        now_unix: i64,
        max_age_ms: i64,
    ) -> Result<usize, CronStoreError> {
        if max_age_ms <= 0 {
            return Ok(0);
        }
        let cutoff = now_unix.saturating_sub(max_age_ms / 1000);
        let res = sqlx::query(
            "DELETE FROM nexo_cron_entries \
             WHERE recurring = 1 \
               AND created_at < ?1 \
               AND permanent = 0",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() as usize)
    }
}

/// Builder helper used by the `cron_create` tool: validates the
/// expression, enforces the binding cap, and produces a fresh
/// `CronEntry` ready to insert.
#[allow(clippy::too_many_arguments)]
pub async fn build_new_entry(
    store: &Arc<dyn CronStore>,
    binding_id: &str,
    cron_expr: &str,
    prompt: &str,
    channel: Option<&str>,
    recurring: bool,
    recipient: Option<&str>,
    model_provider: Option<&str>,
    model_name: Option<&str>,
) -> Result<CronEntry, CronStoreError> {
    let now = Utc::now().timestamp();
    let next_fire_at = next_fire_after(cron_expr, now)?;
    let count = store.count_by_binding(binding_id).await?;
    if count >= MAX_CRON_ENTRIES_PER_BINDING {
        return Err(CronStoreError::BindingFull(
            binding_id.to_string(),
            count,
            "50",
        ));
    }
    Ok(CronEntry {
        id: Uuid::new_v4().to_string(),
        binding_id: binding_id.to_string(),
        cron_expr: cron_expr.to_string(),
        prompt: prompt.to_string(),
        channel: channel.map(str::to_string),
        model_provider: model_provider.map(str::to_string),
        model_name: model_name.map(str::to_string),
        recurring,
        created_at: now,
        next_fire_at,
        last_fired_at: None,
        failure_count: 0,
        paused: false,
        permanent: false,
        recipient: recipient.map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(binding: &str, expr: &str) -> CronEntry {
        let now = 1_700_000_000;
        CronEntry {
            id: Uuid::new_v4().to_string(),
            binding_id: binding.into(),
            cron_expr: expr.into(),
            prompt: "ping".into(),
            channel: None,
            model_provider: None,
            model_name: None,
            recurring: true,
            created_at: now,
            next_fire_at: next_fire_after(expr, now).unwrap(),
            last_fired_at: None,
            failure_count: 0,
            paused: false,
            permanent: false,
            recipient: None,
        }
    }

    #[test]
    fn next_fire_accepts_5field() {
        // every 5 minutes
        let v = next_fire_after("*/5 * * * *", 1_700_000_000).unwrap();
        assert!(v > 1_700_000_000);
    }

    #[test]
    fn next_fire_accepts_6field_passthrough() {
        // every minute (6-field with explicit seconds 0)
        let v = next_fire_after("0 * * * * *", 1_700_000_000).unwrap();
        assert!(v > 1_700_000_000);
    }

    #[test]
    fn next_fire_rejects_garbage() {
        let err = next_fire_after("not a cron", 1_700_000_000).unwrap_err();
        assert!(matches!(err, CronStoreError::InvalidCron(_, _)));
    }

    #[test]
    fn next_fire_rejects_sub_minute_interval() {
        // every second — 6-field expression with `*` in the
        // seconds position. Two consecutive fires are 1s apart.
        let err = next_fire_after("* * * * * *", 1_700_000_000).unwrap_err();
        assert!(
            matches!(err, CronStoreError::IntervalTooShort(_, _)),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn store_insert_list_count_delete_round_trip() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let e1 = entry("whatsapp:ops", "*/5 * * * *");
        let e2 = entry("whatsapp:ops", "0 9 * * *");
        let e3 = entry("telegram:bot", "0 */2 * * *");
        store.insert(&e1).await.unwrap();
        store.insert(&e2).await.unwrap();
        store.insert(&e3).await.unwrap();

        let listed = store.list_by_binding("whatsapp:ops").await.unwrap();
        assert_eq!(listed.len(), 2);
        let ids: std::collections::HashSet<_> = listed.iter().map(|e| e.id.clone()).collect();
        assert!(ids.contains(&e1.id));
        assert!(ids.contains(&e2.id));

        assert_eq!(store.count_by_binding("whatsapp:ops").await.unwrap(), 2);
        assert_eq!(store.count_by_binding("telegram:bot").await.unwrap(), 1);

        store.delete(&e1.id).await.unwrap();
        assert_eq!(store.count_by_binding("whatsapp:ops").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn delete_unknown_id_errors() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let err = store.delete("nope").await.unwrap_err();
        assert!(matches!(err, CronStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn due_at_filters_paused_and_future() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let mut due = entry("whatsapp:ops", "*/5 * * * *");
        due.next_fire_at = 1_700_000_000;
        let mut paused = entry("whatsapp:ops", "*/5 * * * *");
        paused.next_fire_at = 1_700_000_000;
        paused.paused = true;
        let mut future = entry("whatsapp:ops", "0 9 * * *");
        future.next_fire_at = 1_700_001_000;
        store.insert(&due).await.unwrap();
        store.insert(&paused).await.unwrap();
        store.insert(&future).await.unwrap();

        let now_due = store.due_at(1_700_000_500).await.unwrap();
        assert_eq!(now_due.len(), 1);
        assert_eq!(now_due[0].id, due.id);
    }

    #[tokio::test]
    async fn build_new_entry_caps_at_50_per_binding() {
        let store: Arc<dyn CronStore> = Arc::new(SqliteCronStore::open_memory().await.unwrap());
        for _ in 0..50 {
            let e = build_new_entry(
                &store,
                "whatsapp:ops",
                "*/5 * * * *",
                "ping",
                None,
                true,
                None,
                None,
                None,
            )
            .await
            .unwrap();
            store.insert(&e).await.unwrap();
        }
        let err = build_new_entry(
            &store,
            "whatsapp:ops",
            "*/5 * * * *",
            "ping",
            None,
            true,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CronStoreError::BindingFull(_, 50, _)));
    }

    #[tokio::test]
    async fn set_paused_toggles_and_get_round_trips() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let e = entry("whatsapp:ops", "*/5 * * * *");
        store.insert(&e).await.unwrap();
        assert!(!store.get(&e.id).await.unwrap().paused);
        store.set_paused(&e.id, true).await.unwrap();
        assert!(store.get(&e.id).await.unwrap().paused);
        // Paused entry no longer in due_at output.
        let due = store.due_at(e.next_fire_at + 60).await.unwrap();
        assert!(due.iter().all(|x| x.id != e.id));
        store.set_paused(&e.id, false).await.unwrap();
        let due_again = store.due_at(e.next_fire_at + 60).await.unwrap();
        assert!(due_again.iter().any(|x| x.id == e.id));
    }

    #[tokio::test]
    async fn set_paused_unknown_id_errors() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let err = store.set_paused("nope", true).await.unwrap_err();
        assert!(matches!(err, CronStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn get_unknown_id_errors() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let err = store.get("nope").await.unwrap_err();
        assert!(matches!(err, CronStoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn schedule_one_shot_retry_increments_failure_count_and_moves_next_fire() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let mut e = entry("whatsapp:ops", "*/5 * * * *");
        e.recurring = false;
        e.next_fire_at = 1_700_000_000;
        store.insert(&e).await.unwrap();

        let count1 = store
            .schedule_one_shot_retry(&e.id, 1_700_000_100, 1_700_000_050)
            .await
            .unwrap();
        assert_eq!(count1, 1);
        let reloaded = store.get(&e.id).await.unwrap();
        assert_eq!(reloaded.failure_count, 1);
        assert_eq!(reloaded.next_fire_at, 1_700_000_100);
        assert_eq!(reloaded.last_fired_at, Some(1_700_000_050));

        let count2 = store
            .schedule_one_shot_retry(&e.id, 1_700_000_200, 1_700_000_150)
            .await
            .unwrap();
        assert_eq!(count2, 2);
        let reloaded2 = store.get(&e.id).await.unwrap();
        assert_eq!(reloaded2.failure_count, 2);
        assert_eq!(reloaded2.next_fire_at, 1_700_000_200);
        assert_eq!(reloaded2.last_fired_at, Some(1_700_000_150));
    }

    #[test]
    fn apply_jitter_zero_pct_is_identity() {
        let from = 1_700_000_000;
        let next = 1_700_003_600;
        assert_eq!(apply_jitter(next, from, 0), next);
    }

    #[test]
    fn apply_jitter_within_pct_bound() {
        let from = 1_700_000_000;
        let next = 1_700_003_600; // +1h
        for _ in 0..50 {
            let jittered = apply_jitter(next, from, 10);
            let span = next - from;
            let max_offset = span / 10; // 10% of 3600 = 360
            assert!(
                jittered >= next - max_offset && jittered <= next + max_offset,
                "jittered={jittered} outside [next±10%]"
            );
            assert!(jittered > from, "jitter pulled fire to or past from_unix");
        }
    }

    #[test]
    fn apply_jitter_returns_same_when_already_past() {
        // next_fire_at <= from_unix → no jitter possible.
        assert_eq!(apply_jitter(100, 200, 50), 100);
    }

    #[tokio::test]
    async fn build_new_entry_isolated_per_binding() {
        let store: Arc<dyn CronStore> = Arc::new(SqliteCronStore::open_memory().await.unwrap());
        for _ in 0..50 {
            let e = build_new_entry(
                &store,
                "binding-a",
                "*/5 * * * *",
                "ping",
                None,
                true,
                None,
                None,
                None,
            )
            .await
            .unwrap();
            store.insert(&e).await.unwrap();
        }
        // Different binding admits even when the first one is at cap.
        let e = build_new_entry(
            &store,
            "binding-b",
            "*/5 * * * *",
            "ping",
            None,
            true,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        store.insert(&e).await.unwrap();
        assert_eq!(store.count_by_binding("binding-b").await.unwrap(), 1);
    }

    // ----- Phase 80.2-80.6 jitter cluster -----

    #[test]
    fn jitter_frac_from_entry_id_is_deterministic() {
        let id = "deadbeef-0000-1111-2222-333333333333";
        let a = jitter_frac_from_entry_id(id);
        let b = jitter_frac_from_entry_id(id);
        assert_eq!(a, b, "same id must produce same fraction");
        assert!((0.0..1.0).contains(&a), "fraction must be in [0,1)");
    }

    #[test]
    fn jitter_frac_handles_short_or_non_hex_ids() {
        assert_eq!(jitter_frac_from_entry_id(""), 0.0);
        assert_eq!(jitter_frac_from_entry_id("ZZZ"), 0.0);
        // 8 hex chars even when surrounded by separators
        let f = jitter_frac_from_entry_id("00-00-00-00-ffffff");
        assert!((0.0..1.0).contains(&f));
    }

    #[test]
    fn apply_recurring_jitter_disabled_returns_input_unchanged() {
        let cfg = CronJitterConfig {
            recurring_frac: 0.0,
            ..Default::default()
        };
        let out =
            apply_recurring_jitter(1_700_000_000, 1_700_000_300, 1_699_999_900, "abcd1234", &cfg);
        assert_eq!(out, 1_700_000_000);
    }

    #[test]
    fn apply_recurring_jitter_caps_at_recurring_cap_ms() {
        // With recurring_frac=1.0 and a 1h interval the natural
        // window would be 3 600 000 ms; recurring_cap_ms forces it
        // down to 60 000 ms so the offset cannot exceed 60 s.
        let cfg = CronJitterConfig {
            recurring_frac: 1.0,
            recurring_cap_ms: 60_000,
            ..Default::default()
        };
        let next = 1_700_000_000;
        let following = next + 3_600;
        // Use a deterministic id that yields a high fraction (~ 0.99).
        let id = "ffffffff-0000";
        let out = apply_recurring_jitter(next, following, next - 1, id, &cfg);
        assert!(out >= next, "result must not go before next_fire_at");
        assert!(
            out <= next + 60,
            "offset must be <= cap (60s), got {} ({}s)",
            out,
            out - next
        );
    }

    #[test]
    fn apply_recurring_jitter_clamps_to_from_unix() {
        // If the jittered result would land before `from_unix` we
        // must not reschedule the entry into the past.
        let cfg = CronJitterConfig {
            recurring_frac: 0.5,
            recurring_cap_ms: 600_000,
            ..Default::default()
        };
        let next = 1_700_000_000;
        let following = next + 600;
        let from = 1_700_000_300; // mid-window
        let out = apply_recurring_jitter(next, following, from, "abcd1234", &cfg);
        assert!(out > from);
    }

    #[test]
    fn apply_one_shot_lead_minute_mod_zero_skips_jitter() {
        let cfg = CronJitterConfig {
            one_shot_minute_mod: 0,
            ..Default::default()
        };
        let target = 1_700_000_000;
        let out = apply_one_shot_lead(target, target - 1000, "deadbeef", &cfg, 0);
        assert_eq!(out, target);
    }

    #[test]
    fn apply_one_shot_lead_minute_mod_gates_jitter() {
        let cfg = CronJitterConfig {
            one_shot_max_ms: 30_000,
            one_shot_minute_mod: 5,
            ..Default::default()
        };
        let target = 1_700_000_000;
        // minute=3 → 3 % 5 != 0 → no jitter, returns target
        let out = apply_one_shot_lead(target, target - 1000, "deadbeef", &cfg, 3);
        assert_eq!(out, target);
        // minute=10 → 10 % 5 == 0 → jitter applied
        let out = apply_one_shot_lead(target, target - 1000, "deadbeef", &cfg, 10);
        assert!(out <= target);
    }

    #[test]
    fn apply_one_shot_lead_clamps_floor_to_max() {
        let cfg = CronJitterConfig {
            one_shot_max_ms: 30_000,
            one_shot_floor_ms: 10_000,
            one_shot_minute_mod: 1,
            ..Default::default()
        };
        let target = 1_700_000_000;
        // Use a low-fraction id so the window calculation lands
        // below the floor; floor must promote it to 10s lead.
        let out = apply_one_shot_lead(target, target - 1_000_000, "00000001", &cfg, 0);
        assert!(target - out >= 10);
        assert!(target - out <= 30);
    }

    #[test]
    fn apply_one_shot_lead_clamps_to_from_unix() {
        let cfg = CronJitterConfig {
            one_shot_max_ms: 30_000,
            one_shot_minute_mod: 1,
            ..Default::default()
        };
        let target = 1_700_000_000;
        let from = target - 5; // very tight: 5s before target
        let out = apply_one_shot_lead(target, from, "ffffffff", &cfg, 0);
        assert!(out >= from);
    }

    #[tokio::test]
    async fn sweep_missed_entries_quarantines_overdue_rows() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let now = 1_700_000_000;
        let mut e = entry("binding-x", "*/5 * * * *");
        // Force this row 1 hour in the past.
        e.next_fire_at = now - 3_600;
        e.permanent = false;
        store.insert(&e).await.unwrap();

        // 60 s skew → row is well past
        let n = store.sweep_missed_entries(now, 60_000).await.unwrap();
        assert_eq!(n, 1);

        let after = store.get(&e.id).await.unwrap();
        assert_eq!(after.next_fire_at, i64::MAX);
    }

    #[tokio::test]
    async fn sweep_missed_entries_exempts_permanent() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let now = 1_700_000_000;
        let mut e = entry("binding-x", "*/5 * * * *");
        e.next_fire_at = now - 3_600;
        e.permanent = true;
        store.insert(&e).await.unwrap();

        let n = store.sweep_missed_entries(now, 60_000).await.unwrap();
        assert_eq!(n, 0);
        let after = store.get(&e.id).await.unwrap();
        assert_eq!(after.next_fire_at, now - 3_600);
    }

    #[tokio::test]
    async fn sweep_missed_entries_skew_zero_is_noop() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let now = 1_700_000_000;
        let mut e = entry("binding-x", "*/5 * * * *");
        e.next_fire_at = now - 3_600;
        store.insert(&e).await.unwrap();

        let n = store.sweep_missed_entries(now, 0).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn sweep_expired_recurring_deletes_old_recurring_rows() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let now = 1_700_000_000;
        let mut e = entry("binding-x", "*/5 * * * *");
        e.created_at = now - 10 * 86_400; // 10 days old
        e.recurring = true;
        e.permanent = false;
        store.insert(&e).await.unwrap();

        // 7 days max age → should delete
        let n = store
            .sweep_expired_recurring(now, 7 * 86_400 * 1_000)
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert!(store.get(&e.id).await.is_err());
    }

    #[tokio::test]
    async fn sweep_expired_recurring_exempts_permanent() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let now = 1_700_000_000;
        let mut e = entry("binding-x", "*/5 * * * *");
        e.created_at = now - 30 * 86_400;
        e.recurring = true;
        e.permanent = true;
        store.insert(&e).await.unwrap();

        let n = store
            .sweep_expired_recurring(now, 7 * 86_400 * 1_000)
            .await
            .unwrap();
        assert_eq!(n, 0);
        // Row still alive
        let _ = store.get(&e.id).await.unwrap();
    }

    #[tokio::test]
    async fn sweep_expired_recurring_keeps_one_shots() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let now = 1_700_000_000;
        let mut e = entry("binding-x", "*/5 * * * *");
        e.created_at = now - 30 * 86_400;
        e.recurring = false; // one-shot
        store.insert(&e).await.unwrap();

        let n = store
            .sweep_expired_recurring(now, 7 * 86_400 * 1_000)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn permanent_field_round_trips_through_sqlite() {
        let store = SqliteCronStore::open_memory().await.unwrap();
        let mut e = entry("binding-x", "*/5 * * * *");
        e.permanent = true;
        store.insert(&e).await.unwrap();

        let got = store.get(&e.id).await.unwrap();
        assert!(got.permanent);
    }
}
