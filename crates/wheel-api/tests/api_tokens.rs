// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! API tokens through the real router, on every backend this environment has.
//!
//! SQLite always runs, because it is what `wheeld` ships; Postgres joins wherever
//! TEST_DATABASE_URL is set, because it is what the cloud API runs, and the SQL that stamps use
//! and revokes a lineage is written per dialect.

#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sha2::Digest as _;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use wheel_api::auth::api_token;
use wheel_api::config::{AuthMode, Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const ISSUER: &str = "https://api.wheel.test";
const OWNER: &str = "operator@wheeld.invalid";

fn cfg(url: &str, auth_mode: AuthMode) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode,
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: ISSUER.into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
        signup: wheel_api::config::SignupPolicy::Open,
        ws_max_bridges_per_project: 16,
        ws_max_lifetime_secs: 3600,
    }
}

fn router(db: &Db, url: &str, auth_mode: AuthMode) -> Router {
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(url, auth_mode),
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: None,
        membership: wheel_api::membership::MembershipEvents::new(),
        bridges: wheel_api::http::bridges::BridgeCounter::new(),
    });
    wheel_api::build_router(state, &[])
}

async fn sqlite() -> (Db, String) {
    let path = std::env::temp_dir().join(format!("wheel-tokens-{}.db", Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    (Db::connect(&url).await.expect("sqlite store"), url)
}

#[cfg(feature = "postgres")]
async fn postgres() -> Option<(Db, String)> {
    let url = match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => u,
        Err(_) if std::env::var("WHEEL_CI_HAS_DB").as_deref() == Ok("1") => {
            panic!("WHEEL_CI_HAS_DB=1 but TEST_DATABASE_URL is unset")
        }
        Err(_) => return None,
    };
    Some((Db::connect(&url).await.expect("postgres store"), url))
}

#[cfg(not(feature = "postgres"))]
async fn postgres() -> Option<(Db, String)> {
    None
}

async fn stores() -> Vec<(Db, String)> {
    let mut all = vec![sqlite().await];
    all.extend(postgres().await);
    all
}

enum Auth<'a> {
    None,
    Header(&'a str),
    Bearer(&'a str),
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    auth: Auth<'_>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    req = match auth {
        Auth::None => req,
        Auth::Header(t) => req.header("x-auth-token", t),
        Auth::Bearer(t) => req.header("authorization", format!("Bearer {t}")),
    };
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A fresh account: its session token and its user id.
async fn account(app: &Router) -> (String, String) {
    let email = format!("t-{}@example.test", Uuid::new_v4().simple());
    let (status, body) = call(
        app,
        "POST",
        "/v1/auth/signup",
        Auth::None,
        Some(json!({"email": email, "password": "Correct-Horse-9!"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "signup: {body}");
    (
        body["token"].as_str().unwrap().to_string(),
        body["user"]["id"].as_str().unwrap().to_string(),
    )
}

async fn mint(app: &Router, auth: Auth<'_>, name: &str) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/v1/auth/tokens",
        auth,
        Some(json!({ "name": name })),
    )
    .await
}

async fn minted(app: &Router, auth: Auth<'_>, name: &str) -> (String, String) {
    let (status, body) = mint(app, auth, name).await;
    assert_eq!(status, StatusCode::CREATED, "mint: {body}");
    (
        body["token"].as_str().unwrap().to_string(),
        body["id"].as_str().unwrap().to_string(),
    )
}

async fn row(db: &Db, id: &str) -> (String, Option<String>, Option<String>) {
    let id = Uuid::parse_str(id).unwrap();
    let r: (
        String,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = wheel_api::db_fetch_one!(
        db,
        "SELECT token_hash, last_used_at, revoked_at FROM api_tokens WHERE id = $1",
        id
    )
    .unwrap();
    (
        r.0,
        r.1.map(|t| t.to_rfc3339()),
        r.2.map(|t| t.to_rfc3339()),
    )
}

#[tokio::test]
async fn a_minted_token_authenticates_by_either_header_as_its_owner() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, user) = account(&app).await;
        let (token, _) = minted(&app, Auth::Header(&session), "cli").await;
        assert!(token.starts_with("wht_"), "{token}");

        for auth in [Auth::Header(&token), Auth::Bearer(&token)] {
            let (status, me) = call(&app, "GET", "/v1/auth/me", auth, None).await;
            assert_eq!(status, StatusCode::OK, "{me}");
            assert_eq!(me["id"], user);
        }
        let (status, _) = call(&app, "GET", "/v1/projects", Auth::Bearer(&token), None).await;
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn the_value_is_returned_once_and_stored_only_as_its_hash() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, _) = account(&app).await;
        let (token, id) = minted(&app, Auth::Header(&session), "cli").await;

        let (stored, _, _) = row(&db, &id).await;
        assert_eq!(stored, hex::encode(sha2::Sha256::digest(token.as_bytes())));

        let (status, list) =
            call(&app, "GET", "/v1/auth/tokens", Auth::Header(&session), None).await;
        assert_eq!(status, StatusCode::OK);
        let text = list.to_string();
        assert!(!text.contains(&token) && !text.contains(&stored), "{text}");
        assert_eq!(list[0]["id"], id);
        assert_eq!(list[0]["name"], "cli");
        assert!(list[0].get("token").is_none(), "{text}");
    }
}

/// A revoked token must be indistinguishable from one that never existed.
#[tokio::test]
async fn revoked_and_unknown_tokens_get_the_same_401() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, _) = account(&app).await;
        let (token, id) = minted(&app, Auth::Header(&session), "cli").await;

        let (status, _) = call(
            &app,
            "DELETE",
            &format!("/v1/auth/tokens/{id}"),
            Auth::Header(&session),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let revoked = call(&app, "GET", "/v1/projects", Auth::Header(&token), None).await;
        let unknown = call(
            &app,
            "GET",
            "/v1/projects",
            Auth::Header("wht_never-issued"),
            None,
        )
        .await;
        let garbage = call(
            &app,
            "GET",
            "/v1/projects",
            Auth::Header("not-a-token"),
            None,
        )
        .await;
        assert_eq!(revoked.0, StatusCode::UNAUTHORIZED);
        assert_eq!(revoked, unknown);
        assert_eq!(revoked, garbage);
    }
}

