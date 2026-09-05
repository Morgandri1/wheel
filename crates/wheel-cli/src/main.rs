//! `wheel` — the CLI an agent or script uses to reach its board.
//!
//! Deliberately shaped like `yoke`, because that is the grammar agents already
//! know: every node is a keyspace, identity is proven by the token and never
//! passed as an argument, and a denial is **exit 3** with one plain line.
//!
//! Exit codes: 0 ok · 1 usage/local · 2 engine error · 3 wire denied · 4 no
//! such node.

use std::process::ExitCode;

use anyhow::Result;

mod transport;

use transport::{Engine, Reply};

const USAGE: &str = "\
wheel — talk to your Wheel board

  wheel whoami                      who am I, and what am I wired to
  wheel connections                 my wires, in plain language
  wheel list                        every agent on the board
  wheel ls [<node>] [<prefix>]      reachable keyspaces, or keys inside one
  wheel read  <node>[/<row>]        ctx markdown / table row / chest blob
  wheel write <node>[/<row>] <value>|--file <path>|--stdin
  wheel msg   <agent> <text>|--file <path>|--stdin
  wheel inbox [<message-id>]        re-read what I was sent

Values: prefer --file or --stdin. A body passed as an argument goes through
your shell first, where backticks and $(...) are substituted before wheel ever
sees it — which silently corrupts the message.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json_out = args.iter().any(|a| a == "--json");
    let args: Vec<String> = args.into_iter().filter(|a| a != "--json").collect();

    match run(&args, json_out) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("wheel: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String], json_out: bool) -> Result<u8> {
    let Some(cmd) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return Ok(1);
    };

    // --help must work without a token, or an agent that cannot authenticate
    // has no way to discover why.
    if matches!(cmd, "-h" | "--help" | "help") {
        print!("{USAGE}");
        return Ok(0);
    }

    let engine = Engine::from_env()?;
    let rest = &args[1..];

    match cmd {
        "whoami" => show(engine.get("/v1/cli/whoami")?, json_out, render_whoami),
        "connections" => show(
            engine.get("/v1/cli/connections")?,
            json_out,
            render_connections,
        ),
        "list" => show(engine.get("/v1/cli/list")?, json_out, render_list),

        "ls" => {
            let path = match rest.first() {
                Some(node) => format!("/v1/cli/ls?node={}", urlencode(node)),
                None => "/v1/cli/ls".to_string(),
            };
            show(engine.get(&path)?, json_out, render_ls)
        }

        "read" => {
            let addr = rest
                .first()
                .ok_or_else(|| usage("read needs <node>[/<row>]"))?;
            show(
                engine.get(&format!("/v1/cli/read?addr={}", urlencode(addr)))?,
                json_out,
                render_read,
            )
        }

        "write" => {
            let addr = rest
                .first()
                .ok_or_else(|| usage("write needs <node>[/<row>]"))?;
            let value = read_value(&rest[1..])?;
            show(
                engine.post(
                    "/v1/cli/write",
                    serde_json::json!({ "addr": addr, "value": value }),
                )?,
                json_out,
                render_ok,
            )
        }

        "msg" => {
            let to = rest.first().ok_or_else(|| usage("msg needs <agent>"))?;
            let body = read_value(&rest[1..])?;
            show(
                engine.post("/v1/cli/msg", serde_json::json!({ "to": to, "body": body }))?,
                json_out,
                render_receipt,
            )
        }

        "inbox" => {
            let path = match rest.first() {
                Some(id) => format!("/v1/cli/inbox?id={}", urlencode(id)),
                None => "/v1/cli/inbox".to_string(),
            };
            show(engine.get(&path)?, json_out, render_inbox)
        }

        other => {
            eprintln!("wheel: unknown command {other:?}\n");
            print!("{USAGE}");
            Ok(1)
        }
    }
}

fn usage(msg: &str) -> anyhow::Error {
    anyhow::anyhow!("{msg}")
}

