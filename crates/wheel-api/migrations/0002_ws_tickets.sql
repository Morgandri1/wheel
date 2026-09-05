-- Single-use tickets for browser WebSocket handshakes.
--
-- A browser cannot set headers on a WebSocket handshake, and the session JWT must never travel in
-- a URL (URLs land in proxy logs, referrers and browser history). So the client exchanges its JWT
-- for a short-lived, single-use ticket over a normal authenticated POST, then opens the socket
-- with `?ticket=…`.
--
-- Only the SHA-256 of the ticket is stored. Read access to this table therefore yields nothing
-- usable: an attacker with the rows still cannot open a socket, because the value the server
-- compares against is derived from a secret they do not have.
CREATE TABLE IF NOT EXISTS ws_tickets (
    ticket_hash bytea PRIMARY KEY,
    user_id     text NOT NULL,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    expires_at  timestamptz NOT NULL,
    -- NULL until redeemed. Redemption flips this in the same statement that reads the row, so two
    -- concurrent replicas cannot both accept the same ticket.
    used_at     timestamptz
);

CREATE INDEX IF NOT EXISTS ws_tickets_expiry_idx ON ws_tickets (expires_at);
