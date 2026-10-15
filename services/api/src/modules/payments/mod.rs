use axum::{Json, Router, extract::{ConnectInfo, State}, http::StatusCode, routing::{get, post}};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use uuid::Uuid;
use crate::{
    errors::{ApiError, ApiResult},
    integrations::payfast as pf,
    middleware::authentication::user_from_headers,
    response::ApiResponse,
    state::SharedState,
    workers::email_outbox,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/payments/checkout", post(checkout))
        .route("/api/v1/payments/payfast/itn", post(itn))
        .route("/api/v1/payments/:id", get(get_payment))
}

#[derive(Deserialize)]
struct CheckoutReq {
    order_id: Uuid,
    name_first: Option<String>,
    name_last: Option<String>,
    email_address: Option<String>,
}

#[derive(Serialize)]
struct CheckoutRes {
    checkout_url: String,
    fields: std::collections::HashMap<String, String>,
}

/// Build hosted-checkout fields from trusted server-side order totals.
/// Never trust browser amounts.
async fn checkout(State(s): State<SharedState>, headers: axum::http::HeaderMap, Json(body): Json<CheckoutReq>) -> ApiResult<(StatusCode, Json<CheckoutRes>)> {
    let me = user_from_headers(&s, &headers).await.ok();
    let rec: Option<(Option<Uuid>, i64, String, String)> = sqlx::query_as(
        "SELECT user_id, total_cents, payment_status::text, reference FROM orders WHERE id=$1",
    )
    .bind(body.order_id).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((owner, total_cents, pay_status, reference)) = rec else { return Err(ApiError::NotFound) };
    if let Some(m) = &me {
        if m.role != "admin" && owner != Some(m.user_id) {
            return Err(ApiError::Forbidden);
        }
    }
    if pay_status == "paid" {
        return Err(ApiError::Unprocessable("order already paid".into()));
    }
    let m_payment_id = Uuid::new_v4().to_string();
    let payment_id: Uuid = sqlx::query_scalar(
        "INSERT INTO payments (order_id, provider, provider_ref, expected_amount_cents, currency, status, merchant_payment_id) VALUES ($1,'payfast',$2,$3,'ZAR','pending',$2) RETURNING id",
    )
    .bind(body.order_id).bind(&m_payment_id).bind(total_cents)
    .fetch_one(&s.db).await.map_err(|_| ApiError::Internal)?;

    // Documented field order matters for signature stability.
    let mut ordered: Vec<(String, String)> = vec![
        ("merchant_id".into(), s.cfg.payfast_merchant_id.clone()),
        ("merchant_key".into(), s.cfg.payfast_merchant_key.clone()),
        ("return_url".into(), format!("{}/checkout/success?order={}", s.cfg.public_web_url, body.order_id)),
        ("cancel_url".into(), format!("{}/checkout/cancel?order={}", s.cfg.public_web_url, body.order_id)),
        ("notify_url".into(), format!("{}/api/v1/payments/payfast/itn", s.cfg.public_api_url)),
        ("name_first".into(), body.name_first.clone().unwrap_or_default()),
        ("name_last".into(), body.name_last.clone().unwrap_or_default()),
        ("email_address".into(), body.email_address.clone().unwrap_or_default()),
        ("m_payment_id".into(), m_payment_id.clone()),
        ("amount".into(), pf::cents_to_payfast(total_cents)),
        ("item_name".into(), format!("Order {reference}")),
    ];
    // append email confirmation defaults
    ordered.push(("email_confirmation".into(), "1".into()));
    let sig = pf::generate_signature(&ordered, Some(&s.cfg.payfast_passphrase));
    let mut fields: std::collections::HashMap<String, String> = ordered.into_iter().collect();
    fields.insert("signature".into(), sig);
    let _ = payment_id;
    Ok((StatusCode::OK, Json(CheckoutRes { checkout_url: s.cfg.payfast_mode.checkout_url(), fields })))
}

