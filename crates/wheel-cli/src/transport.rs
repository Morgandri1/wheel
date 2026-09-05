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
