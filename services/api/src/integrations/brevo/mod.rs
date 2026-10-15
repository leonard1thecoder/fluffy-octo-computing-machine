use serde::Serialize;

#[derive(Clone)]
pub struct BrevoClient {
    http: reqwest::Client,
    api_key: String,
    sender_email: String,
    sender_name: String,
}

#[derive(Serialize)]
struct Sender { email: String, name: String }
#[derive(Serialize)]
struct To { email: String }
#[derive(Serialize)]
struct Payload { sender: Sender, to: Vec<To>, subject: String, #[serde(rename = "htmlContent")] html: String, #[serde(rename = "textContent")] text: String }

impl BrevoClient {
    pub fn new(http: reqwest::Client, api_key: String, sender_email: String, sender_name: String) -> Self {
        Self { http, api_key, sender_email, sender_name }
    }

    /// POST https://api.brevo.com/v3/smtp/email — returns provider messageId.
    pub async fn send(&self, to: &str, subject: &str, html: &str) -> anyhow::Result<String> {
        if self.api_key.is_empty() {
            anyhow::bail!("BREVO_API_KEY not configured");
        }
        let res = self.http.post("https://api.brevo.com/v3/smtp/email")
            .header("api-key", &self.api_key)
            .json(&Payload {
                sender: Sender { email: self.sender_email.clone(), name: self.sender_name.clone() },
                to: vec![To { email: to.to_string() }],
                subject: subject.to_string(),
                html: html.to_string(),
                text: html_to_text(html),
            })
            .send().await?;
        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            anyhow::bail!("brevo {status}: {body:.500}");
        }
        let v: serde_json::Value = res.json().await.unwrap_or_default();
        Ok(v.get("messageId").and_then(|m| m.as_str()).unwrap_or("").to_string())
    }
}

fn html_to_text(h: &str) -> String {
    // naive strip; avoids pulling an html parser for transactional mail
    let mut out = String::with_capacity(h.len());
    let mut in_tag = false;
    for c in h.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}