#[tokio::test]
async fn use_is_recorded_and_revocation_keeps_its_first_time() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, _) = account(&app).await;
        let (token, id) = minted(&app, Auth::Header(&session), "cli").await;
        assert_eq!(row(&db, &id).await.1, None, "used before anyone used it");

        call(&app, "GET", "/v1/projects", Auth::Header(&token), None).await;
        assert!(row(&db, &id).await.1.is_some(), "a use was not recorded");

        let path = format!("/v1/auth/tokens/{id}");
        call(&app, "DELETE", &path, Auth::Header(&session), None).await;
        let first = row(&db, &id).await.2.expect("revoked");
        let (status, _) = call(&app, "DELETE", &path, Auth::Header(&session), None).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "revoking twice is not an error"
        );
        assert_eq!(
            row(&db, &id).await.2.unwrap(),
            first,
            "a second revoke moved the time"
        );
    }
}

/// A token may mint tokens. Revoking it must take its whole lineage with it, or a leaked token's
/// successors outlive the revocation that was meant to end the leak.
#[tokio::test]
async fn revoking_a_token_revokes_every_token_it_minted() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, _) = account(&app).await;
        let (parent, parent_id) = minted(&app, Auth::Header(&session), "leaked").await;
        let (child, _) = minted(&app, Auth::Header(&parent), "child").await;
        let (grandchild, _) = minted(&app, Auth::Bearer(&child), "grandchild").await;
        let (sibling, _) = minted(&app, Auth::Header(&session), "unrelated").await;

        let (_, list) = call(&app, "GET", "/v1/auth/tokens", Auth::Header(&session), None).await;
        let child_row = list
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "child")
            .unwrap();
        assert_eq!(child_row["minted_by"], parent_id);

        let (status, _) = call(
            &app,
            "DELETE",
            &format!("/v1/auth/tokens/{parent_id}"),
            Auth::Header(&session),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        for t in [&parent, &child, &grandchild] {
            let (status, _) = call(&app, "GET", "/v1/projects", Auth::Header(t), None).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "a descendant survived its parent"
            );
        }
        let (status, _) = call(&app, "GET", "/v1/projects", Auth::Header(&sibling), None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "revocation reached a token outside the lineage"
        );
    }
}

