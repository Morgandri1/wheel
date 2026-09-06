//! Reaching the engine, over TCP or a unix socket.
//!
//! `WHEEL_ENGINE_URL` is `http://…` in docker mode and `unix://…` in process
//! mode, so the CLI has to speak both. The token comes from a FILE
//! (`WHEEL_TOKEN_FILE`), never the environment: `/proc/<pid>/environ` is
//! readable by the same uid, so an env token would hand every co-resident
//! child every sibling's authority (ADVERSARY F007).

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

use anyhow::{bail, Context, Result};

pub struct Engine {
    target: Target,
    token: String,
}

enum Target {
    Http(String),
    Unix(PathBuf),
}

/// A response, reduced to what the CLI needs to decide an exit code.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: serde_json::Value,
}

impl Engine {
    pub fn from_env() -> Result<Self> {
        let url = std::env::var(wheel_core::spawn::ENV_ENGINE_URL)
            .unwrap_or_else(|_| format!("http://127.0.0.1:{}", wheel_core::ENGINE_PORT));

        let target = if let Some(path) = url.strip_prefix("unix://") {
            Target::Unix(PathBuf::from(path))
        } else {
            Target::Http(url.trim_end_matches('/').to_string())
        };

        Ok(Self {
            target,
            token: read_token()?,
        })
    }

    pub fn get(&self, path: &str) -> Result<Reply> {
        self.request("GET", path, None)
    }

    pub fn post(&self, path: &str, body: serde_json::Value) -> Result<Reply> {
        self.request("POST", path, Some(body))
    }

    fn request(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Result<Reply> {
        match &self.target {
            Target::Http(base) => self.http(base, method, path, body),
            Target::Unix(sock) => self.unix(sock, method, path, body),
        }
    }

    fn http(
        &self,
        base: &str,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Reply> {
        let req = ureq::request(method, &format!("{base}{path}"))
            .set("authorization", &format!("Bearer {}", self.token));
        let resp = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        // A 4xx is an ANSWER, not a transport failure: the CLI turns it into an
        // exit code, so it must not be flattened into an error here.
        match resp {
            Ok(r) => Ok(Reply {
                status: r.status(),
                body: r.into_json().unwrap_or(serde_json::Value::Null),
            }),
            Err(ureq::Error::Status(status, r)) => Ok(Reply {
                status,
                body: r.into_json().unwrap_or(serde_json::Value::Null),
            }),
            Err(e) => Err(e).context("reaching the engine"),
        }
    }

    /// Minimal HTTP/1.1 over a unix socket.
    ///
    /// Hand-rolled because `ureq` cannot speak to a unix socket, and pulling in
    /// an async stack for `Connection: close` request/response would cost more
    /// than it saves in a binary an agent invokes constantly.
    fn unix(
        &self,
        sock: &PathBuf,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Reply> {
        let mut stream = UnixStream::connect(sock)
            .with_context(|| format!("connecting to the engine socket {}", sock.display()))?;

        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: engine\r\nConnection: close\r\n\
             Authorization: Bearer {}\r\n",
            self.token
        );
        if !payload.is_empty() {
            req.push_str("Content-Type: application/json\r\n");
            req.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
        req.push_str("\r\n");
        req.push_str(&payload);

        stream.write_all(req.as_bytes())?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line)?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .with_context(|| format!("unparseable status line {status_line:?}"))?;

        // Skip headers; Connection: close means the body runs to EOF, so no
        // chunked decoding is needed.
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
                break;
            }
        }
        let mut raw = String::new();
        reader.read_to_string(&mut raw)?;

        Ok(Reply {
            status,
            body: serde_json::from_str(raw.trim()).unwrap_or(serde_json::Value::Null),
        })
    }
}

