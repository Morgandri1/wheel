// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! The external verifier, against the threat model in `docs/proposals/external-auth.md` §6.
//!
//! Every row of that table is a test here. The ones that matter most are the two that would pass
//! silently if they were wrong:
//!
//!   * **A missing `aud` claim.** Measured from `jsonwebtoken-9.3.1/src/validation.rs:306-329`, a
//!     token with no `aud` falls through the audience match and is *accepted*, even with
//!     `validate_aud = true` and an audience configured. Only `required_spec_claims` makes it
//!     mandatory. A verifier that looked correct would have this hole.
//!   * **Algorithm confusion.** The algorithm comes from the key the `kid` resolves to, not from
//!     the token's header, so an attacker chooses only which key is tried.

mod support;

use jsonwebtoken::Algorithm;
use serde_json::json;
use support::*;
use wheel_api::auth::external::{self, Verified};
use wheel_api::auth::jwks::JwksCache;
use wheel_api::config::{ExternalAuth, ExternalVerifier, Provision};

fn ext(url: &str) -> ExternalAuth {
    ExternalAuth {
        provider: "test".into(),
        issuer: EXTERNAL_ISSUER.into(),
        audiences: vec![EXTERNAL_AUDIENCE.into()],
        sole_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: None,
        provision: Provision::Auto,
        verifier: ExternalVerifier::Jwks {
            url: url.to_string(),
            algs: vec![Algorithm::RS256, Algorithm::EdDSA],
        },
    }
}

/// A well-formed claim set for the external plane.
fn claims_for(sub: &str) -> serde_json::Value {
    json!({
        "sub": sub,
        "iss": EXTERNAL_ISSUER,
        "aud": EXTERNAL_AUDIENCE,
        "exp": now() + 300,
        "nbf": now() - 60,
        "iat": now(),
    })
}

async fn plane() -> (TestKey, JwksServer, JwksCache) {
    let key = make_key();
    let server = serve_jwks(external_jwks(&key)).await;
    let cache = JwksCache::new(server.url.clone(), reqwest::Client::new());
    (key, server, cache)
}

async fn verify(
    token: &str,
    cfg: &ExternalAuth,
    cache: &JwksCache,
) -> Result<Verified, wheel_api::error::ApiError> {
    external::verify_token(token, cfg, cache).await
}

// --------------------------------------------------------------------------- the happy paths

/// Both algorithms verify, and the subject that comes back is the one that was signed. EdDSA is the
/// whole reason this plane exists as a separate verifier — the old one accepted RS256 only.
#[tokio::test]
async fn both_configured_algorithms_verify() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);

    let rs = sign_rs256_value(&key, KID, &claims_for("alice"));
    assert_eq!(verify(&rs, &cfg, &cache).await.unwrap().subject, "alice");

    let ed = sign_eddsa(ED_KID, &claims_for("bob"));
    assert_eq!(verify(&ed, &cfg, &cache).await.unwrap().subject, "bob");
}

// --------------------------------------------------------------------------- #1, #2 algorithms

/// The key set decides what a key is for. A token whose header claims one algorithm while its `kid`
/// resolves to a key of the other is refused before any signature is checked — so an attacker
/// cannot pick the verifier by writing a header.
#[tokio::test]
async fn a_header_algorithm_that_disagrees_with_the_key_is_refused() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);

    // RS256 header, Ed25519 kid.
    let confused = sign_rs256_value(&key, ED_KID, &claims_for("alice"));
    assert!(
        verify(&confused, &cfg, &cache).await.is_err(),
        "an RS256 token naming the Ed25519 key was accepted"
    );

    // EdDSA header, RSA kid.
    let confused = sign_eddsa(KID, &claims_for("alice"));
    assert!(
        verify(&confused, &cfg, &cache).await.is_err(),
        "an EdDSA token naming the RSA key was accepted"
    );
}

/// The operator's allowlist is enforced even when the key set would otherwise support the key.
#[tokio::test]
async fn an_algorithm_outside_the_allowlist_is_refused() {
    let (key, server, cache) = plane().await;
    let mut cfg = ext(&server.url);
    cfg.verifier = ExternalVerifier::Jwks {
        url: server.url.clone(),
        algs: vec![Algorithm::RS256],
    };

    // RS256 still works...
    let rs = sign_rs256_value(&key, KID, &claims_for("alice"));
    assert!(verify(&rs, &cfg, &cache).await.is_ok());

    // ...and EdDSA is refused, though the key set holds a perfectly good Ed25519 key.
    let ed = sign_eddsa(ED_KID, &claims_for("alice"));
    assert!(
        verify(&ed, &cfg, &cache).await.is_err(),
        "an algorithm the operator did not allow was accepted"
    );
}

/// The classic confusion attack: re-sign with HMAC, using the provider's published key material as
/// the secret. It cannot work, because no symmetric key is ever imported and the header's
/// algorithm never selects the verifier.
#[tokio::test]
async fn the_hs256_confusion_attack_is_refused() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    let forged = sign_hs256(
        KID,
        key.public_der_b64.as_bytes(),
        &claims("alice"),
    );
    assert!(verify(&forged, &cfg, &cache).await.is_err());
}

