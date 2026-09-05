-- Local auth (AUTH_MODE=local). The identity provider is pluggable; this is the built-in one.

CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE IF NOT EXISTS users (
    id            uuid PRIMARY KEY,
    -- citext so Alice@example.com and alice@example.com are one account. Without it, address
    -- casing silently creates a second user who cannot see the first one's projects.
    email         citext NOT NULL UNIQUE,
    -- argon2id, encoded PHC string: algorithm, parameters and salt travel with the hash, so
    -- parameters can be raised later without breaking existing rows.
    password_hash text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Sessions are revocable: a logout has to end a session that a stateless JWT would otherwise
-- honour until it expired. One row per issued session, deleted on logout, swept once expired.
CREATE TABLE IF NOT EXISTS sessions (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE INDEX IF NOT EXISTS sessions_user_idx ON sessions (user_id);
CREATE INDEX IF NOT EXISTS sessions_expiry_idx ON sessions (expires_at);

-- Shared counters for the unauthenticated auth endpoints. Shared rather than in-process because
-- the API runs as N replicas and a per-replica limit weakens as you scale.
CREATE TABLE IF NOT EXISTS auth_attempts (
    key          text NOT NULL,
    window_start timestamptz NOT NULL,
    attempts     bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (key, window_start)
);
