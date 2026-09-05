-- Wheel engine schema. One sqlite file per project at <data>/wheel.db.
-- Every statement is CREATE ... IF NOT EXISTS because migrate() runs on every
-- boot, including after a crash mid-write.

CREATE TABLE IF NOT EXISTS nodes (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL UNIQUE,
    type        TEXT NOT NULL,
    config      TEXT NOT NULL,           -- JSON: the `config` half of NodeConfig
    x           REAL NOT NULL DEFAULT 0,
    y           REAL NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

-- Wires are stored once, on the source. `granted_by` records the node that
-- delegated this capability (§3e grant/revoke); NULL means the operator made it.
CREATE TABLE IF NOT EXISTS wires (
    from_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    to_id       TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    type        TEXT NOT NULL,
    granted_by  TEXT REFERENCES nodes(id) ON DELETE SET NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (from_id, to_id, type)
) STRICT;

CREATE INDEX IF NOT EXISTS wires_to_idx ON wires(to_id);

CREATE TABLE IF NOT EXISTS messages (
    id           TEXT PRIMARY KEY NOT NULL,
    from_kind    TEXT NOT NULL,          -- node | user | system
    from_id      TEXT,                   -- node id when from_kind = 'node'
    to_id        TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    body         TEXT NOT NULL,
    sha256       TEXT NOT NULL,
    bytes        INTEGER NOT NULL,
    reply_to     TEXT,
    state        TEXT NOT NULL,          -- queued | delivered | consumed
    is_error     INTEGER NOT NULL DEFAULT 0,
    last_error   TEXT,
    created_at   TEXT NOT NULL,
    delivered_at TEXT,
    consumed_at  TEXT
) STRICT;

-- The delivery loop's hot query: the next queued message for an agent, user
-- lane first, then oldest.
CREATE INDEX IF NOT EXISTS messages_queue_idx
    ON messages(to_id, state, from_kind, created_at);

CREATE TABLE IF NOT EXISTS agent_state (
    node_id       TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    status        TEXT NOT NULL,
    session_id    TEXT,
    last_activity TEXT,
    last_error    TEXT,
    hosted_on     TEXT,                  -- NULL = unhosted, a loud state
    turns         INTEGER NOT NULL DEFAULT 0,
    usd           REAL NOT NULL DEFAULT 0
) STRICT;

-- Per-node capability tokens. Only the HASH is stored: reading this database
-- must not yield a usable credential. Rotated on every agent start.
CREATE TABLE IF NOT EXISTS node_tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    node_id    TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS node_tokens_node_idx ON node_tokens(node_id);

CREATE TABLE IF NOT EXISTS logs (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    seq     INTEGER NOT NULL,
    stream  TEXT NOT NULL,               -- stdout | stderr | engine | transcript
    at      TEXT NOT NULL,
    text    TEXT NOT NULL,
    PRIMARY KEY (node_id, seq)
) STRICT;

CREATE TABLE IF NOT EXISTS vault_values (
    node_id    TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    nonce      BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (node_id, key)
) STRICT;

CREATE TABLE IF NOT EXISTS chest_index (
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    key         TEXT NOT NULL,
    bytes       INTEGER NOT NULL,
    modified_at TEXT NOT NULL,
    PRIMARY KEY (node_id, key)
) STRICT;
