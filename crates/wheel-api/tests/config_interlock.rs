//! The dev-bypass interlock.
//!
//! `AUTH_DEV_SECRET` enables HS256 tokens, which anyone holding the secret can mint — a complete
//! authentication bypass, by design, for local testing. The guarantee under test: if that variable
//! is present while we are not explicitly in dev, the process does not start.
//!
//! One test function, not several: environment variables are process-global, so parallel test
//! threads mutating them would race. Sequencing the cases inside a single test makes the
//! interference impossible rather than unlikely.

use wheel_api::config::{Config, Env};

fn base_env() {
    std::env::set_var("DATABASE_URL", "postgres://u:p@localhost/db");
    std::env::set_var("CLERK_JWKS_URL", "https://clerk.example.test/jwks");
    std::env::set_var("CLERK_ISSUER", "https://clerk.example.test");
    // 32 zero bytes, base64.
    std::env::set_var(
        "API_MASTER_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    std::env::set_var("WHEEL_HOST_URL", "http://host.internal:7100");
    std::env::set_var("WHEEL_HOST_SECRET", "host-secret");
    std::env::remove_var("AUTH_DEV_SECRET");
    std::env::remove_var("WHEEL_ENV");
    // AUTH_MODE is required in prod (see the assertions at the end of this test); the interlock
    // cases below are about AUTH_DEV_SECRET, so give them a valid mode to isolate what they check.
    std::env::set_var("AUTH_MODE", "local");
    std::env::remove_var("SESSION_SECRET");
}

#[test]
fn dev_secret_interlock_and_config_validation() {
    // --- the interlock ------------------------------------------------------------------------
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_DEV_SECRET", "letmein");
    // `Config` deliberately derives no `Debug` (it holds the raw master key), so this matches
    // instead of using `expect_err`.
    let err = match Config::from_env() {
        Ok(_) => panic!(
            "CRITICAL: API booted in prod with AUTH_DEV_SECRET set — HS256 forgery would be accepted"
        ),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("AUTH_DEV_SECRET"),
        "error should name the offending variable, got: {err}"
    );

    // Unset WHEEL_ENV is treated as prod, so the interlock must still fire. This is the case that
    // matters most in practice: nobody sets WHEEL_ENV=prod by hand, they just don't set it.
    base_env();
    std::env::set_var("AUTH_DEV_SECRET", "letmein");
    assert!(
        Config::from_env().is_err(),
        "CRITICAL: unset WHEEL_ENV defaulted to permissive instead of prod"
    );

    // A typo in WHEEL_ENV must not silently become dev *or* prod.
    base_env();
    std::env::set_var("WHEEL_ENV", "development");
    assert!(
        Config::from_env().is_err(),
        "unrecognised WHEEL_ENV should refuse to boot"
    );

    // Dev + secret is the one permitted combination.
    base_env();
    std::env::set_var("WHEEL_ENV", "dev");
    std::env::set_var("AUTH_DEV_SECRET", "letmein");
    let cfg = Config::from_env().expect("dev + AUTH_DEV_SECRET should boot");
    assert_eq!(cfg.env, Env::Dev);
    assert_eq!(cfg.dev_secret.as_deref(), Some("letmein"));

    // Prod with no dev secret: the normal production shape.
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    let cfg = Config::from_env().expect("prod without a dev secret should boot");
    assert_eq!(cfg.env, Env::Prod);
    assert!(cfg.dev_secret.is_none());

    // An empty AUTH_DEV_SECRET is treated as absent rather than as an empty HMAC key.
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_DEV_SECRET", "");
    let cfg = Config::from_env().expect("empty AUTH_DEV_SECRET should count as unset");
    assert!(cfg.dev_secret.is_none());

    // --- other fail-closed validation -----------------------------------------------------------
    base_env();
    std::env::set_var("API_MASTER_KEY", "c2hvcnQ="); // "short" — not 32 bytes
    assert!(
        Config::from_env().is_err(),
        "short master key must be rejected"
    );

    base_env();
    std::env::set_var("API_MASTER_KEY", "!!!not base64!!!");
    assert!(
        Config::from_env().is_err(),
        "non-base64 master key must be rejected"
    );

    base_env();
    std::env::remove_var("WHEEL_HOST_SECRET");
    assert!(
        Config::from_env().is_err(),
        "missing host secret must be rejected"
    );

    // The issuer pins tokens to our tenant, so it is required — but only under the mode that uses
    // it. Local auth issues its own sessions and needs no external provider settings at all.
    base_env();
    std::env::set_var("AUTH_MODE", "jwks");
    std::env::remove_var("CLERK_ISSUER");
    assert!(
        Config::from_env().is_err(),
        "AUTH_MODE=jwks with no issuer must be rejected"
    );

    // The master key must never be printed. `Secret` has no Display and a redacted Debug; assert
    // the host secret in particular cannot be stringified into a log line.
    base_env();
    std::env::set_var("WHEEL_HOST_SECRET", "super-secret-host-value");
    let cfg = Config::from_env().unwrap();
    assert!(
        !format!("{:?}", cfg.host_secret).contains("super-secret-host-value"),
        "host secret leaked through Debug"
    );

    // --- AUTH_MODE ------------------------------------------------------------------------------
    // Unset in production is refused rather than defaulted. Guessing wrong means either rejecting
    // every real user or trusting tokens from an issuer we did not intend, and both are worse
    // than not starting.
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::remove_var("AUTH_MODE");
    assert!(
        Config::from_env().is_err(),
        "AUTH_MODE unset in production must refuse to boot"
    );

    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_MODE", "nonsense");
    assert!(
        Config::from_env().is_err(),
        "an unknown AUTH_MODE must refuse to boot"
    );

    // A placeholder that looks like configuration is worse than a missing one: it boots, and then
    // rejects every token for a reason nobody can see.
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_MODE", "jwks");
    std::env::set_var("CLERK_JWKS_URL", "");
    std::env::set_var("CLERK_ISSUER", "");
    assert!(
        Config::from_env().is_err(),
        "AUTH_MODE=jwks with empty provider settings must refuse to boot"
    );

    // Local auth needs no provider settings at all.
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_MODE", "local");
    std::env::remove_var("CLERK_JWKS_URL");
    std::env::remove_var("CLERK_ISSUER");
    let cfg = Config::from_env().expect("local auth should not require provider settings");
    assert_eq!(cfg.auth_mode, wheel_api::config::AuthMode::Local);

    // Session key: derived from the master key when unset, so there is no extra required secret,
    // and distinct from the master key itself so one use cannot weaken the other.
    assert!(
        cfg.session_secret.expose().len() >= 32,
        "derived session key is too short"
    );
    assert_ne!(
        cfg.session_secret.expose().as_bytes(),
        &cfg.master_key[..],
        "the session key must not be the master key reused"
    );

    // An explicit secret wins, so it can be rotated on its own.
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_MODE", "local");
    std::env::set_var("SESSION_SECRET", "an-explicit-session-secret-32ch+");
    assert_eq!(
        Config::from_env().unwrap().session_secret.expose(),
        "an-explicit-session-secret-32ch+"
    );

    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_MODE", "local");
    std::env::set_var("SESSION_SECRET", "too-short");
    assert!(
        Config::from_env().is_err(),
        "a short SESSION_SECRET must be refused"
    );

    std::env::remove_var("SESSION_SECRET");
    std::env::remove_var("AUTH_MODE");

    // --- ADVERSARY 017: no stub identity provider in production ---------------------------------
    // A mock issuer does not fail closed. It authenticates everyone, as whoever the caller says
    // they are, and the API's own ownership checks then work perfectly against the wrong identity.
    // The only safe place to stop that is boot.
    for stub in [
        "http://localhost:9999/.well-known/jwks.json",
        "https://localhost/jwks",
        "https://127.0.0.1:3000/jwks",
        "https://[::1]/jwks",
        "https://10.0.0.4/jwks",
        "https://192.168.1.10/jwks",
        "https://172.16.0.9/jwks",
        "https://169.254.169.254/jwks",
        "https://auth.internal/jwks",
        "https://clerk.local/jwks",
        "clerk.example.test/jwks",
    ] {
        base_env();
        std::env::set_var("WHEEL_ENV", "prod");
        std::env::set_var("AUTH_MODE", "jwks");
        std::env::set_var("CLERK_JWKS_URL", stub);
        assert!(
            Config::from_env().is_err(),
            "a production build must refuse the stub identity provider {stub:?}"
        );
    }

    // The issuer is checked too: it is what pins a token to our tenant, so a local issuer is the
    // same hole by another name.
    base_env();
    std::env::set_var("WHEEL_ENV", "prod");
    std::env::set_var("AUTH_MODE", "jwks");
    std::env::set_var("CLERK_ISSUER", "http://localhost:9999");
    assert!(
        Config::from_env().is_err(),
        "a production build must refuse a local CLERK_ISSUER"
    );

    // Addresses that merely look private must still be allowed: 172.15 and 172.32 are outside
    // 172.16.0.0/12, and a hostname containing "localhost" is not localhost.
    for real in [
        "https://clerk.example.test/jwks",
        "https://172.15.0.1/jwks",
        "https://172.32.0.1/jwks",
        "https://not-localhost.example.com/jwks",
        "https://internal-tools.example.com/jwks",
    ] {
        base_env();
        std::env::set_var("WHEEL_ENV", "prod");
        std::env::set_var("AUTH_MODE", "jwks");
        std::env::set_var("CLERK_JWKS_URL", real);
        assert!(
            Config::from_env().is_ok(),
            "a real provider must still boot: {real:?}"
        );
    }

    // Dev is exactly where a local issuer belongs, so the check must not apply there.
    base_env();
    std::env::set_var("WHEEL_ENV", "dev");
    std::env::set_var("AUTH_MODE", "jwks");
    std::env::set_var("CLERK_JWKS_URL", "http://localhost:9999/jwks");
    std::env::set_var("CLERK_ISSUER", "http://localhost:9999");
    assert!(
        Config::from_env().is_ok(),
        "dev must still be able to point at a local issuer"
    );

    // Every auth mode that is not one of ours is refused by name, in either environment: "mock",
    // "dev" and "none" are the spellings someone reaches for when they want the bypass.
    for env_name in ["prod", "dev"] {
        for mode in ["mock", "dev", "none", "test", "off", "MOCK", ""] {
            base_env();
            std::env::set_var("WHEEL_ENV", env_name);
            std::env::set_var("AUTH_MODE", mode);
            assert!(
                Config::from_env().is_err(),
                "AUTH_MODE={mode:?} must be refused in {env_name}"
            );
        }
    }

    base_env();
    std::env::remove_var("WHEEL_ENV");
    std::env::remove_var("AUTH_MODE");
}
