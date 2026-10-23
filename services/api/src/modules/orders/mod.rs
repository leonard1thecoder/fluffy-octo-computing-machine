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
        .route("/api/v1/orders/{id}", get(get_one))
        .route("/api/v1/orders/{id}/payment-status", get(payment_status))
        .route("/api/v1/orders/{id}/cancel", post(cancel))
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
    coupon_code: Option<String>,
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
    let mut discount_cents: i64 = 0;
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

    // Coupon validation + discount: requires a verified subscriber email.
    if let Some(ref code) = body.coupon_code {
        let code_upper = code.trim().to_uppercase();
        let rec: Option<(
            Uuid,
            i32,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<i32>,
            i32,
            bool,
            Option<String>,
            Option<uuid::Uuid>,
        )> = sqlx::query_as(
            "SELECT id, discount_percentage, expires_at, max_usages, usage_count, active, subscriber_email, product_id FROM coupons WHERE code=$1 FOR UPDATE",
        )
        .bind(&code_upper)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
        let Some((
            cid,
            discount_pct,
            expires_at,
            max_usages,
            usage_count,
            active,
            coupon_subscriber_email,
            coupon_product_id,
        )) = rec
        else {
            return Err(ApiError::Unprocessable("unknown coupon code".into()));
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
        let sub: Option<String> = sqlx::query_scalar(
            "SELECT status::text FROM subscribers WHERE email_normalized=$1 AND status='verified'",
        )
        .bind(&body.contact_email.trim().to_lowercase())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
        if sub.is_none() {
            return Err(ApiError::Unprocessable(
                "coupon requires a verified subscription".into(),
            ));
        }

        // Individual-subscriber coupon: only the nominated email may claim it.
        if let Some(ref sub_email) = coupon_subscriber_email {
            let order_email = body.contact_email.trim().to_lowercase();
            if order_email != *sub_email {
                return Err(ApiError::Unprocessable(
                    "coupon is not valid for this email".into(),
                ));
            }
        }

        // Single-product coupon: at least one order line must match the product.
        if let Some(ref prod_id) = coupon_product_id {
            let has_product = lines.iter().any(|(pid, _, _, _, _)| pid == prod_id);
            if !has_product {
                return Err(ApiError::Unprocessable(
                    "coupon is only valid for a specific product in this order".into(),
                ));
            }
        }

        // One coupon, one subscriber: reject if this email already claimed any
        // coupon, or if this coupon was already claimed by anyone.
        let already_used: Option<Uuid> = sqlx::query_scalar(
            "SELECT coupon_id FROM coupon_usages WHERE email_normalized=$1 LIMIT 1",
        )
        .bind(&body.contact_email.trim().to_lowercase())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
        if already_used.is_some() {
            return Err(ApiError::Unprocessable(
                "subscriber already used a coupon".into(),
            ));
        }
        let coupon_claimed: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM coupon_usages WHERE coupon_id=$1 LIMIT 1")
                .bind(cid)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| ApiError::Internal)?;
        if coupon_claimed.is_some() {
            return Err(ApiError::Unprocessable("coupon already claimed".into()));
        }

        discount_cents = (subtotal as f64 * discount_pct as f64 / 100.0).round() as i64;

        sqlx::query("UPDATE coupons SET usage_count = usage_count + 1 WHERE id=$1")
            .bind(cid)
            .execute(&mut *tx)
            .await
            .map_err(|_| ApiError::Internal)?;
        sqlx::query(
            "INSERT INTO coupon_usages (coupon_id, email_normalized, used_at) VALUES ($1,$2,now())",
        )
        .bind(cid)
        .bind(&body.contact_email.trim().to_lowercase())
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
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
    let total = subtotal + delivery_fee - discount_cents;
    let reference = order_ref();
    let order_id: Uuid = sqlx::query_scalar(
        "INSERT INTO orders (reference, user_id, contact_name, contact_email, contact_phone, fulfilment_method, delivery_zone_id, delivery_address, notes, currency, subtotal_cents, discount_cents, delivery_fee_cents, total_cents, payment_status, fulfilment_status) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'ZAR',$10,$11,$12,$13,'pending','new') RETURNING id",
    )
    .bind(&reference)
    .bind(me.map(|m| m.user_id))
    .bind(&body.contact_name)
    .bind(&body.contact_email)
    .bind(&body.contact_phone)
    .bind(&body.fulfilment_method)
    .bind(body.delivery_zone_id)
    .bind(&body.delivery_address)
    .bind(&body.notes)
    .bind(subtotal)
    .bind(discount_cents)
    .bind(delivery_fee)
    .bind(total)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;
    for (pid, vid, name, qty, unit) in lines {
        sqlx::query("INSERT INTO order_items (order_id, product_id, variant_id, product_name, qty, unit_price_cents, line_total_cents) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(order_id).bind(pid).bind(vid).bind(name).bind(qty).bind(unit).bind(unit * (qty as i64))
            .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    }
    let html = email_outbox::branded_template(
        "Order received",
        &format!(
            "<p>Reference <b>{}</b> — subtotal R{:.2}{discount}. Pay via checkout to confirm.</p>",
            reference,
            subtotal as f64 / 100.0,
            discount = if discount_cents > 0 {
                format!(" — discount -R{:.2}", discount_cents as f64 / 100.0)
            } else {
                String::new()
            }
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
    let (st, Json(inner)) = ApiResponse::created(serde_json::json!({
        "id": order_id,
        "reference": reference,
        "subtotal_cents": subtotal,
        "discount_cents": discount_cents,
        "total_cents": total,
        "currency": "ZAR",
        "coupon_code": body.coupon_code.map(|c| c.trim().to_uppercase()),
    }));
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
