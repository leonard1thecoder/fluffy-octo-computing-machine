use crate::{
    errors::{ApiError, ApiResult},
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/categories", get(list).post(create))
}

#[derive(Serialize)]
struct Category {
    id: uuid::Uuid,
    name: String,
    slug: String,
}

async fn list(State(s): State<SharedState>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let rows: Vec<(uuid::Uuid, String, String)> =
        sqlx::query_as("SELECT id, name, slug FROM categories WHERE active=true ORDER BY name")
            .fetch_all(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let items: Vec<Category> = rows
        .into_iter()
        .map(|(id, name, slug)| Category { id, name, slug })
        .collect();
    let (st, Json(inner)) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(inner.data).unwrap())))
}

#[derive(Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min = 1, max = 120))]
    name: String,
    #[validate(length(min = 1, max = 140))]
    slug: String,
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
    let id: uuid::Uuid =
        sqlx::query_scalar("INSERT INTO categories (name, slug) VALUES ($1,$2) RETURNING id")
            .bind(&body.name)
            .bind(&body.slug)
            .fetch_one(&s.db)
            .await
            .map_err(|_| ApiError::Conflict("slug exists".into()))?;
    let (st, Json(inner)) = ApiResponse::created(serde_json::json!({"id": id}));
    Ok((st, Json(inner.data)))
}
