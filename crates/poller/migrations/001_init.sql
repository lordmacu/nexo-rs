-- Phase 19 — pollers state.
-- WAL + foreign keys keep concurrent reads consistent without
-- blocking on writers. Single writer pattern is enforced at the
-- pool level (`max_connections=1` for writes) when this DB is
-- shared with other subsystems.
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS poll_state (
    job_id                 TEXT PRIMARY KEY,
    cursor                 BLOB,
    last_run_at            INTEGER,                -- ms since epoch
    next_run_at            INTEGER,                -- ms since epoch
    last_status            TEXT,                   -- 'ok' | 'transient' | 'permanent' | 'skipped'
    last_error             TEXT,
    last_duration_ms       INTEGER,
    consecutive_errors     INTEGER NOT NULL DEFAULT 0,
    items_seen_total       INTEGER NOT NULL DEFAULT 0,
    items_dispatched_total INTEGER NOT NULL DEFAULT 0,
    paused                 INTEGER NOT NULL DEFAULT 0,  -- bool
    last_failure_alert_at  INTEGER,                -- ms; cooldown anchor
    updated_at             INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS poll_state_next_run_at ON poll_state(next_run_at);
CREATE INDEX IF NOT EXISTS poll_state_paused      ON poll_state(paused);

CREATE TABLE IF NOT EXISTS poll_lease (
    job_id        TEXT PRIMARY KEY,
    leaseholder   TEXT NOT NULL,                   -- pid + nonce
    running_until INTEGER NOT NULL                 -- ms since epoch
);

CREATE INDEX IF NOT EXISTS poll_lease_running_until ON poll_lease(running_until);
