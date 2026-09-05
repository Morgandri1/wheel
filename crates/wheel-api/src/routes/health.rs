use axum::Json;
use serde_json::json;

/// Unauthenticated liveness probe. Deliberately reveals nothing about internals.
pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}
