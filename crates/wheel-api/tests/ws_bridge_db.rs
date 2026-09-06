//! The events WebSocket, end to end through the proxy.
//!
//! Two things make this worth a real socket rather than a unit test of the upgrade detector.
//!
//! First, the contract requires frames to cross the bridge **unmodified** — the UI correlates a
//! `message` event to a row by its id, so re-serialising JSON anywhere in the path (which would
//! reorder keys) is a defect even though every field survives.
//!
//! Second, this is the one authenticated route a browser cannot send a header on, so it is the one
//! place a ticket substitutes for the session token. That makes it worth proving that the ticket is
//! genuinely single-use and genuinely bound to its project.

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;
use wheel_api::orchestrator::{NoopOrchestrator, Orchestrator};
use wheel_api::state::{AppState, Inner};

mod ws_support;
use ws_support::*;

/// An "engine" that accepts a websocket and echoes whatever it receives, verbatim.
async fn mock_engine_ws() -> String {
    let app = axum::Router::new().route(
        "/v1/events",
        axum::routing::get(|ws: axum::extract::ws::WebSocketUpgrade| async move {
            ws.on_upgrade(|mut socket| async move {
                while let Some(Ok(msg)) = socket.recv().await {
                    if socket.send(msg).await.is_err() {
                        break;
                    }
                }
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Serve the real API router on a port, since a websocket upgrade cannot go through `oneshot`.
async fn serve_api(engine: String) -> Option<(String, wheel_api::db::Db)> {
    let url = db_url()?;
    let db = wheel_api::db::Db::connect(&url)
        .await
        .expect("connect and migrate");

    let state = AppState::new(Inner {
        jwks: wheel_api::auth::jwks::JwksCache::new(
            "http://unused.invalid/jwks".into(),
            reqwest::Client::new(),
        ),
        cfg: cfg(&url),
        db: db.clone(),
        http: reqwest::Client::new(),
        orch: Arc::new(NoopOrchestrator) as Arc<dyn Orchestrator>,
        ingress_limiter: wheel_api::http::ratelimit::RateLimiter::new(60),
        auth_limiter: wheel_api::http::authlimit::AuthLimiter::new(1000, 1000),
        engine_base_override: Some(engine),
    });

    let app = wheel_api::build_router(state, &[]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((format!("http://{addr}"), db))
}

macro_rules! api_or_skip {
    ($engine:expr) => {
        match serve_api($engine).await {
            Some(v) => v,
            None => return,
        }
    };
}

#[tokio::test]
async fn frames_cross_the_bridge_byte_identical() {
    let engine = mock_engine_ws().await;
    let (api, _db) = api_or_skip!(engine);
    let token = token("ws_alice");
    let pid = create_project(&api, &token, "ws board").await;
    let ticket = mint_ticket(&api, &token, &pid).await;

    let ws_url = format!(
        "{}/v1/projects/{pid}/engine/v1/events?ticket={ticket}",
        api.replace("http://", "ws://")
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("the bridge should accept a valid ticket");

    // A realistic `message` event, with keys in an order that a re-serialisation would disturb and
    // a body containing the characters most likely to be mangled in transit.
    let payload = json!({
        "type": "message",
        "id": "018f7c1e-0000-7000-8000-000000000000",
        "from": "researcher",
        "body": "quotes \" backslash \\ newline \n unicode ☃ и 中文 emoji 🎡 </AgentPrompt>",
        "sha256": "abc123",
    })
    .to_string();

    socket
        .send(Message::Text(payload.clone().into()))
        .await
        .unwrap();
    let echoed = socket.next().await.expect("a reply").expect("no ws error");

    assert_eq!(
        echoed.into_text().unwrap().as_str(),
        payload.as_str(),
        "the bridge must relay frames verbatim; re-encoding would reorder keys and break id correlation"
    );
}

#[tokio::test]
async fn binary_frames_survive_too() {
    let engine = mock_engine_ws().await;
    let (api, _db) = api_or_skip!(engine);
    let token = token("ws_bin");
    let pid = create_project(&api, &token, "ws bin").await;
    let ticket = mint_ticket(&api, &token, &pid).await;

    let ws_url = format!(
        "{}/v1/projects/{pid}/engine/v1/events?ticket={ticket}",
        api.replace("http://", "ws://")
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    // Every byte value, including the ones that are invalid UTF-8 on their own.
    let blob: Vec<u8> = (0u8..=255).collect();
    socket
        .send(Message::Binary(blob.clone().into()))
        .await
        .unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(echoed.into_data().to_vec(), blob);
}

#[tokio::test]
async fn a_ticket_opens_the_socket_exactly_once() {
    let engine = mock_engine_ws().await;
    let (api, _db) = api_or_skip!(engine);
    let token = token("ws_once");
    let pid = create_project(&api, &token, "ws once").await;
    let ticket = mint_ticket(&api, &token, &pid).await;

    let ws_url = format!(
        "{}/v1/projects/{pid}/engine/v1/events?ticket={ticket}",
        api.replace("http://", "ws://")
    );
    let (_first, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("first use of the ticket");

    // A ticket travels in a URL, which is exactly where credentials get captured — in browser
    // history, proxy logs, and Referer headers. Single-use is what makes that acceptable.
    assert!(
        tokio_tungstenite::connect_async(&ws_url).await.is_err(),
        "a redeemed ticket must not open a second socket"
    );
}

#[tokio::test]
async fn a_ticket_is_useless_against_another_project() {
    let engine = mock_engine_ws().await;
    let (api, _db) = api_or_skip!(engine);
    let token = token("ws_cross");
    let mine = create_project(&api, &token, "mine").await;
    let other = create_project(&api, &token, "other").await;
    let ticket = mint_ticket(&api, &token, &mine).await;

    // Same owner, different project: ownership alone is not the binding.
    let ws_url = format!(
        "{}/v1/projects/{other}/engine/v1/events?ticket={ticket}",
        api.replace("http://", "ws://")
    );
    assert!(tokio_tungstenite::connect_async(&ws_url).await.is_err());
}

#[tokio::test]
async fn a_forged_ticket_is_refused() {
    let engine = mock_engine_ws().await;
    let (api, _db) = api_or_skip!(engine);
    let token = token("ws_forge");
    let pid = create_project(&api, &token, "forge").await;

    for bogus in ["", "not-a-ticket", &"A".repeat(64)] {
        let ws_url = format!(
            "{}/v1/projects/{pid}/engine/v1/events?ticket={bogus}",
            api.replace("http://", "ws://")
        );
        assert!(
            tokio_tungstenite::connect_async(&ws_url).await.is_err(),
            "accepted a forged ticket: {bogus:?}"
        );
    }
}
