mod app;
mod config;
mod db;
mod errors;
mod integrations;
mod middleware;
mod modules;
mod response;
mod state;
mod telemetry;
mod workers;

use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    telemetry::init();
    let cfg = config::Config::from_env()?;
    tracing::info!(env = %cfg.app_env, "starting nhlanganiso-api");

    let pool = db::connect(&cfg).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    let app_state = state::AppState::new(cfg.clone(), pool).await?;
    workers::spawn_all(app_state.clone());

    let app = app::router(app_state);
    let addr: SocketAddr = cfg.bind_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