/// `alg: none`. There is no `Algorithm` variant for it, so the header does not even parse —
/// incidental to a dependency, which is exactly why it is pinned here rather than assumed.
#[tokio::test]
async fn alg_none_is_refused() {
    let (_key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    let forged = forge_alg_none(&claims("alice"));
    assert!(verify(&forged, &cfg, &cache).await.is_err());
}

/// A token with no `kid` cannot resolve a key, and therefore cannot resolve an algorithm.
#[tokio::test]
async fn a_token_with_no_kid_is_refused() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    let no_kid = jsonwebtoken::encode(
        &jsonwebtoken::Header::new(Algorithm::RS256),
        &claims_for("alice"),
        &jsonwebtoken::EncodingKey::from_rsa_pem(key.private_pem.as_bytes()).unwrap(),
    )
    .unwrap();
    assert!(verify(&no_kid, &cfg, &cache).await.is_err());
}

// --------------------------------------------------------------------------- #3, #4, #5 audience

/// **The fail-open one.** A token with no `aud` claim at all must be refused; the library would
/// accept it.
#[tokio::test]
async fn a_token_with_no_audience_is_refused() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    let mut c = claims_for("alice");
    c.as_object_mut().unwrap().remove("aud");
    let token = sign_rs256_value(&key, KID, &c);
    assert!(
        verify(&token, &cfg, &cache).await.is_err(),
        "a token with no audience was accepted: the aud check is fail-open without \
         required_spec_claims"
    );
}

#[tokio::test]
async fn an_audience_for_someone_else_is_refused() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    for wrong in ["relay", "agd:host-1", "wheel-other-deployment"] {
        let mut c = claims_for("alice");
        c["aud"] = json!(wrong);
        let token = sign_rs256_value(&key, KID, &c);
        assert!(
            verify(&token, &cfg, &cache).await.is_err(),
            "a token minted for {wrong} was accepted here"
        );
    }
}

/// Exact equality, never a prefix. A prefix rule would accept every audience under a namespace,
/// which is the cross-tenant confusion the audience exists to prevent.
#[tokio::test]
async fn the_audience_is_compared_by_equality_not_by_prefix() {
    let (key, server, cache) = plane().await;
    let mut cfg = ext(&server.url);
    cfg.audiences = vec!["wheel:".into()];

    let mut c = claims_for("alice");
    c["aud"] = json!("wheel:evil");
    let token = sign_rs256_value(&key, KID, &c);
    assert!(
        verify(&token, &cfg, &cache).await.is_err(),
        "an audience was matched by prefix"
    );

    c["aud"] = json!("wheel:");
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_ok(), "exact match must still work");
}

/// A multi-audience token is accepted when ours is among them — RFC 7519's rule, and what real
/// providers emit — and refused when it is not.
#[tokio::test]
async fn a_multi_audience_token_must_still_name_us() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);

    let mut c = claims_for("alice");
    c["aud"] = json!(["someone-else", EXTERNAL_AUDIENCE]);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_ok());

    c["aud"] = json!(["someone-else", "a-third-party"]);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err());
}

/// `WHEEL_EXTERNAL_SOLE_AUDIENCE` is the lever for a deployer who does not extend trust to the
/// other relying parties their IdP names in the same token.
#[tokio::test]
async fn sole_audience_refuses_a_token_shared_with_another_party() {
    let (key, server, cache) = plane().await;
    let mut cfg = ext(&server.url);
    cfg.sole_audience = true;

    let mut c = claims_for("alice");
    c["aud"] = json!(["someone-else", EXTERNAL_AUDIENCE]);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err());

    c["aud"] = json!(EXTERNAL_AUDIENCE);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_ok());
}

// --------------------------------------------------------------------------- issuer and time

#[tokio::test]
async fn another_issuer_is_refused_even_with_a_valid_signature() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    let mut c = claims_for("alice");
    // Signed by the same key — the pin is what refuses it, not the cryptography.
    c["iss"] = json!(ISSUER);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err());
}

#[tokio::test]
async fn a_token_with_no_exp_or_an_expired_one_is_refused() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);

    let mut c = claims_for("alice");
    c.as_object_mut().unwrap().remove("exp");
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err(), "a token with no exp never expires");

    let mut c = claims_for("alice");
    c["exp"] = json!(now() - 3600);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err());

    let mut c = claims_for("alice");
    c["nbf"] = json!(now() + 3600);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err());
}

/// A lifetime cap that cannot be computed must refuse rather than pass: without `iat` there is no
/// lifetime to measure, and treating that as "within the cap" would make the cap decorative.
#[tokio::test]
async fn the_lifetime_cap_is_enforced_and_needs_iat() {
    let (key, server, cache) = plane().await;
    let mut cfg = ext(&server.url);
    cfg.max_ttl_secs = Some(300);

    let mut c = claims_for("alice");
    c.as_object_mut().unwrap().remove("iat");
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err(), "no iat, so no cap");

    let mut c = claims_for("alice");
    let issued = now();
    c["iat"] = json!(issued);
    c["exp"] = json!(issued + 301);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_err(), "a token over the cap was accepted");

    c["exp"] = json!(issued + 300);
    let token = sign_rs256_value(&key, KID, &c);
    assert!(verify(&token, &cfg, &cache).await.is_ok(), "a token at the cap must pass");
}

