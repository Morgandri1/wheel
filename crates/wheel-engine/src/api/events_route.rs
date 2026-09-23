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
                    let Some(json) = frame_for(ev, tier, caller.as_deref()) else { continue };
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

/// What one subscriber is shown of one event: `None` to skip it, else the JSON frame.
///
/// Masking happens here, per subscriber, not at `Bus::publish` — the bus fans one event out to
/// every tier at once, and only this connection's own upgrade headers say which tier is on the
/// other end of THIS socket. A message is masked; a transcript line, which is every message body
/// the agent was given, is dropped rather than edited (finding 062), so an admin's socket on the
/// same broadcast still gets the exact bytes.
fn frame_for(
    ev: wheel_core::Event,
    tier: super::actor::ActorTier,
    caller: Option<&str>,
) -> Option<String> {
    let ev = match ev {
        wheel_core::Event::Message { message } => wheel_core::Event::Message {
            message: super::actor::mask_message_for_tier(message, tier, caller),
        },
        wheel_core::Event::Log { line }
            if line.stream == wheel_core::LogStream::Transcript
                && !super::actor::may_read_bodies(tier) =>
        {
            return None
        }
        other => other,
    };
    serde_json::to_string(&ev).ok()
}

#[cfg(test)]
mod tests {
    use super::super::actor::{ActorTier, HIDDEN_BODY};
    use super::*;
    use wheel_core::{Event, LogLine, LogStream, Timestamp};

    fn log(stream: LogStream, text: &str) -> Event {
        Event::Log {
            line: LogLine {
                node_id: uuid::Uuid::nil(),
                seq: 1,
                stream,
                at: Timestamp::now(),
                text: text.into(),
            },
        }
    }

    fn message(on_behalf_of: &str, body: &str) -> Event {
        Event::Message {
            message: wheel_core::Message {
                id: uuid::Uuid::nil(),
                from: wheel_core::MessageSender::User,
                to: uuid::Uuid::nil(),
                body: body.into(),
                sha256: String::new(),
                bytes: 0,
                state: wheel_core::MessageState::default(),
                reply_to: None,
                on_behalf_of: Some(on_behalf_of.into()),
                created_at: Timestamp::now(),
                delivered_at: None,
                consumed_at: None,
                last_error: None,
                redacted: false,
            },
        }
    }

    #[test]
    fn a_guest_never_receives_a_transcript_frame_but_an_admin_and_a_prompter_do() {
        let secret = "<AgentPrompt>the launch code is 0000</AgentPrompt>";
        let frame = |t| frame_for(log(LogStream::Transcript, secret), t, None);
        assert_eq!(frame(ActorTier::Guest), None);
        for tier in [ActorTier::Prompter, ActorTier::Admin] {
            let json = frame(tier).expect("bodies-tier callers get the transcript");
            assert!(json.contains("launch code"), "{json}");
        }
    }

    #[test]
    fn a_guest_still_receives_what_the_agent_said() {
        for stream in [LogStream::Stdout, LogStream::Stderr, LogStream::Engine] {
            assert!(frame_for(log(stream, "hello"), ActorTier::Guest, None).is_some());
        }
    }

    #[test]
    fn a_guest_sees_a_placeholder_for_a_message_they_did_not_send_and_the_body_for_their_own() {
        let theirs = frame_for(
            message("alice@example.com", "secret instruction"),
            ActorTier::Guest,
            Some("bob@example.com"),
        )
        .unwrap();
        assert!(theirs.contains(HIDDEN_BODY), "{theirs}");
        assert!(theirs.contains("\"redacted\":true"), "{theirs}");
        assert!(!theirs.contains("secret instruction"), "{theirs}");

        let own = frame_for(
            message("bob@example.com", "my own words"),
            ActorTier::Guest,
            Some("bob@example.com"),
        )
        .unwrap();
        assert!(own.contains("my own words"), "{own}");
        assert!(!own.contains("redacted"), "{own}");
    }

    #[test]
    fn a_prompter_sees_message_bodies() {
        let json = frame_for(
            message("alice@example.com", "secret instruction"),
            ActorTier::Prompter,
            Some("carol@example.com"),
        )
        .unwrap();
        assert!(json.contains("secret instruction"), "{json}");
    }
}
