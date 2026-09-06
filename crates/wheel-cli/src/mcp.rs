//! `wheel mcp-serve` — the board as MCP tools, over stdio (§3c #1).
//!
//! This exists because of a specific failure: a message body passed as argv
//! goes through a shell first, where backticks and `$(…)` are substituted
//! before `wheel` ever sees it, so what arrives is silently not what was sent.
//! A tool call is structured all the way down, which is why the preamble tells
//! agents to prefer these over shelling out.
//!
//! Deliberately thin. The tool LIST comes from the engine (it depends on the
//! caller's wires, which change while the agent is running) and every call is
//! forwarded to the same `/v1/cli/*` route the CLI uses. There is no second
//! implementation of what a tool does, and therefore nothing to drift.

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{json, Value};

use crate::transport::{Engine, Reply};

/// The MCP revision this speaks. Clients send their own; we answer with ours.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Read JSON-RPC from `input`, write responses to `output`, until EOF.
pub fn serve(engine: &Engine, input: impl BufRead, mut output: impl Write) -> Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            // A malformed frame is answered, not ignored: a client waiting on
            // a reply that never comes looks like a hung agent.
            write_frame(&mut output, &error(Value::Null, -32700, "invalid JSON"))?;
            continue;
        };
        let Some(response) = handle(engine, &req) else {
            // A notification has no id and takes no reply, by the protocol.
            continue;
        };
        write_frame(&mut output, &response)?;
    }
    Ok(())
}

fn write_frame(out: &mut impl Write, v: &Value) -> Result<()> {
    writeln!(out, "{v}")?;
    out.flush()?;
    Ok(())
}

/// Handle one request. `None` means "this was a notification, say nothing".
pub fn handle(engine: &Engine, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = req
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // A notification is a request with no id. Answering one is a protocol
    // error, and clients differ in how badly they take it.
    let id = id?;

    match method {
        "initialize" => Some(result(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "wheel", "version": env!("CARGO_PKG_VERSION")},
            }),
        )),
        "ping" => Some(result(id, json!({}))),
        "tools/list" => Some(match engine.get("/v1/cli/mcp/tools") {
            Ok(r) if r.status < 300 => result(id, json!({"tools": r.body["tools"]})),
            Ok(r) => error(id, -32603, &reply_message(&r)),
            Err(e) => error(id, -32603, &format!("{e:#}")),
        }),
        "tools/call" => Some(call_tool(engine, id, req)),
        other => Some(error(id, -32601, &format!("unknown method {other:?}"))),
    }
}

fn call_tool(engine: &Engine, id: Value, req: &Value) -> Value {
    let params = req.get("params").cloned().unwrap_or(json!({}));
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let reply = match route_for(name, &args) {
        Some(Route::Get(path)) => engine.get(&path),
        Some(Route::Post(path, body)) => engine.post(&path, body),
        None => return error(id, -32602, &format!("unknown tool {name:?}")),
    };

    match reply {
        // A wire denial or a bad argument is a TOOL error, not a protocol
        // error: the model should see it, reconsider and try something else,
        // which is exactly what isError is for. A protocol error would look to
        // the harness like the server is broken.
        Ok(r) if r.status >= 300 => tool_error(id, &reply_message(&r)),
        Ok(r) => result(
            id,
            json!({
                "content": [{"type": "text", "text": render(&r.body)}],
                "isError": false,
            }),
        ),
        Err(e) => tool_error(id, &format!("{e:#}")),
    }
}

enum Route {
    Get(String),
    Post(String, Value),
}

