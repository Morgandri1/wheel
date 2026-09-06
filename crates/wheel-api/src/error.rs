//! The single error type crossing the HTTP boundary.
//!
//! Rule: the *client* sees a stable machine code and a generic message. The *operator* sees the
//! cause in the logs. Internal detail (SQL text, docker daemon replies, upstream URLs, anything
//! that might carry a secret) is logged, never serialised into a response body.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized(&'static str),

    /// Used for both "no such project" and "not your project". Deliberately one variant so the two
    /// cases cannot drift apart into an enumeration oracle.
    #[error("not found")]
    NotFound,

    #[error("forbidden")]
    Forbidden(&'static str),

    #[error("bad request")]
    BadRequest(String),

    #[error("conflict")]
    Conflict(String),

    #[error("payload too large")]
    PayloadTooLarge,

    #[error("rate limited")]
    RateLimited,

    #[error("upstream unavailable")]
    BadGateway(&'static str),

    #[error("upstream timed out")]
    GatewayTimeout,

    /// The engine answered an ingress request with a bodiless 404: it has no `/ingress/*` route at
    /// all. A blank 404 is indistinguishable from a mistyped path, which is exactly the confusion
    /// this replaces.
    #[error("ingress is not implemented by this engine")]
    IngressUnavailable,

    /// Anything unexpected. The inner error is logged and dropped from the response.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            // Note the message: it does not distinguish "no token" from "bad token" from
            // "expired token" to a client. Operators get the specific reason in the log.
            ApiError::Unauthorized(_) => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing or invalid authentication token.".into(),
            ),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "The requested resource does not exist.".into(),
            ),
            ApiError::Forbidden(_) => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "This operation is not permitted.".into(),
            ),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m.clone()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.clone()),
            ApiError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Request body exceeds the maximum allowed size.".into(),
            ),
            ApiError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many requests. Slow down.".into(),
            ),
            ApiError::BadGateway(_) => (
                StatusCode::BAD_GATEWAY,
                "bad_gateway",
                "The project engine is not reachable.".into(),
            ),
            ApiError::GatewayTimeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "gateway_timeout",
                "The project engine did not respond in time.".into(),
            ),
            ApiError::IngressUnavailable => (
                StatusCode::NOT_IMPLEMENTED,
                "ingress_unavailable",
                "This project's engine does not serve endpoints yet.".into(),
            ),
            ApiError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "An unexpected error occurred.".into(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();

        // Log the real cause exactly once, here, at a level matching its severity.
        match &self {
            ApiError::Internal(e) => {
                tracing::error!(error = %format_args!("{e:#}"), code, "request failed")
            }
            ApiError::Unauthorized(why) => tracing::debug!(reason = why, "auth rejected"),
            ApiError::Forbidden(why) => tracing::debug!(reason = why, "forbidden"),
            ApiError::BadGateway(why) => tracing::warn!(reason = why, "upstream unavailable"),
            _ => tracing::debug!(code, "request rejected"),
        }

        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // RowNotFound is a legitimate 404 rather than a server fault.
        match e {
            sqlx::Error::RowNotFound => ApiError::NotFound,
            other => ApiError::Internal(anyhow::Error::new(other).context("database")),
        }
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
