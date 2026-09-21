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
use policy::{Admission, Policy, Reply};
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

/// The path a caller can ask for to learn it is talking to this proxy and not to a daemon. A real
/// daemon answers 404 here, which is how the sandbox host tells the two apart.
pub const IDENTITY_PATH: &str = "/_wheel_proxy";
pub const IDENTITY_BODY: &str = r#"{"wheel-docker-proxy":1}"#;

async fn handle(State(proxy): State<Proxy>, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let method = parts.method.as_str().to_string();
    let target = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_default();

    if method == "GET" && target == IDENTITY_PATH {
        return Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(IDENTITY_BODY))
            .expect("static response");
    }
    // Nothing that turns this connection into something else, or asks for a reply the proxy did not
    // plan for.
    for h in [header::UPGRADE, header::EXPECT, header::TRANSFER_ENCODING] {
        if parts.headers.contains_key(&h) {
            return docker_error(
                StatusCode::FORBIDDEN,
                "docker proxy: refused (unsupported header)",
            );
        }
    }

    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return docker_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "docker proxy: body too large",
            )
        }
    };

    let admission = match proxy.policy.decide(&method, &target, &body) {
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

    match forward(&proxy.upstream, admission).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(%method, %target, error = ?e, "docker proxy could not reach the daemon");
            docker_error(StatusCode::BAD_GATEWAY, "docker proxy: daemon unreachable")
        }
    }
}

/// Send what was admitted — rebuilt, never the caller's own bytes — and reduce the answer.
async fn forward(upstream: &std::path::Path, admission: Admission) -> Result<Response> {
    let stream = tokio::net::UnixStream::connect(upstream)
        .await
        .context("connecting to the docker socket")?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("docker handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let has_body = admission.body.is_some();
    let mut builder = hyper::Request::builder()
        .method(admission.method)
        .uri(admission.target.as_str())
        .header(header::HOST, "docker");
    if has_body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(http_body_util::Full::new(bytes::Bytes::from(
            admission.body.unwrap_or_default(),
        )))
        .context("building the docker request")?;

    let upstream = sender
        .send_request(request)
        .await
        .context("docker request")?;
    let (parts, incoming) = upstream.into_parts();
    let raw = incoming
        .collect()
        .await
        .context("docker response")?
        .to_bytes();

    let status = parts.status;
    let body: Vec<u8> = if !status.is_success() {
        // A daemon's error text can echo what it was sent. A missing thing keeps its "No such …"
        // wording, because the host tells a missing image from a daemon fault by it.
        let text = std::str::from_utf8(&raw)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
            .and_then(|v| {
                v.get("message")
                    .and_then(|m| m.as_str().map(str::to_string))
            })
            .filter(|m| {
                status == StatusCode::NOT_FOUND && m.starts_with("No such ") && m.len() <= 200
            });
        let message = text.unwrap_or_else(|| policy::fixed_error(status.as_u16()).to_string());
        serde_json::json!({ "message": message })
            .to_string()
            .into_bytes()
    } else {
        match admission.reply {
            Reply::Fixed => Vec::new(),
            Reply::Inspect => policy::project_inspect(&raw).context("inspection had no State")?,
            Reply::List => policy::project_list(&raw).context("container list was not a list")?,
            Reply::Create => policy::project_create(&raw).context("create had no Id")?,
            Reply::Volume => policy::project_volume(&raw).context("volume create had no Name")?,
        }
    };

    let mut out = Response::builder().status(status);
    if !body.is_empty() {
        out = out.header(header::CONTENT_TYPE, "application/json");
    }
    out.body(Body::from(body)).context("building the response")
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
    // Created private from the first instant; the mode is then set explicitly below.
    // SAFETY: `umask` only swaps this process's file-creation mask.
    let previous = unsafe { libc::umask(0o177) };
    let bound = tokio::net::UnixListener::bind(path);
    unsafe { libc::umask(previous) };
    let listener = bound.with_context(|| format!("binding {}", path.display()))?;
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
