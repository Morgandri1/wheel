#!/usr/bin/env python3
"""Mutation harness for the `AUTH_MODE=external` security checks.

Restore a bug, watch the named test go red, restore the fix. A test that passes against a build
with the control removed is not evidence that the control works, and this is the only way to find
out which of them are. Each mutant is a minimal, plausible regression -- the edit someone actually
makes -- and it must die on a NAMED assertion, not on a compile error and not on an unrelated test.

    python3 qa/tools/mutation_external_auth.py              # all nine
    python3 qa/tools/mutation_external_auth.py allowlist    # one, by id

Deliberately NOT wired into `make check`: it edits tracked source and restores it with
`git checkout --`, which is not a thing a merge gate should do to somebody's working tree, and it
costs a rebuild per mutant. It is a tool for whoever changes `auth/external.rs`, and the results
of the run that accompanied #132 are recorded in `docs/TESTPLAN.md` §7a-ii.

Exit 0 only if every selected mutant DIED. SURVIVED is the finding; COMPILE-FAIL and PATCH-MISS
mean the mutant is stale and proves nothing -- a mutant that does not build is not evidence that
the control holds, it is evidence that this file needs updating.

**Run it against a clean tree, and preferably in a detached worktree.** It restores files with
`git checkout -- <file>`, so uncommitted work in the tree it runs in is at risk, and interrupting
it mid-mutant leaves the edit applied — the verdict line names the file.

It uses its OWN `CARGO_TARGET_DIR` (override with `WHEEL_MUTATION_TARGET_DIR`); see `MUT_ENV`.
"""
import os
import subprocess
import sys
import tempfile
from pathlib import Path

MUTANTS = [
  dict(
    id="alg-from-key",
    why="the algorithm is taken from the key set, not from the token header",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[
      ("""    if header.alg != entry.alg {
        return Err(ApiError::Unauthorized(
            "token algorithm does not match the signing key's",
        ));
    }
""", ""),
      ("    let mut v = Validation::new(entry.alg);", "    let mut v = Validation::new(header.alg);"),
      ("    if !algs.contains(&entry.alg) {", "    if !algs.contains(&header.alg) {"),
      ("    let claims = decode::<serde_json::Value>(token, &entry.key, &v)",
       "    let claims = decode::<serde_json::Value>(token, &entry.key, &v)"),
    ],
    test="external_auth a_header_algorithm_that_disagrees_with_the_key_is_refused",
  ),
  dict(
    id="allowlist",
    why="the operator's algorithm allowlist",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[("""    if !algs.contains(&entry.alg) {
        return Err(ApiError::Unauthorized(
            "signing key's algorithm is not in the configured allowlist",
        ));
    }
""", "    let _ = algs;\n")],
    test="external_auth an_algorithm_outside_the_allowlist_is_refused",
  ),
  dict(
    id="aud-mandatory",
    why="`aud` is mandatory, including a token whose `aud` is absent",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[('v.set_required_spec_claims(&["exp", "iss", "aud"]);',
            'v.set_required_spec_claims(&["exp", "iss"]);')],
    test="external_auth a_token_with_no_audience_is_refused",
  ),
  dict(
    id="issuer-pin",
    why="the issuer pin",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[("    v.set_issuer(&[ext.issuer.as_str()]);", "")],
    test="external_auth another_issuer_is_refused_even_with_a_valid_signature",
  ),
  dict(
    id="proxy-peer",
    why="proxy-header network containment: the TCP peer must be a trusted proxy",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[("""    if !trusted_peer {
        return Err(ApiError::Unauthorized(
            "proxy-header auth from a peer that is not a trusted proxy",
        ));
    }
""", "    let _ = trusted_peer;\n")],
    test="external_identities the_same_assertion_from_an_untrusted_peer_is_refused",
  ),
  dict(
    id="cross-origin",
    why="the cross-origin refusal for an ambient proxy credential",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[("""    let Some(origin) = headers.get(axum::http::header::ORIGIN) else {
        return Ok(());
    };""", """    let Some(origin) = headers.get(axum::http::header::ORIGIN) else {
        return Ok(());
    };
    return Ok(());
    #[allow(unreachable_code)]""")],
    test="external_identities a_cross_origin_page_may_not_spend_an_ambient_proxy_credential",
  ),
  dict(
    id="hop-strip-authenticated",
    why="the configured subject/email headers are stripped at the hop, authenticated path",
    file="crates/wheel-api/src/http/actor.rs",
    edits=[("        super::hop::sanitize_for_upstream(inbound, &[WHEEL_PREFIX], &credential_headers(cfg));",
            "        super::hop::sanitize_for_upstream(inbound, &[WHEEL_PREFIX], &{ let _ = cfg; [] });")],
    test="credential_hop the_proxy_assertion_never_reaches_an_engine_through_the_authenticated_proxy",
  ),
  dict(
    id="hop-strip-ingress",
    why="the configured subject/email headers are stripped at the hop, ingress path",
    file="crates/wheel-api/src/routes/ingress.rs",
    edits=[("        &crate::http::actor::credential_headers(&state.cfg),", "        &[],")],
    test="credential_hop the_proxy_assertion_never_reaches_an_engine_through_public_ingress",
  ),
  dict(
    id="iss-required",
    why="`iss` is mandatory, so an absent issuer cannot skip the pin (ADVERSARY 063-E)",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[('v.set_required_spec_claims(&["exp", "iss", "aud"]);',
            'v.set_required_spec_claims(&["exp", "aud"]);')],
    test="external_auth a_token_with_no_issuer_is_refused",
  ),
  dict(
    id="azp-allowlist",
    why="the azp allowlist decides which client applications may authenticate (ADVERSARY 063-E)",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[("""    if !ext.azp.is_empty() {
        let azp = claims.get("azp").and_then(|v| v.as_str()).unwrap_or("");
        if !ext.azp.iter().any(|a| a == azp) {
            return Err("azp not in allowlist");
        }
    }
""", "")],
    test="external_auth the_azp_allowlist_admits_only_the_named_client_applications",
  ),
  dict(
    id="jwks-max-age-ceiling",
    why="an issuer may not lengthen the key-set trust window without bound (ADVERSARY 063-E)",
    file="crates/wheel-api/src/auth/jwks.rs",
    edits=[("""            .clamp(
                self.timing.min_refresh,
                self.timing.max_age_ceiling.max(self.timing.min_refresh),
            );""", "            .max(self.timing.min_refresh);")],
    test="jwks_expiry an_issuer_may_not_lengthen_the_trust_window_past_the_ceiling",
  ),
  dict(
    id="hs256-confusion",
    why="the HS256-with-public-key forgery is refused by OUR check, not by an invalid claim set (ADVERSARY 067)",
    file="crates/wheel-api/src/auth/external.rs",
    edits=[("""    if header.alg != entry.alg {
        return Err(ApiError::Unauthorized(
            "token algorithm does not match the signing key's",
        ));
    }
""", "")],
    test="external_auth the_hs256_confusion_attack_is_refused",
  ),
  dict(
    id="proxy-trusted-wildcard",
    why="an all-addresses WHEEL_TRUSTED_PROXIES is refused under proxy_header (ADVERSARY 065)",
    file="crates/wheel-api/src/http/client_ip.rs",
    edits=[("        self.0.iter().any(|c| c.bits == 0)", "        false")],
    test="external_config external_auth_refuses_every_configuration_that_would_be_unsafe",
  ),
  dict(
    id="dev-hs256-interlock",
    why="the dev HS256 interlock",
    file="crates/wheel-api/src/config.rs",
    edits=[("""            (Env::Prod, Some(_)) => bail!(
                "AUTH_DEV_SECRET is set but WHEEL_ENV is not \\"dev\\". This would enable HS256 \\
                 token forgery against a production deployment. Refusing to boot."
            ),""", "            (Env::Prod, Some(s)) => Some(s),")],
    test="config_interlock dev_secret_interlock_and_config_validation",
  ),
]

