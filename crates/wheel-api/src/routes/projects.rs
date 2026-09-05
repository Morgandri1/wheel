//! Project CRUD and lifecycle.
//!
//! Note the handler signatures: any handler that acts on an existing project takes `ProjectScope`,
//! which cannot be constructed without a verified token and a proven ownership match. Handlers
//! that operate on the collection take `AuthUser`. No handler takes a bare project id.

use crate::auth::{AuthUser, ProjectScope};
use crate::crypto;
use crate::error::{ApiError, ApiResult};
use crate::models::{validate_project_name, Capabilities, Project, ProjectRow, ProjectStatus};
use crate::orchestrator::EngineSecrets;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct CreateProject {
    pub name: String,
}

#[derive(Deserialize)]
pub struct UpdateProject {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Capabilities>,
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateProject>,
) -> ApiResult<(axum::http::StatusCode, Json<Project>)> {
    let name = body.name.trim().to_string();
    validate_project_name(&name).map_err(ApiError::BadRequest)?;

    // Per-user quota. Checked before we do any work that costs money or disk.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE owner_id = $1")
        .bind(user.id())
        .fetch_one(&state.db)
        .await?;
    if count >= state.cfg.max_projects_per_user {
        return Err(ApiError::Conflict(format!(
            "project limit reached ({} per user)",
            state.cfg.max_projects_per_user
        )));
    }

    let id = Uuid::new_v4();
    let secrets = EngineSecrets {
        engine_secret: crypto::generate_secret(),
        vault_key: crypto::generate_secret(),
    };
    let engine_enc = crypto::seal(&state.cfg.master_key, &secrets.engine_secret)?;
    let vault_enc = crypto::seal(&state.cfg.master_key, &secrets.vault_key)?;

    // Row and secrets are written atomically: a project without secrets could never start, and a
    // secret row without a project would be an orphan holding key material.
    let mut tx = state.db.begin().await?;
    let row: ProjectRow = sqlx::query_as::<_, ProjectRow>(
        "INSERT INTO projects (id, owner_id, name, capabilities, status) \
         VALUES ($1, $2, $3, $4, 'stopped') \
         RETURNING id, owner_id, name, capabilities, status, created_at, updated_at",
    )
    .bind(id)
    .bind(user.id())
    .bind(&name)
    .bind(serde_json::to_value(Capabilities::default()).expect("capabilities serialise"))
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO project_secrets (project_id, engine_secret_enc, vault_key_enc) \
         VALUES ($1, $2, $3)",
    )
    .bind(id)
    .bind(&engine_enc)
    .bind(&vault_enc)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Provision outside the transaction: sandbox creation is not transactional, and holding a pg
    // transaction open across it would pin a connection for seconds.
    let mut project: Project = row.into();
    if let Err(e) = state.orch.provision(&id, &secrets).await {
        tracing::error!(project_id = %id, error = ?e, "provisioning failed after row insert");
        set_status(&state, &id, ProjectStatus::Error).await?;
        // Report what we just persisted. Returning the pre-update row would tell the caller
        // "stopped" while the database says "error", and the UI would offer a Start button for a
        // project that has no sandbox to start.
        project.status = ProjectStatus::Error;
    }

    Ok((axum::http::StatusCode::CREATED, Json(project)))
}

pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Vec<Project>>> {
    let rows: Vec<ProjectRow> = sqlx::query_as::<_, ProjectRow>(
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE owner_id = $1 ORDER BY created_at DESC",
    )
    .bind(user.id())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(Project::from).collect()))
}

pub async fn get_one(
    State(state): State<AppState>,
    scope: ProjectScope,
) -> ApiResult<Json<Project>> {
    // Reconcile the stored status against what the runtime actually reports, so a container that
    // died out from under us is not reported as running.
    let mut project = scope.project;
    if let Ok(observed) = state.orch.status(&project.id).await {
        if observed != project.status {
            set_status(&state, &project.id, observed).await?;
            project.status = observed;
        }
    }
    Ok(Json(project))
}

pub async fn update(
    State(state): State<AppState>,
    scope: ProjectScope,
    Json(body): Json<UpdateProject>,
) -> ApiResult<Json<Project>> {
    if let Some(name) = &body.name {
        validate_project_name(name).map_err(ApiError::BadRequest)?;
    }
    let caps = body
        .capabilities
        .map(|c| serde_json::to_value(c).expect("capabilities serialise"));

    // COALESCE keeps this a single statement while leaving omitted fields untouched.
    let row: ProjectRow = sqlx::query_as::<_, ProjectRow>(
        "UPDATE projects SET \
           name = COALESCE($3, name), \
           capabilities = COALESCE($4, capabilities), \
           updated_at = now() \
         WHERE id = $1 AND owner_id = $2 \
         RETURNING id, owner_id, name, capabilities, status, created_at, updated_at",
    )
    .bind(scope.project.id)
    .bind(scope.user.id())
    .bind(body.name.as_ref().map(|n| n.trim()))
    .bind(caps)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(row.into()))
}

pub async fn destroy(
    State(state): State<AppState>,
    scope: ProjectScope,
) -> ApiResult<axum::http::StatusCode> {
    // Tear down the runtime first. If this fails we keep the row, so the container cannot be
    // orphaned beyond our knowledge — an orphan we have no record of is an orphan nobody cleans up.
    state
        .orch
        .destroy(&scope.project.id)
        .await
        .map_err(ApiError::Internal)?;

    sqlx::query("DELETE FROM projects WHERE id = $1 AND owner_id = $2")
        .bind(scope.project.id)
        .bind(scope.user.id())
        .execute(&state.db)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn start(State(state): State<AppState>, scope: ProjectScope) -> ApiResult<Json<Project>> {
    set_status(&state, &scope.project.id, ProjectStatus::Starting).await?;
    state
        .orch
        .start(&scope.project.id)
        .await
        .map_err(ApiError::Internal)?;
    let observed = state
        .orch
        .status(&scope.project.id)
        .await
        .unwrap_or(ProjectStatus::Starting);
    set_status(&state, &scope.project.id, observed).await?;
    reload(&state, &scope).await
}

pub async fn stop(State(state): State<AppState>, scope: ProjectScope) -> ApiResult<Json<Project>> {
    state
        .orch
        .stop(&scope.project.id)
        .await
        .map_err(ApiError::Internal)?;
    set_status(&state, &scope.project.id, ProjectStatus::Stopped).await?;
    reload(&state, &scope).await
}

pub async fn restart(
    State(state): State<AppState>,
    scope: ProjectScope,
) -> ApiResult<Json<Project>> {
    state
        .orch
        .restart(&scope.project.id)
        .await
        .map_err(ApiError::Internal)?;
    let observed = state
        .orch
        .status(&scope.project.id)
        .await
        .unwrap_or(ProjectStatus::Starting);
    set_status(&state, &scope.project.id, observed).await?;
    reload(&state, &scope).await
}

async fn set_status(state: &AppState, id: &Uuid, status: ProjectStatus) -> ApiResult<()> {
    sqlx::query("UPDATE projects SET status = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(status.as_str())
        .execute(&state.db)
        .await?;
    Ok(())
}

async fn reload(state: &AppState, scope: &ProjectScope) -> ApiResult<Json<Project>> {
    let row: ProjectRow = sqlx::query_as::<_, ProjectRow>(
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE id = $1 AND owner_id = $2",
    )
    .bind(scope.project.id)
    .bind(scope.user.id())
    .fetch_one(&state.db)
    .await?;
    Ok(Json(row.into()))
}
