// Copyright Morgan Metz
// Licensed under the PolyForm Noncommercial License 1.0.0.
// See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0

//! Vault routes. Values go IN through here and never come back out.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use super::{ApiError, ApiResult, AppState};
use crate::db::board;

#[derive(Debug, serde::Deserialize)]
pub struct PutValue {
    pub value: String,
}

/// `PUT /v1/vault/:id/:key`
///
/// Write-only. The response says what was stored, never what it is.
pub async fn put_value(
    State(s): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
    Json(body): Json<PutValue>,
) -> ApiResult<Json<serde_json::Value>> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err(ApiError::invalid("a vault key cannot be empty"));
    }
    // Env var names are the whole point of a credential key, and a name the
    // shell cannot express would be silently undeliverable at spawn.
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(ApiError::invalid(
            "a vault key may contain only letters, digits, '_', '-' and '.'",
        ));
    }
    if body.value.is_empty() {
        // An empty secret reads as "configured" everywhere and authenticates
        // nothing, which is the worst of both.
        return Err(ApiError::invalid("an empty value is not a secret"));
    }
    // A refreshable login only ever comes from a sign-in the engine ran
    // itself. One pasted in here — a laptop's own store, say — has another
    // live holder that will spend the same single-use refresh token, and the
    // loser of that race is wiped.
    if key == wheel_core::CLAUDE_OAUTH_SESSION {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_writable",
            format!(
                "{key} is stored only by signing in (auth/complete with save_to_vault); \
                 it cannot be written directly"
            ),
        ));
    }
    // Under any key name: an agent can export whatever it can read.
    if s.cfg.harness_auth == crate::config::HarnessAuthPolicy::ApiKeyOnly
        && body.value.contains("sk-ant-oat")
    {
        return Err(ApiError::policy_denied("an OAuth token"));
    }

    let warning = {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        store_in_vault(&s, &conn, id, &key, &body.value)?
    };

    s.events.publish(wheel_core::Event::BoardChanged {
        at: wheel_core::Timestamp::now(),
    });
    let mut out = serde_json::json!({ "key": key, "stored": true });
    if let Some(w) = warning {
        out["warning"] = serde_json::json!(w);
    }
    Ok(Json(out))
}

/// Store one value in a vault: the ambiguity check, the encrypted write, and
/// the declared-key bookkeeping that go with it.
///
/// Shared by `PUT /v1/vault/:id/:key` and by the paste-code login's
/// `save_to_vault`, so a credential written by either route is written the
/// same way rather than by two implementations that can drift.
///
/// `Ok(Some(warning))` is a value that was stored but deserves the operator's
/// attention (028 face 5: another vault merely DECLARES this key) — not an
/// error, and must not be treated as one.
pub(crate) fn store_in_vault(
    s: &AppState,
    conn: &rusqlite::Connection,
    vault: Uuid,
    key: &str,
    value: &str,
) -> ApiResult<Option<String>> {
    store_in_vault_until(s, conn, vault, key, value, None)
}

