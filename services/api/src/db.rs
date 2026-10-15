use anyhow::Context;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::time::Duration;

use crate::config::Config;

/// Startup dial attempts. Render provisions the API and Postgres simultaneously,
/// so the first dials routinely fail while the database is still coming up.
/// Retrying here (instead of crashlooping) also keeps the real error visible.
///
/// Budget is deliberately under ~30s (platform startup grace periods kill the
/// container past that and restart it, which retries the whole sequence).
const MAX_ATTEMPTS: u32 = 8;
const RETRY_DELAY: Duration = Duration::from_secs(3);

pub async fn connect(cfg: &Config) -> anyhow::Result<sqlx::PgPool> {
    let mut opts: PgConnectOptions = cfg
        .database_url
        .parse()
        .context("DATABASE_URL is not a valid Postgres connection string")?;

    // Log the dial target with the password redacted, so a startup failure in
    // the deploy logs names the host instead of only an OS error.
    tracing::info!(
        host = %opts.get_host(),
        port = opts.get_port(),
        database = opts.get_database().unwrap_or("?"),
        user = opts.get_username(),
        "connecting to postgres"
    );

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

    let mut last_err = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match PgPoolOptions::new()
            .max_connections(cfg.database_max_connections)
            .min_connections(cfg.database_min_connections)
            .acquire_timeout(Duration::from_secs(5))
            .idle_timeout(Duration::from_secs(60))
            .connect_with(opts.clone())
            .await
        {
            Ok(pool) => return Ok(pool),
            Err(e) => {
                tracing::warn!(attempt, "postgres dial failed: {e:#}; retrying");
                last_err = Some(e);
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
    Err(last_err.unwrap()).with_context(|| {
        format!(
            "connecting to postgres at {}:{} (db {})",
            opts.get_host(),
            opts.get_port(),
            opts.get_database().unwrap_or("?")
        )
    })
}
