use axum::{Json, Router, extract::{Path, Query, State}, http::StatusCode, routing::{get, post}};
use serde::{Deserialize, Serialize};
use validator::Validate;
use crate::{errors::{ApiError, ApiResult}, middleware::authentication::user_from_headers, response::ApiResponse, state::SharedState};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/products", get(list).post(create))
        .route("/api/v1/products/:slug", get(get_by_slug))
        .route("/api/v1/products/:id/availability", get(availability))
}

#[derive(Serialize)]
struct Product { id: uuid::Uuid, name: String, slug: String, price_cents: i64, currency: String, active: bool }

#[derive(Deserialize)]
#[allow(dead_code)]
struct ListQ { search: Option<String>, category: Option<String>, page: Option<i64>, per_page: Option<i64>, sort: Option<String> }

async fn list(State(s): State<SharedState>, Query(q): Query<ListQ>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let page = q.page.unwrap_or(1).max(1);
    let per = q.per_page.unwrap_or(20).clamp(1, 100);
    let off = (page - 1) * per;
    let rows: Vec<(uuid::Uuid, String, String, i64, String, bool)> = sqlx::query_as(
        "SELECT id, name, slug, price_cents, currency, active FROM products WHERE active=true AND ($1 IS NULL OR name ILIKE '%'||$1||'%') ORDER BY name LIMIT $2 OFFSET $3",
    )
    .bind(q.search.clone()).bind(per).bind(off).fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM products WHERE active=true").fetch_one(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<Product> = rows.into_iter().map(|(id, name, slug, price_cents, currency, active)| Product { id, name, slug, price_cents, currency, active }).collect();
    Ok((StatusCode::OK, Json(serde_json::json!({"data": items, "meta": {"page": page, "per_page": per, "total": total}}))))
}

async fn get_by_slug(State(s): State<SharedState>, Path(slug): Path<String>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if slug.len() > 160 || slug.is_empty() { return Err(ApiError::NotFound); }
    let rec: Option<(uuid::Uuid, String, String, i64)> = sqlx::query_as("SELECT id, name, slug, price_cents FROM products WHERE slug=$1 AND active=true")
        .bind(&slug).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((id, name, slug, price_cents)) = rec else { return Err(ApiError::NotFound) };
    let (st, b) = ApiResponse::ok(serde_json::json!({"id": id, "name": name, "slug": slug, "price_cents": price_cents}));
    Ok((st, Json(b.data)))
}

async fn availability(State(s): State<SharedState>, Path(id): Path<uuid::Uuid>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let rec: Option<(bool, Option<i32>)> = sqlx::query_as("SELECT active, stock_on_hand FROM products WHERE id=$1")
        .bind(id).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((active, stock)) = rec else { return Err(ApiError::NotFound) };
    let (st, b) = ApiResponse::ok(serde_json::json!({"active": active, "stock_on_hand": stock}));
    Ok((st, Json(b.data)))
}

#[derive(Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min=1, max=160))] name: String,
    #[validate(length(min=1, max=180))] slug: String,
    #[validate(range(min=0))] price_cents: i64,
    category_id: Option<uuid::Uuid>,
}

async fn create(State(s): State<SharedState>, headers: axum::http::HeaderMap, Json(body): Json<CreateReq>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" { return Err(ApiError::Forbidden); }
    body.validate().map_err(|e| ApiError::Validation { fields: serde_json::json!(e.to_string()) })?;
    if body.price_cents < 0 { return Err(ApiError::Unprocessable("price must be >= 0".into())); }
    let id: uuid::Uuid = sqlx::query_scalar("INSERT INTO products (name, slug, price_cents, currency, active, category_id) VALUES ($1,$2,$3,'ZAR',true,$4) RETURNING id")
        .bind(&body.name).bind(&body.slug).bind(body.price_cents).bind(body.category_id)
        .fetch_one(&s.db).await.map_err(|_| ApiError::Conflict("slug exists".into()))?;
    let (st, b) = ApiResponse::created(serde_json::json!({"id": id}));
    Ok((st, Json(b.data)))
}
