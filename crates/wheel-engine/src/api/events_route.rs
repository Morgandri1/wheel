//! `GET /v1/events` — the WebSocket the API proxies straight through to the
//! browser.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use tokio::sync::broadcast::error::RecvError;

use super::AppState;

pub async fn events_ws(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| pump(socket, s))
}

async fn pump(mut socket: WebSocket, s: AppState) {
    let mut rx = s.events.subscribe();

    loop {
        tokio::select! {
            // A client that closes, errors, or sends anything we don't expect
            // ends the connection. Nothing a client sends is interpreted: this
            // socket is strictly one-directional, so it cannot become a second
            // control plane.
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => continue,
            },

            event = rx.recv() => match event {
                Ok(ev) => {
                    let Ok(json) = serde_json::to_string(&ev) else { continue };
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                // This subscriber fell behind. Dropping it is deliberate: a
                // slow browser must never stall the supervisor. The client
                // refetches GET /v1/board to resynchronise.
                Err(RecvError::Lagged(_)) => {
                    let _ = socket
                        .send(Message::Text(
                            serde_json::json!({
                                "type": "lagged",
                                "hint": "events were dropped; refetch GET /v1/board"
                            })
                            .to_string()
                            .into(),
                        ))
                        .await;
                    continue;
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}
