use axum::{Router, http::StatusCode, middleware::from_fn, routing::get};
use std::time::Duration;
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer, trace::TraceLayer};
use crate::state::SharedState;

pub fn router(state: SharedState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(crate::middleware::cors::allowed_origins(state.cfg.cors_allowed_origins.clone()))
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST, axum::http::Method::PATCH, axum::http::Method::DELETE, axum::http::Method::PUT])
        .allow_headers([axum::http::header::AUTHORIZATION, axum::http::header::CONTENT_TYPE, axum::http::header::ACCEPT]);

    Router::new()
        .route("/api/v1/health/live", get(|| async { (StatusCode::OK, "ok") }))
        .route("/api/v1/health/ready", get(ready))
        .merge(crate::modules::auth::router())
        .merge(crate::modules::users::router())
        .merge(crate::modules::categories::router())
        .merge(crate::modules::products::router())
        .merge(crate::modules::inventory::router())
        .merge(crate::modules::cart::router())
        .merge(crate::modules::orders::router())
        .merge(crate::modules::payments::router())
        .merge(crate::modules::catering::router())
        .merge(crate::modules::quotations::router())
        .merge(crate::modules::notifications::router())
        .merge(crate::modules::admin::router())
        .layer(from_fn(crate::middleware::request_id::inject))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)))
        .with_state(state)
}

async fn ready(axum::extract::State(s): axum::extract::State<SharedState>) -> (StatusCode, String) {
    if sqlx::query("SELECT 1").fetch_one(&s.db).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "db unavailable".into());
    }
    (StatusCode::OK, "ready".into())
}
