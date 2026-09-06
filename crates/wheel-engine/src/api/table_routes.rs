//! Table-node routes for the UI (§4).
//!
//! These are the operator's view, authenticated with the engine secret rather
//! than a node token: the UI is not a node and has no wires, so there is no
//! wire check here. Agents reach the same data through `/v1/cli/*`, which does.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{ApiError, ApiResult, AppState};
use crate::db::{board, tables};

#[derive(Debug, Deserialize)]
pub struct Paging {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

fn table_node(s: &AppState, id: Uuid) -> ApiResult<(wheel_core::Node, wheel_core::TableConfig)> {
    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let node = board::get(&conn, id)
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(id.to_string()))?;
    let cfg = match &node.config {
        wheel_core::NodeConfig::Table(c) => c.clone(),
        other => {
            return Err(ApiError::invalid(format!(
                "not a table node (it is {} {})",
                other.node_type().article(),
                other.node_type()
            )))
        }
    };
    Ok((node, cfg))
}

/// `GET /v1/tables/:id/rows?limit&offset`
pub async fn rows(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(p): Query<Paging>,
) -> ApiResult<Json<serde_json::Value>> {
    let (node, cfg) = table_node(&s, id)?;
    let limit = p.limit.unwrap_or(100).min(tables::MAX_ROWS);
    let offset = p.offset.unwrap_or(0);

    let conn = s.db.lock().map_err(|_| ApiError::internal("db poisoned"))?;
    let rows = tables::list_rows(&conn, &node.name, &cfg, limit, offset)
        .map_err(|e| ApiError::invalid(e.to_string()))?;
    let total = tables::count_rows(&conn, &node.name).unwrap_or(0);

    Ok(Json(serde_json::json!({
        "node": node.name,
        // The UI renders a header per column, and `key` is implicit in the
        // config but real in every row.
        "columns": std::iter::once(serde_json::json!({
                "name": tables::KEY_COLUMN, "type": "text"
            }))
            .chain(cfg.columns.iter().map(|c| serde_json::json!({
                "name": c.name.as_str(), "type": c.column_type
            })))
            .collect::<Vec<_>>(),
        "rows": rows,
        "total": total,
        "limit": limit,
        "offset": offset,
    })))
}

#[derive(Debug, Deserialize)]
pub struct QueryBody {
    pub sql: String,
}

/// `POST /v1/tables/:id/query` — read-only SQL, scoped to this table.
///
/// The operator gets exactly the same box an agent does. Widening it for the
/// UI would mean two SQL surfaces to keep safe instead of one.
pub async fn query(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<QueryBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let (node, _cfg) = table_node(&s, id)?;
    let table = tables::table_name(&node.name).map_err(|e| ApiError::invalid(e.to_string()))?;
    let rows = tables::query(&s.cfg.db_path(), &table, &body.sql)
        .map_err(|e| ApiError::invalid(e.to_string()))?;
    Ok(Json(serde_json::json!({ "node": node.name, "rows": rows })))
}
