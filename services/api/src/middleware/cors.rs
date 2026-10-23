use axum::http::HeaderValue;

pub fn allowed_origins(origins: Vec<String>) -> tower_http::cors::AllowOrigin {
    let list: Vec<HeaderValue> = origins.iter().filter_map(|o| o.parse().ok()).collect();
    tower_http::cors::AllowOrigin::list(list)
}
