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
  wheel rm    <node>/<row>          delete a table row or chest blob
  wheel query <table> \"<SELECT ...>\"  read-only SQL, scoped to that one table
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
                Some(node) => {
                    let mut p = format!("/v1/cli/ls?node={}", urlencode(node));
                    // `wheel ls <node> [prefix]` (§3). Without this the
                    // argument was accepted and silently ignored, which reads
                    // as "the prefix matched nothing".
                    if let Some(prefix) = rest.get(1) {
                        p.push_str(&format!("&prefix={}", urlencode(prefix)));
                    }
                    p
                }
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

        "secret" => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            match sub {
                "get" => {
                    let addr = rest
                        .get(1)
                        .ok_or_else(|| usage("secret get needs <vault>/<key>"))?;
                    show(
                        engine.get(&format!("/v1/cli/secret?addr={}", urlencode(addr)))?,
                        json_out,
                        render_secret,
                    )
                }
                "list" => {
                    let node = rest
                        .get(1)
                        .ok_or_else(|| usage("secret list needs <vault>"))?;
                    show(
                        engine.get(&format!("/v1/cli/secret/keys?node={}", urlencode(node)))?,
                        json_out,
                        render_keys,
                    )
                }
                // `set` is deliberately absent: vaults are read-only to
                // agents (§3e), and an agent that could write one could
                // rewrite the credential another agent runs as.
                "set" => {
                    eprintln!(
                        "wheel: agents cannot write vaults; ask the operator to set it in the UI"
                    );
                    Ok(1)
                }
                _ => {
                    eprintln!("wheel: secret needs `get <vault>/<key>` or `list <vault>`\n");
                    Ok(1)
                }
            }
        }

        "rm" => {
            let addr = rest.first().ok_or_else(|| usage("rm needs <node>/<row>"))?;
            show(
                engine.post("/v1/cli/rm", serde_json::json!({ "addr": addr }))?,
                json_out,
                render_ok,
            )
        }

        "query" => {
            let table = rest.first().ok_or_else(|| usage("query needs <table>"))?;
            let sql = read_value(&rest[1..])?;
            show(
                engine.post(
                    "/v1/cli/query",
                    serde_json::json!({ "table": table, "sql": sql }),
                )?,
                json_out,
                render_rows,
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

/// A secret prints RAW, with no decoration and no trailing context: the whole
/// point is `TOKEN=$(wheel secret get v/KEY)`, and a friendly prefix would end
/// up inside the credential.
/// Prints the bare value and nothing else, so `$(wheel secret get v/K)` is
/// the secret rather than the secret plus a label.
fn render_secret(v: &serde_json::Value) {
    println!(
        "{}",
        v.get("value").and_then(|x| x.as_str()).unwrap_or_default()
    );
}

/// Names only. A vault never lists its values, here or anywhere.
fn render_keys(v: &serde_json::Value) {
    let keys: Vec<&str> = v
        .get("keys")
        .and_then(|k| k.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
        .unwrap_or_default();
    if keys.is_empty() {
        println!("no keys");
    } else {
        println!("{}", keys.join("\n"));
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

/// Query results print one JSON object per line, so `wheel query ... | jq` and
/// a human reading the terminal both get something usable without `--json`.
fn render_rows(v: &serde_json::Value) {
    let rows = v.get("rows").and_then(|r| r.as_array());
    match rows {
        Some(rows) if rows.is_empty() => println!("no rows"),
        Some(rows) => {
            for row in rows {
                println!("{row}");
            }
            let n = rows.len();
            eprintln!("{n} row{}", if n == 1 { "" } else { "s" });
        }
        None => println!("{v}"),
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

/// Serialises the tests that mutate process-wide environment. Without it they
/// race: one clears the token file variable while the other is relying on it,
/// and the failure looks like a bug in the code under test.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn reply(status: u16, body: serde_json::Value) -> Reply {
        Reply { status, body }
    }

    fn err_reply(status: u16, code: &str) -> Reply {
        reply(
            status,
            serde_json::json!({"error": {"code": code, "message": "nope"}}),
        )
    }

    /// An unrecognised code must not become 0. Exit 0 means "it worked", and a
    /// caller that trusts it would carry on from a failure.
    #[test]
    fn an_unknown_error_code_is_still_a_failure() {
        assert_eq!(
            show(err_reply(418, "something_new"), true, render_ok).unwrap(),
            2
        );
        // ...including an error body with no code at all.
        assert_eq!(
            show(reply(500, serde_json::json!({})), true, render_ok).unwrap(),
            2
        );
    }

    /// A renderer runs on whatever the engine sent. If one panics on a body it
    /// did not expect, a successful call turns into a crash — and the work the
    /// engine already did is reported to the user as a failure.
    #[test]
    fn no_renderer_panics_on_an_unexpected_body() {
        let bodies = [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!("a bare string"),
            serde_json::json!(42),
            serde_json::json!({"nodes": "not an array"}),
            serde_json::json!({"keys": [1, 2, 3]}),
            serde_json::json!({"messages": [{"id": null}]}),
            serde_json::json!({"wires": [{"to": {}}]}),
        ];
        type Renderer = fn(&serde_json::Value);
        let renderers: [(&str, Renderer); 10] = [
            ("whoami", render_whoami),
            ("connections", render_connections),
            ("list", render_list),
            ("ls", render_ls),
            ("read", render_read),
            ("ok", render_ok),
            ("receipt", render_receipt),
            ("inbox", render_inbox),
            ("secret", render_secret),
            ("keys", render_keys),
        ];
        for (name, r) in renderers {
            for b in &bodies {
                // The assertion is simply that this returns at all.
                r(b);
                let _ = name;
            }
        }
    }

    /// §3c#1. The warning is on by default because the defect is silent: the
    /// shell substitutes before wheel runs, so nothing downstream can tell
    /// that what arrived is not what was typed.
    #[test]
    fn a_shell_substituted_value_is_still_returned_verbatim() {
        // What the shell left behind is what we send — we warn, we do not
        // alter it. Rewriting the body would be a second corruption.
        let got = read_value(&s(&["result", "is", "empty"])).unwrap();
        assert_eq!(got, "result is empty");
    }

    #[test]
    fn stdin_and_file_are_not_confused_with_a_literal_value() {
        // A body that merely CONTAINS the word --file is not a flag: only the
        // first argument selects the source.
        let got = read_value(&s(&["please", "--file", "that"])).unwrap();
        assert_eq!(got, "please --file that");
    }

    #[test]
    fn a_missing_file_is_an_error_rather_than_an_empty_body() {
        assert!(read_value(&s(&["--file", "/definitely/not/here"])).is_err());
    }

    /// Every byte a query string cannot carry must be escaped, or an address
    /// with a slash in it silently addresses something else.
    #[test]
    fn urlencoding_covers_the_bytes_that_actually_break_addresses() {
        assert_eq!(urlencode("t/row"), "t%2Frow");
        assert_eq!(urlencode("a?b#c"), "a%3Fb%23c");
        assert_eq!(urlencode("a+b"), "a%2Bb");
        assert_eq!(urlencode(""), "");
        // Unreserved characters must pass through untouched, or every address
        // becomes unreadable in a log.
        assert_eq!(urlencode("A-Z_a.z~0"), "A-Z_a.z~0");
        // Multi-byte utf-8 is percent-encoded per BYTE.
        assert_eq!(urlencode("é"), "%C3%A9");
    }

    /// The grammar-to-route mapping is a documented contract (PROTOCOL.md) and
    /// nothing else checks it. A command wired to the wrong path still returns
    /// 200 and still prints something plausible — it just asks the engine a
    /// different question than the operator did.
    #[test]
    fn commands_address_the_routes_they_are_documented_to() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!("wheel-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("engine.sock");
        let token = dir.join("token");
        std::fs::write(&token, "t").unwrap();
        let _ = std::fs::remove_file(&sock);

        let cases: Vec<(Vec<&str>, &str, &str)> = vec![
            (vec!["whoami"], "GET", "/v1/cli/whoami"),
            (vec!["connections"], "GET", "/v1/cli/connections"),
            (vec!["list"], "GET", "/v1/cli/list"),
            (vec!["read", "notes"], "GET", "/v1/cli/read?addr=notes"),
            (vec!["ls", "table"], "GET", "/v1/cli/ls?node=table"),
            (
                vec!["ls", "table", "2026-"],
                "GET",
                "/v1/cli/ls?node=table&prefix=2026-",
            ),
            (vec!["rm", "notes/r1"], "POST", "/v1/cli/rm"),
            (vec!["query", "notes", "SELECT 1"], "POST", "/v1/cli/query"),
            (vec!["inbox"], "GET", "/v1/cli/inbox"),
            (
                vec!["secret", "get", "v/K"],
                "GET",
                "/v1/cli/secret?addr=v%2FK",
            ),
            (
                vec!["secret", "list", "v"],
                "GET",
                "/v1/cli/secret/keys?node=v",
            ),
            (vec!["msg", "peer", "hello"], "POST", "/v1/cli/msg"),
            (vec!["write", "notes", "body"], "POST", "/v1/cli/write"),
        ];

        let listener = UnixListener::bind(&sock).unwrap();
        let n = cases.len();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..n {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let read = stream.read(&mut buf).unwrap_or(0);
                let text = String::from_utf8_lossy(&buf[..read]).to_string();
                seen.push(text.lines().next().unwrap_or_default().to_string());
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n{}");
                let _ = stream.flush();
            }
            seen
        });

        std::env::set_var(
            wheel_core::spawn::ENV_ENGINE_URL,
            format!("unix://{}", sock.display()),
        );
        std::env::set_var(wheel_core::spawn::ENV_TOKEN_FILE, &token);
        std::env::remove_var(wheel_core::spawn::ENV_TOKEN);

        for (argv, _, _) in &cases {
            let args: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
            // --json so the renderers do not spray the test output.
            assert_eq!(run(&args, true).unwrap(), 0, "{argv:?} should succeed");
        }

        let seen = server.join().unwrap();
        for (i, (argv, method, path)) in cases.iter().enumerate() {
            assert_eq!(
                seen[i],
                format!("{method} {path} HTTP/1.1"),
                "{argv:?} addressed the wrong route"
            );
        }

        // Usage errors must be caught BEFORE a request is made, or a
        // half-typed command reaches the engine as a differently-shaped one.
        // The fake server above accepts exactly `n` connections, so anything
        // that tried to connect here would hang rather than pass.
        for bad in [
            vec!["read"],
            vec!["ls"],
            vec!["write"],
            vec!["write", "notes"],
            vec!["msg"],
            vec!["msg", "peer"],
            vec!["secret"],
            vec!["secret", "get"],
            vec!["secret", "list"],
            vec!["rm"],
            vec!["query"],
            vec!["not-a-command"],
        ] {
            let args: Vec<String> = bad.iter().map(|a| a.to_string()).collect();
            let r = run(&args, true);
            assert!(
                r.is_err() || r.as_ref().unwrap() != &0u8,
                "{bad:?} must not report success"
            );
        }

        std::env::remove_var(wheel_core::spawn::ENV_ENGINE_URL);
        std::env::remove_var(wheel_core::spawn::ENV_TOKEN_FILE);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `--help` must work with no token at all, or an agent that cannot
    /// authenticate has no way to find out why.
    #[test]
    fn help_needs_no_token() {
        for h in ["--help", "-h", "help"] {
            assert_eq!(run(&[h.to_string()], false).unwrap(), 0);
        }
        // No arguments prints usage and fails, rather than succeeding silently.
        assert_eq!(run(&[], false).unwrap(), 1);
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