/// Map a tool name onto the `/v1/cli/*` route that already implements it.
///
/// Anything with `__` in it is a tool-node operation (§3d rule 7): the part
/// before is the node, the part after is the operation.
fn route_for(name: &str, args: &Value) -> Option<Route> {
    let s = |k: &str| {
        args.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    if let Some((node, op)) = name.split_once("__") {
        return Some(Route::Post(
            "/v1/cli/tool".into(),
            json!({"node": node, "op": op, "args": args}),
        ));
    }

    Some(match name {
        "whoami" => Route::Get("/v1/cli/whoami".into()),
        "connections" => Route::Get("/v1/cli/connections".into()),
        "list" => Route::Get("/v1/cli/list".into()),
        "read" => Route::Get(format!(
            "/v1/cli/read?addr={}",
            crate::urlencode(&s("addr"))
        )),
        "secret_get" => Route::Get(format!(
            "/v1/cli/secret?addr={}",
            crate::urlencode(&s("addr"))
        )),
        "ls" => {
            let mut path = "/v1/cli/ls".to_string();
            let node = s("node");
            if !node.is_empty() {
                path.push_str(&format!("?node={}", crate::urlencode(&node)));
                let prefix = s("prefix");
                if !prefix.is_empty() {
                    path.push_str(&format!("&prefix={}", crate::urlencode(&prefix)));
                }
            }
            Route::Get(path)
        }
        "inbox" => {
            let id = s("id");
            Route::Get(if id.is_empty() {
                "/v1/cli/inbox".into()
            } else {
                format!("/v1/cli/inbox?id={}", crate::urlencode(&id))
            })
        }
        "msg" => Route::Post("/v1/cli/msg".into(), args.clone()),
        "write" => Route::Post("/v1/cli/write".into(), args.clone()),
        "rm" => Route::Post("/v1/cli/rm".into(), args.clone()),
        "query" => Route::Post("/v1/cli/query".into(), args.clone()),
        "ctx_clear" => Route::Post("/v1/cli/ctx/clear".into(), json!({})),
        _ => return None,
    })
}

/// The engine's own message, which is written to be read by whoever hit the
/// wall — an agent included.
fn reply_message(r: &Reply) -> String {
    r.body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("engine returned {}", r.status))
}

/// A string result is given as-is; anything else as JSON. A model reading
/// `"# My notes"` should see the markdown, not a quoted string.
fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => match o.get("value") {
            Some(Value::String(s)) => s.clone(),
            _ => v.to_string(),
        },
        _ => v.to_string(),
    }
}

fn result(id: impl Into<Value>, value: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id.into(), "result": value})
}

fn error(id: impl Into<Value>, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id.into(), "error": {"code": code, "message": message}})
}