/// As above, recording when the value stops working.
///
/// A credential lifted out of a login carries an expiry, and the UI can only
/// warn before it lapses if the expiry is stored beside the value it belongs
/// to.
pub(crate) fn store_in_vault_until(
    s: &AppState,
    conn: &rusqlite::Connection,
    vault: Uuid,
    key: &str,
    value: &str,
    expires_at: Option<wheel_core::Timestamp>,
) -> ApiResult<Option<String>> {
    let vk = s.supervisor.require_vault_key().map_err(ApiError::config)?;

    let node = board::get(conn, vault)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(vault.to_string()))?;
    let mut cfg = match node.config.clone() {
        wheel_core::NodeConfig::Vault(v) => v,
        _ => return Err(ApiError::invalid("not a vault node")),
    };

    // Adding a key can create an ambiguity that did not exist when the wires
    // were made, so every agent already reading this vault is re-checked
    // before the write, not after. Only a REAL value elsewhere blocks (028
    // face 5); a bare declaration elsewhere is a warning, collected below and
    // returned once the write itself has succeeded.
    let known = cfg.keys.iter().any(|k| k == key);
    let mut warning = None;
    // Two keys in ONE vault that reach a child as the same variable (a
    // refreshable login and a bare token). The session's own save removes the
    // bare token itself, so only the other direction is refused here.
    if !known && key != wheel_core::CLAUDE_OAUTH_SESSION {
        let twin = crate::vault::list_keys(conn, vault)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .into_iter()
            .find(|k| k != key && crate::vault::slot(k) == crate::vault::slot(key));
        if let Some(twin) = twin {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "ambiguous_credential",
                format!(
                    "ambiguous credential {key}: {} already holds {twin}, which reaches its \
                     agents as {}",
                    node.name,
                    crate::vault::slot(&twin)
                ),
            ));
        }
    }
    if !known {
        for agent in crate::vault::agents_reading(conn, vault)
            .map_err(|e| ApiError::internal(e.to_string()))?
        {
            let what = if wheel_core::is_credential_key(key) {
                "credential"
            } else {
                "vault key"
            };
            let agent_name = || {
                board::get(conn, agent)
                    .ok()
                    .flatten()
                    .map(|n| n.name.to_string())
                    .unwrap_or_else(|| agent.to_string())
            };
            if let Some(other) = crate::vault::supplies_key(conn, agent, key, vault)
                .map_err(|e| ApiError::internal(e.to_string()))?
            {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "ambiguous_credential",
                    format!(
                        "ambiguous {what} {key}: {other} already supplies it to {}",
                        agent_name()
                    ),
                ));
            }
            if warning.is_none() {
                if let Some(other) = crate::vault::declares_key(conn, agent, key, vault)
                    .map_err(|e| ApiError::internal(e.to_string()))?
                {
                    warning = Some(format!(
                        "{other} also declares {what} {key} for {} -- no value is stored there yet, \
                         so this write is not blocked, but two vaults intending to supply the same \
                         key to one agent is worth resolving",
                        agent_name()
                    ));
                }
            }
        }
    }

    crate::vault::put_with_expiry(conn, vk, vault, key, value, expires_at)
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Keep the declared key list in step with what is stored, so the UI and
    // the ambiguity checks see the same vault.
    if !known {
        cfg.keys.push(key.to_string());
        cfg.keys.sort();
        let mut updated = node.clone();
        updated.config = wheel_core::NodeConfig::Vault(cfg);
        board::update(conn, &updated).map_err(ApiError::from)?;
    }
    Ok(warning)
}

/// `DELETE /v1/vault/:id/:key`
pub async fn delete_value(
    State(s): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        remove_from_vault(&conn, id, &key)?;
    }
    s.events.publish(wheel_core::Event::BoardChanged {
        at: wheel_core::Timestamp::now(),
    });
    Ok(StatusCode::NO_CONTENT)
}

/// Delete a value and drop its name from the vault's declared keys, so the
/// two cannot disagree about what the vault holds.
pub(crate) fn remove_from_vault(
    conn: &rusqlite::Connection,
    vault: Uuid,
    key: &str,
) -> ApiResult<()> {
    let node = board::get(conn, vault)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(vault.to_string()))?;
    let mut cfg = match node.config.clone() {
        wheel_core::NodeConfig::Vault(v) => v,
        _ => return Err(ApiError::invalid("not a vault node")),
    };
    crate::vault::delete(conn, vault, key).map_err(|e| ApiError::internal(e.to_string()))?;
    cfg.keys.retain(|k| k != key);
    let mut updated = node.clone();
    updated.config = wheel_core::NodeConfig::Vault(cfg);
    board::update(conn, &updated).map_err(ApiError::from)?;
    Ok(())
}

/// `GET /v1/vault/:id` — key NAMES only.
pub async fn list_keys(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;
    if !matches!(node.config, wheel_core::NodeConfig::Vault(_)) {
        return Err(ApiError::invalid("not a vault node"));
    }
    let keys = crate::vault::list_keys(&conn, id).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "keys": keys })))
}

