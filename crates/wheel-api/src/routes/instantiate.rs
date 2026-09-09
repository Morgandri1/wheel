//! `POST /v1/projects/instantiate` — create a project from a board in one atomic server sequence.
//!
//! Design: `docs/proposals/wow-templates-instantiate-route.md` (ADVERSARY-cleared,
//! `reports/20260909T200006Z-adversary-task3-review`). The failure mode this route exists to
//! remove: a client sequencing "create project" then "apply board" as two separate calls can drop
//! the connection in between and orphan an empty project it never learns exists. Here the whole
//! sequence is one request; anything short of full success rolls back before replying.
//!
//! Validation runs BEFORE anything is created — `apply::validate` is pure and a template only ever
//! targets a fresh, empty project, so checking against `ExistingBoard::default()` up front is
//! exactly as authoritative as checking after creation, and the common "malformed board" failure
//! spends no quota and starts no sandbox.

use crate::apply::{
    execute, validate, ApplyPolicy, ApplyReport, EmittedBoard, ExistingBoard, Failure,
};
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::models::Capabilities;
use crate::routes::board_apply::HttpBoardClient;
use crate::routes::projects::{create_project, destroy_project, update_project, UpdateProject};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct InstantiateRequest {
    pub name: String,
    pub board: EmittedBoard,
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
}

/// One request, one outcome — the contract callers get, spelled out so a status-code-only check
/// (rather than reading `applied`) cannot be tempted here either: 201 is the only status where a
/// project exists AND the board landed.
pub async fn instantiate(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<InstantiateRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    // Nothing is created until the board itself is known-legal — same discipline `apply.rs`'s
    // module doc states for a single apply, one level up: not even the project exists yet.
    let plan = match validate(
        &req.board,
        &ExistingBoard::default(),
        ApplyPolicy::default(),
    ) {
        Ok(plan) => plan,
        Err(refusals) => {
            let listed: Vec<_> = refusals
                .iter()
                .map(|r| serde_json::json!({"refusal": r, "message": r.message()}))
                .collect();
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({
                    "applied": false,
                    "refusals": listed,
                    "message": "the board was refused; nothing was created",
                })),
            ));
        }
    };

    let mut project = create_project(&state, &user, req.name).await?;
    let owner_id = user.id();

    // The sandbox never coming up makes an apply attempt pointless (it can only fail with a
    // confusing `engine_unreachable`) — treat it as a synthetic failed step and roll back the same
    // way any other failure does, rather than trying anyway.
    if !matches!(
        project.status,
        crate::models::ProjectStatus::Running | crate::models::ProjectStatus::Starting
    ) {
        let report = ApplyReport {
            failures: vec![Failure::sandbox_did_not_start()],
            ..Default::default()
        };
        return rollback(&state, project.id, owner_id, report).await;
    }

    if let Some(caps) = req.capabilities {
        let patch = UpdateProject {
            name: None,
            capabilities: Some(caps),
        };
        match update_project(&state, project.id, owner_id, patch).await {
            // The patch is authoritative for what the caller asked for; the response must reflect
            // it, not the pre-patch project `create_project` returned.
            Ok(patched) => project = patched,
            Err(e) => {
                let report = ApplyReport {
                    failures: vec![Failure::capabilities(e.to_string())],
                    ..Default::default()
                };
                return rollback(&state, project.id, owner_id, report).await;
            }
        }
    }

    let client = HttpBoardClient::new(&state, &project.id);
    let report = execute(&plan, &ExistingBoard::default(), &client).await;

    if report.is_complete() {
        return Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({
                "applied": true,
                "project": project,
                "report": report,
            })),
        ));
    }

    rollback(&state, project.id, owner_id, report).await
}

/// Tear the just-created project back down and report honestly.
///
/// Two outcomes, both `207`, distinguished by `rolled_back`: the overwhelmingly common case is
/// "nothing survives, same as if the request had never been made"; the rare one is "the rollback
/// itself failed", which hands back the project id so the caller can say so rather than losing
/// track of it — matching `destroy_project`'s own rule of never dropping the row while the sandbox
/// might still exist.
async fn rollback(
    state: &AppState,
    project_id: uuid::Uuid,
    owner_id: &str,
    report: ApplyReport,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let rolled_back = destroy_project(state, project_id, owner_id).await.is_ok();
    let mut body = serde_json::json!({
        "applied": false,
        "rolled_back": rolled_back,
        "report": report,
    });
    if !rolled_back {
        body["project_id"] = serde_json::json!(project_id);
    }
    Ok((StatusCode::MULTI_STATUS, Json(body)))
}
