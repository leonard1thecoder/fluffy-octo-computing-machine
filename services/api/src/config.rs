use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub app_env: String,
    pub bind_addr: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub database_min_connections: u32,
    /// Override sqlx prepared-statement cache size. `None` = auto:
    /// 0 when a :6543 transaction pooler is detected, driver default otherwise.
    pub database_statement_cache_size: Option<usize>,
    /// Optional Supabase project URL (https://[REF].supabase.co). Not used for DB
    /// connectivity (DATABASE_URL is authoritative) — kept for future Supabase
    /// Auth/PostgREST interop and operational clarity.
    pub supabase_url: String,
    pub jwt_issuer: String,
    pub jwt_audience: String,
    pub jwt_access_ttl_secs: i64,
    pub jwt_refresh_ttl_secs: i64,
    pub jwt_secret: String,
    /// Optional. When unset, the service boots without rate limiting
    /// (fail-open, warned). Set REDIS_URL to enforce distributed limits.
    pub redis_url: Option<String>,
    pub cors_allowed_origins: Vec<String>,
    pub gmail_client_id: String,
    pub gmail_client_secret: String,
    pub gmail_refresh_token: String,
    pub gmail_sender_email: String,
    pub gmail_sender_name: String,
    pub payfast_merchant_id: String,
    pub payfast_merchant_key: String,
    pub payfast_passphrase: String,
    pub payfast_mode: PayfastMode,
    pub public_web_url: String,
    pub public_api_url: String,
    pub log_level: String,
    pub payfast_validate_ip: bool,
    pub payfast_allowed_ips: Vec<String>,
    /// Google OAuth 2.0 / OIDC (optional; validated at startup only when enabled).
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_redirect_uri: String,
    pub google_issuer: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayfastMode {
    Sandbox,
    Production,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let req = |k: &str| std::env::var(k).with_context(|| format!("missing required env {k}"));
        let app_env = std::env::var("APP_ENV").unwrap_or_else(|_| "development".into());
        let is_prod = app_env == "production";
        let jwt_secret = req("JWT_SECRET")?;
        if is_prod {
            anyhow::ensure!(jwt_secret.len() >= 32, "JWT_SECRET too short in production");
            {
                let v = "DATABASE_URL";
                let val = req(v)?;
                anyhow::ensure!(
                    !val.trim().is_empty(),
                    "{v} must not be empty in production"
                );
            }
            if std::env::var("REDIS_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .is_none()
            {
                tracing::warn!(
                    "REDIS_URL is empty in production; rate limiting is DISABLED until it is set"
                );
            }
            // Merchant integrations are warned, not fatal: the service must boot
            // (and pass health checks) before Gmail/PayFast credentials are entered.
            // Related features fail loudly at use-time until configured.
            for v in [
                "GMAIL_CLIENT_ID",
                "GMAIL_CLIENT_SECRET",
                "GMAIL_REFRESH_TOKEN",
                "GMAIL_SENDER_EMAIL",
                "PAYFAST_MERCHANT_ID",
                "PAYFAST_MERCHANT_KEY",
            ] {
                match std::env::var(v) {
                    Ok(val) if !val.trim().is_empty() => {}
                    _ => tracing::warn!(
                        "{v} is empty in production; email/payments will fail until it is set"
                    ),
                }
            }
            if std::env::var("PAYFAST_MODE").unwrap_or_default() == "sandbox" {
                tracing::warn!("PAYFAST_MODE=sandbox while APP_ENV=production");
            }
        }
        let payfast_mode = match std::env::var("PAYFAST_MODE")
            .unwrap_or_else(|_| "sandbox".into())
            .to_lowercase()
            .as_str()
        {
            "production" | "live" => PayfastMode::Production,
            _ => PayfastMode::Sandbox,
        };
        Ok(Self {
            app_env,
            bind_addr: std::env::var("BIND_ADDR")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| {
                    // Container hosts (Render, Railway, Fly.io) inject PORT;
                    // honor it unless BIND_ADDR is explicitly set.
                    match std::env::var("PORT") {
                        Ok(p) if !p.trim().is_empty() => format!("0.0.0.0:{p}"),
                        _ => "0.0.0.0:8080".into(),
                    }
                }),
            database_url: req("DATABASE_URL")?,
            database_max_connections: std::env::var("DATABASE_MAX_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
            database_min_connections: std::env::var("DATABASE_MIN_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            database_statement_cache_size: std::env::var("DATABASE_STATEMENT_CACHE_SIZE")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(|v| v.parse())
                .transpose()
                .context("DATABASE_STATEMENT_CACHE_SIZE")?,
            supabase_url: std::env::var("SUPABASE_URL").unwrap_or_default(),
            jwt_issuer: std::env::var("JWT_ISSUER").unwrap_or_else(|_| "nhlanganiso-api".into()),
            jwt_audience: std::env::var("JWT_AUDIENCE")
                .unwrap_or_else(|_| "nhlanganiso-web".into()),
            jwt_access_ttl_secs: std::env::var("JWT_ACCESS_TTL_SECONDS")
                .unwrap_or_else(|_| "900".into())
                .parse()
                .context("JWT_ACCESS_TTL_SECONDS")?,
            jwt_refresh_ttl_secs: std::env::var("JWT_REFRESH_TTL_SECONDS")
                .unwrap_or_else(|_| "2592000".into())
                .parse()
                .context("JWT_REFRESH_TTL_SECONDS")?,
            jwt_secret,
            redis_url: std::env::var("REDIS_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            cors_allowed_origins: std::env::var("CORS_ALLOWED_ORIGINS")
                .unwrap_or_else(|_| "http://localhost:3000".into())
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            gmail_client_id: std::env::var("GMAIL_CLIENT_ID")
                .unwrap_or_default()
                .trim()
                .to_owned(),
            gmail_client_secret: std::env::var("GMAIL_CLIENT_SECRET")
                .unwrap_or_default()
                .trim()
                .to_owned(),
            gmail_refresh_token: std::env::var("GMAIL_REFRESH_TOKEN")
                .unwrap_or_default()
                .trim()
                .to_owned(),
            gmail_sender_email: std::env::var("GMAIL_SENDER_EMAIL")
                .unwrap_or_default()
                .trim()
                .to_owned(),
            gmail_sender_name: std::env::var("GMAIL_SENDER_NAME")
                .unwrap_or_else(|_| "Nhlanganiso Catering and Baking".into())
                .trim()
                .to_owned(),
            payfast_merchant_id: std::env::var("PAYFAST_MERCHANT_ID").unwrap_or_default(),
            payfast_merchant_key: std::env::var("PAYFAST_MERCHANT_KEY").unwrap_or_default(),
            payfast_passphrase: std::env::var("PAYFAST_PASSPHRASE").unwrap_or_default(),
            payfast_mode,
            public_web_url: std::env::var("PUBLIC_WEB_URL")
                .unwrap_or_else(|_| "http://localhost:3000".into()),
            public_api_url: std::env::var("PUBLIC_API_URL")
                .unwrap_or_else(|_| "http://localhost:8080".into()),
            log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into()),
            payfast_validate_ip: std::env::var("PAYFAST_VALIDATE_IP")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            payfast_allowed_ips: std::env::var("PAYFAST_ALLOWED_IPS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            google_client_id: std::env::var("GOOGLE_CLIENT_ID")
                .unwrap_or_default()
                .trim()
                .to_owned(),
            google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET")
                .unwrap_or_default()
                .trim()
                .to_owned(),
            google_redirect_uri: std::env::var("GOOGLE_REDIRECT_URI").unwrap_or_default(),
            google_issuer: std::env::var("GOOGLE_ISSUER")
                .unwrap_or_else(|_| "https://accounts.google.com".into()),
        })
    }
}

impl Config {
    /// True when DATABASE_URL points at Supabase (direct or pooler).
    pub fn uses_supabase(&self) -> bool {
        let u = self.database_url.to_lowercase();
        u.contains("supabase.co") || u.contains("pooler.supabase")
    }

    /// True for transaction-mode poolers (Supavisor/PgBouncer Tx, port 6543),
    /// where prepared statements are unsupported and must be disabled.
    pub fn uses_supabase_tx_pooler(&self) -> bool {
        self.database_url.contains(":6543")
    }
}

impl PayfastMode {
    pub fn host(&self) -> &'static str {
        match self {
            PayfastMode::Sandbox => "sandbox.payfast.co.za",
            PayfastMode::Production => "www.payfast.co.za",
        }
    }
    pub fn checkout_url(&self) -> String {
        format!("https://{}/eng/process", self.host())
    }
    pub fn validate_url(&self) -> String {
        format!("https://{}/eng/query/validate", self.host())
    }
}
