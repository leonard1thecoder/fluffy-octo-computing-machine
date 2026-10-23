use crate::{
    config::PayfastMode,
    errors::{ApiError, ApiResult},
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
    workers::email_outbox,
};
use axum::{
    extract::{Path, State},
    http::{header::HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        // Customer asks.
        .route("/api/v1/orders/{id}/refund-request", post(request_refund))
        // Back office decides.
        .route("/api/v1/admin/refunds/{id}/approve", post(approve_refund))
        .route("/api/v1/admin/refunds/{id}/reject", post(reject_refund))
        // Views.
        .route("/api/v1/admin/orders/{id}/refund", post(create_refund))
        .route("/api/v1/admin/orders/{id}/refundable", get(refundable))
        .route("/api/v1/admin/refunds", get(admin_list))
        .route("/api/v1/orders/{id}/refunds", get(customer_list))
}

#[derive(Deserialize, Validate)]
struct RefundReq {
    /// Amount to refund in cents. Defaults to the full refundable amount.
    gross_cents: Option<i64>,
    #[validate(length(min = 1, max = 500))]
    reason: String,
}

#[derive(Deserialize, Validate)]
struct RejectReq {
    #[validate(length(min = 1, max = 500))]
    reason: String,
}

/// Split a gross refund into the bank charge and the net paid to the customer.
fn split_amount(gross_cents: i64, charge_pct: i32) -> (i64, i64) {
    let charge = (gross_cents as f64 * charge_pct as f64 / 100.0).round() as i64;
    (charge, gross_cents - charge)
}

/// A refund as stored.
#[derive(Serialize)]
struct RefundRow {
    id: Uuid,
    order_id: Uuid,
    gross_cents: i64,
    bank_charge_cents: i64,
    net_cents: i64,
    bank_charge_pct: i32,
    currency: String,
    status: String,
    reason: Option<String>,
    decision_note: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl RefundRow {
    fn into_json(self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "order_id": self.order_id,
            "gross_cents": self.gross_cents,
            "bank_charge_cents": self.bank_charge_cents,
            "net_cents": self.net_cents,
            "bank_charge_pct": self.bank_charge_pct,
            "currency": self.currency,
            "status": self.status,
            "reason": self.reason,
            "decision_note": self.decision_note,
            "created_at": self.created_at,
            "completed_at": self.completed_at,
        })
    }
}

const REFUND_COLUMNS: &str = "id, order_id, gross_cents, bank_charge_cents, net_cents, bank_charge_pct, currency, status, reason, decision_note, created_at, completed_at";

/// Order + payment facts needed to price a refund.
struct RefundContext {
    paid_cents: i64,
    refunded_cents: i64,
    payment_status: String,
    payment_id: Option<Uuid>,
    merchant_payment_id: Option<String>,
    provider_txn_id: Option<String>,
    contact_email: Option<String>,
    order_reference: String,
    order_user_id: Option<Uuid>,
}

async fn refund_context(s: &SharedState, order_id: Uuid) -> ApiResult<RefundContext> {
    let row: Option<(
        i64,
        Option<i64>,
        String,
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<Uuid>,
    )> = sqlx::query_as(
        "SELECT o.total_cents,
                COALESCE((SELECT SUM(r.gross_cents) FROM refunds r
                           WHERE r.order_id = o.id AND r.status NOT IN ('failed','rejected')), 0),
                o.payment_status::text,
                p.id,
                p.merchant_payment_id,
                p.provider_txn_id,
                o.contact_email,
                o.reference,
                o.user_id
           FROM orders o
           LEFT JOIN payments p ON p.order_id = o.id AND p.status IN ('paid','partially_refunded')
          WHERE o.id = $1
          ORDER BY p.created_at DESC
          LIMIT 1",
    )
    .bind(order_id)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let Some((
        paid,
        refunded,
        payment_status,
        payment_id,
        merchant_payment_id,
        provider_txn_id,
        contact_email,
        order_reference,
        order_user_id,
    )) = row
    else {
        return Err(ApiError::NotFound);
    };

    Ok(RefundContext {
        paid_cents: paid,
        refunded_cents: refunded,
        payment_status,
        payment_id,
        merchant_payment_id,
        provider_txn_id,
        contact_email,
        order_reference,
        order_user_id,
    })
}

