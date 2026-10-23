use crate::{config::Config, errors::ApiError, state::SharedState};
use axum::{
    extract::{Query, State},
    response::Redirect,
    routing::get,
    Router,
};
use base64::Engine;
use chrono::Utc;
use openidconnect::{
    core::{CoreClient, CoreIdTokenVerifier, CoreProviderMetadata, CoreResponseType},
    AuthorizationCode, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge, RedirectUrl, Scope,
};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/auth/google", get(google_login))
        .route("/api/v1/auth/google/callback", get(google_callback))
}

/// Runtime Google OAuth configuration.
#[derive(Clone, Debug)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

/// A pending OAuth transaction stored in the DB (single use, short TTL).
#[derive(Debug, FromRow)]
struct OAuthTransaction {
    pub id: Uuid,
    pub state: String,
    pub nonce: String,
    pub code_verifier: String,
    pub code_challenge: String,
    pub provider_user_id: Option<String>,
    pub email: Option<String>,
    pub expires_at: chrono::DateTime<Utc>,
    pub consumed_at: Option<chrono::DateTime<Utc>>,
}

/// Build Google OAuth client from OpenID Connect discovery.
async fn build_google_client(cfg: &Config) -> Result<CoreClient, ApiError> {
    if cfg.google_client_id.is_empty()
        || cfg.google_client_secret.is_empty()
        || cfg.google_redirect_uri.is_empty()
    {
        return Err(ApiError::Internal);
    }

    let issuer = IssuerUrl::new(cfg.google_issuer.clone()).map_err(|_| ApiError::Internal)?;
    let provider_meta = CoreProviderMetadata::discover_async(issuer, &reqwest::Client::new())
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "google oidc discovery failed");
            ApiError::Internal
        })?;

    Ok(CoreClient::from_provider_metadata(
        provider_meta,
        openidconnect::ClientId::new(cfg.google_client_id.clone()),
        Some(openidconnect::ClientSecret::new(
            cfg.google_client_secret.clone(),
        )),
    ))
}

fn random_token() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// PKCE S256 challenge from raw verifier bytes.
fn pkce_challenge_from_verifier(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

async fn google_login(
    State(s): State<SharedState>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let client = build_google_client(&s.cfg).await?;

    let state = random_token();
    let nonce = random_token();

    // PKCE verifier + S256 challenge.
    let mut verifier_buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_buf);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_buf);
    let code_challenge_str = pkce_challenge_from_verifier(&code_verifier);
    let code_challenge = PkceCodeChallenge::from_code_verifier_sha256(
        &openidconnect::PkceCodeVerifier::new(code_verifier.clone()),
    );

    let redirect_url =
        RedirectUrl::new(s.cfg.google_redirect_uri.clone()).map_err(|_| ApiError::Internal)?;
    let client = client.set_redirect_uri(redirect_url);

    let (auth_url, _csrf_token) = client
        .authorize_url(CoreResponseType::Code, CsrfToken::new_random)
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .set_nonce(Nonce::new(nonce.clone()))
        .set_pkce_challenge(code_challenge)
        .url();

    // Persist single-use transaction (10-minute TTL).
    let expires_at = Utc::now() + chrono::Duration::minutes(10);
    sqlx::query(
        "INSERT INTO oauth_transactions (id, state, nonce, code_verifier, code_challenge, expires_at) VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(Uuid::new_v4())
    .bind(&state)
    .bind(&nonce)
    .bind(&code_verifier)
    .bind(&code_challenge_str)
    .bind(expires_at)
    .execute(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Redirect::to(auth_url.as_str()))
}

