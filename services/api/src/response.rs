use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> (StatusCode, Json<Self>) {
        (StatusCode::OK, Json(Self { data, meta: None }))
    }
    pub fn created(data: T) -> (StatusCode, Json<Self>) {
        (StatusCode::CREATED, Json(Self { data, meta: None }))
    }
}

#[derive(Serialize)]
pub struct Paged<T: Serialize> {
    pub items: Vec<T>,
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
}

#[allow(dead_code)]
pub fn paged<T: Serialize>(
    items: Vec<T>,
    page: u32,
    per_page: u32,
    total: i64,
) -> impl IntoResponse {
    let (status, body) = ApiResponse::ok(Paged {
        items,
        page,
        per_page,
        total,
    });
    let meta = serde_json::json!({"page": page, "per_page": per_page, "total": total});
    (
        status,
        Json(serde_json::json!({"data": body.0.data, "meta": meta})),
    )
        .into_response()
}
