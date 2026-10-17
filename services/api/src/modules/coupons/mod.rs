use crate::{
    errors::ApiError, middleware::authentication::user_from_headers, response::ApiResponse,
    state::SharedState, workers::email_outbox,
};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    http::StatusCode,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/admin/coupons", get(list).post(create))
        .route("/api/v1/admin/coupons/{id}", patch(update).delete(remove))
        .route("/api/v1/coupons/{code}/validate", get(validate_coupon))
}

#[derive(Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min = 3, max = 40))]
    code: String,
    #[validate(range(min = 1, max = 100))]
    discount_percentage: i32,
    expires_at: Option<String>,
    max_usages: Option<i32>,
    active: Option<bool>,
}

#[derive(Deserialize, Validate)]
struct UpdateReq {
    #[validate(range(min = 1, max = 100))]
    discount_percentage: Option<i32>,
    expires_at: Option<String>,
    max_usages: Option<i32>,
    active: Option<bool>,
}

#[derive(Deserialize)]
struct ValidateReq {
    code: String,
    email: Option<String>,
}

fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

async fn list(
    State(s): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(Uuid, String, i32, Option<String>, Option<i32>, i32, bool)> = sqlx::query_as(
        "SELECT id, code, discount_percentage, expires_at, max_usages, usage_count, active FROM coupons ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows
        .into_iter()
        .map(
            |(id, code, discount, expires_at, max_usages, usage_count, active)| {
                serde_json::json!({
                    "id": id,
                    "code": code,
                    "discount_percentage": discount,
                    "expires_at": expires_at,
                    "max_usages": max_usages,
                    "usage_count": usage_count,
                    "active": active,
                })
            },
        )
        .collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(inner.data)))
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

    if body.expires_at.is_none() && body.max_usages.is_none() {
        return Err(ApiError::Unprocessable(
            "set expires_at or max_usages (or both)".into(),
        ));
    }

    let expires_at: Option<chrono::NaiveDateTime> = body
        .expires_at
        .as_deref()
        .map(|s| {
            s.parse()
                .map_err(|_| ApiError::Unprocessable("expires_at must be RFC3339".into()))
        })
        .transpose()?;

    let code_upper = body.code.trim().to_uppercase();

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO coupons (code, discount_percentage, expires_at, max_usages, active) VALUES ($1,$2,$3,$4,$5) RETURNING id",
    )
    .bind(&code_upper)
    .bind(body.discount_percentage)
    .bind(expires_at.map(|d| d.and_utc()))
    .bind(body.max_usages)
    .bind(body.active.unwrap_or(true))
    .fetch_one(&s.db)
    .await
    .map_err(|_| ApiError::Conflict("coupon code already exists".into()))?;

    let (st, inner) = ApiResponse::created(serde_json::json!({
        "id": id, "code": code_upper
    }));
    Ok((st, Json(inner.data)))
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

    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let rec: Option<(Option<chrono::NaiveDateTime>, Option<i32>)> =
        sqlx::query_as("SELECT expires_at, max_usages FROM coupons WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ApiError::Internal)?;
    let Some((existing_expires, existing_max)) = rec else {
        return Err(ApiError::NotFound);
    };

    let expires_at = body
        .expires_at
        .as_deref()
        .map(|s| {
            s.parse()
                .map_err(|_| ApiError::Unprocessable("expires_at must be RFC3339".into()))
        })
        .transpose()?
        .or(existing_expires)
        .map(|d| d.and_utc());
    let max_usages = body.max_usages.or(existing_max);

    if expires_at.is_none() && max_usages.is_none() {
        return Err(ApiError::Unprocessable(
            "set expires_at or max_usages (or both)".into(),
        ));
    }

    let mut q = sqlx::query("UPDATE coupons SET active=$1 WHERE id=$2");
    if let Some(d) = body.discount_percentage {
        q = q;
    }
    let _ = q;

    sqlx::query(
        "UPDATE coupons SET discount_percentage=COALESCE($1,discount_percentage), expires_at=$2, max_usages=$3, active=$4 WHERE id=$5",
    )
    .bind(body.discount_percentage)
    .bind(expires_at)
    .bind(max_usages)
    .bind(body.active.unwrap_or(true))
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;

    tx.commit().await.map_err(|_| ApiError::Internal)?;
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
    let deleted: Option<Uuid> = sqlx::query_scalar("DELETE FROM coupons WHERE id=$1 RETURNING id")
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    if deleted.is_none() {
        return Err(ApiError::NotFound);
    }
    let (st, inner) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(inner.data)))
}

async fn validate_coupon(
    State(s): State<SharedState>,
    Query(q): Query<ValidateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let code = q.code.trim().to_uppercase();
    let email_normalized = q.email.map(normalize_email);

    // Coupon must exist and be active.
    let rec: Option<(Uuid, i32, Option<chrono::DateTime<chrono::Utc>>, Option<i32>, i32, bool)> =
        sqlx::query_as(
            "SELECT id, discount_percentage, expires_at, max_usages, usage_count, active FROM coupons WHERE code=$1",
        )
        .bind(&code)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some((id, discount, expires_at, max_usages, usage_count, active)) = rec else {
        return Err(ApiError::NotFound);
    };

    if !active {
        return Err(ApiError::Unprocessable("coupon is not active".into()));
    }
    if let Some(expires) = expires_at {
        if expires < chrono::Utc::now() {
            return Err(ApiError::Unprocessable("coupon expired".into()));
        }
    }
    if let Some(max) = max_usages {
        if usage_count >= max {
            return Err(ApiError::Unprocessable("coupon usage limit reached".into()));
        }
    }

    // Coupon requires a verified subscriber email.
    let email = email_normalized
        .ok_or_else(|| ApiError::Unprocessable("email required to validate coupon".into()))?;
    let sub: Option<String> = sqlx::query_scalar(
        "SELECT status::text FROM subscribers WHERE email_normalized=$1 AND status='verified'",
    )
    .bind(&email)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    if sub.is_none() {
        return Err(ApiError::Unprocessable(
            "coupon requires a verified subscription".into(),
        ));
    }

    // One coupon, one subscriber: reject if this email already claimed any
    // coupon, or if this coupon was already claimed by anyone.
    let already_used: Option<Uuid> =
        sqlx::query_scalar("SELECT coupon_id FROM coupon_usages WHERE email_normalized=$1 LIMIT 1")
            .bind(&email)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    if already_used.is_some() {
        return Err(ApiError::Unprocessable(
            "subscriber already used a coupon".into(),
        ));
    }
    let coupon_claimed: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM coupon_usages WHERE coupon_id=$1 LIMIT 1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    if coupon_claimed.is_some() {
        return Err(ApiError::Unprocessable("coupon already claimed".into()));
    }

    let discount_amount =
        |subtotal_cents: i64| (subtotal_cents as f64 * discount as f64 / 100.0).round() as i64;

    let (st, inner) = ApiResponse::ok(serde_json::json!({
        "code": code,
        "discount_percentage": discount,
        "valid": true,
        "remaining_usages": max_usages.map(|m| m.saturating_sub(usage_count)),
    }));
    Ok((st, Json(inner.data)))
}
