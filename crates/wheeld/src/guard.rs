// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! What a request must look like before the API sees it, and what a bind exposes.
//!
//! Loopback alone does not keep a browser out. A page on a name its author controls can re-point
//! that name at 127.0.0.1 after it loads (DNS rebinding), and from then on its requests to wheeld
//! are same-origin: CORS never applies, and an open signup is a way in. What the page cannot change
//! is the `Host` it sends, which is still its own name. So a request addressed to a name this
//! daemon was not told about is refused before the router sees it.
//!
//! An IP literal needs no allowance: a browser sends one only when the page itself is at that
//! address, which a rebinding page is not. `/p/*` is exempt, because public ingress is public.

use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::IpAddr;
use std::sync::Arc;

pub const ENV_ALLOWED_HOSTS: &str = "WHEEL_ALLOWED_HOSTS";
pub const ENV_SIGNUP: &str = "WHEEL_SIGNUP";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    names: Vec<String>,
    signup_open: bool,
}

impl Guard {
    pub fn from_env(bind: &str) -> anyhow::Result<Self> {
        Self::new(
            bind,
            &std::env::var(ENV_ALLOWED_HOSTS).unwrap_or_default(),
            std::env::var(ENV_SIGNUP).ok().as_deref(),
        )
    }

    pub fn new(bind: &str, allowed: &str, signup: Option<&str>) -> anyhow::Result<Self> {
        let signup_open = match signup.map(str::trim) {
            None | Some("") | Some("open") => true,
            Some("closed") => false,
            Some(other) => {
                anyhow::bail!("{ENV_SIGNUP} must be \"open\" or \"closed\", got {other:?}")
            }
        };
        let mut names: Vec<String> = allowed
            .split(',')
            .map(|name| normalise(host_part(name)))
            .filter(|name| !name.is_empty())
            .collect();
        let bound = normalise(host_part(bind));
        if !bound.is_empty() && bound.parse::<IpAddr>().is_err() {
            names.push(bound);
        }
        Ok(Self { names, signup_open })
    }

    pub fn admits(&self, host: &str) -> bool {
        let host = normalise(host_part(host));
        host.parse::<IpAddr>().is_ok() || is_localhost(&host) || self.names.contains(&host)
    }
}

pub async fn check(State(guard): State<Arc<Guard>>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path.starts_with("/p/") {
        return next.run(req).await;
    }
    let addressed_to = match req.headers().get(header::HOST) {
        Some(value) => value.to_str().ok().map(str::to_owned),
        None => Some(
            req.uri()
                .authority()
                .map(|a| a.to_string())
                .unwrap_or_default(),
        ),
    };
    let admitted = match &addressed_to {
        Some(host) if host.is_empty() => true,
        Some(host) => guard.admits(host),
        None => false,
    };
    if !admitted {
        return refuse(
            StatusCode::FORBIDDEN,
            "forbidden",
            "This request was addressed to a host name wheeld does not answer to. If the name is \
             yours, add it to WHEEL_ALLOWED_HOSTS.",
        );
    }
    if !guard.signup_open && req.method() == Method::POST && path == "/v1/auth/signup" {
        return refuse(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested resource does not exist.",
        );
    }
    next.run(req).await
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

/// The warning a bind deserves, or `None` when only this machine can reach it.
pub fn exposure(bind: &str) -> Option<String> {
    let host = normalise(host_part(bind));
    let reach = match host.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => return None,
        Ok(ip) if ip.is_unspecified() => "every network interface of this machine".to_string(),
        Ok(ip) => format!("the network address {ip}"),
        Err(_) if is_localhost(&host) => return None,
        Err(_) => format!("whatever {host:?} resolves to"),
    };
    Some(format!(
        "wheeld is listening on {bind}, which is {reach}. Anyone who can route to it reaches \
         sign-in, signup and public ingress, and only tokens and passwords stand in the way. In a \
         container that is expected: publish the port on 127.0.0.1 only (-p 127.0.0.1:8080:8080). \
         Anywhere else, bind 127.0.0.1, or put TLS and a firewall in front of it."
    ))
}

fn is_localhost(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost")
}

/// The host of an authority: no port, no IPv6 brackets.
fn host_part(authority: &str) -> &str {
    let authority = authority.trim();
    if let Some(bracketed) = authority.strip_prefix('[') {
        return bracketed.split(']').next().unwrap_or("");
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => {
            host
        }
        _ => authority,
    }
}

