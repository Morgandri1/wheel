-- The Postgres schema (../migrations) translated for SQLite. Same tables, same constraints, same
-- meaning; only the types change, because SQLite has no uuid, jsonb, citext or timestamptz.
--
-- uuid       -> TEXT   (sqlx encodes Uuid as text here; pinned by tests/sqlite_dialect.rs)
-- jsonb      -> TEXT   (serde_json values round-trip as text)
-- bytea      -> BLOB
-- timestamptz-> TEXT   (RFC3339, so lexical order is chronological order)

CREATE TABLE IF NOT EXISTS projects (
    id           TEXT PRIMARY KEY,
    owner_id     TEXT NOT NULL,
    name         TEXT NOT NULL,
    -- Fail-closed default: public HTTP ingress is opt-in.
    capabilities TEXT NOT NULL DEFAULT '{"http": false}',
    status       TEXT NOT NULL DEFAULT 'stopped',
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS projects_owner_idx ON projects (owner_id);

CREATE TABLE IF NOT EXISTS project_secrets (
    project_id        TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    -- AES-256-GCM sealed under API_MASTER_KEY: nonce(12) || ciphertext || tag(16).
    engine_secret_enc BLOB NOT NULL,
    vault_key_enc     BLOB NOT NULL,
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Kept even though a local install is one process: the counter is part of the ingress contract,
-- and a limiter that exists on one backend and not the other is a difference that only shows up
-- under attack.
CREATE TABLE IF NOT EXISTS ingress_rate_limits (
    project_id   TEXT NOT NULL,
    window_start TEXT NOT NULL,
    hits         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, window_start)
);
