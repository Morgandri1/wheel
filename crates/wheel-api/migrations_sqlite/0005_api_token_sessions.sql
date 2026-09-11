-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- ../migrations/0005_api_token_sessions.sql for SQLite.
ALTER TABLE api_tokens ADD COLUMN session_id TEXT;