/// 028 face 5: the response `PUT /v1/vault/:id/:key` actually sends. Nothing
/// above `vault.rs`'s own unit layer ever called this handler before this —
/// `declares_key`/`supplies_key` were tested in isolation, but never through
/// the route a client actually calls, so a handler that dropped the
/// `warning` field (or added it when it should not) would have shipped
/// green.
#[cfg(test)]
mod tests {
    use super::*;
    use wheel_core::{AgentConfig, Node, NodeConfig, Position, VaultConfig, WireType};

    fn mk(conn: &rusqlite::Connection, name: &str, config: NodeConfig) -> Uuid {
        let n = Node::new(
            Uuid::new_v4(),
            name.parse().unwrap(),
            Position::default(),
            config,
        );
        board::create(conn, &n).unwrap();
        n.id
    }

    #[tokio::test]
    async fn a_declared_only_overlap_stores_the_value_and_warns() {
        let state = crate::api::test_state();
        let (_v1, v2) = {
            let conn = state.db.lock().unwrap();
            let agent = mk(&conn, "agent", NodeConfig::Agent(AgentConfig::default()));
            let v1 = mk(
                &conn,
                "v1",
                NodeConfig::Vault(VaultConfig {
                    keys: vec!["ANTHROPIC_API_KEY".into()],
                }),
            );
            let v2 = mk(&conn, "v2", NodeConfig::Vault(VaultConfig { keys: vec![] }));
            board::add_wire(&conn, agent, v1, WireType::Read, None).unwrap();
            board::add_wire(&conn, agent, v2, WireType::Read, None).unwrap();
            (v1, v2)
        };

        let resp = put_value(
            State(state.clone()),
            Path((v2, "ANTHROPIC_API_KEY".to_string())),
            Json(PutValue {
                value: "sk-ant-api03-real".into(),
            }),
        )
        .await
        .expect("a declared-only overlap must not block the write")
        .0;

        assert_eq!(resp["stored"], true);
        assert!(
            resp["warning"]
                .as_str()
                .is_some_and(|w| w.contains("v1") || w.contains("v2")),
            "the response must name the other vault: {resp}"
        );

        let conn = state.db.lock().unwrap();
        assert_eq!(
            crate::vault::get(
                &conn,
                state.supervisor.vault_key().unwrap(),
                v2,
                "ANTHROPIC_API_KEY"
            )
            .unwrap()
            .as_deref(),
            Some("sk-ant-api03-real"),
            "the warning must not have stopped the value from actually being stored"
        );
    }

