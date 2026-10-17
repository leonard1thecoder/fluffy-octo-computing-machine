use crate::{
    errors::{ApiError, ApiResult},
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
    workers::email_outbox,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/catering/requests", post(create_request))
        .route("/api/v1/catering/requests/{id}", get(get_request))
        .route("/api/v1/admin/catering/requests", get(admin_list))
        .route("/api/v1/admin/catering/requests/{id}", patch(admin_update))
        .route(
            "/api/v1/admin/catering/requests/{id}/quotes",
            post(create_quote),
        )
        .route("/api/v1/catering/quotes/{id}/accept", post(accept_quote))
}

#[derive(Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min = 1, max = 160))]
    event_type: String,
    venue: Option<String>,
    budget_cents: Option<i64>,
    menu_requirements: Option<String>,
    #[validate(length(min = 1, max = 120))]
    contact_name: String,
    #[validate(email)]
    contact_email: String,
    contact_phone: Option<String>,
}

async fn create_request(
    State(s): State<SharedState>,
    Json(body): Json<CreateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO catering_requests (event_type, venue, budget_cents, menu_requirements, contact_name, contact_email, contact_phone, status) VALUES ($1,$2,$3,$4,$5,$6,$7,'new') RETURNING id",
    )
    .bind(&body.event_type)
    .bind(&body.venue)
    .bind(body.budget_cents)
    .bind(&body.menu_requirements)
    .bind(&body.contact_name)
    .bind(&body.contact_email)
    .bind(&body.contact_phone)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;
    let html = email_outbox::branded_template(
        "Catering enquiry received",
        &format!(
            "<p>Thanks {} — we received your {} enquiry.</p>",
            email_outbox::escape_html(&body.contact_name),
            email_outbox::escape_html(&body.event_type),
        ),
    );
    email_outbox::enqueue(
        &mut tx,
        &body.contact_email,
        "Catering enquiry received — Nhlanganiso",
        &html,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    let admin_html = email_outbox::branded_template(
        "New catering enquiry",
        &format!(
            "<p>{} · {}</p>",
            email_outbox::escape_html(&body.contact_name),
            email_outbox::escape_html(&body.event_type),
        ),
    );
    email_outbox::enqueue(
        &mut tx,
        &s.cfg.brevo_sender_email,
        "New catering enquiry",
        &admin_html,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (st, Json(inner)) = ApiResponse::created(serde_json::json!({"id": id}));
    Ok((st, Json(inner.data)))
}

async fn get_request(
    State(s): State<SharedState>,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let rec: Option<(String, String)> =
        sqlx::query_as("SELECT event_type, status::text FROM catering_requests WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let Some((event_type, status)) = rec else {
        return Err(ApiError::NotFound);
    };
    let (st, Json(inner)) =
        ApiResponse::ok(serde_json::json!({"id": id, "event_type": event_type, "status": status}));
    Ok((st, Json(inner.data)))
}

async fn admin_list(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as("SELECT id, event_type, status::text FROM catering_requests ORDER BY created_at DESC LIMIT 100")
        .fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|(id, t, st)| serde_json::json!({"id": id, "event_type": t, "status": st}))
        .collect();
    let (st, Json(inner)) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(inner.data).unwrap())))
}

#[derive(Deserialize)]
struct UpdateReq {
    status: Option<String>,
}

async fn admin_update(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    if let Some(st) = body.status {
        if !["new", "reviewing", "quoted", "accepted", "declined"].contains(&st.as_str()) {
            return Err(ApiError::Unprocessable("bad status".into()));
        }
        sqlx::query("UPDATE catering_requests SET status=$2::catering_status WHERE id=$1")
            .bind(id)
            .bind(st)
            .execute(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
        sqlx::query("INSERT INTO audit_logs (actor_user_id, action, entity, entity_id) VALUES ($1,'catering.update','catering_request',$2)")
            .bind(u.user_id).bind(id.to_string()).execute(&s.db).await.map_err(|_| ApiError::Internal)?;
    }
    let (st, Json(inner)) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(inner.data)))
}

#[derive(Deserialize, Validate)]
struct QuoteReq {
    #[validate(length(min = 1))]
    items: Vec<QuoteItem>,
    valid_until: String,
    deposit_cents: Option<i64>,
}
#[derive(Deserialize, Serialize)]
struct QuoteItem {
    description: String,
    qty: i32,
    unit_price_cents: i64,
}

async fn create_quote(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(req_id): Path<Uuid>,
    Json(body): Json<QuoteReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;
    let valid_until: chrono::NaiveDate = body
        .valid_until
        .parse()
        .map_err(|_| ApiError::Unprocessable("valid_until YYYY-MM-DD".into()))?;
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let total: i64 = body
        .items
        .iter()
        .map(|i| i.qty as i64 * i.unit_price_cents)
        .sum();
    let qid: Uuid = sqlx::query_scalar("INSERT INTO catering_quotes (request_id, total_cents, currency, status, valid_until, deposit_cents, created_by) VALUES ($1,$2,'ZAR','sent',$3,$4,$5) RETURNING id")
        .bind(req_id).bind(total).bind(valid_until).bind(body.deposit_cents).bind(u.user_id)
        .fetch_one(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    for i in &body.items {
        sqlx::query("INSERT INTO catering_quote_items (quote_id, description, qty, unit_price_cents, line_total_cents) VALUES ($1,$2,$3,$4,$5)")
            .bind(qid).bind(&i.description).bind(i.qty).bind(i.unit_price_cents).bind(i.qty as i64 * i.unit_price_cents)
            .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    }
    sqlx::query("UPDATE catering_requests SET status='quoted' WHERE id=$1")
        .bind(req_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
    let contact: Option<String> =
        sqlx::query_scalar("SELECT contact_email FROM catering_requests WHERE id=$1")
            .bind(req_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ApiError::Internal)?;
    if let Some(email) = contact {
        let html = email_outbox::branded_template(
            "Your catering quote",
            &format!(
                "<p>Quote total R{:.2}. Valid until {}.</p>",
                total as f64 / 100.0,
                valid_until
            ),
        );
        email_outbox::enqueue(&mut tx, &email, "Your Nhlanganiso catering quote", &html)
            .await
            .map_err(|_| ApiError::Internal)?;
    }
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (st, Json(inner)) =
        ApiResponse::created(serde_json::json!({"id": qid, "total_cents": total}));
    Ok((st, Json(inner.data)))
}

async fn accept_quote(
    State(s): State<SharedState>,
    Path(qid): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let rec: Option<(Uuid, String, chrono::NaiveDate)> = sqlx::query_as(
        "SELECT request_id, status::text, valid_until FROM catering_quotes WHERE id=$1 FOR UPDATE",
    )
    .bind(qid)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;
    let Some((req_id, status, valid_until)) = rec else {
        return Err(ApiError::NotFound);
    };
    if status != "sent" {
        return Err(ApiError::Unprocessable("quote not open".into()));
    }
    if valid_until < chrono::Utc::now().date_naive() {
        sqlx::query("UPDATE catering_quotes SET status='expired' WHERE id=$1")
            .bind(qid)
            .execute(&mut *tx)
            .await
            .map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Err(ApiError::Unprocessable("quote expired".into()));
    }
    sqlx::query("UPDATE catering_quotes SET status='accepted' WHERE id=$1")
        .bind(qid)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
    sqlx::query("UPDATE catering_requests SET status='accepted' WHERE id=$1")
        .bind(req_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
    // Accepted != paid. If a deposit is due, the customer pays via the normal
    // PayFast checkout linked to an order; never mark paid here.
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (st, Json(inner)) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(inner.data)))
}
