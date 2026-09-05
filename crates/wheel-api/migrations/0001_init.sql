-- Projects and their secrets. See docs/ARCHITECTURE.md §5.

CREATE TABLE IF NOT EXISTS projects (
    id           uuid PRIMARY KEY,
    owner_id     text NOT NULL,
    name         text NOT NULL,
    -- Fail-closed default: public HTTP ingress is opt-in.
    capabilities jsonb NOT NULL DEFAULT '{"http": false}'::jsonb,
    status       text NOT NULL DEFAULT 'stopped',
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

-- Every project lookup is scoped by owner, so the index matches the access pattern.
CREATE INDEX IF NOT EXISTS projects_owner_idx ON projects (owner_id);

CREATE TABLE IF NOT EXISTS project_secrets (
    project_id        uuid PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    -- AES-256-GCM sealed under API_MASTER_KEY: nonce(12) || ciphertext || tag(16).
    engine_secret_enc bytea NOT NULL,
    vault_key_enc     bytea NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now()
);

-- Shared fixed-window counter for the public ingress route. Must be shared rather than in-process
-- because the API runs as N stateless replicas.
CREATE TABLE IF NOT EXISTS ingress_rate_limits (
    project_id   uuid NOT NULL,
    window_start timestamptz NOT NULL,
    hits         bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, window_start)
);
