use tokio::task_local;

task_local! { static REQ_ID: String }

pub fn current_request_id() -> String {
    REQ_ID.try_with(|v| v.clone()).unwrap_or_else(|_| "unknown".into())
}

pub async fn inject(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let id = req.headers().get("x-request-id").and_then(|v| v.to_str().ok()).map(str::to_string).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut res = REQ_ID.scope(id.clone(), next.run(req)).await;
    if let Ok(hv) = axum::http::header::HeaderValue::from_str(&id) {
        res.headers_mut().insert("x-request-id", hv);
    }
    res
}
