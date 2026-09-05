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

    base_env();
    std::env::remove_var("CLERK_ISSUER");
    assert!(
        Config::from_env().is_err(),
        "missing issuer must be rejected"
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
}
