//! Project CRUD and lifecycle.
//!
//! Note the handler signatures: any handler that acts on an existing project takes `ProjectScope`,
//! which cannot be constructed without a verified token and a proven ownership match. Handlers
//! that operate on the collection take `AuthUser`. No handler takes a bare project id.

use crate::auth::{AuthUser, ProjectScope};
use crate::crypto;
use crate::db::Db;
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
    let count: i64 = crate::db_scalar!(
        &state.db,
        "SELECT count(*) FROM projects WHERE owner_id = $1",
        user.id()
    )?;
    if count >= state.cfg.max_projects_per_user {
        return Err(ApiError::Conflict(format!(
            "project limit reached ({} per user)",
            state.cfg.max_projects_per_user
        )));
    }

    let id = Uuid::new_v4();
    let secrets = EngineSecrets {
        engine_secret: crypto::generate_secret(),
        vault_key: crypto::generate_vault_key(),
    };
    let engine_enc = crypto::seal(&state.cfg.master_key, &secrets.engine_secret)?;
    let vault_enc = crypto::seal(&state.cfg.master_key, &secrets.vault_key)?;

    // Row and secrets are written atomically: a project without secrets could never start, and a
    // secret row without a project would be an orphan holding key material.
    //
    // Spelled out per backend rather than through the dispatch macros because a transaction is a
    // connection, not a pool, and sqlx types it per database.
    const INSERT_PROJECT: &str = "INSERT INTO projects (id, owner_id, name, capabilities, status) \
         VALUES ($1, $2, $3, $4, 'stopped') \
         RETURNING id, owner_id, name, capabilities, status, created_at, updated_at";
    const INSERT_SECRETS: &str =
        "INSERT INTO project_secrets (project_id, engine_secret_enc, vault_key_enc) \
         VALUES ($1, $2, $3)";
    let caps = serde_json::to_value(Capabilities::default()).expect("capabilities serialise");

    let row: ProjectRow = match &state.db {
        Db::Pg(pool) => {
            let mut tx = pool.begin().await?;
            let row = sqlx::query_as::<_, ProjectRow>(INSERT_PROJECT)
                .bind(id)
                .bind(user.id())
                .bind(&name)
                .bind(&caps)
                .fetch_one(&mut *tx)
                .await?;
            sqlx::query(INSERT_SECRETS)
                .bind(id)
                .bind(&engine_enc)
                .bind(&vault_enc)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            row
        }
        Db::Sqlite(pool) => {
            let mut tx = pool.begin().await?;
            let row = sqlx::query_as::<_, ProjectRow>(INSERT_PROJECT)
                .bind(id)
                .bind(user.id())
                .bind(&name)
                .bind(&caps)
                .fetch_one(&mut *tx)
                .await?;
            sqlx::query(INSERT_SECRETS)
                .bind(id)
                .bind(&engine_enc)
                .bind(&vault_enc)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            row
        }
    };

    // Provision outside the transaction: sandbox creation is not transactional, and holding a pg
    // transaction open across it would pin a connection for seconds.
    let mut project: Project = row.into();
    project.status = match state.orch.provision(&id, &secrets).await {
        // A new project comes up running (ARCHITECTURE M1: "create project -> sandbox starts"). The
        // first thing anyone does after signing up is create a project, and a project whose engine
        // answers nothing is indistinguishable from a broken install.
        Ok(()) => start_and_observe(&state, &id)
            .await
            .unwrap_or(ProjectStatus::Error),
        Err(e) => {
            tracing::error!(project_id = %id, error = ?e, "provisioning failed after row insert");
            set_status(&state, &id, ProjectStatus::Error).await?;
            ProjectStatus::Error
        }
    };

    Ok((axum::http::StatusCode::CREATED, Json(project)))
}

pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Vec<Project>>> {
    let rows: Vec<ProjectRow> = crate::db_fetch_all!(
        &state.db,
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE owner_id = $1 ORDER BY created_at DESC",
        user.id()
    )?;
    Ok(Json(
        rows.into_iter()
            .map(|r| Project::from(r).with_ingress_base(&state.cfg.public_base_url))
            .collect(),
    ))
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
    Ok(Json(project.with_ingress_base(&state.cfg.public_base_url)))
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
    const PG: &str = "UPDATE projects SET \
           name = COALESCE($3, name), \
           capabilities = COALESCE($4, capabilities), \
           updated_at = now() \
         WHERE id = $1 AND owner_id = $2 \
         RETURNING id, owner_id, name, capabilities, status, created_at, updated_at";
    const SQLITE: &str = "UPDATE projects SET \
           name = COALESCE($3, name), \
           capabilities = COALESCE($4, capabilities), \
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE id = $1 AND owner_id = $2 \
         RETURNING id, owner_id, name, capabilities, status, created_at, updated_at";
    let row: ProjectRow = crate::db_fetch_one!(
        &state.db,
        state.db.pick(PG, SQLITE),
        scope.project.id,
        scope.user.id(),
        body.name.as_ref().map(|n| n.trim()),
        caps
    )?;
    Ok(Json(
        Project::from(row).with_ingress_base(&state.cfg.public_base_url),
    ))
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

    crate::db_execute!(
        &state.db,
        "DELETE FROM projects WHERE id = $1 AND owner_id = $2",
        scope.project.id,
        scope.user.id()
    )?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Decrypt this project's engine secrets, in the encoding the engine spawn contract requires.
async fn load_secrets(state: &AppState, id: &Uuid) -> ApiResult<EngineSecrets> {
    let row: (Vec<u8>, Vec<u8>) = crate::db_fetch_one!(
        &state.db,
        "SELECT engine_secret_enc, vault_key_enc FROM project_secrets WHERE project_id = $1",
        id
    )?;
    let engine_secret = crypto::open(&state.cfg.master_key, &row.0).map_err(ApiError::Internal)?;
    let vault_key = crypto::open(&state.cfg.master_key, &row.1).map_err(ApiError::Internal)?;
    let vault_key = crypto::canonical_vault_key(&vault_key).map_err(ApiError::Internal)?;
    Ok(EngineSecrets {
        engine_secret,
        vault_key,
    })
}

/// Re-send this project's secrets to the host before starting it.
///
/// `PUT` is idempotent by contract, so this costs one call and buys two things: a project whose
/// key was provisioned in an encoding the engine could not decode heals on its next start, and a
/// host that came up on an empty volume gets its record back instead of starting a keyless engine.
async fn reprovision(state: &AppState, id: &Uuid) -> ApiResult<()> {
    let secrets = load_secrets(state, id).await?;
    state
        .orch
        .provision(id, &secrets)
        .await
        .map_err(ApiError::Internal)
}

pub async fn start(State(state): State<AppState>, scope: ProjectScope) -> ApiResult<Json<Project>> {
    reprovision(&state, &scope.project.id).await?;
    start_and_observe(&state, &scope.project.id).await?;
    reload(&state, &scope).await
}

/// Start a project's sandbox and persist the status the runtime actually reports.
///
/// A start the host accepted, followed by a status of `stopped`, is not a stopped project — it is a
/// disagreement between the two, and reporting "stopped" invites the caller into a poll loop that
/// will never terminate. It reads as `error`, so the UI shows something is wrong rather than
/// something is pending.
async fn start_and_observe(state: &AppState, id: &Uuid) -> ApiResult<ProjectStatus> {
    set_status(state, id, ProjectStatus::Starting).await?;
    if let Err(e) = state.orch.start(id).await {
        tracing::error!(project_id = %id, error = ?e, "starting the sandbox failed");
        set_status(state, id, ProjectStatus::Error).await?;
        return Err(ApiError::Internal(e));
    }
    let observed = match state.orch.status(id).await {
        Ok(ProjectStatus::Stopped) => {
            tracing::warn!(
                project_id = %id,
                "host reported a successful start but the sandbox is still stopped"
            );
            ProjectStatus::Error
        }
        Ok(other) => other,
        Err(e) => {
            tracing::warn!(project_id = %id, error = ?e, "status probe after start failed");
            ProjectStatus::Starting
        }
    };
    set_status(state, id, observed).await?;
    Ok(observed)
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
    reprovision(&state, &scope.project.id).await?;
    state
        .orch
        .restart(&scope.project.id)
        .await
        .map_err(ApiError::Internal)?;
    // A start that the host reported as successful, followed by a status of `stopped`, is not a
    // stopped project — it is an inconsistency between the two, and reporting "stopped" invites the
    // caller to sit in a poll loop that will never terminate. Surface it as `error` so the UI shows
    // something is wrong instead of something is pending.
    let observed = match state.orch.status(&scope.project.id).await {
        Ok(ProjectStatus::Stopped) => {
            tracing::warn!(
                project_id = %scope.project.id,
                "host reported a successful start but the sandbox is still stopped"
            );
            ProjectStatus::Error
        }
        Ok(other) => other,
        Err(e) => {
            tracing::warn!(project_id = %scope.project.id, error = ?e, "status probe after start failed");
            ProjectStatus::Starting
        }
    };
    set_status(&state, &scope.project.id, observed).await?;
    reload(&state, &scope).await
}

async fn set_status(state: &AppState, id: &Uuid, status: ProjectStatus) -> ApiResult<()> {
    const PG: &str = "UPDATE projects SET status = $2, updated_at = now() WHERE id = $1";
    const SQLITE: &str =
        "UPDATE projects SET status = $2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = $1";
    crate::db_execute!(&state.db, state.db.pick(PG, SQLITE), id, status.as_str())?;
    Ok(())
}

async fn reload(state: &AppState, scope: &ProjectScope) -> ApiResult<Json<Project>> {
    let row: ProjectRow = crate::db_fetch_one!(
        &state.db,
        "SELECT id, owner_id, name, capabilities, status, created_at, updated_at \
         FROM projects WHERE id = $1 AND owner_id = $2",
        scope.project.id,
        scope.user.id()
    )?;
    Ok(Json(
        Project::from(row).with_ingress_base(&state.cfg.public_base_url),
    ))
}
