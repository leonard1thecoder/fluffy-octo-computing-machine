pub fn init() {
    let filter = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(format!("nhlanganiso_api={filter},tower_http={filter}"))
        .try_init();
}
