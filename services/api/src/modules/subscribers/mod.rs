use crate::{
    errors::ApiError, errors::ApiResult, middleware::authentication::user_from_headers,
    response::ApiResponse, state::SharedState, workers::email_outbox,
};
use axum::{
    extract::{Path, Query, State},
    http::{header::HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/subscribers", post(subscribe))
        .route("/api/v1/subscribers/verify", get(verify))
        .route("/api/v1/admin/subscribers", get(admin_list))
        .route("/api/v1/admin/subscribers/stats", get(admin_stats))
        .route("/api/v1/admin/subscribers/{id}", delete(admin_unsubscribe))
}

#[derive(Deserialize, Validate)]
struct SubscribeReq {
    #[validate(email, length(max = 320))]
    email: String,
}

fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

/// Human-readable receipt reference, e.g. `NL-20261020-AB12CD`.
fn receipt_reference() -> String {
    let raw = Uuid::new_v4().to_string().replace('-', "");
    format!(
        "NL-{}-{}",
        chrono::Utc::now().format("%Y%m%d"),
        raw[..6].to_uppercase()
    )
}

/// Build the receipt the storefront shows and the confirmation email carries.
fn receipt(
    id: Uuid,
    reference: &str,
    email: &str,
    status: &str,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), serde_json::json!(id));
    out.insert("reference".into(), serde_json::json!(reference));
    out.insert("email".into(), serde_json::json!(email));
    out.insert("status".into(), serde_json::json!(status));
    out.insert("created_at".into(), serde_json::json!(chrono::Utc::now()));
    if let serde_json::Value::Object(extra) = extra {
        for (k, v) in extra {
            out.insert(k, v);
        }
    }
    serde_json::Value::Object(out)
}

async fn subscribe(
    State(s): State<SharedState>,
    Json(body): Json<SubscribeReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    let email = normalize_email(&body.email);

    // Existing subscriber?
    let existing: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, status::text, reference FROM subscribers WHERE email_normalized=$1",
    )
    .bind(&email)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    if let Some((id, status, reference)) = existing {
        let reference = reference.unwrap_or_else(receipt_reference);

        if status == "verified" {
            let (st, inner) = ApiResponse::ok(receipt(
                id,
                &reference,
                &email,
                "verified",
                serde_json::json!({"already_subscribed": true}),
            ));
            return Ok((st, Json(serde_json::json!(inner.data))));
        }

        // Pending: resend the verification link (receipt stays the same).
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

        let html = email_outbox::branded_template(
            "Confirm your Nhlanganiso subscription",
            &receipt_email_html(
                &email,
                &reference,
                &format!("{}/newsletter?token={}", s.cfg.public_web_url, raw_token),
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

        let (st, inner) = ApiResponse::ok(receipt(
            id,
            &reference,
            &email,
            "pending",
            serde_json::json!({"verification_sent": true}),
        ));
        return Ok((st, Json(serde_json::json!(inner.data))));
    }

    // New subscriber: create it pending and send the confirmation.
    let (raw_token, token_hash) = crate::modules::auth::jwt::new_refresh_token();
    let reference = receipt_reference();
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO subscribers (email, email_normalized, status, reference, verification_token_hash, verification_expires_at) VALUES ($1,$2,'pending',$3,$4,now()+interval '24 hours') RETURNING id",
    )
    .bind(&email)
    .bind(&email)
    .bind(&reference)
    .bind(&token_hash)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::Conflict("email already subscribed".into()))?;

    let html = email_outbox::branded_template(
        "Confirm your Nhlanganiso subscription",
        &receipt_email_html(
            &email,
            &reference,
            &format!("{}/newsletter?token={}", s.cfg.public_web_url, raw_token),
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

    let (st, inner) = ApiResponse::created(receipt(
        id,
        &reference,
        &email,
        "pending",
        serde_json::json!({"verification_sent": true}),
    ));
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// The subscription receipt as an email: reference, what was subscribed to and
/// the confirmation link.
fn receipt_email_html(email: &str, reference: &str, verify_link: &str) -> String {
    format!(
        "<p>Hi,</p><p>Thank you for subscribing to the Nhlanganiso Catering &amp; Baking newsletter.</p>\
         <div style=\"border:1px solid #E6DCC8;border-radius:8px;padding:16px;margin:16px 0\">\
         <p style=\"margin:0 0 8px\"><strong>Subscription receipt</strong></p>\
         <p style=\"margin:0\">Reference: <strong>{ref}</strong></p>\
         <p style=\"margin:0\">Email: {email}</p>\
         <p style=\"margin:0\">Status: awaiting confirmation</p></div>\
         <p><a href=\"{link}\" style=\"background:#C2571B;color:#fff;padding:12px 18px;border-radius:6px;text-decoration:none\">Confirm subscription</a></p>\
         <p style=\"color:#8A7A63;font-size:12px\">This link expires in 24 hours. If you did not sign up, you can ignore this email.</p>",
        ref = email_outbox::escape_html(reference),
        email = email_outbox::escape_html(email),
        link = email_outbox::escape_html(verify_link),
    )
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

    let rec: Option<(Uuid, String, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, email_normalized, reference, verification_expires_at FROM subscribers WHERE verification_token_hash=$1 AND status='pending'",
    )
    .bind(&token_hash)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let Some((id, email, reference, expires_at)) = rec else {
        return Err(ApiError::Unprocessable(
            "invalid or already used token".into(),
        ));
    };

    if expires_at < chrono::Utc::now() {
        return Err(ApiError::Unprocessable("verification link expired".into()));
    }

    let reference = reference.unwrap_or_else(receipt_reference);
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
            "<p>Hi {},</p><p>Your subscription is confirmed.</p>\
             <div style=\"border:1px solid #E6DCC8;border-radius:8px;padding:16px;margin:16px 0\">\
             <p style=\"margin:0 0 8px\"><strong>Subscription receipt</strong></p>\
             <p style=\"margin:0\">Reference: <strong>{}</strong></p>\
             <p style=\"margin:0\">Status: confirmed</p></div>\
             <p>— Nhlanganiso Catering &amp; Baking</p>",
            email_outbox::escape_html(&reference),
            email_outbox::escape_html(&email),
        ),
    );
    let _ = s
        .brevo
        .send(&email, "Subscription confirmed — Nhlanganiso", &html)
        .await;

    let (st, inner) = ApiResponse::ok(receipt(
        id,
        &reference,
        &email,
        "verified",
        serde_json::json!({"subscribed_at": chrono::Utc::now()}),
    ));
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// Row shape for the admin subscriber listing.
type AdminSubscriberRow = (Uuid, String, String, Option<String>, Option<String>);

async fn admin_list(
    State(s): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<AdminSubscriberRow> = sqlx::query_as(
        "SELECT id, email_normalized, status::text, reference, subscribed_at FROM subscribers ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, email, status, reference, subscribed_at)| {
            serde_json::json!({
                "id": id,
                "email": email,
                "status": status,
                "reference": reference,
                "subscribed_at": subscribed_at,
            })
        })
        .collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// Subscriber counts for the back office newsletter dashboard.
async fn admin_stats(
    State(s): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let row: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE status='verified'), COUNT(*) FILTER (WHERE status='pending'), COUNT(*) FILTER (WHERE status='unsubscribed') FROM subscribers",
    )
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let (total, verified, pending, unsubscribed) = row.unwrap_or((0, 0, 0, 0));
    let (st, inner) = ApiResponse::ok(serde_json::json!({
        "total": total,
        "verified": verified,
        "pending": pending,
        "unsubscribed": unsubscribed,
    }));
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
