use crate::{errors::ApiError, middleware::authentication::AuthUser};
use axum::{extract::Request, middleware::Next, response::Response};

pub async fn require_admin(req: Request, next: Next) -> Result<Response, ApiError> {
    let u = req
        .extensions()
        .get::<AuthUser>()
        .ok_or(ApiError::Unauthorized)?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    Ok(next.run(req).await)
}

pub fn ensure_owner_or_admin(auth: &AuthUser, owner: &uuid::Uuid) -> Result<(), ApiError> {
    if auth.role == "admin" || &auth.user_id == owner {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}
