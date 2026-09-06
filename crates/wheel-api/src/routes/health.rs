use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::json;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unauthenticated liveness probe. Deliberately reveals nothing about internals.
pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

/// How long a host liveness answer is reused.
///
/// This route is unauthenticated, and each call would otherwise become a request to the host — a
/// small amplifier pointed at the one machine every tenant's sandbox runs on. Answering from a
/// cache bounds a flood to one upstream probe per second, which is far finer-grained than any
/// deploy gate needs.
const HOST_LIVENESS_TTL_SECS: i64 = 1;

static LAST_CHECK_UNIX: AtomicI64 = AtomicI64::new(0);
static LAST_RESULT_OK: AtomicU64 = AtomicU64::new(0);

/// Liveness of the sandbox host, as seen from the API.
///
/// The host has no public domain by design, so nothing outside this process can ask it whether it
/// is up — which made a post-deploy check impossible to write without either exposing the host or
/// trusting that a green API implies a green host. It does not: the API stayed healthy through an
/// outage where the host was stopped and every project create hung.
///
/// Liveness only. No backend name, no project counts, no error text from upstream: an
/// unauthenticated caller learns whether the pair is serving and nothing else.
pub async fn host_healthz(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let last = LAST_CHECK_UNIX.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < HOST_LIVENESS_TTL_SECS {
        return answer(LAST_RESULT_OK.load(Ordering::Relaxed) == 1);
    }

    let ok = state.orch.host_alive().await.is_ok();
    LAST_RESULT_OK.store(u64::from(ok), Ordering::Relaxed);
    LAST_CHECK_UNIX.store(now, Ordering::Relaxed);
    answer(ok)
}

fn answer(ok: bool) -> (StatusCode, Json<serde_json::Value>) {
    if ok {
        (StatusCode::OK, Json(json!({ "ok": true })))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false })),
        )
    }
}
