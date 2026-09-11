-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- ../migrations/0004_api_tokens.sql translated for SQLite: uuid and timestamptz become TEXT, and
-- timestamps use the same RFC3339 format the driver writes, so they compare as they order.
CREATE TABLE IF NOT EXISTS api_tokens (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    minted_by    TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_used_at TEXT,
    revoked_at   TEXT
);

CREATE INDEX IF NOT EXISTS api_tokens_user_idx ON api_tokens (user_id);
CREATE INDEX IF NOT EXISTS api_tokens_minted_by_idx ON api_tokens (minted_by);
