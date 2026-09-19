// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Configurations of external auth that must not boot.
//!
//! Verification is configuration (`docs/proposals/external-auth.md` §3), which means a
//! misconfiguration is a *security* outcome rather than an inconvenience: an unvalidated audience
//! accepts tokens minted for somebody else, and a header-trusting mode with no trusted peer list
//! accepts an identity from anyone who can reach the port. So the interesting assertions here are
//! all of the form "the process refuses to start".
//!
//! One test function, not several: environment variables are process-global, so parallel test
//! threads mutating them would race. Sequencing the cases inside one test makes the interference
//! impossible rather than unlikely — the same reasoning as `config_interlock.rs`.

use wheel_api::config::{Config, ExternalVerifier, Provision};

/// A valid non-external baseline. Every case below starts from this and breaks one thing.
fn base_env() {
    std::env::set_var("DATABASE_URL", "postgres://u:p@localhost/db");
    std::env::set_var("CLERK_JWKS_URL", "https://clerk.example.test/jwks");
    std::env::set_var("CLERK_ISSUER", "https://clerk.example.test");
    std::env::set_var(
        "API_MASTER_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    std::env::set_var("WHEEL_HOST_URL", "http://host.internal:7100");
    std::env::set_var("WHEEL_HOST_SECRET", "host-secret");
    std::env::set_var("PUBLIC_BASE_URL", "https://api.wheel.test");
    std::env::set_var("AUTH_MODE", "local");
    std::env::set_var("WHEEL_ENV", "prod");
    for k in [
        "AUTH_DEV_SECRET",
        "STORE",
        "SESSION_SECRET",
        "WHEEL_TRUSTED_PROXIES",
        "WHEEL_EXTERNAL_VERIFIER",
        "WHEEL_EXTERNAL_PROVIDER",
        "WHEEL_EXTERNAL_ISSUER",
        "WHEEL_EXTERNAL_AUDIENCE",
        "WHEEL_EXTERNAL_ALGS",
        "WHEEL_EXTERNAL_JWKS_URL",
        "WHEEL_EXTERNAL_TOKEN_HEADER",
        "WHEEL_EXTERNAL_SUBJECT_CLAIM",
        "WHEEL_EXTERNAL_AZP",
        "WHEEL_EXTERNAL_MAX_TTL_SECS",
        "WHEEL_EXTERNAL_SOLE_AUDIENCE",
        "WHEEL_EXTERNAL_PROVISION",
        "WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER",
        "WHEEL_EXTERNAL_PROXY_EMAIL_HEADER",
    ] {
        std::env::remove_var(k);
    }
}

/// A complete, valid `jwks` external configuration.
fn external_jwks_env() {
    base_env();
    std::env::set_var("AUTH_MODE", "external");
    std::env::set_var("WHEEL_EXTERNAL_VERIFIER", "jwks");
    std::env::set_var("WHEEL_EXTERNAL_ISSUER", "https://idp.example.test");
    std::env::set_var("WHEEL_EXTERNAL_JWKS_URL", "https://idp.example.test/jwks");
    std::env::set_var("WHEEL_EXTERNAL_AUDIENCE", "wheel-prod");
    std::env::set_var("WHEEL_EXTERNAL_ALGS", "RS256,EdDSA");
    std::env::set_var("WHEEL_EXTERNAL_PROVISION", "auto");
}

fn refuses(why: &str) -> String {
    // `Config` deliberately derives no `Debug` (it holds the raw master key), so this matches
    // rather than using `expect_err`.
    match Config::from_env() {
        Ok(_) => panic!("booted when it should have refused: {why}"),
        Err(e) => e.to_string(),
    }
}

fn boots(why: &str) -> Config {
    match Config::from_env() {
        Ok(c) => c,
        Err(e) => panic!("refused to boot but should not have ({why}): {e}"),
    }
}

#[test]
fn external_auth_refuses_every_configuration_that_would_be_unsafe() {
    // --- the baseline works, so a failure below is about the thing that changed ----------------
    external_jwks_env();
    let cfg = boots("a complete jwks configuration");
    let ext = cfg
        .external
        .as_ref()
        .expect("the block is present under external mode");
    assert_eq!(ext.issuer, "https://idp.example.test");
    assert_eq!(ext.audiences, vec!["wheel-prod".to_string()]);
    assert_eq!(ext.provision, Provision::Auto);
    assert_eq!(ext.subject_claim, "sub", "the default subject claim");
    assert!(!ext.sole_audience, "multi-audience is accepted by default");
    assert!(
        ext.max_ttl_secs.is_none(),
        "no lifetime cap unless asked for"
    );
    match &ext.verifier {
        ExternalVerifier::Jwks { url, algs } => {
            assert_eq!(url, "https://idp.example.test/jwks");
            assert_eq!(algs.len(), 2);
        }
        _ => panic!("wrong verifier"),
    }

    // --- the block exists exactly when the mode does -------------------------------------------
    base_env();
    assert!(
        boots("local mode with no external variables")
            .external
            .is_none(),
        "an external block appeared without external mode"
    );

    // A knob that looks configured and is never read is how a deployer comes to believe they
    // pinned an audience. Refuse rather than ignore.
    base_env();
    std::env::set_var("WHEEL_EXTERNAL_AUDIENCE", "wheel-prod");
    let e = refuses("a WHEEL_EXTERNAL_* variable under AUTH_MODE=local");
    assert!(e.contains("WHEEL_EXTERNAL_AUDIENCE"), "{e}");
    assert!(
        e.contains("external"),
        "the message should name the mode: {e}"
    );

    // --- each required field is required -------------------------------------------------------
    for missing in [
        "WHEEL_EXTERNAL_VERIFIER",
        "WHEEL_EXTERNAL_ISSUER",
        "WHEEL_EXTERNAL_AUDIENCE",
        "WHEEL_EXTERNAL_ALGS",
        "WHEEL_EXTERNAL_JWKS_URL",
        "WHEEL_EXTERNAL_PROVISION",
    ] {
        external_jwks_env();
        std::env::remove_var(missing);
        let e = refuses(missing);
        assert!(
            e.contains(missing),
            "the error should name {missing}, got: {e}"
        );
    }

    // An empty value is a missing value, not an empty audience list.
    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_AUDIENCE", "   ");
    let e = refuses("a blank audience");
    assert!(e.contains("WHEEL_EXTERNAL_AUDIENCE"), "{e}");

    // --- the algorithm allowlist ----------------------------------------------------------------
    // A symmetric algorithm beside a public key set is the confusion attack written into
    // configuration. Refused by name so an operator cannot believe they enabled something.
    for symmetric in ["HS256", "RS256,HS256", "hs512"] {
        external_jwks_env();
        std::env::set_var("WHEEL_EXTERNAL_ALGS", symmetric);
        let e = refuses(symmetric);
        assert!(
            e.contains("symmetric") || e.contains("forge"),
            "the error should explain why, got: {e}"
        );
    }

    // An algorithm no key in `auth::jwks` can ever match would boot and then reject every token
    // for a reason nobody can see.
    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_ALGS", "ES256");
    let e = refuses("an unverifiable algorithm");
    assert!(e.contains("ES256"), "{e}");

    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_ALGS", "eddsa");
    assert!(
        boots("a lowercase algorithm name").external.is_some(),
        "algorithm names are case-insensitive"
    );

    // --- issuer collisions ----------------------------------------------------------------------
    // Two verifiers pinned to one issuer are two token populations that can stand in for each
    // other, which is the confusion the pin exists to prevent.
    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_ISSUER", "https://clerk.example.test");
    std::env::set_var("WHEEL_EXTERNAL_JWKS_URL", "https://clerk.example.test/jwks");
    let e = refuses("an issuer equal to CLERK_ISSUER");
    assert!(e.contains("CLERK_ISSUER"), "{e}");

    // Our own session issuer above all: a local session JWT must never route to the external
    // verifier, nor the reverse.
    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_ISSUER", "https://api.wheel.test");
    std::env::set_var("WHEEL_EXTERNAL_JWKS_URL", "https://api.wheel.test/jwks");
    let e = refuses("an issuer equal to PUBLIC_BASE_URL");
    assert!(e.contains("PUBLIC_BASE_URL"), "{e}");

    // --- the production identity-provider interlock, extended to this plane ---------------------
    // A stub issuer does not fail closed: it authenticates everyone, as anyone (ADVERSARY 017).
    for (url, issuer) in [
        ("http://127.0.0.1:9911/jwks", "https://idp.example.test"),
        ("https://localhost/jwks", "https://idp.example.test"),
        ("https://idp.example.test/jwks", "http://idp.example.test"),
    ] {
        external_jwks_env();
        std::env::set_var("WHEEL_EXTERNAL_JWKS_URL", url);
        std::env::set_var("WHEEL_EXTERNAL_ISSUER", issuer);
        refuses(&format!(
            "a local or plaintext provider at {url} / {issuer}"
        ));
    }

    // In dev, pointing at a local issuer is exactly what dev is for.
    external_jwks_env();
    std::env::set_var("WHEEL_ENV", "dev");
    std::env::set_var("WHEEL_EXTERNAL_JWKS_URL", "http://127.0.0.1:9911/jwks");
    assert!(boots("a local issuer in dev").external.is_some());

    // --- provisioning ---------------------------------------------------------------------------
    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_PROVISION", "linked");
    assert_eq!(
        boots("linked provisioning").external.unwrap().provision,
        Provision::Linked
    );

    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_PROVISION", "sometimes");
    let e = refuses("an unrecognised provisioning policy");
    assert!(e.contains("WHEEL_EXTERNAL_PROVISION"), "{e}");

    // --- the lifetime cap -------------------------------------------------------------------------
    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_MAX_TTL_SECS", "300");
    assert_eq!(boots("a cap").external.unwrap().max_ttl_secs, Some(300));

    for bad in ["0", "-1", "soon"] {
        external_jwks_env();
        std::env::set_var("WHEEL_EXTERNAL_MAX_TTL_SECS", bad);
        let e = refuses(bad);
        assert!(e.contains("WHEEL_EXTERNAL_MAX_TTL_SECS"), "{e}");
    }

    // --- proxy_header: the dangerous mode --------------------------------------------------------
    // Believing a header from everyone is not a configuration, it is an open door.
    base_env();
    std::env::set_var("AUTH_MODE", "external");
    std::env::set_var("WHEEL_EXTERNAL_VERIFIER", "proxy_header");
    std::env::set_var("WHEEL_EXTERNAL_ISSUER", "proxy:oauth2-proxy");
    std::env::set_var("WHEEL_EXTERNAL_AUDIENCE", "wheel-prod");
    std::env::set_var("WHEEL_EXTERNAL_PROVISION", "auto");
    std::env::set_var("WHEEL_EXTERNAL_PROXY_SUBJECT_HEADER", "X-Forwarded-User");
    let e = refuses("proxy_header with no trusted proxies");
    assert!(
        e.contains("WHEEL_TRUSTED_PROXIES"),
        "the error must name what is missing: {e}"
    );

    std::env::set_var("WHEEL_TRUSTED_PROXIES", "127.0.0.1/32");
    let cfg = boots("proxy_header with a trusted proxy");
    match &cfg.external.as_ref().unwrap().verifier {
        ExternalVerifier::ProxyHeader { subject_header, .. } => assert_eq!(
            subject_header, "x-forwarded-user",
            "header names are matched case-insensitively, so they are stored folded"
        ),
        _ => panic!("wrong verifier"),
    }

    // A verifier name nobody recognises must not fall back to either real one.
    base_env();
    std::env::set_var("AUTH_MODE", "external");
    std::env::set_var("WHEEL_EXTERNAL_VERIFIER", "introspection");
    std::env::set_var("WHEEL_EXTERNAL_ISSUER", "https://idp.example.test");
    std::env::set_var("WHEEL_EXTERNAL_AUDIENCE", "wheel-prod");
    std::env::set_var("WHEEL_EXTERNAL_PROVISION", "auto");
    let e = refuses("an unsupported verifier");
    assert!(e.contains("introspection"), "{e}");

    // --- optional knobs read where they are set ---------------------------------------------------
    external_jwks_env();
    std::env::set_var("WHEEL_EXTERNAL_SOLE_AUDIENCE", "1");
    std::env::set_var("WHEEL_EXTERNAL_SUBJECT_CLAIM", "oid");
    std::env::set_var("WHEEL_EXTERNAL_TOKEN_HEADER", "Cf-Access-Jwt-Assertion");
    std::env::set_var("WHEEL_EXTERNAL_AZP", "app-1, app-2");
    std::env::set_var("WHEEL_EXTERNAL_AUDIENCE", "a, b ,c");
    let ext = boots("the optional knobs").external.unwrap();
    assert!(ext.sole_audience);
    assert_eq!(ext.subject_claim, "oid");
    assert_eq!(
        ext.token_header.as_deref(),
        Some("cf-access-jwt-assertion"),
        "a header name is stored folded, because that is how it is matched"
    );
    assert_eq!(ext.azp, vec!["app-1".to_string(), "app-2".to_string()]);
    assert_eq!(
        ext.audiences,
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        "a comma list is trimmed, and a blank entry is not an audience"
    );

    // Leave the environment as we found it for anything sharing this process.
    base_env();
}
