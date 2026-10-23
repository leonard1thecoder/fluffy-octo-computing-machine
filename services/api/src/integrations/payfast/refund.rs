use serde::Deserialize;

/// PayFast Refunds API (https://developers.payfast.co.za/api).
///
/// POST https://api.payfast.co.za/refunds/{pf_payment_id}
/// Headers: merchant-id, version: v1, timestamp: ISO-8601
///
/// The id in the path is the PayFast transaction id (`pf_payment_id`), i.e. the
/// `pf_payment_id` we received on the ITN - NOT our merchant payment id.
const REFUND_API_BASE: &str = "https://api.payfast.co.za";

#[derive(Debug, Deserialize)]
pub struct RefundResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub response: Option<RefundPayload>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct RefundPayload {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub amount: Option<f64>,
    #[serde(default)]
    pub note: Option<String>,
}

/// ISO-8601 timestamp PayFast expects, e.g. 2026-10-21T15:04:05+02:00.
fn timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S+00:00")
        .to_string()
}

/// Request a refund from PayFast for `pf_payment_id`.
///
/// `amount_cents` is the amount PayFast returns to the customer (already net of
/// our bank charge). Returns the raw response so the caller can record it.
pub async fn request_refund(
    http: &reqwest::Client,
    merchant_id: &str,
    pf_payment_id: &str,
    amount_cents: i64,
    reason: &str,
) -> anyhow::Result<RefundResponse> {
    let url = format!("{REFUND_API_BASE}/refunds/{pf_payment_id}");
    let body = serde_json::json!({
        "amount": amount_cents as f64 / 100.0,
        "reason": reason,
        "type": "manual",
    });

    let resp = http
        .post(&url)
        .header("merchant-id", merchant_id)
        .header("version", "v1")
        .header("timestamp", timestamp())
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("payfast refund api {status}: {text:.400}");
    }
    serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("payfast refund response invalid: {e}"))
}
