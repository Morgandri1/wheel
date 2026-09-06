//! Messages, their delivery states, and the `<AgentPrompt>` envelope
//! (ARCHITECTURE.md §3 and §3c).
//!
//! §3c is a list of failure modes observed running a real agent team on YOKE.
//! The ones this module answers: attribution cannot be forged from a body
//! (§3c#5), delivery is byte-exact and provable (§3c#3), delivery state is
//! explicit rather than opaque (§3c#4), and nothing is ever silently truncated
//! (§3c#11) — a body that cannot be delivered stays `queued` with an error
//! rather than being clipped.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{name::NodeName, node::NodeType, timestamp::Timestamp};

/// Maximum message body, enforced by the CLI/MCP tool *before* sending so the
/// sender gets a clear error instead of discovering the limit by failing
/// (§3c#6).
pub const MAX_MESSAGE_BODY: usize = 256 * 1024;

/// Who sent a message. The `type` rendered into the envelope comes from here
/// and is **engine-generated** — a body can never forge it (§3c#5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MessageSender {
    /// A board node. Only `agent`, `endpoint` and `script` nodes can send;
    /// [`MessageSender::sender_type`] is what appears in the envelope.
    Node {
        id: Uuid,
        name: NodeName,
        #[serde(rename = "type")]
        node_type: NodeType,
    },
    /// The operator, via the UI chat box or `POST /v1/agents/:id/send`.
    User,
    /// The engine itself (e.g. a delivery failure notice).
    System,
}

impl MessageSender {
    /// The `from` attribute of the envelope.
    pub fn name(&self) -> String {
        match self {
            MessageSender::Node { name, .. } => name.to_string(),
            MessageSender::User => "user".into(),
            MessageSender::System => "system".into(),
        }
    }

    /// The `type` attribute of the envelope: one of
    /// `agent | user | endpoint | script | system` (§3c#5).
    pub fn sender_type(&self) -> &'static str {
        match self {
            MessageSender::Node { node_type, .. } => node_type.as_str(),
            MessageSender::User => "user",
            MessageSender::System => "system",
        }
    }

    /// Node types that are actually permitted to originate a message, per the
    /// wire matrix (agent→agent, endpoint→agent, script→agent are the only
    /// `send` edges into an agent).
    pub fn is_valid_origin(&self) -> bool {
        match self {
            MessageSender::Node { node_type, .. } => matches!(
                node_type,
                NodeType::Agent | NodeType::Endpoint | NodeType::Script
            ),
            MessageSender::User | MessageSender::System => true,
        }
    }
}

/// Delivery state (§3c#4). Strictly forward-moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum MessageState {
    /// Persisted, not yet written to a child's stdin (agent stopped, or an
    /// earlier message is still in flight).
    #[default]
    Queued,
    /// Written to the child's stdin.
    Delivered,
    /// The harness reported the turn containing it complete.
    Consumed,
    /// The body could not be encoded for delivery, so it was set aside rather
    /// than retried forever. A stored message that kills the encoder is
    /// replayed at every start, which turns one bad body into a board that
    /// never comes up (ADVERSARY 035); quarantine is what makes that
    /// impossible independently of whether the encoder has a bug today.
    Undeliverable,
}

impl MessageState {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageState::Queued => "queued",
            MessageState::Delivered => "delivered",
            MessageState::Consumed => "consumed",
            MessageState::Undeliverable => "undeliverable",
        }
    }

    /// Legal transitions. The engine asserts on this so a bug cannot walk a
    /// message backwards and cause a re-delivery.
    pub fn can_advance_to(self, next: MessageState) -> bool {
        matches!(
            (self, next),
            (MessageState::Queued, MessageState::Delivered)
                | (MessageState::Delivered, MessageState::Consumed)
                // A message may be set aside from either lane, and never
                // leaves: quarantine is terminal by design, so a body that
                // cannot be encoded cannot come back on the next start.
                | (MessageState::Queued, MessageState::Undeliverable)
                | (MessageState::Delivered, MessageState::Undeliverable)
        )
    }
}

impl std::fmt::Display for MessageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A message row, persisted before any delivery is attempted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub id: Uuid,
    pub from: MessageSender,
    /// Target agent node id.
    pub to: Uuid,
    pub body: String,
    /// Lowercase hex SHA-256 of `body` as sent, so the sender can prove what
    /// arrived is what was sent (§3c#3).
    pub sha256: String,
    /// Byte length of `body`.
    pub bytes: u64,
    pub state: MessageState,
    /// Threading (§3c#9): the message this one replies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Uuid>,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<Timestamp>,
    /// Why this message is still `queued`. Never a reason to truncate it
    /// (§3c#11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// What `wheel msg` returns to the sender (§3c#3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MessageReceipt {
    pub id: Uuid,
    pub sha256: String,
    pub bytes: u64,
    pub state: MessageState,
}

impl From<&Message> for MessageReceipt {
    fn from(m: &Message) -> Self {
        Self {
            id: m.id,
            sha256: m.sha256.clone(),
            bytes: m.bytes,
            state: m.state,
        }
    }
}