#[tokio::test]
async fn an_account_cannot_see_or_revoke_another_accounts_tokens() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (alice, _) = account(&app).await;
        let (bob, _) = account(&app).await;
        let (token, id) = minted(&app, Auth::Header(&alice), "alice").await;

        let (status, _) = call(
            &app,
            "DELETE",
            &format!("/v1/auth/tokens/{id}"),
            Auth::Header(&bob),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            &app,
            "DELETE",
            "/v1/auth/tokens/not-a-uuid",
            Auth::Header(&bob),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            &app,
            "DELETE",
            &format!("/v1/auth/tokens/{}", Uuid::new_v4()),
            Auth::Header(&alice),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (_, list) = call(&app, "GET", "/v1/auth/tokens", Auth::Header(&bob), None).await;
        assert_eq!(list, json!([]), "bob sees alice's tokens");
        let (status, _) = call(&app, "GET", "/v1/projects", Auth::Header(&token), None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "bob's attempt revoked alice's token"
        );
    }
}

#[tokio::test]
async fn the_token_routes_require_authentication_and_a_sane_name() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, _) = account(&app).await;
        assert_eq!(
            mint(&app, Auth::None, "x").await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", "/v1/auth/tokens", Auth::None, None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            mint(&app, Auth::Header(&session), "  ").await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            mint(&app, Auth::Header(&session), &"n".repeat(65)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn minting_is_rate_limited_per_account() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (alice, _) = account(&app).await;
        let (bob, _) = account(&app).await;
        for i in 0..wheel_api::http::authlimit::MINTS_PER_HOUR {
            assert_eq!(
                mint(&app, Auth::Header(&alice), &format!("t{i}")).await.0,
                StatusCode::CREATED
            );
        }
        assert_eq!(
            mint(&app, Auth::Header(&alice), "one too many").await.0,
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            mint(&app, Auth::Header(&bob), "bob").await.0,
            StatusCode::CREATED,
            "one account's limit spent another's"
        );
    }
}

/// The cloud API signs sessions with an identity provider, and a desktop client still signs in
/// with a token there. The subject is the provider's `sub`, which has no local account.
#[tokio::test]
async fn tokens_work_under_jwks_for_a_subject_with_no_local_account() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Jwks);
        let sub = format!("user_{}", Uuid::new_v4().simple());
        let issued = api_token::issue(&db, &sub, "desktop", api_token::Mint::Operator)
            .await
            .unwrap();

        let (status, projects) = call(
            &app,
            "GET",
            "/v1/projects",
            Auth::Bearer(&issued.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{projects}");
        let (child, child_id) = minted(&app, Auth::Header(&issued.token), "second device").await;
        let (_, list) = call(&app, "GET", "/v1/auth/tokens", Auth::Header(&child), None).await;
        let mine = list.as_array().unwrap();
        assert_eq!(mine.len(), 2, "{list}");
        assert!(mine
            .iter()
            .any(|t| t["id"] == child_id && t["minted_by"] == issued.id.to_string()));

        let everyone = api_token::list_all(&db).await.unwrap();
        let theirs = everyone.iter().find(|t| t.id == issued.id).unwrap();
        assert_eq!(theirs.user_id, sub);
        assert_eq!(theirs.email, None, "a provider subject has no local email");
    }
}

#[tokio::test]
async fn an_empty_store_gets_exactly_one_token_only_owner() {
    let (db, url) = sqlite().await;
    let app = router(&db, &url, AuthMode::Local);
    let issued = api_token::bootstrap_owner(&db, OWNER, "operator")
        .await
        .unwrap()
        .expect("an empty store gets an owner");
    assert!(api_token::bootstrap_owner(&db, OWNER, "operator")
        .await
        .unwrap()
        .is_none());

    let (status, me) = call(
        &app,
        "GET",
        "/v1/auth/me",
        Auth::Bearer(&issued.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["email"], OWNER);

    let login = json!({"email": OWNER, "password": "any-password-at-all"});
    assert_eq!(
        call(&app, "POST", "/v1/auth/login", Auth::None, Some(login))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let signup = json!({"email": OWNER, "password": "Correct-Horse-9!"});
    assert_eq!(
        call(&app, "POST", "/v1/auth/signup", Auth::None, Some(signup))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let change = json!({"current_password": "", "new_password": "Correct-Horse-9!"});
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/auth/password",
            Auth::Header(&issued.token),
            Some(change)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );

    let listed = api_token::list_all(&db).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].email.as_deref(), Some(OWNER));
}

