use crate::{errors::ApiError, errors::ApiResult, state::SharedState};
use axum::{extract::State, routing::get, Json, Router};
use serde::Serialize;
use uuid::Uuid;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/v1/delivery-zones", get(list))
}

#[derive(Serialize, sqlx::FromRow)]
struct DeliveryZone {
    id: Uuid,
    name: String,
    fee_cents: i64,
}

/// Public list of the areas we deliver to and their fee. Checkout takes one of these ids as
/// `delivery_zone_id`. Only active zones are returned.
async fn list(State(s): State<SharedState>) -> ApiResult<Json<Vec<DeliveryZone>>> {
    let zones: Vec<DeliveryZone> = sqlx::query_as(
        "SELECT id, name, fee_cents FROM delivery_zones WHERE active = true ORDER BY fee_cents, name",
    )
    .fetch_all(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(Json(zones))
}
