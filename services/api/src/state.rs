use std::sync::Arc;

use crate::config::Config;
use crate::integrations::brevo::BrevoClient;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub db: PgPool,
    pub redis: redis::Client,
    pub http: reqwest::Client,
    pub brevo: BrevoClient,
}

impl AppState {
    pub async fn new(cfg: Config, db: PgPool) -> anyhow::Result<Arc<Self>> {
        let redis = redis::Client::open(cfg.redis_url.clone())?;
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("nhlanganiso-api/0.1")
            .build()?;
        let brevo = BrevoClient::new(
            http.clone(),
            cfg.brevo_api_key.clone(),
            cfg.brevo_sender_email.clone(),
            cfg.brevo_sender_name.clone(),
        );
        Ok(Arc::new(Self {
            cfg,
            db,
            redis,
            http,
            brevo,
        }))
    }
}

pub type SharedState = Arc<AppState>;
