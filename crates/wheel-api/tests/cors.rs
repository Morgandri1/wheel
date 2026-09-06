//! API-cors-covers-every-served-method.
//!
//! The vault write is a `PUT`, `PUT` was missing from a hand-kept allow list, and the operator saw
//! "Can't reach the API" — a preflight failure gives the browser nothing to render, so the symptom
//! names neither the method nor the route. A list maintained beside the router is a second copy of
//! what the router serves, and second copies drift.
//!
//! So this suite reads the routes out of `src/lib.rs` and holds the CORS layer to them: every
//! method any route accepts, including everything reachable through the engine proxy and the public
//! ingress, must survive a real preflight against the real router. A method the router serves and
//! the preflight refuses fails here rather than in front of the operator.

// Exercises the SQLite backend, so it exists only in a build that has one.
#![cfg(feature = "sqlite")]

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use std::sync::Arc;
use tower::ServiceExt;
use wheel_api::config::{AuthMode, Config, Env};
use wheel_api::crypto::Secret;
use wheel_api::db::Db;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

const ORIGIN: &str = "https://wheel-2708.vercel.app";
const OTHER_ORIGIN: &str = "https://not-ours.example";

/// Every method a route may be declared with. `any(..)` serves all of them.
const ALL: [Method; 7] = [
    Method::GET,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
    Method::HEAD,
    Method::OPTIONS,
];

fn cfg(db_url: &str) -> Config {
    Config {
        env: Env::Prod,
        bind_addr: "127.0.0.1:0".into(),
        database_url: db_url.into(),
        clerk_jwks_url: "https://clerk.test/jwks".into(),
        clerk_issuer: "https://clerk.test".into(),
        clerk_azp: vec![],
        dev_secret: None,
        auth_mode: AuthMode::Local,
        session_secret: Secret::new("session-secret-that-is-at-least-32-chars"),
        master_key: [7u8; 32],
        host_url: "http://host.invalid".into(),
        host_secret: Secret::new("host-secret"),
        engine_port: 7000,
        public_base_url: "https://api.wheel.test".into(),
        max_projects_per_user: 20,
        ingress_rate_per_min: 60,
        ingress_body_limit_bytes: 5 * 1024 * 1024,
        proxy_timeout_secs: 30,
        host_connect_timeout_secs: 3,
    }
}

async fn app() -> Router {
    let path = std::env::temp_dir().join(format!("wheel-cors-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}", path.display());
    let db = Db::connect(&url).await.expect("connect and migrate");
    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "https://clerk.test/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url),
        db,
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: None,
    });
    wheel_api::build_router(state, &[ORIGIN.to_string()])
}

