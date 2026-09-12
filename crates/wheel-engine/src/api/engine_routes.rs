// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! `GET /v1/engine`: capability discovery (`docs/PROTOCOL.md` §2).

use axum::{extract::State, Json};
use wheel_core::{EngineInfo, Harness};

use super::AppState;
use crate::config::HarnessAuthPolicy;

/// Stable ids a client may test for before depending on a capability. Every id
/// names a route or config field that exists in this build; the tests hold
/// each one to that by calling it.
const FEATURES: &[&str] = &[
    "board",
    "wires",
    "messages",
    "inbox",
    "tables",
    "vault",
    "tools",
    "ingress",
    "idle_parking",
    "ephemeral_context",
    "budgets",
    "oauth_paste_code",
    "oauth_refresh",
    "interrupt",
    "script_run",
];

pub async fn engine_info(State(s): State<AppState>) -> Json<EngineInfo> {
    Json(EngineInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        build: super::build_id().into(),
        api_version: "v1".into(),
        harnesses: Harness::ALL
            .into_iter()
            .filter(|h| crate::harness::has_driver(*h))
            .map(|h| h.as_str().into())
            .collect(),
        profiles: vec!["sandboxed".into()],
        features: features(s.cfg.harness_auth).map(String::from).collect(),
    })
}

