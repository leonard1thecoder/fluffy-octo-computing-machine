use std::sync::Arc;

use anyhow::Context;

use crate::config::Config;
use crate::integrations::gmail::GmailClient;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub db: PgPool,
    pub redis: Option<redis::Client>,
    pub http: reqwest::Client,
    pub gmail: GmailClient,
}

impl AppState {
    pub async fn new(cfg: Config, db: PgPool) -> anyhow::Result<Arc<Self>> {
        let redis = match cfg.redis_url.clone() {
            Some(url) => Some(redis::Client::open(url).with_context(|| {
                "REDIS_URL did not parse; expected a Redis TCP URL like \
                 rediss://default:PASSWORD@host:6379 (Upstash: use the Redis endpoint, \
                 not the https:// REST endpoint)"
            })?),
            None => {
                tracing::warn!("REDIS_URL unset; starting WITHOUT rate limiting");
                None
            }
        };
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("nhlanganiso-api/0.1")
            .build()?;
        let gmail = GmailClient::new(
            cfg.gmail_client_id.clone(),
            cfg.gmail_client_secret.clone(),
            cfg.gmail_refresh_token.clone(),
            cfg.gmail_sender_email.clone(),
            cfg.gmail_sender_name.clone(),
        )?;
        Ok(Arc::new(Self {
            cfg,
            db,
            redis,
            http,
            gmail,
        }))
    }
}

pub type SharedState = Arc<AppState>;
