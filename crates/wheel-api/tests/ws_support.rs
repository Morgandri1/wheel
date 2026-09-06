//! Helpers for the websocket bridge tests: config, dev tokens, and a tiny HTTP client for the
//! setup calls that have to happen before a socket can be opened.

#![allow(dead_code)]

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::Value;
use wheel_api::config::{Config, Env};
use wheel_api::crypto::Secret;

pub const ISSUER: &str = "https://dev.wheel.local";
pub const DEV_SECRET: &str = "dev-only-hs256-secret";

pub fn db_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(u) => Some(u),
        Err(_) if std::env::var("WHEEL_CI_HAS_DB").as_deref() == Ok("1") => {
            panic!("WHEEL_CI_HAS_DB=1 but TEST_DATABASE_URL is unset")
        }
        Err(_) => {
            eprintln!("skipping websocket bridge tests: TEST_DATABASE_URL not set");
            None
        }
    }
}

pub fn cfg(db_url: &str) -> Config {
    Config {
        env: Env::Dev,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "http://unused.invalid/jwks".into(),
        clerk_issuer: ISSUER.into(),
        clerk_azp: vec![],
        dev_secret: Some(DEV_SECRET.into()),
        auth_mode: wheel_api::config::AuthMode::Jwks,
        session_secret: wheel_api::crypto::Secret::new("test-session-secret-at-least-32-chars"),
        master_key: [3u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "http://localhost".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 1 << 20,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

/// A dev HS256 token. Each test uses a distinct `sub` so tests stay isolated without truncating
/// shared tables.
pub fn token(prefix: &str) -> String {
    #[derive(serde::Serialize)]
    struct C {
        sub: String,
        iss: &'static str,
        exp: i64,
        nbf: i64,
    }
    let now = chrono::Utc::now().timestamp();
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &C {
            sub: format!("{prefix}_{}", uuid::Uuid::new_v4()),
            iss: ISSUER,
            exp: now + 3600,
            nbf: now - 60,
        },
        &EncodingKey::from_secret(DEV_SECRET.as_bytes()),
    )
    .unwrap()
}

async fn post(api: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let c = reqwest::Client::new();
    let mut r = c.post(format!("{api}{path}")).header("x-auth-token", token);
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await.expect("request");
    let status = resp.status().as_u16();
    let v = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, v)
}

pub async fn create_project(api: &str, token: &str, name: &str) -> String {
    let (status, v) = post(
        api,
        "/v1/projects",
        token,
        Some(serde_json::json!({"name": name})),
    )
    .await;
    assert_eq!(status, 201, "creating a project: {v}");
    v["id"].as_str().expect("project id").to_string()
}

pub async fn mint_ticket(api: &str, token: &str, project_id: &str) -> String {
    let (status, v) = post(
        api,
        &format!("/v1/projects/{project_id}/ws-ticket"),
        token,
        None,
    )
    .await;
    assert_eq!(status, 200, "minting a ticket: {v}");
    v["ticket"].as_str().expect("ticket").to_string()
}
