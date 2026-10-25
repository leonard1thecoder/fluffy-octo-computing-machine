use std::{sync::Arc, time::Duration};

use anyhow::Context;
use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SEND_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";

#[derive(Clone)]
pub struct GmailClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    refresh_token: String,
    sender_email: String,
    sender_name: String,
    access_token: Arc<Mutex<Option<CachedToken>>>,
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: tokio::time::Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Serialize)]
struct SendRequest {
    raw: String,
}

impl GmailClient {
    pub fn new(
        client_id: String,
        client_secret: String,
        refresh_token: String,
        sender_email: String,
        sender_name: String,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(15))
            .user_agent("nhlanganiso-api/0.1")
            .build()?;
        Ok(Self {
            http,
            client_id: client_id.trim().to_owned(),
            client_secret: client_secret.trim().to_owned(),
            refresh_token: refresh_token.trim().to_owned(),
            sender_email: sender_email.trim().to_owned(),
            sender_name: sender_name.trim().to_owned(),
            access_token: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn send(&self, to: &str, subject: &str, html: &str) -> anyhow::Result<String> {
        anyhow::ensure!(
            !self.client_id.is_empty()
                && !self.client_secret.is_empty()
                && !self.refresh_token.is_empty(),
            "GMAIL_CLIENT_ID, GMAIL_CLIENT_SECRET, and GMAIL_REFRESH_TOKEN must be configured"
        );
        anyhow::ensure!(
            !self.sender_email.is_empty(),
            "GMAIL_SENDER_EMAIL must be configured"
        );
        anyhow::ensure!(
            !to.chars().any(|c| matches!(c, '\r' | '\n')),
            "recipient contains an invalid line break"
        );
        anyhow::ensure!(
            !self.sender_email.chars().any(|c| matches!(c, '\r' | '\n')),
            "GMAIL_SENDER_EMAIL contains an invalid line break"
        );

        let token = self.get_access_token().await?;
        let raw = general_purpose::URL_SAFE_NO_PAD.encode(build_mime_message(
            to,
            &self.sender_email,
            &self.sender_name,
            subject,
            html,
        ));
        let started_at = tokio::time::Instant::now();
        let response = self
            .http
            .post(SEND_URL)
            .bearer_auth(token)
            .json(&SendRequest { raw })
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    timed_out = error.is_timeout(),
                    connection_error = error.is_connect(),
                    error = %error,
                    "Gmail API request failed"
                );
                return Err(error).context("sending email through Gmail API");
            }
        };
        tracing::info!(
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            status = %response.status(),
            "Gmail API response received"
        );
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Gmail API {status}: {body:.500}");
        }
        let result: serde_json::Value = response
            .json()
            .await
            .context("parsing Gmail API send response")?;
        result
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .context("Gmail API response did not include a message id")
    }

    async fn get_access_token(&self) -> anyhow::Result<String> {
        let mut cached = self.access_token.lock().await;
        if let Some(token) = cached.as_ref() {
            if token.expires_at > tokio::time::Instant::now() + Duration::from_secs(60) {
                return Ok(token.value.clone());
            }
        }

        let mut response = None;
        for attempt in 1..=3 {
            let started_at = tokio::time::Instant::now();
            let result = self
                .http
                .post(TOKEN_URL)
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("client_secret", self.client_secret.as_str()),
                    ("refresh_token", self.refresh_token.as_str()),
                    ("grant_type", "refresh_token"),
                ])
                .send()
                .await;

            match result {
                Ok(result) if result.status().is_server_error() && attempt < 3 => {
                    tracing::warn!(
                        attempt,
                        status = %result.status(),
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "Google OAuth token endpoint returned a server error; retrying"
                    );
                }
                Ok(result) => {
                    response = Some(result);
                    break;
                }
                Err(error) if attempt < 3 && (error.is_timeout() || error.is_connect()) => {
                    tracing::warn!(
                        attempt,
                        timed_out = error.is_timeout(),
                        connection_error = error.is_connect(),
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error = %error,
                        "Google OAuth token request failed transiently; retrying"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        attempt,
                        timed_out = error.is_timeout(),
                        connection_error = error.is_connect(),
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error = %error,
                        "Google OAuth token request failed"
                    );
                    return Err(error).context("refreshing Gmail API access token");
                }
            }

            tokio::time::sleep(Duration::from_secs(attempt as u64)).await;
        }
        let response = response.context("Google OAuth token request retries exhausted")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Google OAuth token refresh {status}: {body:.500}");
        }
        let token: TokenResponse = response
            .json()
            .await
            .context("parsing Google OAuth token response")?;
        let expires_at = tokio::time::Instant::now() + Duration::from_secs(token.expires_in);
        *cached = Some(CachedToken {
            value: token.access_token.clone(),
            expires_at,
        });
        Ok(token.access_token)
    }
}

fn build_mime_message(
    to: &str,
    sender_email: &str,
    sender_name: &str,
    subject: &str,
    html: &str,
) -> String {
    let boundary = format!("nhlanganiso_{}", uuid::Uuid::new_v4().simple());
    let from = if sender_name.is_empty() {
        sender_email.to_owned()
    } else {
        format!(
            "=?UTF-8?B?{}?= <{}>",
            general_purpose::STANDARD.encode(sender_name),
            sender_email
        )
    };
    let plain_text = html_to_text(html);
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: =?UTF-8?B?{}?=\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}\r\nContent-Type: text/html; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--{boundary}--\r\n",
        general_purpose::STANDARD.encode(subject),
        mime_base64(plain_text.as_bytes()),
        mime_base64(html.as_bytes())
    )
}

fn mime_base64(value: &[u8]) -> String {
    general_purpose::STANDARD
        .encode(value)
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 output is valid ASCII"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut inside_tag = false;
    for character in html.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => inside_tag = false,
            _ if !inside_tag => text.push(character),
            _ => {}
        }
    }
    text
}
