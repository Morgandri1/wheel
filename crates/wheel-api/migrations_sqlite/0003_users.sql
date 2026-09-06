-- Local auth (AUTH_MODE=local), the built-in identity provider.

CREATE TABLE IF NOT EXISTS users (
    id            TEXT PRIMARY KEY,
    -- COLLATE NOCASE is the citext stand-in: Alice@example.com and alice@example.com must be one
    -- account. Without it, address casing silently creates a second user who cannot see the
    -- first one's projects. The collation is on the column, so the UNIQUE index inherits it.
    email         TEXT NOT NULL COLLATE NOCASE UNIQUE,
    -- argon2id, encoded PHC string: algorithm, parameters and salt travel with the hash.
    password_hash TEXT NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Sessions are revocable: a logout has to end a session a stateless JWT would otherwise honour
-- until it expired. ON DELETE CASCADE only fires when foreign_keys is ON, which Db::connect asks
-- for explicitly — SQLite leaves it off by default.
CREATE TABLE IF NOT EXISTS sessions (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS sessions_user_idx ON sessions (user_id);
CREATE INDEX IF NOT EXISTS sessions_expiry_idx ON sessions (expires_at);

CREATE TABLE IF NOT EXISTS auth_attempts (
    key          TEXT NOT NULL,
    window_start TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (key, window_start)
);
