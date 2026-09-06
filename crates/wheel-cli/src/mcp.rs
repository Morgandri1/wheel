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
        "run" => Route::Post("/v1/cli/run".into(), args.clone()),
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