/// The preflight a browser sends before `method path` from `origin`.
async fn preflight(
    app: &Router,
    origin: &str,
    method: &Method,
    path: &str,
    headers: &str,
) -> (StatusCode, Option<String>, Option<String>) {
    let mut req = Request::builder()
        .method(Method::OPTIONS)
        .uri(path)
        .header(header::ORIGIN, origin)
        .header("access-control-request-method", method.as_str());
    if !headers.is_empty() {
        req = req.header("access-control-request-headers", headers);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let get = |n: &str| {
        res.headers()
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    (
        res.status(),
        get("access-control-allow-methods"),
        get("access-control-allow-origin"),
    )
}

fn allows(allowed: &Option<String>, method: &Method) -> bool {
    match allowed {
        None => false,
        Some(v) if v.trim() == "*" => true,
        Some(v) => v
            .split(',')
            .any(|m| m.trim().eq_ignore_ascii_case(method.as_str())),
    }
}

/// The routes the router actually declares, read from its source.
///
/// Parsed rather than listed: a list here would be the same second copy that drifted in the first
/// place. `any(..)` means the route takes whatever arrives, so it contributes every method.
fn declared_routes() -> Vec<(String, Vec<Method>)> {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
        .expect("the router's source");
    let mut routes = Vec::new();
    for chunk in src.split(".route(").skip(1) {
        let mut parts = chunk.splitn(3, '"');
        parts.next();
        let path = parts.next().expect("a route path").to_string();
        let after = parts.next().expect("a routing call");
        let open = after.find('(').expect("a routing call");
        let verb = after[..open]
            .trim()
            .trim_start_matches(',')
            .trim()
            .rsplit("::")
            .next()
            .unwrap()
            .trim()
            .to_string();
        let methods = match verb.as_str() {
            "any" => ALL.to_vec(),
            "get" => vec![Method::GET, Method::HEAD],
            "post" => vec![Method::POST],
            "put" => vec![Method::PUT],
            "patch" => vec![Method::PATCH],
            "delete" => vec![Method::DELETE],
            "head" => vec![Method::HEAD],
            other => panic!(
                "route {path} is declared with `{other}`, which this suite does not know how to \
                 check — teach it the verb rather than deleting the case"
            ),
        };
        routes.push((path, methods));
    }
    routes
}

/// Route templates are not URLs; give the browser something it could actually request.
fn concrete(path: &str) -> String {
    path.replace("{id}", &uuid::Uuid::new_v4().to_string())
        .replace("{project_id}", &uuid::Uuid::new_v4().to_string())
        .replace("{*rest}", "v1/board")
}

#[tokio::test]
async fn every_method_the_router_serves_survives_a_preflight() {
    let app = app().await;
    let routes = declared_routes();
    assert!(
        routes.len() >= 15,
        "only {} routes parsed out of src/lib.rs — the parser has stopped seeing them, which would \
         make this suite pass by finding nothing",
        routes.len()
    );

    for (template, methods) in &routes {
        let path = concrete(template);
        for method in methods {
            let (status, allowed, acao) = preflight(&app, ORIGIN, method, &path, "").await;
            assert!(
                status.is_success(),
                "preflight for {method} {template} was {status}"
            );
            assert_eq!(
                acao.as_deref(),
                if template.starts_with("/p/") {
                    Some("*")
                } else {
                    Some(ORIGIN)
                },
                "unexpected allow-origin for {method} {template}"
            );
            assert!(
                allows(&allowed, method),
                "the router serves {method} {template} but the preflight allows {allowed:?} — a \
                 browser cannot call it and the operator sees no error that says why"
            );
        }
    }
}

#[tokio::test]
async fn the_headers_the_app_sends_are_allowed() {
    let app = app().await;
    // Every header the browser app sets on an API call. x-auth-token is the session, x-project-id
    // scopes it, and a missing one of these fails the same opaque way a missing method does.
    let asked = "x-auth-token, x-project-id, x-request-id, content-type, authorization";
    let res = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/projects")
        .header(header::ORIGIN, ORIGIN)
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", asked)
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(res).await.unwrap();
    let allowed = res
        .headers()
        .get("access-control-allow-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    for header in asked.split(',').map(str::trim) {
        assert!(
            allowed.contains(header),
            "the app sends {header} but the preflight allows only {allowed:?}"
        );
    }
}

#[tokio::test]
async fn an_origin_we_do_not_know_gets_nothing() {
    // Mirroring methods and headers must not become mirroring origins: the allowlist is the whole
    // boundary, so a page we have never heard of gets no allow-origin at all and cannot read a
    // reply even with a user's token in hand.
    let app = app().await;
    let (_, _, acao) = preflight(&app, OTHER_ORIGIN, &Method::PUT, "/v1/projects", "").await;
    assert_eq!(acao, None, "an unlisted origin was allowed to read replies");
}

#[tokio::test]
async fn the_public_ingress_answers_any_origin() {
    // /p/<project>/<path> is public by definition, so the board's "test this endpoint" button --
    // and anyone else's page -- may read what came back. No credentials go with it.
    let app = app().await;
    let path = format!("/p/{}/hook", uuid::Uuid::new_v4());
    let (status, allowed, acao) = preflight(&app, OTHER_ORIGIN, &Method::POST, &path, "").await;
    assert!(status.is_success());
    assert_eq!(acao.as_deref(), Some("*"));
    assert!(allows(&allowed, &Method::POST));
}