    #[tokio::test]
    async fn no_other_vault_declaring_the_key_means_no_warning() {
        let state = crate::api::test_state();
        let v = {
            let conn = state.db.lock().unwrap();
            let agent = mk(&conn, "agent", NodeConfig::Agent(AgentConfig::default()));
            let v = mk(&conn, "v", NodeConfig::Vault(VaultConfig { keys: vec![] }));
            board::add_wire(&conn, agent, v, WireType::Read, None).unwrap();
            v
        };

        let resp = put_value(
            State(state),
            Path((v, "ANTHROPIC_API_KEY".to_string())),
            Json(PutValue {
                value: "sk-ant-api03-real".into(),
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(resp["stored"], true);
        assert!(
            resp.get("warning").is_none(),
            "no other vault declares this key, so there is nothing to warn about: {resp}"
        );
    }

    /// The block this feature must NOT have loosened: a vault that actually
    /// HOLDS a value for the key still 409s, warning or not.
    #[tokio::test]
    async fn a_vault_that_actually_holds_the_key_still_blocks() {
        let state = crate::api::test_state();
        let (v1, v2) = {
            let conn = state.db.lock().unwrap();
            let agent = mk(&conn, "agent", NodeConfig::Agent(AgentConfig::default()));
            let v1 = mk(&conn, "v1", NodeConfig::Vault(VaultConfig { keys: vec![] }));
            let v2 = mk(&conn, "v2", NodeConfig::Vault(VaultConfig { keys: vec![] }));
            board::add_wire(&conn, agent, v1, WireType::Read, None).unwrap();
            board::add_wire(&conn, agent, v2, WireType::Read, None).unwrap();
            (v1, v2)
        };

        let _ = put_value(
            State(state.clone()),
            Path((v1, "ANTHROPIC_API_KEY".to_string())),
            Json(PutValue {
                value: "sk-ant-api03-first".into(),
            }),
        )
        .await
        .unwrap();

        let err = put_value(
            State(state),
            Path((v2, "ANTHROPIC_API_KEY".to_string())),
            Json(PutValue {
                value: "sk-ant-api03-second".into(),
            }),
        )
        .await
        .expect_err("a second vault actually holding the same key must still be refused");
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    /// A renewable login only ever comes from a sign-in the engine ran: one
    /// pasted in here has another live holder (a laptop's own Claude Code)
    /// that will spend the same single-use refresh token, and the loser of
    /// that race has its store wiped.
    #[tokio::test]
    async fn a_renewable_login_cannot_be_written_directly() {
        let state = crate::api::test_state();
        let v = {
            let conn = state.db.lock().unwrap();
            mk(
                &conn,
                "creds",
                NodeConfig::Vault(VaultConfig { keys: vec![] }),
            )
        };
        let err = put_value(
            State(state),
            Path((v, wheel_core::CLAUDE_OAUTH_SESSION.to_string())),
            Json(PutValue {
                value: r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x"}}"#.into(),
            }),
        )
        .await
        .expect_err("a login is saved by signing in, not by PUT");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1, "not_writable");
    }

    /// An agent can export whatever it can read, so the policy is on the
    /// VALUE, under any key name it arrives under.
    ///
    /// Mutation-checked: drop the check and a cloud project holds an OAuth
    /// token under a name nothing looks at.
    #[tokio::test]
    async fn api_key_only_refuses_an_oauth_token_under_any_key_name() {
        let state =
            crate::api::test_state_with(crate::config::HarnessAuthPolicy::ApiKeyOnly, None, |_| {});
        let v = {
            let conn = state.db.lock().unwrap();
            mk(
                &conn,
                "creds",
                NodeConfig::Vault(VaultConfig { keys: vec![] }),
            )
        };
        for key in ["CLAUDE_CODE_OAUTH_TOKEN", "SOMETHING_ELSE"] {
            let err = put_value(
                State(state.clone()),
                Path((v, key.to_string())),
                Json(PutValue {
                    value: "sk-ant-oat01-durable".into(),
                }),
            )
            .await
            .expect_err("{key} carried an OAuth token into an api-key-only project");
            assert_eq!(err.0, StatusCode::FORBIDDEN);
            assert_eq!(err.1, "policy_denied");
        }
        // An API key under the same name is exactly what this deployment is for.
        let stored = put_value(
            State(state),
            Path((v, "ANTHROPIC_API_KEY".to_string())),
            Json(PutValue {
                value: "sk-ant-api03-real".into(),
            }),
        )
        .await
        .expect("an api key is not refused");
        assert_eq!(stored.0["stored"], true);
    }

    /// Two keys in ONE vault that reach a child as the same variable are the
    /// same coin-flip the ambiguity rule refuses everywhere else.
    #[tokio::test]
    async fn one_vault_may_not_hold_a_login_and_a_bare_token_for_the_same_variable() {
        let state = crate::api::test_state();
        let v = {
            let conn = state.db.lock().unwrap();
            let v = mk(
                &conn,
                "creds",
                NodeConfig::Vault(VaultConfig { keys: vec![] }),
            );
            crate::vault::put(
                &conn,
                state.supervisor.vault_key().unwrap(),
                v,
                wheel_core::CLAUDE_OAUTH_SESSION,
                r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-x"}}"#,
            )
            .unwrap();
            v
        };
        let err = put_value(
            State(state),
            Path((v, "CLAUDE_CODE_OAUTH_TOKEN".to_string())),
            Json(PutValue {
                value: "sk-ant-oat01-second".into(),
            }),
        )
        .await
        .expect_err("two values for one variable must be refused, not resolved");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "ambiguous_credential");
    }
}
