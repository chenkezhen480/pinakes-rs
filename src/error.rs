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
    #[error("{0}")]
    Unprocessable(String),
    #[error("{0}")]
    UnsupportedMediaType(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    BadGateway(String),
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
            ApiError::Unprocessable(_) => (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable"),
            ApiError::UnsupportedMediaType(_) => {
                (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
            }
            ApiError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            ApiError::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            ApiError::BadGateway(_) => (StatusCode::BAD_GATEWAY, "bad_gateway"),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl ApiError {
    /// The error body, `{"error": {"code", "message"}}`; an internal error is logged and its
    /// details are left out.
    pub fn body(&self) -> serde_json::Value {
        let (_, code) = self.status_and_code();
        let message = match self {
            ApiError::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                "internal error".to_string()
            }
            other => other.to_string(),
        };
        json!({ "error": { "code": code, "message": message } })
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status_and_code().0, Json(self.body())).into_response()
    }
}

// axum's own extractor failures (bad JSON, unknown query values) use the same error shape.
impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        match r.status() {
            StatusCode::PAYLOAD_TOO_LARGE => ApiError::TooLarge(r.body_text()),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => ApiError::UnsupportedMediaType(r.body_text()),
            _ => ApiError::BadRequest(r.body_text()),
        }
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
