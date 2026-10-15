use axum::{Json, Router, extract::State, http::StatusCode, routing::{get, post}};
use serde::Deserialize;
use crate::{errors::{ApiError, ApiResult}, middleware::authentication::user_from_headers, response::ApiResponse, state::SharedState};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/admin/inventory/movements", get(list))
        .route("/api/v1/admin/inventory/adjust", post(adjust))
}

async fn list(State(s): State<SharedState>, headers: axum::http::HeaderMap) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" { return Err(ApiError::Forbidden); }
    let rows: Vec<(uuid::Uuid, uuid::Uuid, i32, String)> = sqlx::query_as("SELECT id, product_id, qty_delta, reason FROM inventory_movements ORDER BY created_at DESC LIMIT 100")
        .fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(|(id, pid, d, r)| serde_json::json!({"id": id, "product_id": pid, "qty_delta": d, "reason": r})).collect();
    let (st, b) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(b.data).unwrap())))
}

#[derive(Deserialize)]
struct Adjust { product_id: uuid::Uuid, qty_delta: i32, reason: String }

async fn adjust(State(s): State<SharedState>, headers: axum::http::HeaderMap, Json(body): Json<Adjust>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" { return Err(ApiError::Forbidden); }
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    // Atomic stock update with negative-stock guard.
    let res: Option<i32> = sqlx::query_scalar(
        "UPDATE products SET stock_on_hand = COALESCE(stock_on_hand,0)+$2 WHERE id=$1 AND COALESCE(stock_on_hand,0)+$2 >= 0 RETURNING stock_on_hand",
    ).bind(body.product_id).bind(body.qty_delta).fetch_optional(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    if res.is_none() {
        return Err(ApiError::Unprocessable("insufficient stock or unknown product".into()));
    }
    sqlx::query("INSERT INTO inventory_movements (product_id, qty_delta, reason, created_by) VALUES ($1,$2,$3,$4)")
        .bind(body.product_id).bind(body.qty_delta).bind(&body.reason).bind(u.user_id)
        .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (st, b) = ApiResponse::ok(serde_json::json!({"ok": true, "stock_on_hand": res}));
    Ok((st, Json(b.data)))
}
