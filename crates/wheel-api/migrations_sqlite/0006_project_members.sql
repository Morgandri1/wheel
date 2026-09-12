-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- ../migrations/0006_project_members.sql translated for SQLite, by the rules 0001_init.sql states:
-- uuid -> TEXT, timestamptz -> TEXT (RFC3339, so lexical order is chronological order),
-- integer -> INTEGER. Same tables, same constraints, same meaning. The reasoning for each table
-- lives in the Postgres copy and is not duplicated here, so the two cannot drift in their prose.

CREATE TABLE IF NOT EXISTS project_members (
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('admin', 'prompter', 'guest')),
    invited_by TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    revoked_at TEXT,
    PRIMARY KEY (project_id, user_id)
);

CREATE INDEX IF NOT EXISTS project_members_user_idx
    ON project_members (user_id) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS project_invites (
    id         TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('admin', 'prompter', 'guest')),
    token_hash TEXT NOT NULL UNIQUE,
    email      TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    max_uses   INTEGER NOT NULL DEFAULT 1 CHECK (max_uses > 0),
    uses       INTEGER NOT NULL DEFAULT 0,
    revoked_at TEXT
);

CREATE INDEX IF NOT EXISTS project_invites_project_idx ON project_invites (project_id);

CREATE TABLE IF NOT EXISTS external_identities (
    id           TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    issuer       TEXT NOT NULL,
    subject      TEXT NOT NULL,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email        TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_seen_at TEXT,
    disabled_at  TEXT,
    UNIQUE (issuer, subject)
);

CREATE INDEX IF NOT EXISTS external_identities_user_idx ON external_identities (user_id);
