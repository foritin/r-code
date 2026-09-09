//! Fresh v2 database schema. This file owns DDL only; the legacy migration
//! machinery never runs against v2 databases and vice versa.

/// Schema DDL applied once per v2 database. Idempotent.
pub const V2_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS v2_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Domain aggregates (tasks/branches/runs) stored as canonical JSON with an
-- optimistic revision counter. The aggregate and its events commit together.
CREATE TABLE IF NOT EXISTS tasks (
    task_id       TEXT PRIMARY KEY,
    branch_id     TEXT NOT NULL,
    revision      INTEGER NOT NULL DEFAULT 1,
    state_json    TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_branch ON tasks(branch_id);

-- Ordered, append-only event journal. Sequences are dense from 1.
CREATE TABLE IF NOT EXISTS events (
    seq      INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id  TEXT NOT NULL,
    kind     TEXT NOT NULL,
    payload  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_task ON events(task_id, seq);

-- Durable operation receipts (attempt-scoped idempotency).
CREATE TABLE IF NOT EXISTS operation_receipts (
    attempt_id          TEXT NOT NULL,
    operation_key       TEXT NOT NULL,
    method              TEXT NOT NULL,
    input_hash          TEXT NOT NULL,
    state_json          TEXT NOT NULL,
    generation_recorded INTEGER NOT NULL,
    generation_completed INTEGER,
    PRIMARY KEY (attempt_id, operation_key)
);

-- One active run per branch: enforced by the branch_id primary key.
CREATE TABLE IF NOT EXISTS run_leases (
    branch_id      TEXT PRIMARY KEY,
    run_id         TEXT NOT NULL,
    attempt_id     TEXT NOT NULL,
    generation     INTEGER NOT NULL,
    owner          TEXT NOT NULL,
    acquired_at_ms INTEGER NOT NULL
);

-- Content-addressed blob registry (bytes live in the blobs root).
CREATE TABLE IF NOT EXISTS blobs (
    blob_id        TEXT PRIMARY KEY,
    sha256         TEXT NOT NULL,
    bytes          INTEGER NOT NULL,
    created_at_ms  INTEGER NOT NULL
);

-- Versioned opaque plugin checkpoints.
CREATE TABLE IF NOT EXISTS checkpoints (
    attempt_id         TEXT NOT NULL,
    revision           INTEGER NOT NULL,
    consumed_input_seq INTEGER NOT NULL,
    state              BLOB NOT NULL,
    blob_id            TEXT NOT NULL,
    created_at_ms      INTEGER NOT NULL,
    PRIMARY KEY (attempt_id, revision)
);

-- Persistent questions (persisted before suspension).
CREATE TABLE IF NOT EXISTS questions (
    question_id    TEXT PRIMARY KEY,
    task_id        TEXT NOT NULL,
    run_id         TEXT NOT NULL,
    text           TEXT NOT NULL,
    options_json   TEXT NOT NULL DEFAULT '[]',
    state          TEXT NOT NULL DEFAULT 'open',
    answer         TEXT,
    created_at_ms  INTEGER NOT NULL,
    answered_at_ms INTEGER
);

-- User review disposition, independent of execution and validation.
CREATE TABLE IF NOT EXISTS reviews (
    task_id      TEXT PRIMARY KEY,
    disposition  TEXT NOT NULL,
    notes        TEXT,
    updated_at_ms INTEGER NOT NULL
);

-- Evidence records keyed by candidate/check/environment identity.
CREATE TABLE IF NOT EXISTS evidence (
    evidence_id     TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    check_id        TEXT NOT NULL,
    candidate_digest TEXT NOT NULL,
    environment     TEXT NOT NULL,
    passed          INTEGER NOT NULL,
    host_output_ref TEXT,
    provenance_json TEXT NOT NULL,
    created_at_ms   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_evidence_candidate ON evidence(candidate_digest, check_id);

-- Canonical branch history: every task records its parent branch, if any.
CREATE TABLE IF NOT EXISTS task_branches (
    task_id         TEXT PRIMARY KEY,
    parent_task_id  TEXT,
    created_at_ms   INTEGER NOT NULL
);

-- Installed plugin packages: identity is (id, version, content digest).
CREATE TABLE IF NOT EXISTS plugin_catalog (
    id               TEXT NOT NULL,
    version          TEXT NOT NULL,
    content_digest   TEXT NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    granted_services TEXT NOT NULL DEFAULT '[]',
    config           TEXT NOT NULL DEFAULT '{}',
    manifest_json    TEXT NOT NULL,
    install_dir      TEXT NOT NULL,
    installed_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (id, version, content_digest)
);

-- Run pins: attempts freeze the exact package bytes they run with.
CREATE TABLE IF NOT EXISTS plugin_pins (
    attempt_id     TEXT PRIMARY KEY,
    task_id        TEXT NOT NULL,
    id             TEXT NOT NULL,
    version        TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plugin_pins_package ON plugin_pins(id, content_digest);

-- Application-level command receipts: (profile, client, command) identity
-- with canonical method+payload hashes, independent of attempt operation
-- keys. Acceptance and result commit atomically.
CREATE TABLE IF NOT EXISTS application_commands (
    profile_id   TEXT NOT NULL,
    client_id    TEXT NOT NULL,
    command_id   TEXT NOT NULL,
    method       TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    state        TEXT NOT NULL,
    result_json  TEXT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (profile_id, client_id, command_id)
);

-- Durable writer barriers: while an old owner's processes may still write,
-- a new write run is blocked until termination is proven.
CREATE TABLE IF NOT EXISTS writer_barriers (
    barrier_id    TEXT PRIMARY KEY,
    workspace_key TEXT NOT NULL,
    owner_pid     INTEGER NOT NULL,
    owner_start   TEXT NOT NULL,
    reason        TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
"#;

pub const V2_SCHEMA_VERSION: &str = "1";
