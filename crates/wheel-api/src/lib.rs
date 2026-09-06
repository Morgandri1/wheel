pub mod auth;
pub mod boot;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod http;
pub mod models;
pub mod orchestrator;
pub mod routes;
pub mod state;

use axum::routing::{delete, get, patch, post};
use axum::Router;
use state::AppState;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

/// CORS for the browser app.
///
/// The origin allowlist is the boundary, and it is the only one here. Methods and headers are
/// mirrored from the preflight rather than listed, because a hand-kept list is a second copy of
/// what the router serves and it drifted: the vault write is a PUT, PUT was missing, and the
/// operator saw "Can't reach the API" instead of a 405 anyone could have read. Mirroring cannot
/// drift — a method the router does not serve gets a 405 with a body, which is the honest answer,
/// and nothing is granted that a non-browser client did not already have.
///
/// Note what this is *not*: `Any` origin combined with credentials. The web app authenticates with
/// an `x-auth-token` header rather than cookies, so we never need `allow_credentials`, and the
/// explicit origin allowlist keeps a hostile page from scripting the API with a user's token.
pub fn cors_layer(allowed_origins: &[String]) -> CorsLayer {
    let origins: Vec<axum::http::HeaderValue> = allowed_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(AllowMethods::mirror_request())
        .allow_headers(AllowHeaders::mirror_request())
        .max_age(std::time::Duration::from_secs(600))
}

/// CORS for the public ingress route.
///
/// `/p/<project>/<path>` is a public URL by definition — an endpoint node's whole purpose is that
/// anyone can call it — so restricting which page may *read* the reply protects nothing and only
/// stops the board's own "test this endpoint" button from showing what came back. No credentials
/// are allowed with it, so a browser cannot use a visitor's session to reach one.
pub fn public_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::any())
        .allow_methods(AllowMethods::mirror_request())
        .allow_headers(AllowHeaders::mirror_request())
        .max_age(std::time::Duration::from_secs(600))
}

pub fn build_router(state: AppState, allowed_origins: &[String]) -> Router {
    Router::new()
        .route("/healthz", get(routes::health::healthz))
        // Unauthenticated, like /healthz: a deploy gate runs before anyone has a token, and the
        // answer is a single bit that reveals nothing.
        .route("/v1/host/healthz", get(routes::health::host_healthz))
        // Local auth. These 404 when AUTH_MODE is not `local`, so a provider swap cannot leave a
        // second way in.
        .route("/v1/auth/signup", post(routes::auth::signup))
        .route("/v1/auth/login", post(routes::auth::login))
        .route("/v1/auth/logout", post(routes::auth::logout))
        .route("/v1/auth/me", get(routes::auth::me))
        .route("/v1/auth/password", post(routes::auth::change_password))
        .route("/v1/projects", post(routes::projects::create))
        .route("/v1/projects", get(routes::projects::list))
        .route("/v1/projects/{id}", get(routes::projects::get_one))
        .route("/v1/projects/{id}", patch(routes::projects::update))
        .route("/v1/projects/{id}", delete(routes::projects::destroy))
        .route("/v1/projects/{id}/start", post(routes::projects::start))
        .route("/v1/projects/{id}/stop", post(routes::projects::stop))
        .route("/v1/projects/{id}/restart", post(routes::projects::restart))
        .route("/v1/projects/{id}/ws-ticket", post(routes::ws_ticket::mint))
        // Registered before the engine wildcard: this one route also accepts a single-use ticket
        // in the query string, because browsers cannot set headers on a WebSocket handshake.
        .route(
            "/v1/projects/{id}/engine/v1/events",
            axum::routing::any(routes::proxy::engine_events),
        )
        // Authenticated engine proxy. `any` because the engine control plane uses every verb, and
        // the WebSocket upgrade for /engine/v1/events arrives as a GET.
        .route(
            "/v1/projects/{id}/engine/{*rest}",
            axum::routing::any(routes::proxy::engine_proxy),
        )
        .layer(cors_layer(allowed_origins))
        // Public ingress. No auth by design; gated on the project's `http` capability. Merged after
        // the layer above so it carries its own, permissive, CORS instead of the app allowlist.
        .merge(
            Router::new()
                .route(
                    "/p/{project_id}/{*rest}",
                    axum::routing::any(routes::ingress::ingress),
                )
                .layer(public_cors_layer()),
        )
        .with_state(state)
}
