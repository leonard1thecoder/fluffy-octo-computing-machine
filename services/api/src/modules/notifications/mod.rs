use crate::{
    errors::{ApiError, ApiResult},
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/admin/notifications/outbox", get(outbox))
}

async fn outbox(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(uuid::Uuid, String, String, i32)> = sqlx::query_as("SELECT id, recipient, status::text, attempts FROM email_outbox ORDER BY created_at DESC LIMIT 100")
        .fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(|(id, r, st, a)| serde_json::json!({"id": id, "recipient": r, "status": st, "attempts": a})).collect();
    let (st, Json(inner)) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(inner.data).unwrap())))
}
