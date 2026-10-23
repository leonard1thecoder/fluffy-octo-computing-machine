use crate::{
    errors::ApiError, errors::ApiResult, middleware::authentication::user_from_headers,
    response::ApiResponse, state::SharedState,
};
use axum::{
    extract::{Path, Query, State},
    http::{header::HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use uuid::Uuid;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/v1/products/{id}/favorite",
            post(add_favorite).delete(remove_favorite),
        )
        .route("/api/v1/products/favorites", get(list_favorites))
}

#[derive(Serialize)]
struct FavoriteProduct {
    id: Uuid,
    name: String,
    slug: String,
    price_cents: i64,
    currency: String,
    active: bool,
}

async fn add_favorite(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;

    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM products WHERE id=$1 AND active=true")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    if exists.is_none() {
        return Err(ApiError::NotFound);
    }

    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO product_favorites (user_id, product_id) VALUES ($1,$2) ON CONFLICT (user_id, product_id) DO NOTHING RETURNING id",
    )
    .bind(u.user_id)
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let favorited = inserted.is_some();
    let (st, inner) = ApiResponse::ok(serde_json::json!({
        "product_id": id,
        "favorited": favorited,
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn remove_favorite(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;

    let deleted: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM product_favorites WHERE user_id=$1 AND product_id=$2 RETURNING id",
    )
    .bind(u.user_id)
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    if deleted.is_none() {
        return Err(ApiError::NotFound);
    }
    let (st, inner) = ApiResponse::ok(serde_json::json!({"product_id": id, "favorited": false}));
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn list_favorites(
    State(s): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;

    let rows: Vec<(Uuid, String, String, i64, String, bool)> = sqlx::query_as(
        "SELECT p.id, p.name, p.slug, p.price_cents, p.currency, p.active FROM products p
         JOIN product_favorites f ON f.product_id = p.id
         WHERE f.user_id = $1 AND p.active = true
         ORDER BY f.created_at DESC LIMIT 100",
    )
    .bind(u.user_id)
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<FavoriteProduct> = rows
        .into_iter()
        .map(
            |(id, name, slug, price_cents, currency, active)| FavoriteProduct {
                id,
                name,
                slug,
                price_cents,
                currency,
                active,
            },
        )
        .collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}