/// Neutralise any `<AgentPrompt` or `</AgentPrompt` sequence in a body
/// (ADVERSARY finding 001).
///
/// Both tags matter, for different attacks:
/// - a forged **closing** tag ends the envelope early, so following body text
///   appears to the model as engine-authored framing;
/// - a forged **opening** tag starts what looks like a *new, authentic* message
///   with attribution the attacker chose.
///
/// Escaping only the closing tag would leave the second, which is the more
/// useful attack. So the `<` of either form is backslash-escaped, yielding
/// `<\AgentPrompt` / `<\/AgentPrompt`.
///
/// Matching is case-insensitive on the **decoded bytes** we are about to write,
/// because the model — the actual consumer — does not care about tag case even
/// though XML does.
///
/// This is deliberately not reversed on read: the recipient sees the escaped
/// form, and `wheel inbox <id>` returns the original body from sqlite (§3c#2),
/// so nothing is lost.
pub fn escape_envelope_body(body: &str) -> String {
    // The comparison is on BYTES, and the string is never sliced by a byte
    // offset we computed.
    //
    // It used to be `body[name_at..name_at + TAG.len()]`, and slicing a `str`
    // at an index that is not a character boundary PANICS. `name_at` was
    // always a boundary -- it follows `<` and an optional `/`, both ASCII --
    // so the bug hid until the byte 11 further on landed mid-character. An em
    // dash did it: PM writes them in most messages, one was stored on the
    // wheel-dev board, and every engine start replayed it and died. A whole
    // project stayed offline through reboots because of one character in one
    // message.
    //
    // The wider rule this is written to obey: message content must never be
    // able to kill an engine. A body that is awkward, malformed or hostile is
    // a bad message, not a dead board -- whatever this cannot interpret it
    // emits as ordinary text.
    const TAG: &[u8] = b"agentprompt";
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Optional '/' immediately after '<', then the tag name.
            let after = i + 1;
            let name_at = if bytes.get(after) == Some(&b'/') {
                after + 1
            } else {
                after
            };
            // Byte slicing, not `&str` slicing. `body.len() >= name_at + TAG.len()`
            // proves the index is IN RANGE; it proves nothing about it being on
            // a CHARACTER boundary, and that is what panicked -- one '<' with a
            // multi-byte character straddling the offset eleven bytes later took
            // the wheel-dev board down for hours. A byte slice cannot land
            // mid-character, so the whole class goes away, and it is what the
            // comment above this function already claimed: matching is on the
            // decoded BYTES.
            let matches_tag = bytes
                .get(name_at..name_at + TAG.len())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(TAG));
            if matches_tag {
                // Consume only the '<'; the rest is emitted as ordinary text.
                out.push_str("<\\");
                i += 1;
                continue;
            }
        }
        // `i` is on a character boundary: every step advances by a whole
        // character, and the only other move is over `<`, which is one byte.
        // `get` rather than an index anyway -- an invariant worth stating is
        // not worth panicking over if it is ever broken by a later edit.
        let Some(c) = body.get(i..).and_then(|rest| rest.chars().next()) else {
            break;
        };
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Lowercase-hex SHA-256, implemented here so `wheel-core` stays dependency
/// light and the CLI, engine and tests all agree byte-for-byte.
pub fn sha256_hex(data: &[u8]) -> String {
    let h = Sha256::digest(data);
    let mut s = String::with_capacity(64);
    for b in h {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

impl Message {
    /// Render the `<AgentPrompt>` envelope written to a running agent's stdin
    /// (§3 "Inbound message framing").
    ///
    /// ```text
    /// <AgentPrompt id="<uuid>" from="<from name>" type="<from type>">
    /// <body>
    /// </AgentPrompt>
    /// ```
    /// `reply_to="<id>"` is added when the message is a reply (§3c#9).
    pub fn envelope(&self) -> String {
        let reply = match self.reply_to {
            Some(r) => format!(" reply_to=\"{r}\""),
            None => String::new(),
        };
        format!(
            "<AgentPrompt id=\"{}\" from=\"{}\" type=\"{}\"{}>\n{}\n</AgentPrompt>",
            self.id,
            self.from.name(),
            self.from.sender_type(),
            reply,
            escape_envelope_body(&self.body)
        )
    }

    /// The single line written to the child's stdin: a stream-json user turn
    /// wrapping [`Message::envelope`]. Newline-terminated, flushed immediately,
    /// and the only thing ever written to a child's stdin.
    pub fn stdin_line(&self) -> String {
        let turn = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [ { "type": "text", "text": self.envelope() } ]
            }
        });
        format!("{turn}\n")
    }
}

// ---------------------------------------------------------------------------
// Minimal SHA-256 (FIPS 180-4). Vendored to keep wheel-core dependency-free;
// verified against the NIST test vectors in tests/envelope.rs.
// ---------------------------------------------------------------------------

struct Sha256;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn digest(data: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];

        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in msg.as_chunks::<64>().0 {
            let mut w = [0u32; 64];
            for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
                w[i] = u32::from_be_bytes(*word);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }

            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);

            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);

                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }

            for (i, v) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
                h[i] = h[i].wrapping_add(v);
            }
        }

        let mut out = [0u8; 32];
        for (i, v) in h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }
}
