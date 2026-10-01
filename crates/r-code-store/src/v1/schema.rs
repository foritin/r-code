//! Fresh v1 database schema. This file owns DDL only; the legacy migration
//! machinery never runs against v1 databases and vice versa.

/// Schema DDL applied once per v1 database. Idempotent.
pub const V1_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS v2_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Additive migration ledger for features introduced within API/storage v1.
-- Keeping this separate from the legacy schema-version row lets old v1
-- databases acquire new tables idempotently when opened by a newer daemon.
CREATE TABLE IF NOT EXISTS v1_schema_migrations (
    migration_id TEXT PRIMARY KEY,
    applied_at_ms INTEGER NOT NULL
);

-- Immutable, content-addressed configuration frozen before a run starts.
CREATE TABLE IF NOT EXISTS run_snapshots (
    snapshot_id    TEXT PRIMARY KEY,
    task_id        TEXT NOT NULL,
    phase          TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    snapshot_json  TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_run_snapshots_task
    ON run_snapshots(task_id, created_at_ms);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('run-snapshots', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Immutable, task-owned plan revisions. `payload_json` preserves the legacy
-- opaque plan API byte-for-byte while `material_json` is the canonical,
-- credential-free identity used by new callers.
CREATE TABLE IF NOT EXISTS plan_revisions (
    plan_revision    TEXT PRIMARY KEY,
    task_id          TEXT NOT NULL,
    revision_number  INTEGER NOT NULL CHECK (revision_number > 0),
    parent_revision  TEXT,
    current_base_hash TEXT NOT NULL,
    content_sha256   TEXT NOT NULL,
    material_json    TEXT NOT NULL,
    payload_json     TEXT NOT NULL,
    created_at_ms    INTEGER NOT NULL,
    UNIQUE (task_id, revision_number),
    UNIQUE (task_id, plan_revision),
    FOREIGN KEY (task_id, parent_revision)
        REFERENCES plan_revisions(task_id, plan_revision)
);
CREATE INDEX IF NOT EXISTS idx_plan_revisions_task
    ON plan_revisions(task_id, revision_number);

-- One compare-and-swap head per task. Plans deliberately do not reuse the
-- user-review table: review disposition and plan approval are independent.
CREATE TABLE IF NOT EXISTS task_plan_heads (
    task_id          TEXT PRIMARY KEY,
    plan_revision    TEXT NOT NULL,
    revision_number  INTEGER NOT NULL CHECK (revision_number > 0),
    updated_at_ms    INTEGER NOT NULL,
    FOREIGN KEY (task_id, plan_revision)
        REFERENCES plan_revisions(task_id, plan_revision)
);

-- Exact-revision approvals. Effect approvals live elsewhere and cannot
-- satisfy this foreign-keyed aggregate.
CREATE TABLE IF NOT EXISTS plan_approvals (
    approval_id      TEXT PRIMARY KEY,
    task_id          TEXT NOT NULL,
    plan_revision    TEXT NOT NULL,
    actor_id         TEXT NOT NULL,
    session_id       TEXT NOT NULL,
    scope            TEXT NOT NULL CHECK (scope = 'plan.approve'),
    state            TEXT NOT NULL CHECK (state IN ('active', 'superseded')),
    approval_json    TEXT NOT NULL,
    created_at_ms    INTEGER NOT NULL,
    superseded_at_ms INTEGER,
    FOREIGN KEY (task_id, plan_revision)
        REFERENCES plan_revisions(task_id, plan_revision)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_approvals_one_active
    ON plan_approvals(task_id) WHERE state = 'active';
CREATE INDEX IF NOT EXISTS idx_plan_approvals_revision
    ON plan_approvals(task_id, plan_revision, state);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('plan-revisions-and-approvals', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Durable effect ownership. Leases remain active across daemon restarts and
-- are released only by the recorded owner carrying the current fence.
CREATE TABLE IF NOT EXISTS lease_epochs (
    workspace_key TEXT PRIMARY KEY,
    last_epoch    INTEGER NOT NULL CHECK (last_epoch >= 0)
);

CREATE TABLE IF NOT EXISTS path_leases (
    lease_id       TEXT PRIMARY KEY,
    workspace_key  TEXT NOT NULL,
    operation_id   TEXT NOT NULL,
    owner_id       TEXT NOT NULL,
    fencing_epoch  INTEGER NOT NULL CHECK (fencing_epoch > 0),
    request_hash   TEXT NOT NULL,
    request_json   TEXT NOT NULL,
    active         INTEGER NOT NULL CHECK (active IN (0, 1)),
    acquired_at_ms INTEGER NOT NULL,
    released_at_ms INTEGER,
    UNIQUE (workspace_key, operation_id)
);
CREATE INDEX IF NOT EXISTS idx_path_leases_active
    ON path_leases(workspace_key, active, fencing_epoch);

-- Each side effect is journaled before application. File hashes and CAS
-- references are written only when the effect is known to have been applied.
CREATE TABLE IF NOT EXISTS mutation_operations (
    operation_id    TEXT PRIMARY KEY,
    workspace_key   TEXT NOT NULL,
    lease_id        TEXT NOT NULL,
    owner_id        TEXT NOT NULL,
    fencing_epoch   INTEGER NOT NULL CHECK (fencing_epoch > 0),
    input_hash      TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (
        state IN ('prepared', 'applied', 'receipted', 'conflict')
    ),
    prepared_at_ms  INTEGER NOT NULL,
    applied_at_ms   INTEGER,
    receipted_at_ms INTEGER,
    FOREIGN KEY (lease_id) REFERENCES path_leases(lease_id)
);
CREATE INDEX IF NOT EXISTS idx_mutation_operations_lease
    ON mutation_operations(lease_id, state);

CREATE TABLE IF NOT EXISTS mutation_files (
    operation_id    TEXT NOT NULL,
    logical_path    TEXT NOT NULL,
    before_sha256   TEXT,
    after_sha256    TEXT,
    before_cas_ref  TEXT,
    after_cas_ref   TEXT,
    PRIMARY KEY (operation_id, logical_path),
    FOREIGN KEY (operation_id)
        REFERENCES mutation_operations(operation_id) ON DELETE CASCADE
);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('durable-effect-journal', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

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

-- Host-owned immutable check definitions. Evidence references the exact
-- definition identity rather than embedding mutable definition material.
CREATE TABLE IF NOT EXISTS check_definitions (
    check_id        TEXT PRIMARY KEY,
    identity        TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    created_at_ms   INTEGER NOT NULL
);

-- Evidence records keyed by task/candidate/definition/environment identity.
CREATE TABLE IF NOT EXISTS evidence (
    evidence_id     TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL,
    check_id        TEXT NOT NULL,
    definition_identity TEXT NOT NULL DEFAULT '',
    candidate_digest TEXT NOT NULL,
    environment     TEXT NOT NULL,
    environment_fingerprint TEXT NOT NULL DEFAULT '',
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

-- Fenced process-tree ownership. A non-Exited row is an authoritative write
-- quarantine; workspace epochs are monotonic and never reused.
CREATE TABLE IF NOT EXISTS workspace_ownership_epochs (
    workspace_key TEXT PRIMARY KEY,
    last_epoch    INTEGER NOT NULL CHECK (last_epoch >= 0)
);

CREATE TABLE IF NOT EXISTS process_trees (
    tree_id                         TEXT PRIMARY KEY,
    attempt_id                      TEXT NOT NULL,
    workspace_key                   TEXT NOT NULL,
    profile_id                      TEXT NOT NULL,
    owner_pid                       INTEGER NOT NULL CHECK (owner_pid >= 0),
    owner_start_identity            TEXT NOT NULL,
    owner_boot_identity             TEXT NOT NULL,
    platform_identity_json          TEXT NOT NULL,
    platform_identity_digest        TEXT NOT NULL,
    ownership_epoch                 INTEGER NOT NULL CHECK (ownership_epoch > 0),
    state                           TEXT NOT NULL CHECK (state IN (
        'prepared', 'running', 'terminating', 'exited',
        'quarantined', 'legacy-unverifiable'
    )),
    state_revision                  INTEGER NOT NULL DEFAULT 1 CHECK (state_revision > 0),
    quarantine_reason               TEXT,
    migrated_observed_boot_identity TEXT,
    termination_proof_id            TEXT,
    created_at_ms                   INTEGER NOT NULL,
    updated_at_ms                   INTEGER NOT NULL,
    legacy_barrier_id               TEXT UNIQUE,
    UNIQUE (workspace_key, ownership_epoch)
);
CREATE INDEX IF NOT EXISTS idx_process_trees_workspace_state
    ON process_trees(workspace_key, state, ownership_epoch);

CREATE TABLE IF NOT EXISTS termination_proofs (
    proof_id                   TEXT PRIMARY KEY,
    tree_id                    TEXT NOT NULL,
    ownership_epoch            INTEGER NOT NULL CHECK (ownership_epoch > 0),
    proof_kind                 TEXT NOT NULL CHECK (proof_kind IN ('exit', 'reboot')),
    observed_boot_identity     TEXT NOT NULL,
    proof_identity_json        TEXT NOT NULL,
    proof_identity_digest      TEXT NOT NULL,
    recorded_at_ms             INTEGER NOT NULL,
    UNIQUE (tree_id, ownership_epoch, proof_identity_digest),
    FOREIGN KEY (tree_id) REFERENCES process_trees(tree_id)
);
CREATE INDEX IF NOT EXISTS idx_termination_proofs_tree
    ON termination_proofs(tree_id, ownership_epoch, recorded_at_ms);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('process-tree-ownership', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Content-addressed safety capability reports (P12). One row per report
-- identity; safety_report_heads points at each capability's current report
-- so stale/foreign rows can survive for audit without ever activating.
CREATE TABLE IF NOT EXISTS safety_capability_reports (
    report_id       TEXT PRIMARY KEY,
    capability      TEXT NOT NULL,
    material_digest TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (
        status IN ('unsupported', 'safe-disabled', 'activated')
    ),
    material_json   TEXT NOT NULL,
    created_at_ms   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_safety_reports_capability
    ON safety_capability_reports(capability, created_at_ms);

CREATE TABLE IF NOT EXISTS safety_report_heads (
    capability TEXT PRIMARY KEY,
    report_id  TEXT NOT NULL,
    FOREIGN KEY (report_id) REFERENCES safety_capability_reports(report_id)
);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('safety-capability-reports', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Journaled ACL deltas (P14): every AppContainer ACL mutation is recorded
-- BEFORE it is applied; restore is physical/ACL CAS only and external
-- edits settle as immutable conflicts, never overwritten.
CREATE TABLE IF NOT EXISTS acl_operations (
    operation_id      TEXT PRIMARY KEY,
    target_path       TEXT NOT NULL,
    physical_identity TEXT NOT NULL,
    state             TEXT NOT NULL CHECK (
        state IN ('prepared', 'applied', 'restored', 'conflict')
    ),
    before_descriptor BLOB NOT NULL,
    planned_delta     TEXT NOT NULL,
    actual_after      BLOB,
    conflict_reason   TEXT,
    prepared_at_ms    INTEGER NOT NULL,
    applied_at_ms     INTEGER,
    settled_at_ms     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_acl_operations_target
    ON acl_operations(target_path, prepared_at_ms);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('acl-operation-journal', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Immutable exact-plan effect approvals (P19A). One active approval per
-- (task, work unit); approvals are exact — task/plan-revision/work-unit/
-- class/network/payload-hash must all match for a snapshot to expand.
CREATE TABLE IF NOT EXISTS work_unit_effect_approvals (
    approval_id   TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL,
    plan_revision TEXT NOT NULL,
    work_unit_id  TEXT NOT NULL,
    effect_class  TEXT NOT NULL CHECK (
        effect_class IN ('read-only', 'workspace-mutation', 'dependency-preparation')
    ),
    network       TEXT NOT NULL CHECK (
        network IN ('offline', 'public-internet-client', 'host-network')
    ),
    actor_id      TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    scope         TEXT NOT NULL CHECK (scope = 'effect.approve'),
    payload_hash  TEXT NOT NULL,
    state         TEXT NOT NULL CHECK (state IN ('active', 'superseded')),
    created_at_ms INTEGER NOT NULL,
    superseded_at_ms INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_effect_approvals_one_active
    ON work_unit_effect_approvals(task_id, work_unit_id) WHERE state = 'active';
CREATE INDEX IF NOT EXISTS idx_effect_approvals_plan
    ON work_unit_effect_approvals(task_id, plan_revision, work_unit_id, state);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('work-unit-effect-approvals', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Durable process-effect operations (P25). The complete launch material —
-- verbatim command, tree, lease, before manifest, scan policy, ephemeral
-- roots — is journaled BEFORE a process resumes. Recovery reconciles
-- against the frozen row and never re-derives or re-runs the command; the
-- delta scanner (P26) consumes the same row. Artifact blobs are P26A.
CREATE TABLE IF NOT EXISTS process_effect_operations (
    operation_id           TEXT PRIMARY KEY,
    tree_id                TEXT NOT NULL,
    attempt_id             TEXT NOT NULL,
    workspace_key          TEXT NOT NULL,
    owner_id               TEXT NOT NULL,
    fencing_epoch          INTEGER NOT NULL CHECK (fencing_epoch > 0),
    command_json           TEXT NOT NULL,
    lease_json             TEXT NOT NULL,
    before_manifest_json   TEXT NOT NULL,
    before_manifest_digest TEXT NOT NULL,
    scan_policy_json       TEXT NOT NULL,
    ephemeral_roots_json   TEXT NOT NULL,
    state                  TEXT NOT NULL CHECK (
        state IN ('prepared', 'running', 'receipted', 'quarantined')
    ),
    state_revision         INTEGER NOT NULL DEFAULT 1 CHECK (state_revision > 0),
    quarantine_reason      TEXT,
    receipt_digest         TEXT,
    created_at_ms          INTEGER NOT NULL,
    updated_at_ms          INTEGER NOT NULL,
    FOREIGN KEY (tree_id) REFERENCES process_trees(tree_id)
);
CREATE INDEX IF NOT EXISTS idx_process_effects_tree
    ON process_effect_operations(tree_id, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_process_effects_incomplete
    ON process_effect_operations(workspace_key, state);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('process-effect-operations', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Effect-artifact refs and disk reservations (P26A). Refs are the
-- refcount that keeps effect-owned inverse data alive: one row per
-- (operation, digest), released only after the terminal receipt, so active
-- inverse data is never collectable. Reservations are the disk quota an
-- operation holds from before resume until its receipt releases them.
CREATE TABLE IF NOT EXISTS effect_artifact_refs (
    operation_id  TEXT NOT NULL,
    digest        TEXT NOT NULL,
    bytes         INTEGER NOT NULL CHECK (bytes >= 0),
    kind          TEXT NOT NULL CHECK (kind IN ('manifest', 'before-blob', 'delta-blob', 'output-tail')),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (operation_id, digest)
);
CREATE INDEX IF NOT EXISTS idx_effect_artifact_refs_digest
    ON effect_artifact_refs(digest);

CREATE TABLE IF NOT EXISTS artifact_reservations (
    reservation_id TEXT PRIMARY KEY,
    operation_id   TEXT NOT NULL,
    bytes          INTEGER NOT NULL CHECK (bytes > 0),
    created_at_ms  INTEGER NOT NULL,
    released_at_ms INTEGER
);
CREATE INDEX IF NOT EXISTS idx_artifact_reservations_active
    ON artifact_reservations(operation_id, released_at_ms);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('effect-artifact-refs-and-reservations', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Durable per-WorkUnit execution attempts (E06). One row per (task, plan
-- revision, work unit); the attempt id derives from that triple, so a
-- replayed dispatch converges on the existing row while divergent content
-- under the same id is refused. Every dispatch writes its row BEFORE any
-- spawn, and a row settles exactly once — late or duplicate settles refuse
-- (the resume-once vocabulary).
CREATE TABLE IF NOT EXISTS work_unit_attempts (
    attempt_id     TEXT PRIMARY KEY,
    task_id        TEXT NOT NULL,
    plan_revision  TEXT NOT NULL,
    work_unit_id   TEXT NOT NULL,
    phase          TEXT NOT NULL CHECK (
        phase IN ('prepared', 'dispatched', 'settled-completed', 'settled-failed')
    ),
    content_sha256 TEXT NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL,
    settled_at_ms  INTEGER,
    UNIQUE (task_id, plan_revision, work_unit_id)
);
CREATE INDEX IF NOT EXISTS idx_work_unit_attempts_in_flight
    ON work_unit_attempts(task_id, plan_revision, phase);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('work-unit-attempts', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Durable lease families (E07). One family per in-flight attempt, keyed by
-- that attempt's identity: every member lease is acquired in the family's
-- single all-or-nothing transaction, and none outlives the family's durable
-- settle or quarantine. Fencing is the workspace lease epoch the members
-- already carry — the family derives it, never mints a third currency.
-- The attempt_id key intentionally carries no foreign key into
-- work_unit_attempts: a family is acquired when its executor takes shape,
-- which precedes the attempt row's prepare, and restart reconciliation
-- treats a family without its attempt row as quarantinable, not corrupt.
CREATE TABLE IF NOT EXISTS lease_families (
    attempt_id    TEXT PRIMARY KEY,
    workspace_key TEXT NOT NULL,
    owner_id      TEXT NOT NULL,
    state         TEXT NOT NULL CHECK (state IN ('active', 'released', 'quarantined')),
    created_at_ms INTEGER NOT NULL,
    settled_at_ms INTEGER
);
CREATE INDEX IF NOT EXISTS idx_lease_families_active ON lease_families(state);

CREATE TABLE IF NOT EXISTS lease_family_members (
    attempt_id TEXT NOT NULL,
    lease_id   TEXT NOT NULL,
    PRIMARY KEY (attempt_id, lease_id),
    FOREIGN KEY (lease_id) REFERENCES path_leases(lease_id)
);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('lease-families', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- Immutable unverified-override audit rows (E09-R). One row per (task,
-- override id) written in the SAME transaction as the UnverifiedAccepted
-- verdict it audits: the table is the single query source for overrides and
-- the review journal event is its derived projection. Rows carry the exact
-- failed check ids, never a summary.
CREATE TABLE IF NOT EXISTS unverified_overrides (
    override_id      TEXT NOT NULL,
    task_id          TEXT NOT NULL,
    candidate_digest TEXT NOT NULL,
    actor_id         TEXT NOT NULL,
    session_id       TEXT NOT NULL,
    reason           TEXT NOT NULL,
    checks_json      TEXT NOT NULL,
    created_at_ms    INTEGER NOT NULL,
    PRIMARY KEY (task_id, override_id)
);
CREATE INDEX IF NOT EXISTS idx_unverified_overrides_task
    ON unverified_overrides(task_id, created_at_ms);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('unverified-overrides', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- M1a-02 (FR-7.2 / FR-1.6): the daemon-side injection ledger. Mirrors the
-- desktop memory_injections structure with a kind discriminator so memory,
-- frozen instructions, and JIT injections share one audit surface. The
-- daemon never writes the desktop r-code.db.
CREATE TABLE IF NOT EXISTS injections (
    sequence       INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id         TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('memory', 'instruction', 'jit')),
    snapshot_hash  TEXT NOT NULL,
    refs_json      TEXT NOT NULL,
    chars          INTEGER NOT NULL CHECK (chars >= 0),
    created_at_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_injections_run ON injections(run_id, kind);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('injection-ledger', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

-- M1a-06 (FR-1 / PRD 8): per-workspace instruction-injection settings.
-- Stored under the AppData-side daemon data root, keyed like path leases.
CREATE TABLE IF NOT EXISTS context_settings (
    workspace_key  TEXT PRIMARY KEY,
    settings_json  TEXT NOT NULL,
    updated_at_ms  INTEGER NOT NULL
);

INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
VALUES ('context-settings', CAST(strftime('%s', 'now') AS INTEGER) * 1000);
"#;

pub const V1_SCHEMA_VERSION: &str = "1";

/// Upgrade evidence identity columns on databases created by earlier v1
/// builds. SQLite has no `ADD COLUMN IF NOT EXISTS`, so inspect and alter in
/// one immediate transaction before recording the migration ledger entry.
pub fn apply_additive_migrations(
    connection: &mut rusqlite::Connection,
) -> Result<(), rusqlite::Error> {
    use rusqlite::{params, TransactionBehavior};
    use std::collections::BTreeSet;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let columns = {
        let mut statement = transaction.prepare("PRAGMA table_info(evidence)")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<BTreeSet<_>, _>>()?;
        rows
    };
    if !columns.contains("definition_identity") {
        transaction.execute(
            "ALTER TABLE evidence ADD COLUMN definition_identity TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !columns.contains("environment_fingerprint") {
        transaction.execute(
            "ALTER TABLE evidence ADD COLUMN environment_fingerprint TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    transaction.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_evidence_identity
         ON evidence(task_id, check_id, candidate_digest, definition_identity,
                     environment_fingerprint)
         WHERE definition_identity <> '' AND environment_fingerprint <> '';",
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
         VALUES ('check-definitions-and-evidence-identity', ?1)",
        params![migration_now_ms()],
    )?;
    transaction.commit()
}

fn migration_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
