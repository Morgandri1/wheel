// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! A filtering proxy in front of the docker socket.
//!
//! The sandbox host needs a docker daemon to give each project its own container, and a docker
//! socket is root on the machine. This is the only process that holds the real one; the host talks
//! to this socket instead, and [`policy`] decides — per request, on method, path, query and body —
//! whether the daemon ever hears it. Everything is refused unless it is one of the seven calls the
//! host makes, on names derived from a project uuid, with a create body that is exactly the hardened
//! container the host asks for.
//!
//! An endpoint-only filter would not do: `POST /containers/create` has to stay open, and a create
//! with `Privileged` or a bind of `/` is the escape. So the body is checked too.

pub mod policy;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use policy::{Admitted, Policy};
use std::path::PathBuf;
use std::sync::Arc;

/// A create body is a few hundred bytes; this only bounds what a compromised caller can make the
/// proxy buffer.
const MAX_BODY: usize = 64 * 1024;

#[derive(Clone)]
pub struct Proxy {
    policy: Arc<Policy>,
    upstream: Arc<PathBuf>,
}

impl Proxy {
    pub fn new(policy: Policy, upstream: PathBuf) -> Self {
        Self {
            policy: Arc::new(policy),
            upstream: Arc::new(upstream),
        }
    }

    pub fn router(self) -> axum::Router {
        axum::Router::new().fallback(handle).with_state(self)
    }
}

fn docker_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "message": message }).to_string();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("static response")
}

async fn handle(State(proxy): State<Proxy>, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let target = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_default();

    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return docker_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "docker proxy: body too large",
            )
        }
    };

    let admitted = match proxy.policy.decide(&method, &target, &body) {
        Ok(a) => a,
        Err(policy::Denied(why)) => {
            // Method and target only: a create body carries the engine secret.
            tracing::warn!(%method, %target, %why, "docker proxy refused a request");
            return docker_error(
                StatusCode::FORBIDDEN,
                &format!("docker proxy: refused ({why})"),
            );
        }
    };

    match forward(&proxy.upstream, &method, &target, body, admitted).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(%method, %target, error = ?e, "docker proxy could not reach the daemon");
            docker_error(StatusCode::BAD_GATEWAY, "docker proxy: daemon unreachable")
        }
    }
}

async fn forward(
    upstream: &PathBuf,
    method: &str,
    target: &str,
    body: bytes::Bytes,
    admitted: Admitted,
) -> Result<Response> {
    let stream = tokio::net::UnixStream::connect(upstream.as_path())
        .await
        .context("connecting to the docker socket")?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("docker handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    // Only what docker needs. Nothing the caller sent is passed through unread: no `Upgrade`, no
    // `Connection`, no hijacking a stream.
    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(target)
        .header(header::HOST, "docker");
    if !body.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(http_body_util::Full::new(body))
        .context("building the docker request")?;

    let upstream = sender
        .send_request(request)
        .await
        .context("docker request")?;
    let (parts, incoming) = upstream.into_parts();
    let bytes = incoming
        .collect()
        .await
        .context("docker response")?
        .to_bytes();

    let bytes = match admitted {
        Admitted::Plain => bytes,
        Admitted::StripEnv if parts.status.is_success() => strip_env(&bytes)?,
        Admitted::StripEnv => bytes,
    };

    let mut out = Response::builder().status(parts.status);
    if let Some(ct) = parts.headers.get(header::CONTENT_TYPE) {
        out = out.header(header::CONTENT_TYPE, ct);
    }
    Ok(out
        .body(Body::from(bytes))
        .context("building the response")?)
}

/// Remove `Config.Env` from a container inspection. Parsing failure is an error, not a pass
/// through: an inspection that cannot be read cannot be shown to be free of the secret.
fn strip_env(bytes: &[u8]) -> Result<bytes::Bytes> {
    let mut v: serde_json::Value =
        serde_json::from_slice(bytes).context("container inspection was not JSON")?;
    if let Some(config) = v.get_mut("Config").and_then(|c| c.as_object_mut()) {
        config.remove("Env");
    }
    Ok(bytes::Bytes::from(serde_json::to_vec(&v)?))
}

/// Bind `path` and serve until the process ends. The socket's mode is set explicitly after bind
/// rather than inherited from a umask.
pub async fn serve(
    proxy: Proxy,
    path: &std::path::Path,
    mode: u32,
    owner: Option<(u32, u32)>,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)
        .with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .context("setting the socket mode")?;
    if let Some((uid, gid)) = owner {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
        // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
        if unsafe { libc::chown(c.as_ptr(), uid, gid) } != 0 {
            return Err(std::io::Error::last_os_error()).context("chown of the proxy socket");
        }
    }
    axum::serve(listener, proxy.router())
        .await
        .context("serving the docker proxy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_loses_its_environment_and_keeps_its_state() {
        let out = strip_env(
            br#"{"State":{"Status":"running"},"Config":{"Env":["WHEEL_ENGINE_SECRET=s"],"Image":"i"}}"#,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["State"]["Status"], "running");
        assert_eq!(v["Config"]["Image"], "i");
        assert!(v["Config"].get("Env").is_none(), "{v}");
        assert!(!String::from_utf8_lossy(&out).contains("WHEEL_ENGINE_SECRET"));
    }

    #[test]
    fn an_inspection_that_is_not_json_is_an_error_not_a_pass_through() {
        assert!(strip_env(b"WHEEL_ENGINE_SECRET=leak").is_err());
    }
}
