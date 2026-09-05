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

    let vk = s
        .supervisor
        .vault_key()
        .ok_or_else(|| ApiError::internal("this project has no usable vault key"))?;

    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let node = board::get(&conn, id)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::not_found(id.to_string()))?;
        let mut cfg = match node.config.clone() {
            wheel_core::NodeConfig::Vault(v) => v,
            _ => return Err(ApiError::invalid("not a vault node")),
        };

        // Adding a key can create an ambiguity that did not exist when the
        // wires were made, so every agent already reading this vault is
        // re-checked before the write, not after.
        if !cfg.keys.contains(&key) {
            for agent in crate::vault::agents_reading(&conn, id)
                .map_err(|e| ApiError::internal(e.to_string()))?
            {
                if crate::vault::supplies_key(&conn, agent, &key, id)
                    .map_err(|e| ApiError::internal(e.to_string()))?
                    .is_some()
                {
                    let a = crate::vault::supplies_key(&conn, agent, &key, id)
                        .map_err(|e| ApiError::internal(e.to_string()))?
                        .unwrap_or_default();
                    let what = if wheel_core::is_credential_key(&key) {
                        "credential"
                    } else {
                        "vault key"
                    };
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "ambiguous_credential",
                        format!(
                            "ambiguous {what} {key}: {} already supplies it to {}",
                            a,
                            board::get(&conn, agent)
                                .ok()
                                .flatten()
                                .map(|n| n.name.to_string())
                                .unwrap_or_else(|| agent.to_string())
                        ),
                    ));
                }
            }
        }

        crate::vault::put(&conn, vk, id, &key, &body.value)
            .map_err(|e| ApiError::internal(e.to_string()))?;

        // Keep the declared key list in step with what is stored, so the UI
        // and the ambiguity checks see the same vault.
        if !cfg.keys.contains(&key) {
            cfg.keys.push(key.clone());
            cfg.keys.sort();
            let mut updated = node.clone();
            updated.config = wheel_core::NodeConfig::Vault(cfg);
            board::update(&conn, &updated).map_err(ApiError::from)?;
        }
    }

    s.events.publish(wheel_core::Event::BoardChanged {
        at: wheel_core::Timestamp::now(),
    });
    Ok(Json(serde_json::json!({ "key": key, "stored": true })))
}

/// `DELETE /v1/vault/:id/:key`
pub async fn delete_value(
    State(s): State<AppState>,
    Path((id, key)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    {
        let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
        let node = board::get(&conn, id)
            .map_err(|e| ApiError::internal(e.to_string()))?
            .ok_or_else(|| ApiError::not_found(id.to_string()))?;
        let mut cfg = match node.config.clone() {
            wheel_core::NodeConfig::Vault(v) => v,
            _ => return Err(ApiError::invalid("not a vault node")),
        };
        crate::vault::delete(&conn, id, &key).map_err(|e| ApiError::internal(e.to_string()))?;
        cfg.keys.retain(|k| k != &key);
        let mut updated = node.clone();
        updated.config = wheel_core::NodeConfig::Vault(cfg);
        board::update(&conn, &updated).map_err(ApiError::from)?;
    }
    s.events.publish(wheel_core::Event::BoardChanged {
        at: wheel_core::Timestamp::now(),
    });
    Ok(StatusCode::NO_CONTENT)
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
