-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- Long-lived API tokens (docs/proposals/headless-first.md). Only the SHA-256 of a token is kept:
-- the token itself is shown once, when it is issued, and a copy of this table is not a copy of
-- anyone's credentials.
--
-- user_id is the verified subject, as text, exactly like projects.owner_id: a local user's uuid
-- under AUTH_MODE=local, the identity provider's `sub` under jwks. That is why it has no foreign
-- key to users.
--
-- minted_by is the token that minted this one, when a token did. Revoking a token revokes its
-- whole family, so a leaked token's successors die with it.
CREATE TABLE IF NOT EXISTS api_tokens (
    id           uuid PRIMARY KEY,
    user_id      text NOT NULL,
    name         text NOT NULL,
    token_hash   text NOT NULL UNIQUE,
    minted_by    uuid,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz
);

CREATE INDEX IF NOT EXISTS api_tokens_user_idx ON api_tokens (user_id);
CREATE INDEX IF NOT EXISTS api_tokens_minted_by_idx ON api_tokens (minted_by);