/// The owner is found by its sentinel hash as well as its address, so an account a signup made —
/// which always has a real hash — can never be mistaken for it.
#[tokio::test]
async fn a_signup_can_never_stand_in_for_the_token_only_owner() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let email = format!("owner-{}@wheeld.invalid", Uuid::new_v4().simple());
        let signup = json!({"email": email, "password": "Correct-Horse-9!"});
        assert_eq!(
            call(&app, "POST", "/v1/auth/signup", Auth::None, Some(signup))
                .await
                .0,
            StatusCode::CREATED
        );

        assert!(wheel_api::auth::local::find_token_only_user(&db, &email)
            .await
            .unwrap()
            .is_none());
        assert!(wheel_api::auth::local::find_user_by_email(&db, &email)
            .await
            .unwrap()
            .is_some());

        let other = format!("owner-{}@wheeld.invalid", Uuid::new_v4().simple());
        let made = wheel_api::auth::local::create_token_only_user(&db, &other)
            .await
            .unwrap();
        let found = wheel_api::auth::local::find_token_only_user(&db, &other)
            .await
            .unwrap();
        assert_eq!(found.map(|u| u.id), Some(made.id));
        assert!(
            wheel_api::auth::local::find_token_only_user(&db, "not an address")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            wheel_api::auth::local::find_user_by_email(&db, "not an address")
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn a_store_that_already_has_accounts_gets_no_owner() {
    let (db, url) = sqlite().await;
    let app = router(&db, &url, AuthMode::Local);
    account(&app).await;
    assert!(api_token::bootstrap_owner(&db, OWNER, "operator")
        .await
        .unwrap()
        .is_none());
    assert!(wheel_api::auth::local::find_token_only_user(&db, OWNER)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn revoking_without_an_owner_is_the_operators_store_wide_revoke() {
    let (db, _) = sqlite().await;
    let issued = api_token::issue(&db, "someone", "cli", api_token::Mint::Operator)
        .await
        .unwrap();
    assert!(api_token::revoke(&db, &issued.id, None).await.unwrap());
    assert!(!api_token::revoke(&db, &Uuid::new_v4(), None).await.unwrap());
    assert!(api_token::verify(&db, &issued.token).await.is_err());
}

/// The mint race (review round 1): a mint that passed its credential check just before its parent's
/// revocation landed inserts a child after the family was revoked. Revoking the family cannot see
/// that child; checking the chain at use time does.
#[tokio::test]
async fn a_token_whose_ancestor_is_revoked_never_authenticates() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, user) = account(&app).await;
        let (parent, parent_id) = minted(&app, Auth::Header(&session), "parent").await;
        let (child, _) = minted(&app, Auth::Header(&parent), "child").await;
        let (status, _) = call(&app, "GET", "/v1/projects", Auth::Header(&child), None).await;
        assert_eq!(status, StatusCode::OK, "a live family refused its child");

        let (status, _) = call(
            &app,
            "DELETE",
            &format!("/v1/auth/tokens/{parent_id}"),
            Auth::Header(&session),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let parent_id = Uuid::parse_str(&parent_id).unwrap();
        let late = api_token::issue(&db, &user, "late", api_token::Mint::Token(parent_id))
            .await
            .unwrap();
        let later = api_token::issue(&db, &user, "later", api_token::Mint::Token(late.id))
            .await
            .unwrap();
        for token in [&late.token, &later.token, &child] {
            let (status, _) = call(&app, "GET", "/v1/projects", Auth::Header(token), None).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "a token under a revoked parent authenticated"
            );
        }
    }
}

/// A password is changed because someone else may know it, and whoever knew it could have logged
/// in and minted a token that outlives the password (review round 1). Tokens the operator minted
/// from the data directory are not a session's, and survive.
#[tokio::test]
async fn a_password_change_revokes_what_the_accounts_sessions_minted() {
    for (db, url) in stores().await {
        let app = router(&db, &url, AuthMode::Local);
        let (session, user) = account(&app).await;
        let (from_session, from_session_id) = minted(&app, Auth::Header(&session), "laptop").await;
        let (from_token, _) = minted(&app, Auth::Header(&from_session), "ci").await;
        let operator = api_token::issue(&db, &user, "operator", api_token::Mint::Operator)
            .await
            .unwrap();

        let change =
            json!({"current_password": "Correct-Horse-9!", "new_password": "Another-Horse-10!"});
        let (status, _) = call(
            &app,
            "POST",
            "/v1/auth/password",
            Auth::Header(&session),
            Some(change),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        for token in [&session, &from_session, &from_token] {
            let (status, _) = call(&app, "GET", "/v1/projects", Auth::Header(token), None).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "a credential outlived the password change"
            );
        }
        let (status, _) = call(
            &app,
            "GET",
            "/v1/projects",
            Auth::Header(&operator.token),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the operator's token died with a web password"
        );
        assert!(
            row(&db, &from_session_id).await.2.is_some(),
            "the list does not show the revocation"
        );
    }
}
