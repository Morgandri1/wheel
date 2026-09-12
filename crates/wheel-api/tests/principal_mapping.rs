// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! An external subject becomes a **Wheel** principal, and stays the same one.
//!
//! This is the half of `docs/proposals/external-auth.md` §4 that touches the database. The
//! verifier's half is `external_auth.rs`; neither is sufficient alone, because the interesting
//! failures are about identity *continuity* rather than about a single request.
//!
//! The guarantees under test:
//!
//!   * the same `(issuer, subject)` always resolves to the same Wheel uuid;
//!   * a different issuer is a **different** principal, so changing `WHEEL_EXTERNAL_ISSUER` cannot
//!     hand somebody an existing account;
//!   * email is never a link key, so an IdP that lets a user set an address cannot take over a
//!     local account;
//!   * `linked` provisioning refuses an unknown subject, and `disable` refuses a known one.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use wheel_api::auth::external::{self, Verified};
use wheel_api::config::{ExternalAuth, ExternalVerifier, Provision};
use wheel_api::db::Db;

async fn store() -> Db {
    let path = std::env::temp_dir().join(format!("wheel-principal-{}.db", uuid::Uuid::new_v4()));
    Db::connect(&format!("sqlite://{}", path.display()))
        .await
        .expect("connect and migrate")
}

fn cfg(issuer: &str, provision: Provision) -> ExternalAuth {
    ExternalAuth {
        provider: "test".into(),
        issuer: issuer.into(),
        audiences: vec!["wheel-test".into()],
        sole_audience: false,
        subject_claim: "sub".into(),
        azp: vec![],
        max_ttl_secs: None,
        token_header: None,
        provision,
        verifier: ExternalVerifier::ProxyHeader {
            subject_header: "x-forwarded-user".into(),
            email_header: None,
        },
    }
}

fn verified(subject: &str, email: Option<&str>) -> Verified {
    Verified {
        subject: subject.into(),
        email: email.map(str::to_string),
    }
}

/// The same subject is the same principal, request after request. If it were not, every login would
/// produce a new account and a user's projects would vanish from under them.
#[tokio::test]
async fn the_same_subject_always_maps_to_the_same_wheel_principal() {
    let db = store().await;
    let cfg = cfg("https://idp.example", Provision::Auto);

    let first = external::principal_for(&db, &cfg, &verified("alice", None))
        .await
        .expect("provisioned");
    let second = external::principal_for(&db, &cfg, &verified("alice", None))
        .await
        .expect("resolved");
    assert_eq!(first, second);
    // And it is a Wheel uuid, not the foreign subject.
    assert!(
        uuid::Uuid::parse_str(&first).is_ok(),
        "{first} is not a Wheel principal"
    );
    assert_ne!(first, "alice");

    let other = external::principal_for(&db, &cfg, &verified("bob", None))
        .await
        .expect("provisioned");
    assert_ne!(first, other, "two subjects collapsed into one principal");
}

/// A different issuer is a different principal. Deliberately fail-closed: inheriting an account
/// because a configuration URL changed would be account takeover triggered by a config edit.
#[tokio::test]
async fn the_same_subject_under_another_issuer_is_a_different_principal() {
    let db = store().await;
    let a = external::principal_for(
        &db,
        &cfg("https://idp-a.example", Provision::Auto),
        &verified("alice", None),
    )
    .await
    .unwrap();
    let b = external::principal_for(
        &db,
        &cfg("https://idp-b.example", Provision::Auto),
        &verified("alice", None),
    )
    .await
    .unwrap();
    assert_ne!(a, b, "an issuer change handed over an existing account");
}

/// **Never link by email.** An IdP that lets a user set an unverified address would otherwise be a
/// one-step takeover of any local account whose address an attacker can guess.
#[tokio::test]
async fn an_email_claim_never_attaches_to_an_existing_local_account() {
    let db = store().await;
    let local = wheel_api::auth::local::create_user(&db, "victim@example.com", "Correct-Horse-9!")
        .await
        .expect("a local account");

    let principal = external::principal_for(
        &db,
        &cfg("https://idp.example", Provision::Auto),
        &verified("attacker", Some("victim@example.com")),
    )
    .await
    .expect("provisioned");

    assert_ne!(
        principal,
        local.id.to_string(),
        "an email claim took over a local account"
    );

    // The claim is kept for display, on the identity row rather than as the account's address.
    let row = external::lookup(&db, "https://idp.example", "attacker")
        .await
        .unwrap()
        .expect("a linked identity");
    assert_eq!(row.email.as_deref(), Some("victim@example.com"));

    // The account Wheel minted carries a synthetic, undeliverable address instead.
    let user = wheel_api::auth::local::find_user(&db, &row.user_id)
        .await
        .unwrap()
        .expect("the provisioned account");
    assert!(
        user.email.ends_with("@external.invalid"),
        "a provisioned account took a real address: {}",
        user.email
    );
    assert_ne!(user.email, "victim@example.com");
}

