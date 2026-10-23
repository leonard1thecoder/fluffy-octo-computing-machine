use crate::{
    errors::ApiError, errors::ApiResult, middleware::authentication::user_from_headers,
    response::ApiResponse, state::SharedState, workers::email_outbox,
};
use axum::{
    extract::{Path, Query, State},
    http::{header::HeaderMap, StatusCode},
    routing::{delete, get},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/admin/subscribers", get(admin_list))
        .route("/api/v1/admin/subscribers/{id}", delete(admin_unsubscribe))
}

#[derive(Deserialize, Validate)]
struct SubscribeReq {
    #[validate(email, length(max = 320))]
    email: String,
}

#[derive(serde::Serialize)]
struct SubscribeRes {
    id: Uuid,
    email: String,
    status: String,
}

fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

async fn subscribe(
    State(s): State<SharedState>,
    Json(body): Json<SubscribeReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    let email = normalize_email(&body.email);

    let existing: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, status::text, verification_token_hash FROM subscribers WHERE email_normalized=$1",
    )
    .bind(&email)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    if let Some((id, status, _)) = existing {
        if status == "verified" {
            let (st, inner) = ApiResponse::ok(serde_json::json!({
                "id": id, "email": email, "status": "verified", "already_subscribed": true
            }));
            return Ok((st, Json(serde_json::json!(inner.data))));
        }
        let (raw_token, token_hash) = crate::modules::auth::jwt::new_refresh_token();
        let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
        sqlx::query(
            "UPDATE subscribers SET verification_token_hash=$2, verification_expires_at=now()+interval '24 hours', updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .bind(&token_hash)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
        let link = format!(
            "{}/subscribe/verify?token={}",
            s.cfg.public_web_url, raw_token
        );
        let html = email_outbox::branded_template(
            "Confirm your Nhlanganiso subscription",
            &format!(
                "<p>Hi,</p><p>Click below to confirm your subscription to Nhlanganiso Catering &amp; Baking:</p><p><a href=\"{}\">Confirm subscription</a></p><p>If you did not sign up, you can ignore this email.</p>",
                link
            ),
        );
        email_outbox::enqueue(
            &mut tx,
            &email,
            "Confirm your Nhlanganiso subscription",
            &html,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        let (st, inner) = ApiResponse::ok(serde_json::json!({
            "id": id, "email": email, "status": "pending", "verification_sent": true
        }));
        return Ok((st, Json(serde_json::json!(inner.data))));
    }

    let (raw_token, token_hash) = crate::modules::auth::jwt::new_refresh_token();
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO subscribers (email, email_normalized, status, verification_token_hash, verification_expires_at) VALUES ($1,$2,'pending',$3,now()+interval '24 hours') RETURNING id",
    )
    .bind(&email)
    .bind(&email)
    .bind(&token_hash)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::Conflict("email already subscribed".into()))?;

    let link = format!(
        "{}/subscribe/verify?token={}",
        s.cfg.public_web_url, raw_token
    );
    let html = email_outbox::branded_template(
        "Confirm your Nhlanganiso subscription",
        &format!(
            "<p>Hi {},</p><p>Click below to confirm your subscription to Nhlanganiso Catering &amp; Baking:</p><p><a href=\"{}\">Confirm subscription</a></p><p>If you did not sign up, you can ignore this email.</p>",
            email_outbox::escape_html(&email),
            link
        ),
    );
    email_outbox::enqueue(
        &mut tx,
        &email,
        "Confirm your Nhlanganiso subscription",
        &html,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;

    let (st, inner) = ApiResponse::created(serde_json::json!({
        "id": id, "email": email, "status": "pending", "verification_sent": true
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

#[derive(Deserialize)]
struct VerifyReq {
    token: String,
}

async fn verify(
    State(s): State<SharedState>,
    Query(q): Query<VerifyReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let token_hash = crate::modules::auth::jwt::hash_token(&q.token);

    let rec: Option<(Uuid, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, email_normalized, verification_expires_at FROM subscribers WHERE verification_token_hash=$1 AND status='pending'",
    )
    .bind(&token_hash)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let Some((id, email, expires_at)) = rec else {
        return Err(ApiError::Unprocessable(
            "invalid or already used token".into(),
        ));
    };

    if expires_at < chrono::Utc::now() {
        return Err(ApiError::Unprocessable("verification link expired".into()));
    }

    sqlx::query(
        "UPDATE subscribers SET status='verified', subscribed_at=now(), verification_token_hash=NULL, verification_expires_at=NULL, updated_at=now() WHERE id=$1",
    )
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let html = email_outbox::branded_template(
        "Subscription confirmed — Nhlanganiso",
        &format!(
            "<p>Hi {},</p><p>You are now subscribed to Nhlanganiso Catering &amp; Baking updates.</p>",
            email_outbox::escape_html(&email)
        ),
    );
    let _ = s
        .brevo
        .send(&email, "Subscription confirmed — Nhlanganiso", &html)
        .await;

    let (st, inner) = ApiResponse::ok(serde_json::json!({
        "id": id, "email": email, "status": "verified"
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn admin_list(
    State(s): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, email_normalized, status::text FROM subscribers ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, email, status)| serde_json::json!({"id": id, "email": email, "status": status}))
        .collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn admin_unsubscribe(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    sqlx::query("UPDATE subscribers SET status='unsubscribed', updated_at=now() WHERE id=$1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let (st, inner) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(serde_json::json!(inner.data))))
}