fn normalise(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{any, post};
    use axum::Router;
    use tower::ServiceExt;

    fn guard(allowed: &str, signup: Option<&str>) -> Guard {
        Guard::new("127.0.0.1:8080", allowed, signup).unwrap()
    }

    #[test]
    fn loopback_names_and_ip_literals_are_admitted_without_configuration() {
        let g = guard("", None);
        for host in [
            "localhost:8080",
            "LOCALHOST.",
            "board.localhost:3000",
            "127.0.0.1:8080",
            "[::1]:8080",
            "192.168.1.5",
            "::1",
        ] {
            assert!(g.admits(host), "{host} was refused");
        }
    }

    #[test]
    fn a_name_nobody_configured_is_refused() {
        let g = guard("", None);
        for host in [
            "evil.example",
            "evil.example:8080",
            "wheeld:8080",
            "localhost.evil.example",
            "127.0.0.1.nip.io",
            "",
        ] {
            assert!(!g.admits(host), "{host} was admitted");
        }
    }

    #[test]
    fn configured_names_and_a_named_bind_are_admitted() {
        let g = guard(" wheeld , Api.Internal:8080,", None);
        assert!(g.admits("wheeld:8080"));
        assert!(g.admits("api.internal"));
        assert!(!g.admits("wheeld.evil.example"));

        let named = Guard::new("box.lan:8080", "", None).unwrap();
        assert!(named.admits("box.lan:8080"));
        assert!(!Guard::new("0.0.0.0:8080", "", None)
            .unwrap()
            .admits("0.0.0.0.evil.example"));
    }

    #[test]
    fn signup_is_open_unless_closed_and_nothing_else_is_accepted() {
        assert!(guard("", None).signup_open);
        assert!(guard("", Some("open")).signup_open);
        assert!(guard("", Some("")).signup_open);
        assert!(!guard("", Some("closed")).signup_open);
        let e = Guard::new("127.0.0.1:1", "", Some("nope")).unwrap_err();
        assert!(format!("{e}").contains(ENV_SIGNUP), "{e}");
    }

    #[test]
    fn only_a_loopback_bind_is_quiet() {
        for quiet in [
            "127.0.0.1:8080",
            "127.0.0.2:80",
            "[::1]:8080",
            "localhost:8080",
        ] {
            assert_eq!(exposure(quiet), None, "{quiet}");
        }
        let all = exposure("0.0.0.0:8080").expect("0.0.0.0 is exposed");
        assert!(
            all.contains("0.0.0.0:8080") && all.contains("every network interface"),
            "{all}"
        );
        assert!(exposure("[::]:8080")
            .unwrap()
            .contains("every network interface"));
        assert!(exposure("192.168.1.5:8080")
            .unwrap()
            .contains("192.168.1.5"));
        assert!(exposure("box.lan:8080").unwrap().contains("box.lan"));
    }

    async fn status(g: Guard, method: &str, path: &str, host: Option<&str>) -> StatusCode {
        let app = Router::new()
            .route("/v1/auth/signup", post(|| async { "signed up" }))
            .route("/{*rest}", any(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(Arc::new(g), check));
        let mut req = axum::http::Request::builder().method(method).uri(path);
        if let Some(h) = host {
            req = req.header(header::HOST, h);
        }
        app.oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_layer_refuses_a_foreign_host_but_never_public_ingress() {
        let g = guard("", None);
        assert_eq!(
            status(g.clone(), "GET", "/v1/projects", Some("evil.example")).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(g.clone(), "GET", "/v1/projects", Some("localhost:8080")).await,
            StatusCode::OK
        );
        assert_eq!(
            status(g.clone(), "GET", "/v1/projects", None).await,
            StatusCode::OK
        );
        assert_eq!(
            status(g.clone(), "POST", "/p/abc/hook", Some("evil.example")).await,
            StatusCode::OK
        );
        assert_eq!(
            status(g, "POST", "/v1/auth/signup", Some("evil.example")).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_closed_signup_is_not_found_and_an_open_one_is_served() {
        let closed = guard("", Some("closed"));
        assert_eq!(
            status(closed.clone(), "POST", "/v1/auth/signup", Some("localhost")).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(closed, "POST", "/v1/auth/login", Some("localhost")).await,
            StatusCode::OK
        );
        assert_eq!(
            status(
                guard("", None),
                "POST",
                "/v1/auth/signup",
                Some("localhost")
            )
            .await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_host_that_is_not_text_is_refused() {
        let app = Router::new()
            .route("/{*rest}", any(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(guard("", None)),
                check,
            ));
        let req = axum::http::Request::builder()
            .uri("/v1/projects")
            .header(
                header::HOST,
                axum::http::HeaderValue::from_bytes(b"caf\xe9").unwrap(),
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}
