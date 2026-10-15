use crate::{
    errors::{ApiError, ApiResult},
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch},
    Json, Router,
};
use serde::Deserialize;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/admin/orders", get(list_orders))
        .route(
            "/api/v1/admin/orders/{id}/fulfilment",
            patch(set_fulfilment),
        )
        .route("/api/v1/admin/reports/summary", get(summary))
        .route("/api/v1/admin/audit-logs", get(audit))
}

async fn list_orders(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(uuid::Uuid, String, i64, String, String)> = sqlx::query_as(
        "SELECT id, reference, total_cents, payment_status::text, fulfilment_status::text FROM orders ORDER BY created_at DESC LIMIT 100",
    ).fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(|(id, r, t, ps, fs)| serde_json::json!({"id": id, "reference": r, "total_cents": t, "payment_status": ps, "fulfilment_status": fs})).collect();
    let (st, Json(inner)) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(inner.data).unwrap())))
}

#[derive(Deserialize)]
struct Fulfil {
    status: String,
}

async fn set_fulfilment(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<Fulfil>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    if !["new", "preparing", "ready", "delivered", "cancelled"].contains(&body.status.as_str()) {
        return Err(ApiError::Unprocessable("bad fulfilment status".into()));
    }
    sqlx::query("UPDATE orders SET fulfilment_status=$2::fulfilment_status WHERE id=$1")
        .bind(id)
        .bind(&body.status)
        .execute(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    sqlx::query("INSERT INTO audit_logs (actor_user_id, action, entity, entity_id) VALUES ($1,'order.fulfilment','order',$2)")
        .bind(u.user_id).bind(id.to_string()).execute(&s.db).await.map_err(|_| ApiError::Internal)?;
    let (st, Json(inner)) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(inner.data)))
}

async fn summary(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let revenue: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(SUM(total_cents),0) FROM orders WHERE payment_status='paid'",
    )
    .fetch_one(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orders")
        .fetch_one(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let (st, Json(inner)) = ApiResponse::ok(
        serde_json::json!({"revenue_cents": revenue.unwrap_or(0), "orders": orders, "currency": "ZAR"}),
    );
    Ok((st, Json(inner.data)))
}

async fn audit(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT id, action, entity FROM audit_logs ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, a, e)| serde_json::json!({"id": id, "action": a, "entity": e}))
        .collect();
    let (st, Json(inner)) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(inner.data).unwrap())))
}
