use crate::{
    errors::{ApiError, ApiResult},
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/products", get(list).post(create))
        .route("/api/v1/products/{slug}", get(get_by_slug))
        .route("/api/v1/products/{id}/availability", get(availability))
}

#[derive(Serialize, sqlx::FromRow)]
struct Product {
    id: uuid::Uuid,
    name: String,
    slug: String,
    price_cents: i64,
    currency: String,
    active: bool,
    description: String,
    image_url: Option<String>,
    /// Number of accounts that have favourited this product.
    favorite_count: i64,
}

#[derive(Serialize, sqlx::FromRow)]
struct ProductDetail {
    id: uuid::Uuid,
    name: String,
    slug: String,
    price_cents: i64,
    description: String,
    image_url: Option<String>,
    /// Number of accounts that have favourited this product.
    favorite_count: i64,
}

#[derive(Deserialize)]
struct ListQ {
    search: Option<String>,
    category: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
    sort: Option<String>,
}

/// FROM and WHERE for the product list and its total, so the total always counts the rows
/// that are shown. A category matches by slug, and a top-level category also includes its
/// sub-categories.
const PRODUCT_FROM: &str = "FROM products p
    LEFT JOIN categories c ON c.id = p.category_id
    LEFT JOIN categories pc ON pc.id = c.parent_id";
const PRODUCT_FILTERS: &str = "p.active = true
    AND ($1::text IS NULL OR p.name ILIKE '%' || $1 || '%')
    AND ($2::text IS NULL OR c.slug = $2 OR pc.slug = $2)";

async fn list(
    State(s): State<SharedState>,
    Query(q): Query<ListQ>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let page = q.page.unwrap_or(1).max(1);
    let per = q.per_page.unwrap_or(20).clamp(1, 100);
    let off = (page - 1) * per;
    let most_liked = match q.sort.as_deref() {
        None | Some("") | Some("name") => false,
        Some("most_liked") => true,
        Some(_) => {
            return Err(ApiError::Unprocessable(
                "sort must be name or most_liked".into(),
            ))
        }
    };
    let search = q.search;
    let category = q
        .category
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned);

    let list_sql = format!(
        "SELECT p.id, p.name, p.slug, p.price_cents, p.currency, p.active, p.description, p.image_url, fav.n AS favorite_count \
         {PRODUCT_FROM} \
         CROSS JOIN LATERAL (SELECT COUNT(*) AS n FROM product_favorites f WHERE f.product_id = p.id) fav \
         WHERE {PRODUCT_FILTERS} \
         ORDER BY CASE WHEN $3 THEN fav.n END DESC NULLS LAST, p.name, p.id \
         LIMIT $4 OFFSET $5"
    );
    let items: Vec<Product> = sqlx::query_as(&list_sql)
        .bind(search.as_deref())
        .bind(category.as_deref())
        .bind(most_liked)
        .bind(per)
        .bind(off)
        .fetch_all(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;

    let count_sql = format!("SELECT COUNT(*) {PRODUCT_FROM} WHERE {PRODUCT_FILTERS}");
    let total: i64 = sqlx::query_scalar(&count_sql)
        .bind(search.as_deref())
        .bind(category.as_deref())
        .fetch_one(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    Ok((
        StatusCode::OK,
        Json(
            serde_json::json!({"data": items, "meta": {"page": page, "per_page": per, "total": total}}),
        ),
    ))
}

async fn get_by_slug(
    State(s): State<SharedState>,
    Path(slug): Path<String>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if slug.len() > 160 || slug.is_empty() {
        return Err(ApiError::NotFound);
    }
    let rec: Option<ProductDetail> = sqlx::query_as(
        "SELECT p.id, p.name, p.slug, p.price_cents, p.description, p.image_url, (SELECT COUNT(*) FROM product_favorites f WHERE f.product_id = p.id) AS favorite_count FROM products p WHERE p.slug=$1 AND p.active=true",
    )
    .bind(&slug)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let Some(product) = rec else {
        return Err(ApiError::NotFound);
    };
    let (st, Json(inner)) = ApiResponse::ok(serde_json::json!(product));
    Ok((st, Json(inner.data)))
}

async fn availability(
    State(s): State<SharedState>,
    Path(id): Path<uuid::Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let rec: Option<(bool, Option<i32>)> =
        sqlx::query_as("SELECT active, stock_on_hand FROM products WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let Some((active, stock)) = rec else {
        return Err(ApiError::NotFound);
    };
    let (st, Json(inner)) =
        ApiResponse::ok(serde_json::json!({"active": active, "stock_on_hand": stock}));
    Ok((st, Json(inner.data)))
}

#[derive(Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min = 1, max = 160))]
    name: String,
    #[validate(length(min = 1, max = 180))]
    slug: String,
    #[validate(range(min = 0))]
    price_cents: i64,
    category_id: Option<uuid::Uuid>,
    #[validate(length(max = 5000))]
    description: Option<String>,
    #[validate(length(max = 2048))]
    image_url: Option<String>,
    active: Option<bool>,
}

async fn create(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;
    if body.price_cents < 0 {
        return Err(ApiError::Unprocessable("price must be >= 0".into()));
    }
    let id: uuid::Uuid = sqlx::query_scalar("INSERT INTO products (name, slug, description, image_url, price_cents, currency, active, category_id) VALUES ($1,$2,$3,$4,$5,'ZAR',$6,$7) RETURNING id")
        .bind(&body.name).bind(&body.slug)
        .bind(body.description.clone().unwrap_or_default())
        .bind(body.image_url.clone())
        .bind(body.price_cents).bind(body.active.unwrap_or(true)).bind(body.category_id)
        .fetch_one(&s.db).await.map_err(|_| ApiError::Conflict("slug exists".into()))?;
    let (st, Json(inner)) = ApiResponse::created(serde_json::json!({"id": id}));
    Ok((st, Json(inner.data)))
}
