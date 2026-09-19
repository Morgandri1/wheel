-- Copyright Morgan Metz
-- Licensed under the PolyForm Noncommercial License 1.0.0.
-- See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

-- External identities (docs/proposals/external-auth.md §4.1).
--
-- Numbered 0007, not 0006: 0006_project_members.sql is M1's, and 0005 is api_token_sessions from
-- the headless-first work. Verified by listing this directory rather than by reading a plan.

-- An external subject, and the Wheel principal it maps to.
--
-- The key is (issuer, subject), NOT (provider, subject). `provider` is a label an operator may
-- retitle; `issuer` is what was cryptographically asserted. Keying on the label would mean that
-- pointing one provider name at a different issuer silently merges two populations into one set of
-- accounts — a configuration typo with a cross-tenant outcome. Keying on the issuer makes the same
-- typo fail closed, into new empty accounts.
--
-- `user_id` DOES have a foreign key to users, unlike project_members.user_id: an external principal
-- is always a uuid this deployment minted (auth::external::principal_for), never a provider's `sub`
-- carried as text, so the reason project_members has no key does not apply here.
--
-- `email` is the provider's claim, kept for display. It is NEVER a lookup key: an IdP that lets a
-- user set an unverified address would otherwise be a one-step takeover of any local account whose
-- address an attacker can guess.
--
-- disabled_at is soft rather than a DELETE. It is the only revocation lever Wheel has over a
-- provider with no back-channel logout, and an operator has to be able to see afterwards that they
-- pulled it — a vanished row cannot be re-enabled as a decision.
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
