use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::time::Duration;

use crate::config::Config;

pub async fn connect(cfg: &Config) -> anyhow::Result<sqlx::PgPool> {
    let mut opts: PgConnectOptions = cfg.database_url.parse()?;

    // Supabase transaction pooler (Supavisor/PgBouncer Tx mode, port 6543) does
    // NOT support prepared statements — the statement cache must be disabled or
    // every query fails. Session pooler (5432) and direct connections support them.
    //
    // Precedence: explicit DATABASE_STATEMENT_CACHE_SIZE wins; otherwise auto-disable
    // (size 0) when a :6543 pooler URL is detected; otherwise leave the driver default.
    match cfg.database_statement_cache_size {
        Some(size) => {
            opts = opts.statement_cache_capacity(size);
        }
        None if cfg.uses_supabase_tx_pooler() => {
            tracing::warn!("transaction-mode pooler detected (:6543); disabling prepared statements (statement_cache_capacity=0)");
            opts = opts.statement_cache_capacity(0);
        }
        None => {}
    }

    let pool = PgPoolOptions::new()
        .max_connections(cfg.database_max_connections)
        .min_connections(cfg.database_min_connections)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(60))
        .connect_with(opts)
        .await?;
    Ok(pool)
}
