use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("validation failed")]
    Validation { fields: serde_json::Value },
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unprocessable: {0}")]
    Unprocessable(String),
    #[error("rate limited")]
    RateLimited { retry_after_secs: u64 },
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("internal error")]
    Internal,
}

#[derive(Serialize)]
struct ErrBody {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<serde_json::Value>,
    request_id: String,
}

impl ApiError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "VALIDATION_ERROR",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::NotFound => "NOT_FOUND",
            Self::Conflict(_) => "CONFLICT",
            Self::Unprocessable(_) => "UNPROCESSABLE",
            Self::RateLimited { .. } => "RATE_LIMITED",
            Self::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            Self::Internal => "INTERNAL_ERROR",
        }
    }
    fn status(&self) -> StatusCode {
        match self {
            Self::Validation { .. } => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let request_id = crate::middleware::request_id::current_request_id();
        let (fields, message) = match &self {
            ApiError::Validation { fields } => (Some(fields.clone()), "validation failed".into()),
            ApiError::Conflict(m) | ApiError::Unprocessable(m) => (None, m.clone()),
            ApiError::RateLimited { .. } => (None, "rate limit exceeded".into()),
            _ => (None, self.to_string()),
        };
        if matches!(self, ApiError::Internal) {
            tracing::error!(request_id = %request_id, "internal error");
        }
        let mut res = (
            self.status(),
            Json(ErrBody { code: self.code(), message, fields, request_id }),
        )
            .into_response();
        if let ApiError::RateLimited { retry_after_secs } = &self {
            res.headers_mut().insert(
                "retry-after",
                retry_after_secs.to_string().parse().unwrap(),
            );
        }
        res
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
