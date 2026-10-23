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
    let rows: Vec<(uuid::Uuid, String, String, String, i32, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as("SELECT id, recipient, subject, status::text, attempts, last_error, created_at FROM email_outbox ORDER BY created_at DESC LIMIT 100")
        .fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(|(id, recipient, subject, status, attempts, last_error, created_at)| serde_json::json!({"id": id, "recipient": recipient, "subject": subject, "status": status, "attempts": attempts, "last_error": last_error, "created_at": created_at})).collect();
    let (st, Json(inner)) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(inner.data).unwrap())))
}
