// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `GET /v1/events` — the WebSocket the API proxies straight through to the
//! browser.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::HeaderMap,
    response::Response,
};
use tokio::sync::broadcast::error::RecvError;

use super::AppState;

pub async fn events_ws(
    ws: WebSocketUpgrade,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Response {
    // Read once, at the upgrade: a WebSocket carries no further headers per frame, so this is the
    // only chance to know who is on the other end for the life of the connection.
    let tier = super::actor::tier_from_headers(&headers);
    let caller = super::actor::from_headers(&headers);
    ws.on_upgrade(move |socket| pump(socket, s, tier, caller))
}

async fn pump(
    mut socket: WebSocket,
    s: AppState,
    tier: super::actor::ActorTier,
    caller: Option<String>,
) {
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
                    // `on_behalf_of` is masked per subscriber here, not at `Bus::publish` — the bus
                    // fans one event out to every tier at once, and only this connection's own
                    // upgrade headers say which tier is on the other end of THIS socket.
                    let ev = match ev {
                        wheel_core::Event::Message { message } => wheel_core::Event::Message {
                            message: super::actor::mask_message_for_tier(
                                message,
                                tier,
                                caller.as_deref(),
                            ),
                        },
                        other => other,
                    };
                    let Ok(json) = serde_json::to_string(&ev) else { continue };
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                // This subscriber fell behind. Dropping it is deliberate: a
                // slow browser must never stall the supervisor. The client
                // refetches GET /v1/board to resynchronise.
                Err(RecvError::Lagged(_)) => {
                    let frame = wheel_core::Event::Lagged {
                        hint: wheel_core::LAGGED_HINT.to_string(),
                    };
                    let Ok(json) = serde_json::to_string(&frame) else { continue };
                    if socket.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                    continue;
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}
