-- Single-use tickets for browser WebSocket handshakes. See ../migrations/0002_ws_tickets.sql for
-- why only the hash is stored: read access to this table yields nothing usable.
CREATE TABLE IF NOT EXISTS ws_tickets (
    ticket_hash BLOB PRIMARY KEY,
    user_id     TEXT NOT NULL,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    expires_at  TEXT NOT NULL,
    -- NULL until redeemed, flipped in the same statement that reads the row.
    used_at     TEXT
);

CREATE INDEX IF NOT EXISTS ws_tickets_expiry_idx ON ws_tickets (expires_at);
