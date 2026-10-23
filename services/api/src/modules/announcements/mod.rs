use crate::{
    errors::ApiError, errors::ApiResult, middleware::authentication::user_from_headers,
    response::ApiResponse, state::SharedState, workers::email_outbox,
};
use axum::{
    extract::{HeaderMap, Path, Query, State},
    http::StatusCode,
    routing::{get, patch},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/admin/announcements", get(admin_list).post(create))
        .route(
            "/api/v1/admin/announcements/{id}",
            patch(update).delete(remove),
        )
        .route("/api/v1/announcements", get(public_list))
}

#[derive(Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min = 1, max = 200))]
    title: String,
    #[validate(length(min = 1))]
    body_html: String,
    body_text: Option<String>,
    audience: Option<String>,
    send_now: Option<bool>,
}

#[derive(Deserialize)]
struct PublicList {
    status: Option<String>,
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

    let audience = body.audience.as_deref().unwrap_or("subscribers");
    if !["all", "subscribers"].contains(&audience) {
        return Err(ApiError::Unprocessable(
            "audience must be 'all' or 'subscribers'".into(),
        ));
    }
    let send_now = body.send_now.unwrap_or(false);

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO announcements (title, body_html, body_text, audience, status, created_by) VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(&body.title)
    .bind(&body.body_html)
    .bind(&body.body_text)
    .bind(audience)
    .bind(if send_now { "sent" } else { "draft" })
    .bind(u.user_id)
    .fetch_one(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let mut subs_len: usize = 0;
    if send_now {
        let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;

        let subs: Vec<String> = if audience == "all" {
            sqlx::query_scalar(
                "SELECT DISTINCT email_normalized FROM subscribers WHERE status='verified' UNION SELECT LOWER(contact_email) FROM orders WHERE contact_email IS NOT NULL",
            )
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| ApiError::Internal)?
        } else {
            sqlx::query_scalar("SELECT email_normalized FROM subscribers WHERE status='verified'")
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| ApiError::Internal)?
        };

        subs_len = subs.len();
        let link = format!("{}/announcements/{}", s.cfg.public_web_url, id);
        for sub_email in &subs {
            let html = email_outbox::branded_template(
                &body.title,
                &format!(
                    "<p>Hi,</p><div>{}</div><p><a href=\"{}\">Read announcement</a></p><p>— Nhlanganiso Catering &amp; Baking</p>",
                    body.body_html, link
                ),
            );
            email_outbox::enqueue(&mut tx, sub_email, &body.title, &html)
                .await
                .map_err(|_| ApiError::Internal)?;
        }

        sqlx::query(
            "UPDATE announcements SET status='sent', sent_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;

        tx.commit().await.map_err(|_| ApiError::Internal)?;
    }

    let (st, inner) = ApiResponse::ok(serde_json::json!({
        "id": id,
        "title": body.title,
        "status": if send_now { "sent" } else { "draft" },
        "announced_to": subs_len,
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
    let rows: Vec<(Uuid, String, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT id, title, status::text, audience, created_at > now() - interval '7 days' FROM announcements ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, title, status, audience, recent)| {
            serde_json::json!({
                "id": id,
                "title": title,
                "status": status,
                "audience": audience,
                "recent": recent,
            })
        })
        .collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn public_list(
    State(s): State<SharedState>,
    Query(q): Query<PublicList>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let status = q.status.as_deref().unwrap_or("sent");
    let rows: Vec<(Uuid, String, String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT id, title, body_html, created_at FROM announcements WHERE status=$1 ORDER BY created_at DESC LIMIT 50",
        )
        .bind(status)
        .fetch_all(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, title, body_html, created_at)| {
            serde_json::json!({
                "id": id,
                "title": title,
                "body_html": body_html,
                "created_at": created_at,
            })
        })
        .collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}

async fn update(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    let audience = body.audience.as_deref().unwrap_or("subscribers");
    if !["all", "subscribers"].contains(&audience) {
        return Err(ApiError::Unprocessable(
            "audience must be 'all' or 'subscribers'".into(),
        ));
    }

    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE announcements SET title=$2, body_html=$3, body_text=$4, audience=$5, updated_at=now() WHERE id=$1 RETURNING id",
    )
    .bind(id)
    .bind(&body.title)
    .bind(&body.body_html)
    .bind(&body.body_text)
    .bind(audience)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    if updated.is_none() {
        return Err(ApiError::NotFound);
    }

    let (st, inner) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(serde_json::json!(inner.data))))
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
    let deleted: Option<Uuid> =
        sqlx::query_scalar("DELETE FROM announcements WHERE id=$1 RETURNING id")
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