// --------------------------------------------------------------------------- #10 the subject

/// The subject becomes an `x-wheel-actor-id` header and an `<AgentPrompt on_behalf_of="...">`
/// attribute. A value that could split a header or close that attribute is refused at this
/// boundary, once, rather than sanitised at every use.
#[tokio::test]
async fn a_subject_that_is_not_a_principal_is_refused() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    for hostile in [
        "alice\nx-wheel-actor-tier: admin",
        "alice\" type=\"user",
        "alice<AgentPrompt",
        "alice bob",
        "",
    ] {
        let mut c = claims_for("alice");
        c["sub"] = json!(hostile);
        let token = sign_rs256_value(&key, KID, &c);
        assert!(
            verify(&token, &cfg, &cache).await.is_err(),
            "{hostile:?} was accepted as a principal"
        );
    }
}

/// A deployer whose IdP has a better immutable identifier than `sub` can point at it — the
/// mitigation for subject reuse (§4.3).
#[tokio::test]
async fn a_configured_subject_claim_is_used_instead_of_sub() {
    let (key, server, cache) = plane().await;
    let mut cfg = ext(&server.url);
    cfg.subject_claim = "oid".into();

    let mut c = claims_for("mutable-handle");
    c["oid"] = json!("stable-1");
    let token = sign_rs256_value(&key, KID, &c);
    assert_eq!(verify(&token, &cfg, &cache).await.unwrap().subject, "stable-1");

    // And a token missing that claim is refused rather than silently falling back to `sub`.
    let token = sign_rs256_value(&key, KID, &claims_for("mutable-handle"));
    assert!(verify(&token, &cfg, &cache).await.is_err());
}

/// The email claim is carried for display. The principal mapping never uses it — see
/// `principal_mapping.rs` for the half of that guarantee that touches the database.
#[tokio::test]
async fn the_email_claim_is_carried_but_is_not_the_identity() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    let mut c = claims_for("alice");
    c["email"] = json!("alice@example.com");
    let token = sign_rs256_value(&key, KID, &c);
    let v = verify(&token, &cfg, &cache).await.unwrap();
    assert_eq!(v.subject, "alice");
    assert_eq!(v.email.as_deref(), Some("alice@example.com"));
}

// --------------------------------------------------------------------------- key handling

/// An unknown `kid` must not become a traffic pump aimed at the deployer's IdP. The throttle is the
/// existing one; this asserts it still applies on the external plane.
#[tokio::test]
async fn an_unknown_kid_does_not_refetch_every_time() {
    let (key, server, cache) = plane().await;
    let cfg = ext(&server.url);
    for i in 0..5 {
        let token = sign_rs256_value(&key, &format!("nope-{i}"), &claims_for("alice"));
        assert!(verify(&token, &cfg, &cache).await.is_err());
    }
    assert!(
        server.hits.load(std::sync::atomic::Ordering::SeqCst) <= 1,
        "each unknown kid caused a fetch: the auth path is a DoS amplifier"
    );
}

/// A key set containing a symmetric key must not import it — that would hand an attacker an HMAC
/// key the verifier trusts, delivered by the provider's own document.
#[tokio::test]
async fn a_symmetric_key_in_the_key_set_is_never_used() {
    let key = make_key();
    let mut doc = external_jwks(&key);
    doc["keys"].as_array_mut().unwrap().push(json!({
        "kty": "oct", "kid": "sneaky", "k": "c2VjcmV0LWtleS1tYXRlcmlhbA"
    }));
    let server = serve_jwks(doc).await;
    let cache = JwksCache::new(server.url.clone(), reqwest::Client::new());
    let cfg = ext(&server.url);

    let forged = sign_hs256("sneaky", b"secret-key-material", &claims("alice"));
    assert!(verify(&forged, &cfg, &cache).await.is_err());

    // And the usable keys beside it still work: one bad key must not take the deployment down.
    let good = sign_rs256_value(&key, KID, &claims_for("alice"));
    assert!(verify(&good, &cfg, &cache).await.is_ok());
}

// --------------------------------------------------------------------------- the other verifier

/// The two verifiers do not stand in for each other. A deployment configured for one refuses the
/// other's credential rather than falling through to a weaker check.
#[tokio::test]
async fn a_jwks_deployment_refuses_a_proxy_assertion() {
    let (_key, server, cache) = plane().await;
    let mut cfg = ext(&server.url);
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-user", "alice".parse().unwrap());
    assert!(external::verify_proxy(&headers, true, &cfg).is_err());

    // And the reverse.
    cfg.verifier = ExternalVerifier::ProxyHeader {
        subject_header: "x-forwarded-user".into(),
        email_header: None,
    };
    let token = sign_rs256_value(&make_key(), KID, &claims_for("alice"));
    assert!(verify(&token, &cfg, &cache).await.is_err());
}
