use axum::{extract::{Request, State}, http::header, middleware::Next, response::Response};
use crate::{errors::ApiError, modules::auth::jwt::verify_access, state::SharedState};

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user_id: uuid::Uuid,
    pub role: String,
    pub token_id: String,
}

pub async fn user_from_headers(s: &SharedState, headers: &axum::http::HeaderMap) -> Result<AuthUser, ApiError> {
    let hdr = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    let token = hdr.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
    let claims = verify_access(token, &s.cfg).map_err(|_| ApiError::Unauthorized)?;
    let rec: Option<(String, String)> = sqlx::query_as("SELECT role::text, status::text FROM users WHERE id=$1")
        .bind(claims.sub)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let (role, status) = rec.ok_or(ApiError::Unauthorized)?;
    if status != "active" {
        return Err(ApiError::Unauthorized);
    }
    Ok(AuthUser { user_id: claims.sub, role, token_id: claims.jti })
}

pub async fn require_auth(State(s): State<SharedState>, mut req: Request, next: Next) -> Result<Response, ApiError> {
    let hdr = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let token = hdr.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?.to_string();
    let claims = verify_access(&token, &s.cfg).map_err(|_| ApiError::Unauthorized)?;
    let rec: Option<(String, String)> = sqlx::query_as("SELECT role::text, status::text FROM users WHERE id=$1")
        .bind(claims.sub)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let (role, status) = rec.ok_or(ApiError::Unauthorized)?;
    if status != "active" {
        return Err(ApiError::Unauthorized);
    }
    req.extensions_mut().insert(AuthUser { user_id: claims.sub, role, token_id: claims.jti });
    Ok(next.run(req).await)
}