async fn get_payment(State(s): State<SharedState>, headers: axum::http::HeaderMap, axum::extract::Path(id): axum::extract::Path<Uuid>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await?;
    let rec: Option<(Option<Uuid>, String, i64)> = sqlx::query_as(
        "SELECT o.user_id, p.status::text, p.expected_amount_cents FROM payments p JOIN orders o ON o.id=p.order_id WHERE p.id=$1",
    ).bind(id).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((owner, status, amount)) = rec else { return Err(ApiError::NotFound) };
    if me.role != "admin" && owner != Some(me.user_id) {
        return Err(ApiError::Forbidden);
    }
    let (st, Json(inner)) = ApiResponse::ok(serde_json::json!({"id": id, "status": status, "expected_amount_cents": amount}));
    Ok((st, Json(inner.data)))
}

/// PayFast ITN: `application/x-www-form-urlencoded`. Full validation:
/// signature → source → server VALID → merchant → amount → idempotency → atomic apply.
async fn itn(
    State(s): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: String,
) -> ApiResult<(StatusCode, String)> {
    // Preserve POST order for signature (form_urlencoded preserves order).
    let ordered: Vec<(String, String)> = parse_form_ordered(&body);
    let map: std::collections::HashMap<String, String> = ordered.iter().cloned().collect();
    let received_sig = map.get("signature").cloned().unwrap_or_default();

    // 1. Signature
    let expected = pf::generate_signature(&ordered, Some(&s.cfg.payfast_passphrase));
    if !pf::signatures_match(&expected, &received_sig) {
        return Err(ApiError::Forbidden);
    }
    // 2. Source validation (PayFast hostnames). Configurable strictness.
    if s.cfg.payfast_validate_ip && !source_allowed(&s, &addr.ip().to_string()).await {
        tracing::warn!(ip=%addr.ip(), "payfast ITN from unexpected source");
        return Err(ApiError::Forbidden);
    }
    // 3. Server validation: POST back to PayFast, expect "VALID".
    let valid = server_validate(&s, &ordered).await.map_err(|_| ApiError::Internal)?;
    if !valid {
        return Err(ApiError::Unprocessable("payfast server validation failed".into()));
    }
    // 4-6. Merchant + payment lookup
    let merchant_id = map.get("merchant_id").cloned().unwrap_or_default();
    if merchant_id != s.cfg.payfast_merchant_id {
        return Err(ApiError::Unprocessable("merchant mismatch".into()));
    }
    let m_payment_id = map.get("m_payment_id").cloned().unwrap_or_default();
    let pf_txn_id = map.get("pf_payment_id").cloned().unwrap_or_default();
    let gross = map.get("amount_gross").or(map.get("amount")).cloned().unwrap_or_default();
    let pay_status = map.get("payment_status").cloned().unwrap_or_default();

    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let rec: Option<(Uuid, Uuid, i64, String)> = sqlx::query_as(
        "SELECT p.id, p.order_id, p.expected_amount_cents, p.status::text FROM payments p WHERE p.merchant_payment_id=$1 FOR UPDATE",
    ).bind(&m_payment_id).fetch_optional(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    let Some((payment_id, order_id, expected_cents, cur_status)) = rec else {
        // Unknown payment: record event for reconciliation, do not crash.
        sqlx::query("INSERT INTO payment_events (merchant_payment_id, provider_txn_id, payload, result) VALUES ($1,$2,$3,'unknown-payment')")
            .bind(&m_payment_id).bind(&pf_txn_id).bind(serde_json::json!({"ip": addr.ip().to_string()}))
            .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Err(ApiError::NotFound);
    };
    // 7. Amount (exact)
    if !pf::amounts_match_cents(&gross, expected_cents) {
        sqlx::query("INSERT INTO payment_events (payment_id, merchant_payment_id, provider_txn_id, payload, result) VALUES ($1,$2,$3,$4,'amount-mismatch')")
            .bind(payment_id).bind(&m_payment_id).bind(&pf_txn_id).bind(serde_json::json!({"gross": gross, "expected_cents": expected_cents}))
            .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Err(ApiError::Unprocessable("amount mismatch".into()));
    }
    // 8. Only COMPLETE settles; others record without fulfilment side-effects.
    if pay_status != "COMPLETE" {
        sqlx::query("UPDATE payments SET status='failed' WHERE id=$1 AND status='pending'").bind(payment_id).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        sqlx::query("INSERT INTO payment_events (payment_id, merchant_payment_id, provider_txn_id, payload, result) VALUES ($1,$2,$3,$4,'non-complete')")
            .bind(payment_id).bind(&m_payment_id).bind(&pf_txn_id).bind(serde_json::json!({"status": pay_status}))
            .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Ok((StatusCode::OK, "OK".into()));
    }
    // 9. Duplicate provider txn guard (unique constraint + explicit check).
    let dup: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM payments WHERE provider_txn_id=$1 AND id<>$2 AND status='paid')")
        .bind(&pf_txn_id).bind(payment_id).fetch_one(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    if dup {
        return Err(ApiError::Conflict("duplicate provider transaction".into()));
    }
    // 10. Idempotent apply: if already paid, just record event, no double side-effects.
    if cur_status == "paid" {
        sqlx::query("INSERT INTO payment_events (payment_id, merchant_payment_id, provider_txn_id, payload, result) VALUES ($1,$2,$3,$4,'duplicate-itn')")
            .bind(payment_id).bind(&m_payment_id).bind(&pf_txn_id).bind(serde_json::json!({}))
            .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Ok((StatusCode::OK, "OK".into()));
    }
    sqlx::query("UPDATE payments SET status='paid', provider_txn_id=$2, paid_at=now() WHERE id=$1")
        .bind(payment_id).bind(&pf_txn_id).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    sqlx::query("UPDATE orders SET payment_status='paid' WHERE id=$1 AND payment_status<>'paid'")
        .bind(order_id).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    sqlx::query("INSERT INTO payment_events (payment_id, merchant_payment_id, provider_txn_id, payload, result) VALUES ($1,$2,$3,$4,'paid')")
        .bind(payment_id).bind(&m_payment_id).bind(&pf_txn_id).bind(serde_json::json!({"gross": gross}))
        .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    // 12. Receipt email in same transaction (durable outbox).
    let contact: Option<String> = sqlx::query_scalar("SELECT contact_email FROM orders WHERE id=$1").bind(order_id).fetch_optional(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    if let Some(email) = contact {
        let html = email_outbox::branded_template("Payment confirmed", &format!("<p>Payment <b>{}</b> confirmed. Thank you.</p>", pf_txn_id));
        email_outbox::enqueue(&mut tx, &email, "Payment confirmed — Nhlanganiso", &html).await.map_err(|_| ApiError::Internal)?;
    }
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    Ok((StatusCode::OK, "OK".into()))
}

fn parse_form_ordered(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("").to_string();
            let v_enc = it.next().unwrap_or("");
            // PayFast posts `+` for space.
            let v_plus = v_enc.replace('+', " ");
            let v = urlencoding::decode(&v_plus).map(|c| c.into_owned()).unwrap_or(v_plus);
            let k_norm = urlencoding::decode(&k).map(|c| c.into_owned()).unwrap_or(k);
            (k_norm, v)
        })
        .collect()
}

async fn source_allowed(s: &SharedState, ip: &str) -> bool {
    if s.cfg.payfast_allowed_ips.iter().any(|a| a == ip) {
        return true;
    }
    // Known PayFast egress ranges change over time; canonical check is reverse-DNS
    // to *.payfast.co.za. We cannot do a reverse lookup without DNS here, so in
    // strict mode only explicitly allowed IPs pass, and operators must confirm
    // ranges in the PayFast dashboard docs. Defaults deny; documented override
    // via PAYFAST_ALLOWED_IPS.
    false
}

async fn server_validate(s: &SharedState, ordered: &[(String, String)]) -> anyhow::Result<bool> {
    // PayFast expects the exact ITN body posted back for validation.
    let body: String = ordered.iter().map(|(k, v)| format!("{k}={}", urlencoding::encode(v))).collect::<Vec<_>>().join("&");
    let res = s.http.post(s.cfg.payfast_mode.validate_url())
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send().await?;
    let text = res.text().await.unwrap_or_default();
    Ok(text.trim() == "VALID")
}