/// Read the node token from its 0600 file.
///
/// If the legacy env var is set instead, say so loudly rather than falling back
/// to it: silently accepting an env token would undo F007 the first time
/// something set it.
fn read_token() -> Result<String> {
    if let Ok(path) = std::env::var(wheel_core::spawn::ENV_TOKEN_FILE) {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading the node token from {path}"))?;
        return Ok(raw.trim().to_string());
    }
    if std::env::var(wheel_core::spawn::ENV_TOKEN).is_ok() {
        bail!(
            "{} is set, but the token must be passed as a file via {}. \
             An environment variable is readable through /proc by any process \
             of the same uid, which would share this node's authority with \
             every sibling.",
            wheel_core::spawn::ENV_TOKEN,
            wheel_core::spawn::ENV_TOKEN_FILE
        );
    }
    bail!(
        "no node token: {} is not set. Are you running inside an agent or a script?",
        wheel_core::spawn::ENV_TOKEN_FILE
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn engine_on(sock: PathBuf) -> Engine {
        Engine {
            target: Target::Unix(sock),
            token: "test-token".into(),
        }
    }

    /// Serve one canned HTTP response and hand back what the client sent.
    fn serve_once(sock: &PathBuf, response: &'static str) -> std::thread::JoinHandle<String> {
        let listener = UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut got = String::new();
            // Read only what is buffered: the client half-closes nothing, so
            // reading to EOF here would deadlock against its own read.
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().ok();
            got
        })
    }

    fn tmp_sock(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "wheel-transport-{name}-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_unix_round_trip_parses_status_and_body() {
        let sock = tmp_sock("ok");
        let server = serve_once(
            &sock,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"node\":\"notes\"}",
        );
        let reply = engine_on(sock.clone()).get("/v1/cli/whoami").unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body["node"], "notes");

        let sent = server.join().unwrap();
        assert!(sent.starts_with("GET /v1/cli/whoami HTTP/1.1"), "{sent:?}");
        // The token travels as a bearer header, and `Connection: close` is what
        // lets the body be read to EOF without chunked decoding.
        assert!(
            sent.contains("Authorization: Bearer test-token"),
            "{sent:?}"
        );
        assert!(sent.contains("Connection: close"), "{sent:?}");
        std::fs::remove_file(&sock).ok();
    }

    /// A 4xx is an ANSWER — the CLI turns it into an exit code. Flattening it
    /// into a transport error would lose the engine's reason and the exit code
    /// with it, so a wire denial would surface as "could not reach the engine".
    #[test]
    fn an_error_status_comes_back_as_a_reply_not_an_error() {
        let sock = tmp_sock("denied");
        let _server = serve_once(
            &sock,
            "HTTP/1.1 403 Forbidden\r\n\r\n{\"error\":{\"code\":\"wire_denied\",\"message\":\"no wire\"}}",
        );
        let reply = engine_on(sock.clone()).get("/v1/cli/read").unwrap();
        assert_eq!(reply.status, 403);
        assert_eq!(reply.body["error"]["code"], "wire_denied");
        std::fs::remove_file(&sock).ok();
    }

    #[test]
    fn a_post_sends_a_content_length_and_the_json_body() {
        let sock = tmp_sock("post");
        let server = serve_once(&sock, "HTTP/1.1 200 OK\r\n\r\n{\"ok\":true}");
        let reply = engine_on(sock.clone())
            .post("/v1/cli/msg", serde_json::json!({"to": "peer"}))
            .unwrap();
        assert_eq!(reply.status, 200);

        let sent = server.join().unwrap();
        let payload = r#"{"to":"peer"}"#;
        assert!(
            sent.contains(&format!("Content-Length: {}", payload.len())),
            "{sent:?}"
        );
        assert!(
            sent.ends_with(payload),
            "body must be sent verbatim: {sent:?}"
        );
        std::fs::remove_file(&sock).ok();
    }

    /// An empty or non-JSON body is null, not a crash: the engine answers 204
    /// with no body on some routes, and a parse panic there would turn a
    /// successful call into a failure.
    #[test]
    fn a_body_that_is_not_json_becomes_null_rather_than_panicking() {
        let sock = tmp_sock("empty");
        let _server = serve_once(&sock, "HTTP/1.1 204 No Content\r\n\r\n");
        let reply = engine_on(sock.clone()).get("/v1/cli/ls").unwrap();
        assert_eq!(reply.status, 204);
        assert_eq!(reply.body, serde_json::Value::Null);
        std::fs::remove_file(&sock).ok();
    }

    #[test]
    fn an_unparseable_status_line_is_an_error_not_a_guess() {
        let sock = tmp_sock("garbage");
        let _server = serve_once(&sock, "not http at all\r\n\r\n");
        assert!(engine_on(sock.clone()).get("/v1/cli/ls").is_err());
        std::fs::remove_file(&sock).ok();
    }

    #[test]
    fn a_missing_socket_says_which_socket() {
        let err = engine_on(PathBuf::from("/definitely/not/a/socket"))
            .get("/v1/cli/whoami")
            .unwrap_err()
            .to_string();
        assert!(err.contains("/definitely/not/a/socket"), "{err}");
    }

    /// ADVERSARY F007. The token must come from a 0600 FILE, because
    /// `/proc/<pid>/environ` is readable by any process of the same uid — an
    /// env token would hand every co-resident child every sibling's authority.
    ///
    /// All three branches in one test on purpose: they mutate process-wide
    /// environment, and running them in parallel would let one clear what
    /// another just set.
    #[test]
    fn the_token_comes_from_a_file_and_an_env_token_is_refused_loudly() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let file_var = wheel_core::spawn::ENV_TOKEN_FILE;
        let env_var = wheel_core::spawn::ENV_TOKEN;
        let saved = (std::env::var(file_var).ok(), std::env::var(env_var).ok());

        std::env::remove_var(file_var);
        std::env::remove_var(env_var);
        let err = read_token().unwrap_err().to_string();
        assert!(err.contains("no node token"), "{err}");

        // The legacy env var must be REFUSED, not quietly accepted: falling
        // back to it would undo F007 the first time something set it.
        std::env::set_var(env_var, "sekrit");
        let err = read_token().unwrap_err().to_string();
        assert!(err.contains("/proc"), "the refusal must say why: {err}");
        assert!(
            !err.contains("sekrit"),
            "the refusal must not echo the token"
        );

        // A file wins, and trailing whitespace from an editor is not part of it.
        let dir = std::env::temp_dir().join(format!("wheel-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token");
        std::fs::write(&path, "  abc123\n").unwrap();
        std::env::set_var(file_var, &path);
        assert_eq!(read_token().unwrap(), "abc123");

        // A named but unreadable file is an error naming the path, not a
        // silent fallback to the env var that is still set.
        std::env::set_var(file_var, "/definitely/not/a/token");
        let err = read_token().unwrap_err().to_string();
        assert!(err.contains("/definitely/not/a/token"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
        match saved.0 {
            Some(v) => std::env::set_var(file_var, v),
            None => std::env::remove_var(file_var),
        }
        match saved.1 {
            Some(v) => std::env::set_var(env_var, v),
            None => std::env::remove_var(env_var),
        }
    }
}

/// The HTTP transport is what every agent uses in docker mode (§2), and it was
/// the one path with no test at all: the unix-socket tests cover process mode
/// only. A bug here is invisible until an agent on a real board cannot talk to
/// its engine.
#[cfg(test)]
mod http_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serve one canned response with a CORRECT Content-Length, and hand back
    /// the request bytes we received. Counting the length by hand in each
    /// fixture is how you get a test that fails for its own reasons.
    fn serve_json(status: &str, body: &'static str) -> (u16, std::thread::JoinHandle<String>) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        serve_raw(response)
    }

    fn serve_raw(response: String) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read until the headers are complete AND the declared body has
            // arrived. A single read() returns whatever one write produced,
            // and a client is free to send the body separately -- which ureq
            // does, so a one-shot read sees headers and no body.
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got);
                let Some(headers_end) = text.find("\r\n\r\n") else {
                    continue;
                };
                let want: usize = text[..headers_end]
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if got.len() >= headers_end + 4 + want {
                    break;
                }
            }
            stream.write_all(response.as_bytes()).ok();
            stream.flush().ok();
            String::from_utf8_lossy(&got).into_owned()
        });
        (port, handle)
    }

    fn engine_on(port: u16) -> Engine {
        Engine {
            target: Target::Http(format!("http://127.0.0.1:{port}")),
            token: "test-token".into(),
        }
    }

    #[test]
    fn a_get_carries_the_bearer_and_returns_the_body() {
        let (port, server) = serve_json("200 OK", "{\"name\":\"worker\"}");
        let reply = engine_on(port).get("/v1/cli/whoami").unwrap();
        let sent = server.join().unwrap();

        assert!(sent.starts_with("GET /v1/cli/whoami HTTP/1.1"), "{sent}");
        assert!(
            sent.to_lowercase()
                .contains("authorization: bearer test-token"),
            "the node token must prove identity on every call: {sent}"
        );
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body["name"], "worker");
    }

    #[test]
    fn a_post_sends_json_and_says_so() {
        let (port, server) = serve_json("200 OK", "{\"ok\":true}");
        let reply = engine_on(port)
            .post("/v1/cli/msg", serde_json::json!({"to": "pm", "body": "hi"}))
            .unwrap();
        let sent = server.join().unwrap();

        assert!(sent.starts_with("POST /v1/cli/msg HTTP/1.1"), "{sent}");
        assert!(
            sent.to_lowercase()
                .contains("content-type: application/json"),
            "{sent}"
        );
        assert!(
            sent.contains("\"to\":\"pm\""),
            "the body must arrive: {sent}"
        );
        assert_eq!(reply.body["ok"], true);
    }

    /// A wire denial is an ANSWER the CLI turns into exit 3. Flattening it into
    /// a transport error would lose the engine's reason and report a network
    /// fault for a permissions decision.
    #[test]
    fn a_denial_comes_back_as_a_reply_not_an_error() {
        let (port, server) = serve_json(
            "403 Forbidden",
            "{\"error\":{\"code\":\"wire_denied\",\"message\":\"no wire from a to b (need: write)\"}}",
        );
        let reply = engine_on(port).get("/v1/cli/read?addr=b").unwrap();
        let _ = server.join();

        assert_eq!(reply.status, 403);
        assert_eq!(reply.body["error"]["code"], "wire_denied");
        assert!(reply.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("need: write"));
    }

    /// A 500 with no body must not panic the CLI: the agent still needs its
    /// exit code.
    #[test]
    fn an_empty_or_unparseable_body_is_null_rather_than_a_panic() {
        let (port, server) = serve_raw(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_string(),
        );
        let reply = engine_on(port).get("/v1/cli/whoami").unwrap();
        let _ = server.join();
        assert_eq!(reply.status, 500);
        assert_eq!(reply.body, serde_json::Value::Null);
    }

    #[test]
    fn an_engine_that_is_not_listening_is_an_error_with_context() {
        // Bind and drop, so the port is almost certainly closed.
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let err = engine_on(port).get("/v1/cli/whoami").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("reaching the engine"),
            "the operator needs to know WHICH hop failed: {msg}"
        );
    }
}