/// Read a value from `--file`, `--stdin`, or the remaining argv.
///
/// The argv path warns on `` ` `` and `$(` (§3c#1). This is on by default and
/// not opt-in because the defect it catches is silent: the shell substitutes
/// before `wheel` runs, so by the time we see the value the damage is done and
/// nothing downstream can detect it.
fn read_value(rest: &[String]) -> Result<String> {
    use std::io::Read;

    match rest.first().map(String::as_str) {
        Some("--file") => {
            let path = rest.get(1).ok_or_else(|| usage("--file needs a path"))?;
            Ok(std::fs::read_to_string(path)?)
        }
        Some("--stdin") => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            Ok(s)
        }
        Some(_) => {
            let joined = rest.join(" ");
            if joined.contains('`') || joined.contains("$(") {
                eprintln!(
                    "wheel: warning — this value contains ` or $( , which your shell \
                     substituted BEFORE wheel saw it. What arrives may not be what you \
                     typed. Use --file <path> or --stdin to pass a body safely."
                );
            }
            Ok(joined)
        }
        None => Err(usage("expected a value, or --file <path> / --stdin")),
    }
}

/// Turn a reply into output and an exit code.
///
/// The engine's error `code` maps to the exit status, so `wheel` and the wire
/// matrix agree on what "denied" means without the CLI re-deciding it.
fn show(r: Reply, json_out: bool, render: fn(&serde_json::Value)) -> Result<u8> {
    if json_out {
        println!("{}", serde_json::to_string_pretty(&r.body)?);
    }

    if r.status < 300 {
        if !json_out {
            render(&r.body);
        }
        return Ok(0);
    }

    let code = r.body.pointer("/error/code").and_then(|c| c.as_str());
    let message = r
        .body
        .pointer("/error/message")
        .and_then(|m| m.as_str())
        .unwrap_or("engine error");
    if !json_out {
        eprintln!("wheel: {message}");
    }
    Ok(match code {
        Some("wire_denied") => 3,
        Some("not_found") => 4,
        _ => 2,
    })
}

// --- one-line human output --------------------------------------------------

fn render_whoami(v: &serde_json::Value) {
    println!(
        "{} ({}) — {} wires",
        v["name"].as_str().unwrap_or("?"),
        v["type"].as_str().unwrap_or("?"),
        v["wires"].as_array().map(Vec::len).unwrap_or(0)
    );
    render_connections(v);
}

fn render_connections(v: &serde_json::Value) {
    let Some(wires) = v["wires"].as_array() else {
        return;
    };
    if wires.is_empty() {
        println!("  (no wires — you are not connected to anything yet)");
        return;
    }
    let width = wires
        .iter()
        .filter_map(|w| w["peer"].as_str())
        .map(str::len)
        .max()
        .unwrap_or(0);
    for w in wires {
        let arrow = if w["direction"] == "out" {
            "→"
        } else {
            "←"
        };
        println!(
            "  {arrow} {:width$}  {:5}  {}",
            w["peer"].as_str().unwrap_or("?"),
            w["type"].as_str().unwrap_or("?"),
            w["means"].as_str().unwrap_or(""),
        );
    }
}

fn render_list(v: &serde_json::Value) {
    let Some(agents) = v["agents"].as_array() else {
        return;
    };
    for a in agents {
        println!(
            "  {:20}  {:16}  {}",
            a["name"].as_str().unwrap_or("?"),
            a["status"].as_str().unwrap_or("?"),
            a["hosted_on"].as_str().unwrap_or("unhosted"),
        );
    }
}

fn render_ls(v: &serde_json::Value) {
    if let Some(ks) = v["keyspaces"].as_array() {
        for k in ks {
            println!(
                "  {:20}  {:9}  {}",
                k["name"].as_str().unwrap_or("?"),
                k["type"].as_str().unwrap_or("?"),
                k["wire"].as_str().unwrap_or("?"),
            );
        }
        return;
    }
    for k in v["keys"].as_array().into_iter().flatten() {
        println!("{}", k.as_str().unwrap_or(""));
    }
}

