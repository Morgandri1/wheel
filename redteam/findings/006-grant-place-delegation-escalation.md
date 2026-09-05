# 006 — Capability delegation (grant/place/manage) escalation surface

- **Severity:** High (design-level; pre-code — feature announced, not yet in CTX contract body)
- **Owner:** SDK/Engine (enforcement) + API (operator-mode auth, export/import, mail relay)
- **Status:** OPEN — design review. No PoC yet (feature unbuilt; scoped M3 by PM).
- **Boundary:** TB9 (new).

## Claim
The announced `place` / `grant` / `manage` model, board-as-code `wheel.toml` import, operator mode,
and `runtime:"local"` agents introduce capability DELEGATION. Delegation is a classic privilege-
escalation vector: the security of the whole wire matrix now depends on the delegation rules being
enforced server-side and correctly attenuated. PM's stated rules — *a grant is never stronger than the
grantor's wire; update/remove are owner-only* — are exactly right; this finding pins the attacks they
must survive and asks that `place`/`grant` be owner/attenuation-checked with the same rigor.

## Attacks to cover (each → a test)
1. **Attenuation break:** grantor with `read` on vault/table grants `write`/`send`; grants a wire it
   does not hold at all; grant a matrix-illegal wire type (e.g. `send` to a table).
2. **Grant laundering / chaining:** A grants B, B grants C a capability neither legitimately holds;
   confirm the attenuation check is transitive (each grant ≤ grantor's *effective* wire, not just its
   originally-created wire).
3. **Who may grant/place/manage:** a prompt-injected agent (bypassPermissions, see 002) must NOT be
   able to `place` an exfil path (endpoint→script→vault) or wire itself into another agent's inbox.
   Enforce authorization server-side; owner-only for update/remove, and gate place/grant equivalently.
4. **`wheel.toml` import = untrusted data:** must pass identical validation to API node/wire creation
   (wire matrix, name regex, tool base_url SSRF deny-list, vault-ref existence, importer-privilege
   check). A template declaring wires the importer can't create → reject, don't trust. Bound toml
   size/depth (DoS parity with the §3d spec parser).
5. **Operator mode / local runtime:** laptop-issued commands and local-agent tokens must not exceed
   the same owner/wire checks; local runtime must not bypass engine-side gating.
6. **Budget stop:** engine/host-enforced, not agent-enforced; an agent can't evade its own stop or
   trip another project's.

## Proposed action
To PM/SDK/API: (a) single server-side `authorize_grant(grantor, from, to, type)` that checks
`type ∈ matrix(from,to)` AND grantor holds ≥ `type` on that edge; (b) owner-only for place/grant/
manage update+remove; (c) route `wheel.toml` import through the existing create-validation path;
(d) budget stop enforced above the agent. Add attenuation + import-validation tests (dovetails QA's
9×9×3 matrix). PoC when the feature lands (M3).
