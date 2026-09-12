-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- Shared projects (docs/proposals/shared-projects.md) and external identities
-- (docs/proposals/external-auth.md).
--
-- Numbered 0006, not 0005: 0005_api_token_sessions.sql already exists from the headless-first
-- work. Verified by listing this directory rather than by reading a plan that said otherwise.

-- Membership. `user_id` is a Wheel principal as text, exactly like projects.owner_id and
-- api_tokens.user_id: a users.id uuid under AUTH_MODE=local and =external, an identity provider's
-- `sub` under the legacy jwks mode. That is why it has no foreign key to users.
--
-- The three tiers are the operator's ruling of 2026-09-11: admin, prompter, guest.
--
-- The CREATOR IS NOT A ROW HERE. Ownership is projects.owner_id, and a membership table that could
-- also say who owns a project would be a second answer to that question, free to disagree with the
-- first. Instead `load_member` resolves owner_id to `admin` and joins this table for everyone else,
-- so being the creator can only ever ADD admin and never contradict a row. A member row for the
-- creator is refused by the API for the same reason, and there is deliberately no backfill.
--
-- revoked_at is soft rather than a DELETE: revocation has to be visible afterwards, and a live
-- WebSocket has to be closed when it happens, which needs a row to have changed rather than
-- vanished.
CREATE TABLE IF NOT EXISTS project_members (
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    user_id    text NOT NULL,
    role       text NOT NULL CHECK (role IN ('admin', 'prompter', 'guest')),
    invited_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    PRIMARY KEY (project_id, user_id)
);

-- "Which projects can this person reach" is the project list's query, and it only ever wants live
-- memberships.
CREATE INDEX IF NOT EXISTS project_members_user_idx
    ON project_members (user_id) WHERE revoked_at IS NULL;

-- Invites. Only the SHA-256 of the invite token is kept — the same rule as api_tokens, for the
-- same reason: a copy of this table is not a copy of anyone's credentials, and the digest is the
-- lookup key so an index probe takes time depending on nothing an attacker can steer.
--
-- An invite may grant any tier including admin, because the ruling puts member management in that
-- tier. Bounded by construction: it expires, it has a use count, and it may be locked to an email.
CREATE TABLE IF NOT EXISTS project_invites (
    id         uuid PRIMARY KEY,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    role       text NOT NULL CHECK (role IN ('admin', 'prompter', 'guest')),
    token_hash text NOT NULL UNIQUE,
    email      text,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    max_uses   integer NOT NULL DEFAULT 1 CHECK (max_uses > 0),
    uses       integer NOT NULL DEFAULT 0,
    revoked_at timestamptz
);

CREATE INDEX IF NOT EXISTS project_invites_project_idx ON project_invites (project_id);

-- An external subject, and the Wheel principal it maps to.
--
-- The key is (issuer, subject), NOT (provider, subject). `provider` is a label an operator may
-- retitle; `issuer` is what was cryptographically asserted. Keying on the label would mean that
-- pointing one provider name at a different issuer silently merges two populations into one set of
-- accounts — a configuration typo with a cross-tenant outcome. Keying on the issuer makes the same
-- typo fail closed, into new empty accounts.
--
-- `email` is the provider's claim, kept for display. It is NEVER a lookup key: an IdP that lets a
-- user set an unverified address would otherwise be a one-step takeover of any local account whose
-- address an attacker can guess.
CREATE TABLE IF NOT EXISTS external_identities (
    id           uuid PRIMARY KEY,
    provider     text NOT NULL,
    issuer       text NOT NULL,
    subject      text NOT NULL,
    user_id      uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    email        text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz,
    disabled_at  timestamptz,
    UNIQUE (issuer, subject)
);

CREATE INDEX IF NOT EXISTS external_identities_user_idx ON external_identities (user_id);