/// Ask the provider to move the money and record the outcome.
///
/// Production calls PayFast's Refunds API. PayFast documents no sandbox refunds
/// endpoint, so in sandbox the refund is flagged `manual_action_required` for
/// the operator to finish in the PayFast sandbox dashboard.
async fn call_provider(
    s: &SharedState,
    ctx: &RefundContext,
    net_cents: i64,
    reason: &str,
) -> (&'static str, Option<String>, Option<String>) {
    match (s.cfg.payfast_mode, ctx.provider_txn_id.clone()) {
        (PayfastMode::Production, Some(txn)) => {
            match crate::integrations::payfast::refund::request_refund(
                &s.http,
                &s.cfg.payfast_merchant_id,
                &txn,
                net_cents,
                reason,
            )
            .await
            {
                Ok(resp) if resp.success => ("succeeded", resp.response.and_then(|r| r.id), None),
                Ok(resp) => (
                    "failed",
                    None,
                    Some(
                        resp.error
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "refund rejected by provider".into()),
                    ),
                ),
                Err(e) => ("failed", None, Some(format!("{e}"))),
            }
        }
        (PayfastMode::Production, None) => (
            "failed",
            None,
            Some("no provider transaction id recorded for this order".into()),
        ),
        (PayfastMode::Sandbox, _) => (
            "manual_action_required",
            None,
            Some("complete this refund in the PayFast sandbox dashboard".into()),
        ),
    }
}

