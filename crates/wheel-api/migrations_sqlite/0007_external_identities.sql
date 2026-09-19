-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- ../migrations/0007_external_identities.sql translated for SQLite, by the rules 0001_init.sql
-- states: uuid -> TEXT, timestamptz -> TEXT (RFC3339, so lexical order is chronological order).
-- Same table, same constraints, same meaning. The reasoning lives in the Postgres copy and is not
-- duplicated here, so the two cannot drift in their prose.

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
