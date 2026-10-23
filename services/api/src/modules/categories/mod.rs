use crate::{
    errors::ApiError, errors::ApiResult, middleware::authentication::user_from_headers,
    response::ApiResponse, state::SharedState,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        // Public listing (any authenticated user can browse categories)
        .route("/api/v1/categories", get(list).post(create))
        .route("/api/v1/categories/{id}", get(get_one))
        // Admin only
        .route("/api/v1/admin/categories/{id}", patch(update).delete(remove))
}

#[derive(Debug, Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min = 1, max = 100))]
    name: String,
    #[validate(length(min = 1, max = 120))]
    slug: String,
    /// Optional parent category. When present, the parent must be a top-level
    /// category (parent_id IS NULL). When absent, this becomes a top-level
    /// category itself.
    parent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, Validate)]
struct UpdateReq {
    #[validate(length(min = 1, max = 100))]
    name: String,
    #[validate(length(min = 1, max = 120))]
    slug: String,
    parent_id: Option<Option<uuid::Uuid>>, // None = no change, Some(None) = make top-level, Some(Some(id)) = set parent
}

#[derive(Debug, Serialize)]
struct Category {
    id: Uuid,
    name: String,
    slug: String,
    parent_id: Option<Uuid>,
    parent_name: Option<String>,
    active: bool,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn list(
    State(s): State<SharedState>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let rows: Vec<(Uuid, String, String, Option<Uuid>, Option<String>, bool, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT c.id, c.name, c.slug, c.parent_id, p.name AS parent_name, c.active, c.created_at
             FROM categories c
             LEFT JOIN categories p ON c.parent_id = p.id
             ORDER BY c.parent_id NULLS FIRST, c.name",
        )
        .fetch_all(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let items: Vec<Category> = rows
        .into_iter()
        .map(
            |(id, name, slug, parent_id, parent_name, active, created_at)| Category {
                id,
                name,
                slug,
                parent_id,
                parent_name,
                active,
                created_at,
            },
        )
        .collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn get_one(
    State(s): State<SharedState>,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let row: Option<(Uuid, String, String, Option<Uuid>, Option<String>, bool, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT c.id, c.name, c.slug, c.parent_id, p.name AS parent_name, c.active, c.created_at
             FROM categories c
             LEFT JOIN categories p ON c.parent_id = p.id
             WHERE c.id = $1",
        )
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some((id, name, slug, parent_id, parent_name, active, created_at)) = row else {
        return Err(ApiError::NotFound);
    };
    let (st, inner) = ApiResponse::ok(Category {
        id,
        name,
        slug,
        parent_id,
        parent_name,
        active,
        created_at,
    });
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn create(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Json(body): Json<CreateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    // Validate parent: if provided, it must be a top-level category (parent_id IS NULL).
    if let Some(parent_id) = body.parent_id {
        let parent: Option<(bool,)> = sqlx::query_scalar(
            "SELECT parent_id IS NULL FROM categories WHERE id = $1",
        )
        .bind(parent_id)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
        match parent {
            Some(true) => {} // top-level parent - OK
            Some(false) => return Err(ApiError::Unprocessable(
                "parent category must be a top-level category (no parent itself)".into(),
            )),
            None => return Err(ApiError::NotFound),
        }
    }

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO categories (name, slug, parent_id) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(body.name.trim())
    .bind(body.slug.trim())
    .bind(body.parent_id)
    .fetch_one(&s.db)
    .await
    .map_err(|e| {
        if e.to_string().contains("unique") {
            ApiError::Conflict("slug already exists".into())
        } else {
            ApiError::Internal
        }
    })?;

    let (st, inner) = ApiResponse::created(serde_json::json!({
        "id": id,
        "name": body.name.trim(),
        "slug": body.slug.trim(),
        "parent_id": body.parent_id,
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn update(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    // Verify category exists
    let existing: Option<(Uuid,)> = sqlx::query_scalar("SELECT id FROM categories WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let _ = existing.ok_or(ApiError::NotFound)?;

    // Validate new parent if changing
    if let Some(new_parent_id) = body.parent_id {
        match new_parent_id {
            Some(pid) => {
                let parent: Option<(bool,)> = sqlx::query_scalar(
                    "SELECT parent_id IS NULL FROM categories WHERE id = $1",
                )
                .bind(pid)
                .fetch_optional(&s.db)
                .await
                .map_err(|_| ApiError::Internal)?;
                match parent {
                    Some(true) => {} // top-level - OK
                    Some(false) => return Err(ApiError::Unprocessable(
                        "parent category must be a top-level category (no parent itself)".into(),
                    )),
                    None => return Err(ApiError::NotFound),
                }
            }
            None => {} // make top-level - always valid
        }
    }

    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE categories SET name = $2, slug = $3, parent_id = $4 WHERE id = $1 RETURNING id",
    )
    .bind(id)
    .bind(body.name.trim())
    .bind(body.slug.trim())
    .bind(body.parent_id)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    if updated.is_none() {
        return Err(ApiError::NotFound);
    }

    let (st, inner) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(inner.data)))
}

async fn remove(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    // Check if this category has children - if so, prevent deletion or cascade?
    let has_children: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM categories WHERE parent_id = $1)")
        .bind(id)
        .fetch_one(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    if has_children {
        return Err(ApiError::Unprocessable(
            "cannot delete category that has sub-categories. Move or delete sub-categories first".into(),
        ));
    }
    let deleted: Option<Uuid> = sqlx::query_scalar("DELETE FROM categories WHERE id = $1 RETURNING id")
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    if deleted.is_none() {
        return Err(ApiError::NotFound);
    }
    let (st, inner) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(serde_json::json!(inner.data))))
}