/// `linked` is for an IdP that lets anyone sign up: an unknown subject is refused until an operator
/// links it, so "anyone can register with the provider" does not mean "anyone gets a Wheel account".
#[tokio::test]
async fn linked_provisioning_refuses_an_unknown_subject_until_it_is_linked() {
    let db = store().await;
    let cfg = cfg("https://idp.example", Provision::Linked);

    assert!(
        external::principal_for(&db, &cfg, &verified("alice", None))
            .await
            .is_err(),
        "an unknown subject was provisioned under `linked`"
    );

    // An operator links it to an account that already exists...
    let account = wheel_api::auth::local::create_user(&db, "alice@example.com", "Correct-Horse-9!")
        .await
        .unwrap();
    external::link(
        &db,
        &cfg,
        &verified("alice", None),
        account.id,
        Some("operator"),
    )
    .await
    .expect("linked");

    // ...and now the same subject resolves to exactly that account.
    let principal = external::principal_for(&db, &cfg, &verified("alice", None))
        .await
        .expect("resolved after linking");
    assert_eq!(principal, account.id.to_string());
}

/// The only revocation lever Wheel has over a provider with no back-channel logout. Verification
/// still succeeds — the IdP does still vouch for them — and access does not.
#[tokio::test]
async fn a_disabled_identity_is_refused_even_though_the_provider_still_vouches() {
    let db = store().await;
    let cfg = cfg("https://idp.example", Provision::Auto);

    let principal = external::principal_for(&db, &cfg, &verified("alice", None))
        .await
        .unwrap();
    let row = external::lookup(&db, "https://idp.example", "alice")
        .await
        .unwrap()
        .unwrap();

    assert!(external::disable(&db, &row.id).await.unwrap());
    assert!(
        external::principal_for(&db, &cfg, &verified("alice", None))
            .await
            .is_err(),
        "a disabled identity still resolved"
    );

    // Soft, not deleted: the link is still visible to an operator afterwards, and the account it
    // pointed at is untouched — re-enabling must not orphan its projects under a new principal.
    let after = external::lookup(&db, "https://idp.example", "alice")
        .await
        .unwrap()
        .expect("the row survives");
    assert!(after.disabled_at.is_some());
    assert_eq!(after.user_id.to_string(), principal);
}

/// Linking the same subject twice is a conflict, not a second row. Two rows for one subject would
/// mean the principal depended on which one a query happened to return.
#[tokio::test]
async fn a_subject_can_only_be_linked_once() {
    let db = store().await;
    let cfg = cfg("https://idp.example", Provision::Linked);
    let a = wheel_api::auth::local::create_user(&db, "a@example.com", "Correct-Horse-9!")
        .await
        .unwrap();
    let b = wheel_api::auth::local::create_user(&db, "b@example.com", "Correct-Horse-9!")
        .await
        .unwrap();

    external::link(&db, &cfg, &verified("alice", None), a.id, None)
        .await
        .expect("first link");
    let second = external::link(&db, &cfg, &verified("alice", None), b.id, None).await;
    assert!(second.is_err(), "one subject was linked to two accounts");
}

/// `last_seen_at` is what makes a dormant-then-active identity visible to an operator who looks —
/// the only signal Wheel has that might hint at subject reuse (§4.3, the residual it cannot close).
#[tokio::test]
async fn resolving_an_identity_records_that_it_was_seen() {
    let db = store().await;
    let cfg = cfg("https://idp.example", Provision::Auto);
    external::principal_for(&db, &cfg, &verified("alice", None))
        .await
        .unwrap();

    let fresh = external::lookup(&db, "https://idp.example", "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(
        fresh.last_seen_at.is_none(),
        "first provision is not a sighting"
    );

    external::principal_for(&db, &cfg, &verified("alice", None))
        .await
        .unwrap();
    let seen = external::lookup(&db, "https://idp.example", "alice")
        .await
        .unwrap()
        .unwrap();
    assert!(
        seen.last_seen_at.is_some(),
        "a resolved identity was not stamped"
    );
}
