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
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use validator::Validate;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/orders", post(create).get(mine_alias))
        .route("/api/v1/orders/mine", get(mine_alias))
        .route("/api/v1/orders/:id", get(get_one))
        .route("/api/v1/orders/:id/payment-status", get(payment_status))
        .route("/api/v1/orders/:id/cancel", post(cancel))
}

#[derive(Deserialize, Validate)]
struct CreateReq {
    #[validate(length(min = 1))]
    items: Vec<Item>,
    fulfilment_method: String, // pickup | delivery
    delivery_zone_id: Option<Uuid>,
    #[validate(email)]
    contact_email: String,
    #[validate(length(min = 1, max = 120))]
    contact_name: String,
    contact_phone: Option<String>,
    delivery_address: Option<String>,
    notes: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct Item {
    product_id: Uuid,
    variant_id: Option<Uuid>,
    qty: i32,
}

fn order_ref() -> String {
    format!(
        "NHL-{}-{}",
        chrono::Utc::now().format("%Y%m%d"),
        Uuid::new_v4().to_string()[..8].to_uppercase()
    )
}

async fn create(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await.ok(); // guest checkout allowed
    if body.items.is_empty() || body.items.len() > 50 {
        return Err(ApiError::Unprocessable("1..50 lines".into()));
    }
    if body.fulfilment_method != "pickup" && body.fulfilment_method != "delivery" {
        return Err(ApiError::Unprocessable(
            "fulfilment_method must be pickup|delivery".into(),
        ));
    }
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let mut subtotal: i64 = 0;
    let mut lines: Vec<(Uuid, Option<Uuid>, String, i32, i64)> = vec![];
    for it in &body.items {
        if it.qty < 1 || it.qty > 99 {
            return Err(ApiError::Unprocessable("qty 1..99".into()));
        }
        let rec: Option<(String, i64, bool, Option<i32>)> = sqlx::query_as(
            "SELECT name, price_cents, active, stock_on_hand FROM products WHERE id=$1 FOR UPDATE",
        )
        .bind(it.product_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
        let Some((name, price, active, stock)) = rec else {
            return Err(ApiError::Unprocessable("unknown product".into()));
        };
        if !active {
            return Err(ApiError::Unprocessable("product inactive".into()));
        }
        let delta: i64 = if let Some(vid) = it.variant_id {
            sqlx::query_scalar("SELECT COALESCE(price_delta_cents,0) FROM product_variants WHERE id=$1 AND product_id=$2")
                .bind(vid).bind(it.product_id).fetch_optional(&mut *tx).await.map_err(|_| ApiError::Internal)?.unwrap_or(0)
        } else {
            0
        };
        let unit = price + delta;
        if let Some(on_hand) = stock {
            if on_hand < it.qty {
                return Err(ApiError::Unprocessable(format!(
                    "insufficient stock for {name}"
                )));
            }
            sqlx::query("UPDATE products SET stock_on_hand = stock_on_hand - $2 WHERE id=$1")
                .bind(it.product_id)
                .bind(it.qty)
                .execute(&mut *tx)
                .await
                .map_err(|_| ApiError::Internal)?;
            sqlx::query("INSERT INTO inventory_movements (product_id, qty_delta, reason) VALUES ($1,$2,'order-reserve')")
                .bind(it.product_id).bind(-(it.qty)).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        }
        subtotal += unit * (it.qty as i64);
        lines.push((it.product_id, it.variant_id, name, it.qty, unit));
    }
    let delivery_fee: i64 = if body.fulfilment_method == "delivery" {
        let zid = body
            .delivery_zone_id
            .ok_or(ApiError::Unprocessable("delivery_zone_id required".into()))?;
        sqlx::query_scalar("SELECT fee_cents FROM delivery_zones WHERE id=$1 AND active=true")
            .bind(zid)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or(ApiError::Unprocessable("unknown delivery zone".into()))?
    } else {
        0
    };
    let total = subtotal + delivery_fee;
    let reference = order_ref();
    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (reference, user_id, contact_name, contact_email, contact_phone, fulfilment_method, delivery_zone_id, delivery_address, notes, currency, subtotal_cents, delivery_fee_cents, total_cents, payment_status, fulfilment_status) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'ZAR',$10,$11,$12,'pending','new') RETURNING id",
    )
    .bind(&reference).bind(me.map(|m| m.user_id)).bind(&body.contact_name).bind(&body.contact_email)
    .bind(&body.contact_phone).bind(&body.fulfilment_method).bind(body.delivery_zone_id).bind(&body.delivery_address).bind(&body.notes)
    .bind(subtotal).bind(delivery_fee).bind(total)
    .fetch_one(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    for (pid, vid, name, qty, unit) in lines {
        sqlx::query("INSERT INTO order_items (order_id, product_id, variant_id, product_name, qty, unit_price_cents, line_total_cents) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(order_id).bind(pid).bind(vid).bind(name).bind(qty).bind(unit).bind(unit * (qty as i64))
            .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    }
    let html = email_outbox::branded_template(
        "Order received",
        &format!(
            "<p>Reference <b>{}</b> — total R{:.2}. Pay via checkout to confirm.</p>",
            reference,
            total as f64 / 100.0
        ),
    );
    email_outbox::enqueue(
        &mut tx,
        &body.contact_email,
        &format!("Order {reference} received"),
        &html,
    )
    .await
    .map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (st, Json(inner)) = ApiResponse::created(
        serde_json::json!({"id": order_id, "reference": reference, "total_cents": total, "currency": "ZAR"}),
    );
    Ok((st, Json(inner.data)))
}

async fn mine_alias(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await?;
    let rows: Vec<(Uuid, String, i64, String)> = sqlx::query_as("SELECT id, reference, total_cents, payment_status::text FROM orders WHERE user_id=$1 ORDER BY created_at DESC LIMIT 50")
        .bind(me.user_id).fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(|(id, r, t, ps)| serde_json::json!({"id": id, "reference": r, "total_cents": t, "payment_status": ps})).collect();
    let (st, Json(inner)) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(inner.data).unwrap())))
}

async fn get_one(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await?;
    let rec: Option<(Option<Uuid>, String, i64, String, String)> = sqlx::query_as("SELECT user_id, reference, total_cents, payment_status::text, fulfilment_status::text FROM orders WHERE id=$1")
        .bind(id).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((owner, reference, total, ps, fs)) = rec else {
        return Err(ApiError::NotFound);
    };
    if me.role != "admin" && owner != Some(me.user_id) {
        return Err(ApiError::Forbidden);
    }
    let (st, Json(inner)) = ApiResponse::ok(
        serde_json::json!({"id": id, "reference": reference, "total_cents": total, "payment_status": ps, "fulfilment_status": fs}),
    );
    Ok((st, Json(inner.data)))
}

async fn payment_status(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    get_one(State(s), headers, Path(id)).await
}

async fn cancel(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let me = user_from_headers(&s, &headers).await?;
    let rec: Option<(Option<Uuid>, String)> =
        sqlx::query_as("SELECT user_id, payment_status::text FROM orders WHERE id=$1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let Some((owner, ps)) = rec else {
        return Err(ApiError::NotFound);
    };
    if me.role != "admin" && owner != Some(me.user_id) {
        return Err(ApiError::Forbidden);
    }
    if ps == "paid" {
        return Err(ApiError::Unprocessable(
            "paid orders cannot be cancelled online".into(),
        ));
    }
    sqlx::query("UPDATE orders SET fulfilment_status='cancelled' WHERE id=$1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    let (st, Json(inner)) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(inner.data)))
}