#[derive(Debug, Deserialize)]
struct GoogleCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn google_callback(
    State(s): State<SharedState>,
    Query(q): Query<GoogleCallbackQuery>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    if let Some(err) = q.error {
        tracing::warn!(
            error = %err,
            desc = ?q.error_description,
            "google oauth error"
        );
        return Ok(redirect_frontend_error(&s.cfg, "google_auth_failed"));
    }

    let code = q
        .code
        .ok_or_else(|| ApiError::Unprocessable("missing authorization code".into()))?;
    let state = q
        .state
        .ok_or_else(|| ApiError::Unprocessable("missing state".into()))?;

    let pending: Option<OAuthTransaction> = sqlx::query_as(
        "SELECT id, state, nonce, code_verifier, code_challenge, provider_user_id, email, expires_at, consumed_at FROM oauth_transactions WHERE state=$1 AND consumed_at IS NULL AND expires_at > now() FOR UPDATE",
    )
    .bind(&state)
    .fetch_optional(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let tx = pending
        .ok_or_else(|| ApiError::Unprocessable("invalid, expired, or already used state".into()))?;

    sqlx::query("UPDATE oauth_transactions SET consumed_at=now() WHERE id=$1")
        .bind(tx.id)
        .execute(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;

    let client = build_google_client(&s.cfg).await?;
    let redirect_url =
        RedirectUrl::new(s.cfg.google_redirect_uri.clone()).map_err(|_| ApiError::Internal)?;
    let client = client.set_redirect_uri(redirect_url);

    let token_response = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(openidconnect::PkceCodeVerifier::new(
            tx.code_verifier.clone(),
        ))
        .request_async(reqwest::Client::new())
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "google token exchange failed");
            ApiError::Unauthorized
        })?;

    let id_token = token_response.id_token().ok_or(ApiError::Unauthorized)?;
    let verifier = CoreIdTokenVerifier::new(client.id_token_verifier());
    let claims = id_token
        .claims(&verifier, &Nonce::new(tx.nonce.clone()))
        .map_err(|e| {
            tracing::warn!(error = %e, "google id token validation failed");
            ApiError::Unauthorized
        })?;

    let provider_user_id = claims.subject().to_string();
    let email = claims.get_email().map(|e| e.to_string());

    let user_id = find_or_create_google_user(&s, &tx, &provider_user_id, email.as_deref()).await?;

    let access =
        crate::modules::auth::jwt::mint_access(user_id, &s.cfg).map_err(|_| ApiError::Internal)?;
    let (refresh_raw, refresh_hash) = crate::modules::auth::jwt::new_refresh_token();

    sqlx::query(
        "INSERT INTO refresh_tokens (user_id, token_hash, expires_at, user_agent, ip) VALUES ($1,$2,now()+($3::bigint * interval '1 second'),$4,$5)",
    )
    .bind(user_id)
    .bind(&refresh_hash)
    .bind(s.cfg.jwt_refresh_ttl_secs)
    .bind("google-oauth")
    .bind("oauth-redirect")
    .execute(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    let success_url = format!("{}#access_token={}", s.cfg.public_web_url, access);
    Ok(Redirect::to(&success_url))
}

async fn find_or_create_google_user(
    s: &SharedState,
    _tx: &OAuthTransaction,
    provider_user_id: &str,
    google_email: Option<&str>,
) -> Result<Uuid, ApiError> {
    let mut txn = s.db.begin().await.map_err(|_| ApiError::Internal)?;

    let existing_user: Option<Uuid> = sqlx::query_scalar(
        "SELECT ui.user_id FROM user_identities ui JOIN users u ON u.id = ui.user_id WHERE ui.provider = 'google' AND ui.provider_user_id = $1 AND u.status = 'active'",
    )
    .bind(provider_user_id)
    .fetch_optional(&mut *txn)
    .await
    .map_err(|_| ApiError::Internal)?;

    if let Some(user_id) = existing_user {
        txn.commit().await.map_err(|_| ApiError::Internal)?;
        return Ok(user_id);
    }

    if let Some(email) = google_email {
        let norm = normalize_email(email);
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE email_normalized = $1 AND status = 'active'",
        )
        .bind(&norm)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|_| ApiError::Internal)?;

        if existing.is_some() {
            txn.commit().await.map_err(|_| ApiError::Internal)?;
            return Err(ApiError::Conflict(
                "an account with this email already exists; sign in with password or link your Google account first".into(),
            ));
        }
    }

    let email_normalized = google_email.map(normalize_email).unwrap_or_default();
    let display_name = google_email
        .map(|e| e.split('@').next().unwrap_or("Customer").to_string())
        .unwrap_or_else(|| "Customer".to_string());

    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, email_normalized, full_name, password_hash, role, status) VALUES ($1,$2,$3,$4,'customer','active') RETURNING id",
    )
    .bind(google_email.unwrap_or(""))
    .bind(&email_normalized)
    .bind(display_name.trim())
    .bind("")
    .fetch_one(&mut *txn)
    .await
    .map_err(|_| ApiError::Internal)?;

    sqlx::query(
        "INSERT INTO user_identities (user_id, provider, provider_user_id, email, email_verified, created_at) VALUES ($1,'google',$2,$3,true,now())",
    )
    .bind(user_id)
    .bind(provider_user_id)
    .bind(google_email)
    .execute(&mut *txn)
    .await
    .map_err(|_| ApiError::Internal)?;

    sqlx::query(
        "INSERT INTO audit_logs (actor_user_id, action, entity, entity_id) VALUES ($1,'auth.google_register','user',$2)",
    )
    .bind(user_id)
    .bind(user_id.to_string())
    .execute(&mut *txn)
    .await
    .map_err(|_| ApiError::Internal)?;

    txn.commit().await.map_err(|_| ApiError::Internal)?;
    Ok(user_id)
}

fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

fn redirect_frontend_error(cfg: &Config, reason: &str) -> Redirect {
    Redirect::to(&format!(
        "{}?auth=error&reason={}",
        cfg.public_web_url, reason
    ))
}
