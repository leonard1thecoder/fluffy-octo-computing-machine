use crate::{
    errors::{ApiError, ApiResult},
    response::ApiResponse,
    state::SharedState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/catering/quotes/:id", get(get_quote))
}

async fn get_quote(
    State(s): State<SharedState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let rec: Option<(i64, String)> =
        sqlx::query_as("SELECT total_cents, status::text FROM catering_quotes WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let Some((total, status)) = rec else {
        return Err(ApiError::NotFound);
    };
    let items: Vec<(String, i32, i64)> = sqlx::query_as(
        "SELECT description, qty, unit_price_cents FROM catering_quote_items WHERE quote_id=$1",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let lines: Vec<_> = items
        .into_iter()
        .map(|(d, q, p)| serde_json::json!({"description": d, "qty": q, "unit_price_cents": p}))
        .collect();
    let (st, Json(inner)) = ApiResponse::ok(
        serde_json::json!({"id": id, "total_cents": total, "status": status, "items": lines}),
    );
    Ok((st, Json(inner.data)))
}
