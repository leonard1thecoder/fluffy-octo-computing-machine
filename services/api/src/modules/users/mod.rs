use crate::{
    errors::{ApiError, ApiResult},
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use uuid::Uuid;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/users/:id", get(get_user))
}

async fn get_user(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await?;
    if me.role != "admin" && me.user_id != id {
        return Err(ApiError::Forbidden);
    }
    let rec: Option<(String, String, String)> =
        sqlx::query_as("SELECT email, full_name, role::text FROM users WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let Some((email, full_name, role)) = rec else {
        return Err(ApiError::NotFound);
    };
    let (st, Json(inner)) = ApiResponse::ok(
        serde_json::json!({"id": id, "email": email, "full_name": full_name, "role": role}),
    );
    Ok((st, Json(inner.data)))
}
