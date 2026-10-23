use crate::config::Config;
use crate::integrations::brevo::BrevoClient;
use sqlx::PgPool;
use redis::Client;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub db: PgPool,
    pub redis: Client,
    pub brevo: BrevoClient,
}
