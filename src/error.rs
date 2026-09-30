//! The error type every HTTP handler returns.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("missing or invalid credentials")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        let message = match &self {
            ApiError::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                "internal error".to_string()
            }
            other => other.to_string(),
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}

// axum's own extractor failures (bad JSON, unknown query values) use the same error shape.
impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        ApiError::BadRequest(r.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(r: QueryRejection) -> Self {
        ApiError::BadRequest(r.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(r: PathRejection) -> Self {
        ApiError::BadRequest(r.body_text())
    }
}

impl From<crate::wiki::WikiError> for ApiError {
    fn from(err: crate::wiki::WikiError) -> Self {
        use crate::wiki::WikiError;
        match err {
            WikiError::InvalidPath(msg) => ApiError::BadRequest(msg),
            WikiError::NotFound => ApiError::NotFound,
            other => ApiError::Internal(other.into()),
        }
    }
}