/// A failure the MODEL should handle, reported inside a successful JSON-RPC
/// response. A protocol-level error would tell the harness the server is
/// broken and it would stop asking.
fn tool_error(id: impl Into<Value>, message: &str) -> Value {
    result(
        id,
        json!({"content": [{"type": "text", "text": message}], "isError": true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    }

    fn route(name: &str, args: Value) -> (String, Option<Value>) {
        match route_for(name, &args).expect("known tool") {
            Route::Get(p) => (p, None),
            Route::Post(p, b) => (p, Some(b)),
        }
    }

    /// A notification has no id, and answering one is a protocol error that
    /// clients take badly. This is the difference between a working server and
    /// one that looks broken on the second message.
    #[test]
    fn a_notification_is_not_answered() {
        let engine = Engine::for_test();
        assert!(handle(
            &engine,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .is_none());
        // ...and a request WITH an id always is.
        assert!(handle(&engine, &req("ping", json!({}))).is_some());
    }

    #[test]
    fn initialize_announces_a_protocol_version_and_tools() {
        let engine = Engine::for_test();
        let r = handle(&engine, &req("initialize", json!({}))).unwrap();
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert_eq!(r["result"]["serverInfo"]["name"], "wheel");
    }

    #[test]
    fn an_unknown_method_is_a_protocol_error() {
        let engine = Engine::for_test();
        let r = handle(&engine, &req("resources/list", json!({}))).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    /// Every tool must land on the route that already implements it. A tool
    /// wired to the wrong path still returns 200 and still looks plausible —
    /// it just answers a different question than the model asked.
    #[test]
    fn every_tool_addresses_the_route_that_implements_it() {
        assert_eq!(route("whoami", json!({})).0, "/v1/cli/whoami");
        assert_eq!(route("connections", json!({})).0, "/v1/cli/connections");
        assert_eq!(
            route("read", json!({"addr": "notes/r1"})).0,
            "/v1/cli/read?addr=notes%2Fr1"
        );
        assert_eq!(
            route("secret_get", json!({"addr": "creds/KEY"})).0,
            "/v1/cli/secret?addr=creds%2FKEY"
        );
        assert_eq!(route("ls", json!({})).0, "/v1/cli/ls");
        assert_eq!(
            route("ls", json!({"node": "notes"})).0,
            "/v1/cli/ls?node=notes"
        );
        assert_eq!(
            route("ls", json!({"node": "notes", "prefix": "2026-"})).0,
            "/v1/cli/ls?node=notes&prefix=2026-"
        );
        assert_eq!(route("inbox", json!({})).0, "/v1/cli/inbox");
        assert_eq!(
            route("inbox", json!({"id": "abc"})).0,
            "/v1/cli/inbox?id=abc"
        );

        for (tool, path) in [
            ("msg", "/v1/cli/msg"),
            ("write", "/v1/cli/write"),
            ("rm", "/v1/cli/rm"),
            ("query", "/v1/cli/query"),
        ] {
            let (got, body) = route(tool, json!({"a": 1}));
            assert_eq!(got, path, "{tool}");
            assert!(body.is_some(), "{tool} must POST its arguments");
        }
        assert_eq!(route("ctx_clear", json!({})).0, "/v1/cli/ctx/clear");
    }

    /// §3d rule 7: `<tool>__<op>` is a tool-node operation, and the split is
    /// how the engine knows which node to call.
    #[test]
    fn a_tool_node_operation_is_routed_by_its_double_underscore() {
        let (path, body) = route("petstore__listPets", json!({"limit": 5}));
        assert_eq!(path, "/v1/cli/tool");
        let body = body.unwrap();
        assert_eq!(body["node"], "petstore");
        assert_eq!(body["op"], "listPets");
        assert_eq!(body["args"]["limit"], 5);
    }

    /// An underscore in the OPERATION name must not confuse the split — only
    /// the first `__` separates node from op.
    #[test]
    fn only_the_first_double_underscore_splits_a_tool_name() {
        let (_, body) = route("mailer__send__now", json!({}));
        let body = body.unwrap();
        assert_eq!(body["node"], "mailer");
        assert_eq!(body["op"], "send__now");
    }

    /// `run` needs script nodes, which do not exist yet. It must not be
    /// routable either: a tool that resolves to a 404 route is worse than one
    /// that is honestly absent.
    #[test]
    fn a_tool_with_no_engine_route_is_not_routable() {
        assert!(route_for("run", &json!({"script": "x"})).is_none());
    }

    #[test]
    fn an_unknown_tool_is_refused_rather_than_routed_somewhere() {
        assert!(route_for("definitely_not_a_tool", &json!({})).is_none());
    }

    /// A wire denial is something the MODEL should handle — reconsider and try
    /// something else. Reported as a protocol error it would tell the harness
    /// the server is broken, and it would stop asking.
    #[test]
    fn a_denial_is_a_tool_error_not_a_protocol_error() {
        let r = tool_error(json!(1), "no wire from a to b (need: write)");
        assert!(r["error"].is_null(), "must be a successful JSON-RPC reply");
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(
            r["result"]["content"][0]["text"],
            "no wire from a to b (need: write)"
        );
    }

    /// The engine's message is written to be read by whoever hit the wall, an
    /// agent included. Replacing it with a status code throws that away.
    #[test]
    fn the_engines_own_reason_is_what_reaches_the_model() {
        let r = Reply {
            status: 403,
            body: json!({"error": {"code": "wire_denied", "message": "no wire from a to b"}}),
        };
        assert_eq!(reply_message(&r), "no wire from a to b");
        // ...and a body with no message still says something useful.
        let bare = Reply {
            status: 500,
            body: Value::Null,
        };
        assert_eq!(reply_message(&bare), "engine returned 500");
    }

    /// A model reading a ctx node should see the markdown, not a quoted JSON
    /// string with escaped newlines in it.
    #[test]
    fn a_text_result_reaches_the_model_as_text() {
        assert_eq!(
            render(&json!("# My notes\nline two")),
            "# My notes\nline two"
        );
        assert_eq!(render(&json!({"value": "# My notes"})), "# My notes");
        // Anything structural stays JSON, because that is what it is.
        assert_eq!(render(&json!({"rows": [1, 2]})), "{\"rows\":[1,2]}");
    }

    /// A malformed frame must be ANSWERED. A client waiting on a reply that
    /// never comes looks exactly like a hung agent.
    #[test]
    fn a_malformed_frame_is_answered_rather_than_ignored() {
        let engine = Engine::for_test();
        let mut out = Vec::new();
        serve(&engine, &b"not json at all\n"[..], &mut out).unwrap();
        let reply: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply["error"]["code"], -32700);
    }

    #[test]
    fn blank_lines_are_skipped_and_a_stream_ends_cleanly() {
        let engine = Engine::for_test();
        let mut out = Vec::new();
        let input = b"\n\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n\n";
        serve(&engine, &input[..], &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "one request, one reply: {text}");
        let reply: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(reply["id"], 7);
    }
}