/// The ids this deployment can honour. A paste-code login produces an OAuth
/// credential, which an api-key-only deployment refuses at spawn, so there the
/// login would complete and the agent still could not run.
fn features(policy: HarnessAuthPolicy) -> impl Iterator<Item = &'static str> {
    FEATURES.iter().copied().filter(move |f| {
        !(matches!(*f, "oauth_paste_code" | "oauth_refresh")
            && policy == HarnessAuthPolicy::ApiKeyOnly)
    })
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        Router,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use wheel_core::{EngineInfo, ErrorBody, Harness};

    use std::sync::Arc;

    use super::FEATURES;
    use crate::api::{router, test_state};
    use crate::config::HarnessAuthPolicy;

    struct Engine {
        app: Router,
        secret: String,
    }

    impl Engine {
        fn new() -> Self {
            Self::under(HarnessAuthPolicy::default())
        }

        fn under(policy: HarnessAuthPolicy) -> Self {
            let mut state = test_state();
            let mut cfg = (*state.cfg).clone();
            cfg.harness_auth = policy;
            state.cfg = Arc::new(cfg);
            Self {
                secret: state.cfg.engine_secret.clone(),
                app: router(state),
            }
        }

        async fn call(
            &self,
            method: &str,
            uri: &str,
            bearer: Option<&str>,
            body: Option<Value>,
        ) -> (StatusCode, Vec<u8>) {
            let mut req = Request::builder().method(method).uri(uri);
            if let Some(b) = bearer {
                req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
            }
            let req = match body {
                Some(v) => req
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(v.to_string())),
                None => req.body(Body::empty()),
            }
            .unwrap();
            let resp = self.app.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (status, bytes.to_vec())
        }

        async fn authed(
            &self,
            method: &str,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Vec<u8>) {
            let secret = self.secret.clone();
            self.call(method, uri, Some(&secret), body).await
        }

        async fn create_node(
            &self,
            name: &str,
            node_type: &str,
            config: Value,
        ) -> (StatusCode, Value) {
            let body = json!({"name": name, "type": node_type, "config": config});
            let (status, body) = self.authed("POST", "/v1/nodes", Some(body)).await;
            (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
        }

        async fn info(&self) -> EngineInfo {
            let (status, body) = self.authed("GET", "/v1/engine", None).await;
            assert_eq!(status, StatusCode::OK);
            serde_json::from_slice(&body).unwrap()
        }
    }

    #[tokio::test]
    async fn discovery_is_refused_without_the_engine_secret() {
        let engine = Engine::new();
        for bearer in [None, Some("not-the-engine-secret")] {
            let (status, body) = engine.call("GET", "/v1/engine", bearer, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "bearer {bearer:?}");
            let err: ErrorBody = serde_json::from_slice(&body).unwrap();
            assert_eq!(err.error.code, "unauthorized");
        }
    }

    #[tokio::test]
    async fn discovery_describes_this_build() {
        let engine = Engine::new();
        let (status, body) = engine.authed("GET", "/v1/engine", None).await;
        assert_eq!(status, StatusCode::OK);

        let raw: Value = serde_json::from_slice(&body).unwrap();
        let mut keys: Vec<&str> = raw
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "api_version",
                "build",
                "features",
                "harnesses",
                "profiles",
                "version"
            ]
        );

        let info: EngineInfo = serde_json::from_value(raw).unwrap();
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.build, super::super::build_id());
        assert_eq!(info.api_version, "v1");
        assert_eq!(info.harnesses, ["claude"]);
        assert_eq!(info.profiles, ["sandboxed"]);
        assert_eq!(
            info.features,
            [
                "board",
                "wires",
                "messages",
                "inbox",
                "tables",
                "vault",
                "tools",
                "ingress",
                "idle_parking",
                "ephemeral_context",
                "budgets",
                "oauth_paste_code",
                "oauth_refresh",
                "interrupt",
                "script_run",
            ]
        );
    }

    /// A client that sends a harness it saw advertised must not be refused for
    /// it, whether it creates an agent or switches an existing one.
    #[tokio::test]
    async fn advertised_harnesses_are_exactly_the_ones_node_creation_and_patch_accept() {
        let engine = Engine::new();
        let advertised = engine.info().await.harnesses;
        let is_advertised = |h: Harness| advertised.iter().any(|a| a == h.as_str());

        let claude = json!({"harness": "claude", "system_prompt": ""});
        let (status, existing) = engine.create_node("switched", "agent", claude).await;
        assert_eq!(status, StatusCode::CREATED);
        let uri = format!("/v1/nodes/{}", existing["id"].as_str().unwrap());

        for harness in Harness::ALL {
            let config = json!({"harness": harness.as_str(), "system_prompt": ""});
            let (created, _) = engine
                .create_node(&format!("{harness}-agent"), "agent", config)
                .await;
            assert_eq!(
                created == StatusCode::CREATED,
                is_advertised(harness),
                "{harness}: create answered {created}, advertised {advertised:?}"
            );

            let patch = json!({"config": {"harness": harness.as_str()}});
            let (patched, _) = engine.authed("PATCH", &uri, Some(patch)).await;
            assert_eq!(
                patched == StatusCode::OK,
                is_advertised(harness),
                "{harness}: patch answered {patched}, advertised {advertised:?}"
            );
        }
    }

    /// On an api-key-only deployment an OAuth credential is refused wherever
    /// it comes from, so neither the login that produces one nor the renewal
    /// that keeps one alive is offered.
    #[tokio::test]
    async fn the_oauth_features_are_not_advertised_where_the_credential_is_refused() {
        let key_only = Engine::under(HarnessAuthPolicy::ApiKeyOnly)
            .info()
            .await
            .features;
        let others: Vec<&str> = FEATURES
            .iter()
            .copied()
            .filter(|f| !matches!(*f, "oauth_paste_code" | "oauth_refresh"))
            .collect();
        assert_eq!(
            key_only, others,
            "api-key-only must drop the OAuth features and nothing else"
        );

        let oauth = Engine::under(HarnessAuthPolicy::OauthToken)
            .info()
            .await
            .features;
        for f in ["oauth_paste_code", "oauth_refresh"] {
            assert!(oauth.iter().any(|a| a == f), "{f} missing from {oauth:?}");
        }
    }

    enum Evidence {
        /// Control-plane routes. An unauthenticated call to a routed path is
        /// refused by the realm (401); an unrouted one never reaches it (404).
        /// Authenticated, a routed method reaches its handler, which never
        /// answers with the router's bare 404 or 405.
        Routes(&'static [(&'static str, &'static str)]),
        /// An `AgentConfig` field and a value for it. The config is
        /// `deny_unknown_fields`, so a node carrying a field that does not
        /// exist is refused at creation.
        AgentField(&'static str, &'static str),
        /// Public ingress answers a hit on an endpoint node's path.
        Ingress,
    }

    fn evidence(feature: &str) -> Evidence {
        use Evidence::*;
        match feature {
            "board" => Routes(&[
                ("GET", "/v1/board"),
                ("POST", "/v1/nodes"),
                ("PATCH", "/v1/nodes/{id}"),
                ("DELETE", "/v1/nodes/{id}"),
            ]),
            "wires" => Routes(&[("POST", "/v1/wires"), ("DELETE", "/v1/wires")]),
            "messages" => Routes(&[("POST", "/v1/agents/{id}/send")]),
            "inbox" => Routes(&[
                ("GET", "/v1/agents/{id}/inbox"),
                ("GET", "/v1/agents/{id}/inbox/{id}"),
            ]),
            "tables" => Routes(&[
                ("GET", "/v1/tables/{id}/rows"),
                ("POST", "/v1/tables/{id}/query"),
            ]),
            "vault" => Routes(&[
                ("GET", "/v1/vault/{id}"),
                ("PUT", "/v1/vault/{id}/KEY"),
                ("DELETE", "/v1/vault/{id}/KEY"),
            ]),
            "tools" => Routes(&[
                ("POST", "/v1/tools/import"),
                ("POST", "/v1/tools/{id}/import"),
                ("GET", "/v1/tools/{id}/ops"),
                ("POST", "/v1/tools/{id}/call"),
            ]),
            "ingress" => Ingress,
            "idle_parking" => AgentField("idle_timeout_secs", "60"),
            "ephemeral_context" => AgentField("ephemeral_context", "true"),
            "budgets" => AgentField("budget", r#"{"max_turns": 3, "max_usd": 1.5}"#),
            "oauth_paste_code" => Routes(&[
                ("POST", "/v1/agents/{id}/auth/begin"),
                ("POST", "/v1/agents/{id}/auth/complete"),
            ]),
            // The engine renews a vaulted login itself; `GET auth` is where a
            // client reads that (`refreshable`, `expires_at`, `warning`).
            "oauth_refresh" => Routes(&[("GET", "/v1/agents/{id}/auth")]),
            "interrupt" => Routes(&[("POST", "/v1/agents/{id}/interrupt")]),
            // Reachable and routed at every policy -- whether a given call actually runs a
            // script is the WHEEL_SCRIPT_EXEC/uid-isolation gate's job (script_routes.rs), a
            // per-deployment fact this feature id says nothing about. The id promises "this
            // build has the route," matching every other entry here, not "this deployment has
            // opted in."
            "script_run" => Routes(&[("POST", "/v1/scripts/{id}/run")]),
            other => panic!("{other:?} is advertised with no evidence that it exists"),
        }
    }

    async fn assert_holds(engine: &Engine, feature: &str, evidence: Evidence) {
        match evidence {
            Evidence::Routes(routes) => {
                for (method, path) in routes {
                    let uri = path.replace("{id}", &uuid::Uuid::new_v4().to_string());
                    let (status, _) = engine.call(method, &uri, None, None).await;
                    assert_eq!(
                        status,
                        StatusCode::UNAUTHORIZED,
                        "{feature}: {method} {path} is not routed in the engine-secret realm"
                    );
                    let (status, body) = engine.authed(method, &uri, None).await;
                    let unrouted = status == StatusCode::METHOD_NOT_ALLOWED
                        || (status == StatusCode::NOT_FOUND && body.is_empty());
                    assert!(
                        !unrouted,
                        "{feature}: {method} {path} reached no handler ({status})"
                    );
                }
            }
            Evidence::AgentField(field, value) => {
                let value: Value = serde_json::from_str(value).unwrap();
                let config = json!({"harness": "claude", "system_prompt": "", field: value});
                let (status, body) = engine
                    .authed(
                        "POST",
                        "/v1/nodes",
                        Some(json!({"name": feature, "type": "agent", "config": config})),
                    )
                    .await;
                assert_eq!(
                    status,
                    StatusCode::CREATED,
                    "{feature}: an agent with `{field}` was refused: {}",
                    String::from_utf8_lossy(&body)
                );
                let node: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    node["config"][field], value,
                    "{feature}: `{field}` was accepted but not kept"
                );
            }
            Evidence::Ingress => {
                let hit = || engine.call("POST", "/ingress/engine-probe", None, Some(json!({})));
                let (before, _) = hit().await;
                assert_eq!(before, StatusCode::NOT_FOUND);
                let endpoint =
                    json!({"method": "POST", "path": "/engine-probe", "response_mode": "ack"});
                assert_eq!(
                    engine.create_node("probe", "endpoint", endpoint).await.0,
                    StatusCode::CREATED
                );
                let (after, body) = hit().await;
                assert_eq!(
                    after,
                    StatusCode::ACCEPTED,
                    "{feature}: a hit on an endpoint's path was not accepted: {}",
                    String::from_utf8_lossy(&body)
                );
            }
        }
    }

    #[tokio::test]
    async fn every_advertised_feature_is_callable_or_configurable() {
        for policy in [HarnessAuthPolicy::OauthToken, HarnessAuthPolicy::ApiKeyOnly] {
            for feature in Engine::under(policy).info().await.features {
                let engine = Engine::under(policy);
                assert_holds(&engine, &feature, evidence(&feature)).await;
            }
        }
    }

    /// A tripwire, not the gate: the gate is the test above. This only keeps
    /// the protocol's list of ids from falling behind the engine's.
    #[test]
    fn every_advertised_feature_is_documented() {
        let protocol = include_str!("../../../../docs/PROTOCOL.md");
        let section = protocol
            .split("### Engine discovery")
            .nth(1)
            .and_then(|rest| rest.split("\n### ").next())
            .expect("PROTOCOL.md has an Engine discovery section");
        for feature in FEATURES {
            assert!(
                section.contains(&format!("| `{feature}` |")),
                "{feature} is advertised but PROTOCOL.md does not say what it means"
            );
        }
    }
}
