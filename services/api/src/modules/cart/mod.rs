use axum::{extract::State, http::StatusCode, routing::{get, post}, Json, Router};
use crate::{errors::{ApiError, ApiResult}, response::ApiResponse, state::SharedState};

// Server-side cart is intentionally thin: totals are always recalculated from
// products at order-creation time. This endpoint validates items without trusting prices.
pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/cart/quote", post(quote)).route("/api/v1/cart/health", get(|| async { (StatusCode::OK, "ok") }))
}

#[derive(serde::Deserialize)]
struct QuoteReq { items: Vec<QuoteItem> }
#[derive(serde::Deserialize)]
struct QuoteItem { product_id: uuid::Uuid, variant_id: Option<uuid::Uuid>, qty: i32 }

async fn quote(State(s): State<SharedState>, Json(body): Json<QuoteReq>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if body.items.is_empty() || body.items.len() > 50 {
        return Err(ApiError::Unprocessable("cart must have 1..50 lines".into()));
    }
    let mut subtotal: i64 = 0;
    for it in &body.items {
        if it.qty < 1 || it.qty > 99 {
            return Err(ApiError::Unprocessable("qty must be 1..99".into()));
        }
        let rec: Option<(i64, bool)> = sqlx::query_as("SELECT price_cents, active FROM products WHERE id=$1")
            .bind(it.product_id).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
        let Some((price, active)) = rec else { return Err(ApiError::Unprocessable("unknown product".into())) };
        if !active {
            return Err(ApiError::Unprocessable("product inactive".into()));
        }
        // Variant pricing hook: if variant supplied, use its delta (else 0).
        let delta: i64 = if let Some(vid) = it.variant_id {
            sqlx::query_scalar("SELECT COALESCE(price_delta_cents,0) FROM product_variants WHERE id=$1 AND product_id=$2")
                .bind(vid).bind(it.product_id).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?
                .unwrap_or(0)
        } else { 0 };
        subtotal += (price + delta) * (it.qty as i64);
    }
    let (st, Json(inner)) = ApiResponse::ok(serde_json::json!({"currency": "ZAR", "subtotal_cents": subtotal}));
    Ok((st, Json(inner.data)))
}