# A PRIVATE target directory, and this is not a performance tweak.
#
# Worktrees here share one `cargo-target`, and artifacts for `wheel-api v0.1.0` built from a
# different tree get reused for this one. Measured by ADVERSARY on #145: that produces phantom
# `unresolved import` errors this harness reports as COMPILE-FAIL, and — far worse — **false
# SURVIVED verdicts**, where a control that is genuinely pinned looks unpinned because the test
# binary that ran was built from somebody else's source. A mutation harness whose verdicts depend
# on what another worktree compiled last is not evidence of anything, in either direction.
MUT_ENV = {**os.environ, "CARGO_TARGET_DIR": os.environ.get(
    "WHEEL_MUTATION_TARGET_DIR", str(Path(tempfile.gettempdir()) / "wheel-mutation-target"))}


def run(cmd, **kw):
    return subprocess.run(
        cmd, shell=True, capture_output=True, text=True, env=MUT_ENV, **kw
    )

def restore(paths):
    run("git checkout -- " + " ".join(sorted(set(paths))))

def main():
    only = sys.argv[1:] or None
    results = []
    for m in MUTANTS:
        if only and m["id"] not in only:
            continue
        path = m["file"]
        src = open(path).read()
        out = src
        for old, new in m["edits"]:
            if old not in out:
                results.append((m["id"], "PATCH-MISS", f"anchor not found: {old[:60]!r}"))
                break
            out = out.replace(old, new, 1)
        else:
            if out == src:
                results.append((m["id"], "NO-OP", "the edit changed nothing"))
                continue
            open(path, "w").write(out)
            suite, test = m["test"].split(" ", 1)
            r = run(f"cargo test -p wheel-api --features sqlite --test {suite} {test} -- --exact --nocapture 2>&1")
            restore([path])
            body = r.stdout + r.stderr
            if "error[" in body or "error: could not compile" in body:
                verdict, detail = "COMPILE-FAIL", "the mutant did not build"
            elif f"test {test} ... FAILED" in body or f"{test} ... FAILED" in body:
                line = next((l.strip() for l in body.splitlines()
                             if "assertion" in l.lower() or "panicked at" in l), "")
                verdict, detail = "DIED", line[:200]
            elif "test result: ok" in body:
                verdict, detail = "SURVIVED", "the test still passed with the control removed"
            else:
                verdict, detail = "UNKNOWN", body.strip().splitlines()[-1][:200] if body.strip() else "no output"
            results.append((m["id"], verdict, detail))
            print(f"[{verdict:12}] {m['id']:26} -- {m['why']}", flush=True)
            if detail:
                print(f"               {detail}", flush=True)
        restore([path])

    print("\n=== summary ===")
    bad = 0
    for mid, verdict, detail in results:
        print(f"{verdict:12} {mid}")
        if verdict != "DIED":
            bad += 1
    sys.exit(1 if bad else 0)

main()
