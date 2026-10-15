use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::time::Duration;

use crate::config::Config;

pub async fn connect(cfg: &Config) -> anyhow::Result<sqlx::PgPool> {
    let opts: PgConnectOptions = cfg.database_url.parse()?;
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(60))
        .connect_with(opts)
        .await?;
    Ok(pool)
}
