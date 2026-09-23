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
    /// Held across "count the daemon's projects, then admit or refuse a create" so concurrent
    /// creates cannot each see the same pre-creation count and all pass the ceiling together.
    create_gate: Arc<tokio::sync::Mutex<()>>,
}

impl Proxy {
    pub fn new(policy: Policy, upstream: PathBuf) -> Self {
        Self {
            policy: Arc::new(policy),
            upstream: Arc::new(upstream),
            create_gate: Arc::new(tokio::sync::Mutex::new(())),
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

    // The ceiling only ever applies to a CREATE: nothing else grows the count. Every create is
    // serialised through `create_gate` — count-then-admit is two round trips to the daemon, and
    // without a lock held across both, concurrent creates each see the pre-creation count and the
    // ceiling stops bounding anything (adversary, #163: 10 concurrent creates against a ceiling of
    // 1 admitted more than one). The lock only ever blocks another create, never inspect/start/
    // stop/list/etc., so it costs nothing outside the path it exists to serialise.
    let is_create = matches!(admission.reply, Reply::Create | Reply::Volume);
    let _create_permit = if is_create {
        Some(proxy.create_gate.lock().await)
    } else {
        None
    };
    if is_create && proxy.policy.max_projects > 0 {
        // Refusing a create when the daemon cannot even be asked how many projects exist would
        // turn "the daemon is briefly slow" into "no new project can ever start" — a stronger
        // failure than the ceiling is meant to cause. So an uncountable daemon is treated as
        // "under the ceiling": a real, enforced-elsewhere limit (the per-container caps, the disk
        // floor) is what actually bounds a daemon this proxy cannot even query.
        if let Ok(count) = count_projects(&proxy.upstream).await {
            if count >= proxy.policy.max_projects {
                tracing::warn!(
                    count,
                    max = proxy.policy.max_projects,
                    "docker proxy refused a create: at the project ceiling"
                );
                return docker_error(
                    StatusCode::FORBIDDEN,
                    "docker proxy: refused (at the configured project ceiling)",
                );
            }
        }
    }

    match forward(&proxy.upstream, admission).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(%method, %target, error = ?e, "docker proxy could not reach the daemon");
            docker_error(StatusCode::BAD_GATEWAY, "docker proxy: daemon unreachable")
        }
    }
}

/// How many project containers the daemon already holds. Used only to enforce the ceiling; a
/// daemon that cannot answer this is not itself refused (see the caller).
async fn count_projects(upstream: &std::path::Path) -> Result<usize> {
    // The UNION of container and volume project labels, not their sum: the steady state is one
    // container plus its one matching volume, which must count as one project, not two. What the
    // ceiling exists to catch is a project id present on ONE side without the other (an orphan) or
    // present on neither yet (a brand new one) — counting labels, not objects, gets both right.
    // Both lists have to be asked for: the host creates a project's volume before its container,
    // so counting containers alone leaves `POST /volumes/create` completely uncapped (adversary,
    // #163: 50 volume creates admitted in a row against a ceiling of 2).
    let containers = labelled_project_ids(upstream, policy::list_admission(), "Names").await?;
    let volumes = labelled_project_ids(upstream, policy::volume_list_admission(), "Name").await?;
    Ok(containers.union(&volumes).count())
}

/// The `wheel.project` label of every entry in a reduced list response. `name_field` is `"Names"`
/// for the container list (an array of one name) or `"Name"` for the volume list (a bare string);
/// only used to confirm the entry has a project name at all, since the label is what is counted.
async fn labelled_project_ids(
    upstream: &std::path::Path,
    admission: Admission,
    name_field: &str,
) -> Result<std::collections::HashSet<String>> {
    let resp = forward(upstream, admission).await?;
    if resp.status() != StatusCode::OK {
        anyhow::bail!("listing project objects answered {}", resp.status());
    }
    let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .context("reading the project list")?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).context("project list was not JSON")?;
    // The container list's reduced shape is a bare array; the volume list's is `{"Volumes": [...]}`
    // (matching Docker's own real API, which a real bollard client elsewhere depends on) —
    // whichever this is, `items` ends up the array either way.
    let items: Vec<serde_json::Value> = match parsed {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(mut obj) => match obj.remove("Volumes") {
            Some(serde_json::Value::Array(items)) => items,
            _ => anyhow::bail!("project list had neither a bare array nor a Volumes array"),
        },
        _ => anyhow::bail!("project list was not a JSON array or object"),
    };
    Ok(items
        .into_iter()
        .filter(|item| item.get(name_field).is_some())
        .filter_map(|item| {
            item.pointer("/Labels/wheel.project")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect())
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
            Reply::VolumeList => {
                policy::project_volume_list(&raw).context("volume list was not a list")?
            }
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
    // A unix socket path has to fit in `sockaddr_un.sun_path` (~104 bytes). Past that, `bind`
    // fails with an error that says nothing about the real cause — check it here, by name, the
    // same guard `sandbox/process.rs::provision` already has for the engine's own socket.
    let len = path.as_os_str().len();
    anyhow::ensure!(
        len < 100,
        "docker proxy socket path is {len} bytes and must stay under 100 (sockaddr_un limit): {}",
        path.display()
    );
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
