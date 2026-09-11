-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- The local session that minted a token, when one did. A password change revokes every token its
-- account's sessions minted: a password is changed because someone else may know it, and whoever
-- knew it could have logged in and minted a token that outlives the password.
ALTER TABLE api_tokens ADD COLUMN IF NOT EXISTS session_id uuid;