fn render_read(v: &serde_json::Value) {
    // Raw, with no decoration: an agent pipes this into something.
    print!("{}", v["value"].as_str().unwrap_or(""));
    if !v["value"].as_str().unwrap_or("").ends_with('\n') {
        println!();
    }
}

fn render_ok(v: &serde_json::Value) {
    println!("ok — wrote {}", v["node"].as_str().unwrap_or("?"));
}

/// §3c#3: the sender is told exactly what was accepted, so "did what I sent
/// arrive intact" is answerable rather than a guess.
fn render_receipt(v: &serde_json::Value) {
    println!(
        "queued {} — {} bytes, sha256 {}",
        v["id"].as_str().unwrap_or("?"),
        v["bytes"].as_u64().unwrap_or(0),
        v["sha256"].as_str().unwrap_or("?"),
    );
}

fn render_inbox(v: &serde_json::Value) {
    if let Some(m) = v.get("message") {
        // A single message prints its EXACT body, which is the whole point of
        // inbox: a garbled delivery can be re-read (§3c#2).
        print!("{}", m["body"].as_str().unwrap_or(""));
        println!();
        return;
    }
    for m in v["messages"].as_array().into_iter().flatten() {
        let body = m["body"].as_str().unwrap_or("");
        let first = body.lines().next().unwrap_or("");
        println!(
            "  {}  {:9}  {}",
            m["id"].as_str().unwrap_or("?"),
            m["state"].as_str().unwrap_or("?"),
            first.chars().take(60).collect::<String>(),
        );
    }
}

/// Percent-encode a query value. Node names are `[a-z0-9_-]` so this is only
/// load-bearing for chest paths and message ids, but encoding everything is
/// cheaper than remembering which callers need it.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn a_value_can_come_from_argv() {
        assert_eq!(read_value(&s(&["hello", "there"])).unwrap(), "hello there");
    }

    #[test]
    fn a_missing_value_is_a_usage_error_not_an_empty_string() {
        // Sending an empty body because the user forgot the argument is worse
        // than refusing: the message would be delivered and look intentional.
        assert!(read_value(&[]).is_err());
    }

    #[test]
    fn a_file_value_is_read_verbatim() {
        let dir = std::env::temp_dir().join("wheel-cli-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("body.txt");
        std::fs::write(&p, "line one\nline two\n").unwrap();
        let got = read_value(&s(&["--file", p.to_str().unwrap()])).unwrap();
        assert_eq!(got, "line one\nline two\n");
    }

    #[test]
    fn file_without_a_path_is_a_usage_error() {
        assert!(read_value(&s(&["--file"])).is_err());
    }

    #[test]
    fn urlencoding_escapes_what_a_query_string_cannot_carry() {
        assert_eq!(urlencode("notes"), "notes");
        assert_eq!(urlencode("a/b/c.txt"), "a%2Fb%2Fc.txt");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("x-y_z.1~"), "x-y_z.1~");
    }

    /// The exit codes are the contract agents rely on, so they are pinned.
    #[test]
    fn error_codes_map_to_the_documented_exit_statuses() {
        let denied = Reply {
            status: 403,
            body: serde_json::json!({"error":{"code":"wire_denied","message":"no wire"}}),
        };
        assert_eq!(show(denied, true, render_ok).unwrap(), 3);

        let missing = Reply {
            status: 404,
            body: serde_json::json!({"error":{"code":"not_found","message":"nope"}}),
        };
        assert_eq!(show(missing, true, render_ok).unwrap(), 4);

        // Anything else is an engine error, not a denial: an agent must not
        // read a 500 as "you lack permission".
        for (status, code) in [
            (500u16, "internal"),
            (401, "unauthorized"),
            (413, "too_large"),
        ] {
            let other = Reply {
                status,
                body: serde_json::json!({"error":{"code":code,"message":"x"}}),
            };
            assert_eq!(show(other, true, render_ok).unwrap(), 2, "{code}");
        }
    }

    #[test]
    fn success_is_exit_zero() {
        let ok = Reply {
            status: 200,
            body: serde_json::json!({"node":"notes"}),
        };
        assert_eq!(show(ok, true, render_ok).unwrap(), 0);
    }
}