/// Update the order and payment to reflect the refunded amount.
async fn apply_order_status(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    order_id: Uuid,
    payment_id: Option<Uuid>,
    paid_cents: i64,
    total_refunded: i64,
) -> Result<(), sqlx::Error> {
    let status = if total_refunded >= paid_cents {
        "refunded"
    } else {
        "partially_refunded"
    };
    sqlx::query("UPDATE orders SET payment_status=$2::payment_status WHERE id=$1")
        .bind(order_id)
        .bind(status)
        .execute(&mut **tx)
        .await?;
    if let Some(pid) = payment_id {
        sqlx::query("UPDATE payments SET status=$2::payment_status WHERE id=$1")
            .bind(pid)
            .bind(status)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// Email the customer about a refund outcome.
async fn email_customer(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    contact_email: &str,
    net_cents: i64,
    charge_cents: i64,
    charge_pct: i32,
    reason: &str,
    subject: &str,
    headline: &str,
) -> Result<(), sqlx::Error> {
    let html = email_outbox::branded_template(
        subject,
        &format!(
            "<p>Hi,</p><p>{headline}</p>\
             <div style=\"border:1px solid #E6DCC8;border-radius:8px;padding:16px;margin:16px 0\">\
             <p style=\"margin:0 0 8px\"><strong>Refund receipt</strong></p>\
             <p style=\"margin:0\">Amount returned: <strong>R{net:.2}</strong></p>\
             <p style=\"margin:0\">Bank charge ({pct}%): -R{charge:.2}</p>\
             <p style=\"margin:0\">Reason: {reason}</p></div>\
             <p>— Nhlanganiso Catering &amp; Baking</p>",
            net = net_cents as f64 / 100.0,
            charge = charge_cents as f64 / 100.0,
            pct = charge_pct,
            reason = email_outbox::escape_html(reason),
        ),
    );
    email_outbox::enqueue(tx, contact_email, subject, &html)
        .await
        .map_err(|e| sqlx::Error::Protocol(e.to_string()))
}

/// GET /api/v1/admin/orders/{id}/refundable — refundable amount + charge preview.
async fn refundable(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let ctx = refund_context(&s, id).await?;
    let remaining = (ctx.paid_cents - ctx.refunded_cents).max(0);
    let (charge, net) = split_amount(remaining, s.cfg.refund_bank_charge_pct);
    let (st, inner) = ApiResponse::ok(serde_json::json!({
        "order_id": id,
        "payment_status": ctx.payment_status,
        "paid_cents": ctx.paid_cents,
        "refunded_cents": ctx.refunded_cents,
        "refundable_cents": remaining,
        "bank_charge_pct": s.cfg.refund_bank_charge_pct,
        "bank_charge_cents": charge,
        "net_to_customer_cents": net,
        "currency": "ZAR",
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// POST /api/v1/admin/orders/{id}/refund — refund a paid order straight away.
async fn create_refund(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RefundReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    let ctx = refund_context(&s, id).await?;
    let (gross, charge, net) = validate_amount(&s, &ctx, body.gross_cents)?;

    let (status, provider_ref, failure_reason) =
        call_provider(&s, &ctx, net, body.reason.trim()).await;

    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;

    let refund_id: Uuid = sqlx::query_scalar(
        "INSERT INTO refunds (order_id, payment_id, merchant_payment_id, provider_txn_id, gross_cents, bank_charge_cents, net_cents, bank_charge_pct, currency, reason, status, provider_ref, failure_reason, requested_by, completed_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'ZAR',$9,$10,$11,$12,$13,CASE WHEN $10 = 'succeeded' THEN now() ELSE NULL END) RETURNING id",
    )
    .bind(id)
    .bind(ctx.payment_id)
    .bind(&ctx.merchant_payment_id)
    .bind(&ctx.provider_txn_id)
    .bind(gross)
    .bind(charge)
    .bind(net)
    .bind(s.cfg.refund_bank_charge_pct)
    .bind(body.reason.trim())
    .bind(status)
    .bind(&provider_ref)
    .bind(&failure_reason)
    .bind(u.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;

    apply_order_status(
        &mut tx,
        id,
        ctx.payment_id,
        ctx.paid_cents,
        ctx.refunded_cents + gross,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    if status == "succeeded" || status == "manual_action_required" {
        if let Some(email) = ctx.contact_email.clone() {
            let _ = email_customer(
                &mut tx,
                &email,
                net,
                charge,
                s.cfg.refund_bank_charge_pct,
                body.reason.trim(),
                "Refund issued — Nhlanganiso",
                "We have processed a refund for your order.",
            )
            .await;
        }
    }

    audit(&mut tx, u.user_id, id, "order.refund").await;

    tx.commit().await.map_err(|_| ApiError::Internal)?;

    let (st, inner) = ApiResponse::created(serde_json::json!({
        "id": refund_id,
        "order_id": id,
        "gross_cents": gross,
        "bank_charge_pct": s.cfg.refund_bank_charge_pct,
        "bank_charge_cents": charge,
        "net_to_customer_cents": net,
        "currency": "ZAR",
        "status": status,
        "provider_ref": provider_ref,
        "failure_reason": failure_reason,
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// POST /api/v1/orders/{id}/refund-request — the customer asks for a refund.
///
/// The request is recorded and the back office decides. Nothing moves until
/// an administrator approves it.
async fn request_refund(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RefundReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await?;
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    let ctx = refund_context(&s, id).await?;

    // The requester must own the order (admins use the direct refund route).
    if me.role != "admin" && ctx.order_user_id != Some(me.user_id) {
        return Err(ApiError::Forbidden);
    }
    if ctx.payment_status != "paid" && ctx.payment_status != "partially_refunded" {
        return Err(ApiError::Unprocessable(
            "a refund can only be requested for a paid order".into(),
        ));
    }

    // One open request per order.
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM refunds WHERE order_id=$1 AND status IN ('requested','pending','succeeded','manual_action_required'))",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    if open {
        return Err(ApiError::Conflict(
            "there is already a refund in progress for this order".into(),
        ));
    }

    let (gross, charge, net) = validate_amount(&s, &ctx, body.gross_cents)?;

    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let refund_id: Uuid = sqlx::query_scalar(
        "INSERT INTO refunds (order_id, payment_id, merchant_payment_id, provider_txn_id, gross_cents, bank_charge_cents, net_cents, bank_charge_pct, currency, reason, status, requested_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'ZAR',$9,'requested',$10) RETURNING id",
    )
    .bind(id)
    .bind(ctx.payment_id)
    .bind(&ctx.merchant_payment_id)
    .bind(&ctx.provider_txn_id)
    .bind(gross)
    .bind(charge)
    .bind(net)
    .bind(s.cfg.refund_bank_charge_pct)
    .bind(body.reason.trim())
    .bind(me.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;

    // Tell the back office there is a request waiting.
    let admin_email = s.cfg.brevo_sender_email.clone();
    if !admin_email.is_empty() {
        let html = email_outbox::branded_template(
            "Refund requested",
            &format!(
                "<p>A customer requested a refund for order <strong>{}</strong>.</p>\
                 <p>Amount: R{:.2} (after a {}% bank charge of R{:.2}, the customer receives R{:.2}).</p>\
                 <p>Reason: {}</p>\
                 <p>Review it in the back office.</p>",
                email_outbox::escape_html(&ctx.order_reference),
                gross as f64 / 100.0,
                s.cfg.refund_bank_charge_pct,
                charge as f64 / 100.0,
                net as f64 / 100.0,
                email_outbox::escape_html(body.reason.trim()),
            ),
        );
        let _ = email_outbox::enqueue(
            &mut tx,
            &admin_email,
            "Refund requested — Nhlanganiso",
            &html,
        )
        .await;
    }

    audit(&mut tx, me.user_id, id, "order.refund_requested").await;

    tx.commit().await.map_err(|_| ApiError::Internal)?;

    let (st, inner) = ApiResponse::created(serde_json::json!({
        "id": refund_id,
        "order_id": id,
        "gross_cents": gross,
        "bank_charge_pct": s.cfg.refund_bank_charge_pct,
        "bank_charge_cents": charge,
        "net_to_customer_cents": net,
        "currency": "ZAR",
        "status": "requested",
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// POST /api/v1/admin/refunds/{id}/approve — decide a customer request.
async fn approve_refund(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }

    let row: Option<(Uuid, i64, i64, i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT order_id, gross_cents, bank_charge_cents, net_cents, reason, status, merchant_payment_id FROM refunds WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let Some((order_id, gross, charge, net, reason, status, _mpid)) = row else {
        return Err(ApiError::NotFound);
    };
    if status != "requested" {
        return Err(ApiError::Conflict(
            "only a requested refund can be approved".into(),
        ));
    }

    let ctx = refund_context(&s, order_id).await?;
    let (status, provider_ref, failure_reason) = call_provider(&s, &ctx, net, reason.trim()).await;

    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;

    sqlx::query(
        "UPDATE refunds SET status=$2, provider_ref=$3, failure_reason=$4, decided_by=$5, decided_at=now(), completed_at=CASE WHEN $2='succeeded' THEN now() ELSE NULL END WHERE id=$1",
    )
    .bind(id)
    .bind(status)
    .bind(&provider_ref)
    .bind(&failure_reason)
    .bind(u.user_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;

    if status == "succeeded" || status == "manual_action_required" {
        apply_order_status(
            &mut tx,
            order_id,
            ctx.payment_id,
            ctx.paid_cents,
            ctx.refunded_cents + gross,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        if let Some(email) = ctx.contact_email.clone() {
            let _ = email_customer(
                &mut tx,
                &email,
                net,
                charge,
                s.cfg.refund_bank_charge_pct,
                reason.trim(),
                "Refund issued — Nhlanganiso",
                "We have processed a refund for your order.",
            )
            .await;
        }
    }

    audit(&mut tx, u.user_id, order_id, "order.refund_approved").await;

    tx.commit().await.map_err(|_| ApiError::Internal)?;

    let (st, inner) = ApiResponse::ok(serde_json::json!({
        "id": id,
        "order_id": order_id,
        "status": status,
        "net_to_customer_cents": net,
        "provider_ref": provider_ref,
        "failure_reason": failure_reason,
    }));
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// POST /api/v1/admin/refunds/{id}/reject — decline a customer request.
async fn reject_refund(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<RejectReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    body.validate().map_err(|e| ApiError::Validation {
        fields: serde_json::json!(e.to_string()),
    })?;

    let row: Option<(Uuid, i64, i64, i64, String, String)> = sqlx::query_as(
        "SELECT order_id, gross_cents, bank_charge_cents, net_cents, reason, status FROM refunds WHERE id=$1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let Some((order_id, _gross, charge, net, reason, status)) = row else {
        return Err(ApiError::NotFound);
    };
    if status != "requested" {
        return Err(ApiError::Conflict(
            "only a requested refund can be rejected".into(),
        ));
    }

    let ctx = refund_context(&s, order_id).await?;

    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    sqlx::query(
        "UPDATE refunds SET status='rejected', decision_note=$2, decided_by=$3, decided_at=now() WHERE id=$1",
    )
    .bind(id)
    .bind(body.reason.trim())
    .bind(u.user_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;

    if let Some(email) = ctx.contact_email.clone() {
        let _ = email_customer(
            &mut tx,
            &email,
            net,
            charge,
            s.cfg.refund_bank_charge_pct,
            body.reason.trim(),
            "Refund request declined — Nhlanganiso",
            "We could not approve your refund request.",
        )
        .await;
    }

    audit(&mut tx, u.user_id, order_id, "order.refund_rejected").await;

    tx.commit().await.map_err(|_| ApiError::Internal)?;

    let (st, inner) = ApiResponse::ok(serde_json::json!({"ok": true, "status": "rejected"}));
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// Validate the requested amount against what is left to refund.
fn validate_amount(
    s: &SharedState,
    ctx: &RefundContext,
    requested: Option<i64>,
) -> ApiResult<(i64, i64, i64), ApiError> {
    if ctx.payment_status != "paid" && ctx.payment_status != "partially_refunded" {
        return Err(ApiError::Unprocessable(
            "only a paid order can be refunded".into(),
        ));
    }
    let remaining = ctx.paid_cents - ctx.refunded_cents;
    if remaining <= 0 {
        return Err(ApiError::Unprocessable(
            "order is already fully refunded".into(),
        ));
    }
    let gross = requested.unwrap_or(remaining);
    if gross <= 0 {
        return Err(ApiError::Unprocessable(
            "refund amount must be positive".into(),
        ));
    }
    if gross > remaining {
        return Err(ApiError::Unprocessable(format!(
            "refund of {gross} exceeds the refundable amount of {remaining}"
        )));
    }
    let (charge, net) = split_amount(gross, s.cfg.refund_bank_charge_pct);
    if net <= 0 {
        return Err(ApiError::Unprocessable(
            "refund is fully consumed by the bank charge".into(),
        ));
    }
    Ok((gross, charge, net))
}

async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: Uuid,
    order_id: Uuid,
    action: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO audit_logs (actor_user_id, action, entity, entity_id) VALUES ($1,$2,'order',$3)",
    )
    .bind(actor)
    .bind(action)
    .bind(order_id.to_string())
    .execute(&mut **tx)
    .await;
}

/// GET /api/v1/admin/refunds — all refunds, newest first.
async fn admin_list(
    State(s): State<SharedState>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let u = user_from_headers(&s, &headers).await?;
    if u.role != "admin" {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<RefundRow> = sqlx::query_as(
        "SELECT id, order_id, gross_cents, bank_charge_cents, net_cents, bank_charge_pct, currency, status, reason, decision_note, created_at, completed_at FROM refunds ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(RefundRow::into_json).collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}

/// GET /api/v1/orders/{id}/refunds — the customer's own order refunds.
async fn customer_list(
    State(s): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await?;

    let owner: Option<Option<Uuid>> = sqlx::query_scalar("SELECT user_id FROM orders WHERE id=$1")
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some(owner) = owner else {
        return Err(ApiError::NotFound);
    };
    if me.role != "admin" && owner != Some(me.user_id) {
        return Err(ApiError::Forbidden);
    }

    let rows: Vec<RefundRow> = sqlx::query_as(
        "SELECT id, order_id, gross_cents, bank_charge_cents, net_cents, bank_charge_pct, currency, status, reason, decision_note, created_at, completed_at FROM refunds WHERE order_id=$1 ORDER BY created_at DESC",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(RefundRow::into_json).collect();
    let (st, inner) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::json!(inner.data))))
}
