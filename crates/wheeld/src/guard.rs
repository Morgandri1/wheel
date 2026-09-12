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
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::IpAddr;
use std::sync::Arc;

pub const ENV_ALLOWED_HOSTS: &str = "WHEEL_ALLOWED_HOSTS";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    names: Vec<String>,
}

impl Guard {
    pub fn from_env(bind: &str) -> Self {
        Self::new(bind, &std::env::var(ENV_ALLOWED_HOSTS).unwrap_or_default())
    }

    pub fn new(bind: &str, allowed: &str) -> Self {
        let mut names: Vec<String> = allowed
            .split(',')
            .map(|name| normalise(host_part(name)))
            .filter(|name| !name.is_empty())
            .collect();
        let bound = normalise(host_part(bind));
        if !bound.is_empty() && bound.parse::<IpAddr>().is_err() {
            names.push(bound);
        }
        Self { names }
    }

    pub fn admits(&self, host: &str) -> bool {
        let host = normalise(host_part(host));
        host.parse::<IpAddr>().is_ok() || is_localhost(&host) || self.names.contains(&host)
    }
}

pub async fn check(State(guard): State<Arc<Guard>>, req: Request, next: Next) -> Response {
    if req.uri().path().starts_with("/p/") {
        return next.run(req).await;
    }
    let admitted = match req.headers().get(header::HOST) {
        Some(value) => value.to_str().is_ok_and(|host| guard.admits(host)),
        None => req
            .uri()
            .authority()
            .is_none_or(|authority| guard.admits(authority.as_str())),
    };
    if !admitted {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": {
                "code": "forbidden",
                "message": "This request was addressed to a host name wheeld does not answer to. \
                            If the name is yours, add it to WHEEL_ALLOWED_HOSTS.",
            }})),
        )
            .into_response();
    }
    next.run(req).await
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
         sign-in and public ingress, and only tokens and passwords stand in the way. In a \
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
    use axum::routing::any;
    use axum::Router;
    use tower::ServiceExt;

    fn guard(allowed: &str) -> Guard {
        Guard::new("127.0.0.1:8080", allowed)
    }

    #[test]
    fn loopback_names_and_ip_literals_are_admitted_without_configuration() {
        let g = guard("");
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
        let g = guard("");
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
        let g = guard(" wheeld , Api.Internal:8080,");
        assert!(g.admits("wheeld:8080"));
        assert!(g.admits("api.internal"));
        assert!(!g.admits("wheeld.evil.example"));

        assert!(Guard::new("box.lan:8080", "").admits("box.lan:8080"));
        assert!(!Guard::new("0.0.0.0:8080", "").admits("0.0.0.0.evil.example"));
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

    async fn status(g: Guard, path: &str, host: Option<&[u8]>) -> StatusCode {
        let app = Router::new()
            .route("/{*rest}", any(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(Arc::new(g), check));
        let mut req = axum::http::Request::builder().uri(path);
        if let Some(h) = host {
            req = req.header(
                header::HOST,
                axum::http::HeaderValue::from_bytes(h).unwrap(),
            );
        }
        app.oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn the_layer_refuses_a_foreign_host_but_never_public_ingress() {
        let g = guard("");
        let forbidden = StatusCode::FORBIDDEN;
        assert_eq!(
            status(g.clone(), "/v1/projects", Some(b"evil.example")).await,
            forbidden
        );
        assert_eq!(
            status(g.clone(), "/v1/auth/signup", Some(b"evil.example")).await,
            forbidden
        );
        assert_eq!(
            status(g.clone(), "/v1/projects", Some(b"caf\xe9")).await,
            forbidden
        );
        assert_eq!(
            status(g.clone(), "/v1/projects", Some(b"localhost:8080")).await,
            StatusCode::OK
        );
        assert_eq!(
            status(g.clone(), "/v1/projects", None).await,
            StatusCode::OK
        );
        assert_eq!(
            status(g, "/p/abc/hook", Some(b"evil.example")).await,
            StatusCode::OK
        );
    }
}
