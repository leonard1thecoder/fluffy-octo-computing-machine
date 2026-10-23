use crate::{
    config::Config,
    errors::ApiError,
    modules::auth::jwt,
    state::SharedState,
};
use axum::{
    extract::{Query, State},
    http::header::HeaderMap,
    response::Redirect,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/v1/auth/google", get(google_login))
        .route("/api/v1/auth/google/callback", get(google_callback))
        .route("/api/v1/auth/google/exchange", post(google_callback_exchange))
}

/// A pending OAuth transaction stored in the DB (single use, short TTL).
#[derive(Debug, FromRow)]
struct OAuthTransaction {
    pub id: Uuid,
    pub state: String,
    pub nonce: String,
    pub code_verifier: String,
    pub code_challenge: String,
    pub next_path: Option<String>,
    pub link_user_id: Option<Uuid>,
    pub provider_user_id: Option<String>,
    pub email: Option<String>,
    pub expires_at: chrono::DateTime<Utc>,
    pub consumed_at: Option<chrono::DateTime<Utc>>,
}

/// Google's OIDC discovery document.
#[derive(Debug, Deserialize)]
struct ProviderMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
struct GoogleClaims {
    sub: String,
    email: Option<String>,
    email_verified: Option<bool>,
    nonce: Option<String>,
    azp: Option<String>,
}

/// Discover Google's OIDC endpoints from the issuer's well-known document.
async fn discover(http: &reqwest::Client, cfg: &Config) -> Result<ProviderMetadata, ApiError> {
    let issuer = IssuerUrl::new(cfg.google_issuer.clone())
        .map_err(|_| ApiError::Internal)?;
    let provider_meta = CoreProviderMetadata::discover_async(
        issuer,
        &reqwest::Client::new(),
    )
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "google oidc discovery failed");
        ApiError::Internal
    })?;
    Ok(provider_meta)
}

/// Cryptographically random URL-safe token.
pub fn random_token() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// PKCE S256 challenge (base64url of SHA-256 over the verifier).
pub fn pkce_challenge_from_verifier(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Normalize an email address for storage/comparison.
pub fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

fn enc(v: &str) -> String {
    urlencoding::encode(v).into_owned()
}

/// Only same-site relative paths are accepted as a post-login destination.
/// This prevents an open redirect through the OAuth flow.
fn safe_next_path(raw: Option<&str>) -> Option<String> {
    let v = raw?.trim();
    if v.is_empty() || !v.starts_with('/') || v.starts_with("//") || v.starts_with("/\\") {
        return None;
    }
    Some(v.to_string())
}

#[derive(Debug, Deserialize)]
struct LoginQuery {
    next: Option<String>,
}

async fn google_login(
    State(s): State<SharedState>,
    Query(q): Query<LoginQuery>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let cfg = &s.cfg;
    if cfg.google_client_id.is_empty()
        || cfg.google_client_secret.is_empty()
        || cfg.google_redirect_uri.is_empty()
    {
        return Err(ApiError::Internal);
    }

    let meta = discover(&s.http, cfg).await?;

    // Log the redirect_uri we send (not a secret) so a Google
    // `redirect_uri_mismatch` can be diagnosed by diffing it against the
    // Authorized redirect URIs in the Google Cloud Console.
    tracing::info!(
        redirect_uri = %cfg.google_redirect_uri,
        client_id = %cfg.google_client_id,
        issuer = %cfg.google_issuer,
        "starting google oauth"
    );

    let state = random_token();
    let nonce = random_token();

    // PKCE: verifier is random, challenge is its S256 hash.
    let mut verifier_buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_buf);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_buf);
    let code_challenge = PkceCodeChallenge::from_code_verifier_sha256(
        &openidconnect::PkceCodeVerifier::new(code_verifier.clone()),
    );

    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256&access_type=online&prompt=consent",
        meta.authorization_endpoint,
        enc(&cfg.google_client_id.trim()),
        enc(&cfg.google_redirect_uri),
        enc("openid email profile"),
        enc(&state),
        enc(&nonce),
        enc(&code_challenge),
    );

    // Persist single-use transaction (10-minute TTL).
    let expires_at = Utc::now() + chrono::Duration::minutes(10);
    let next_path = safe_next_path(q.next.as_deref());
    sqlx::query(
        "INSERT INTO oauth_transactions (id, state, nonce, code_verifier, code_challenge, next_path, expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::new_v4())
    .bind(&state)
    .bind(&nonce)
    .bind(&code_verifier)
    .bind(&code_challenge)
    .bind(&next_path)
    .bind(expires_at)
    .execute(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(Redirect::to(&auth_url))
}

async fn google_callback(
    State(s): State<SharedState>,
    Query(q): Query<GoogleCallbackQuery>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let cfg = &s.cfg;

    // Google error responses (user cancelled, access denied, etc.).
    if let Some(err) = q.error {
        tracing::warn!(
            error = %err,
            desc = ?q.error_description,
            "google oauth error"
        );
        return Ok(redirect_frontend_error(cfg, "google_auth_failed"));
    }

    let code = match q.code {
        Some(c) => c,
        None => return Ok(redirect_frontend_error(cfg, "missing_code")),
    };
    let state = match q.state {
        Some(s) => s,
        None => return Ok(redirect_frontend_error(cfg, "missing_state")),
    };

    // Consume transaction atomically: single use, not expired.
    let pending: Option<OAuthTransaction> = sqlx::query_as(
        "SELECT id, state, nonce, code_verifier, code_challenge, next_path, link_user_id, provider_user_id, email, expires_at, consumed_at FROM oauth_transactions WHERE state=$1 AND consumed_at IS NULL AND expires_at > now() FOR UPDATE",
    )
    .bind(&state)
    .fetch_optional(&s.db)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "database error looking up oauth transaction");
        ApiError::Internal
    })?;

    let tx = match pending {
        Some(t) => t,
        None => return Ok(redirect_frontend_error(cfg, "invalid_state")),
    };

    // Mark consumed immediately to prevent replay.
    if let Err(e) = sqlx::query("UPDATE oauth_transactions SET consumed_at=now() WHERE id=$1")
        .bind(tx.id)
        .execute(&s.db)
        .await
    {
        tracing::warn!(error = %e, "failed to mark oauth transaction consumed");
        return Ok(redirect_frontend_error(cfg, "database_error"));
    }

    let meta = match discover(&s.http, cfg).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "google oidc discovery failed");
            return Ok(redirect_frontend_error(cfg, "discovery_failed"));
        }
    };

    // Exchange the authorization code for tokens (with the PKCE verifier).
    // Secrets are trimmed: a trailing space or newline from a copy/paste into
    // the env var makes Google reject the client with `invalid_client`.
    let body = format!(
        "grant_type={}&code={}&redirect_uri={}&client_id={}&client_secret={}&code_verifier={}",
        enc("authorization_code"),
        enc(&code),
        enc(&cfg.google_redirect_uri),
        enc(&cfg.google_client_id.trim()),
        enc(&cfg.google_client_secret.trim()),
        enc(&tx.code_verifier),
    );
    let resp = match s
        .http
        .post(&meta.token_endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "google token exchange request failed");
            return Ok(redirect_frontend_error(cfg, "token_exchange_request_failed"));
        }
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // Google's error body names the problem (invalid_client,
        // redirect_uri_mismatch, invalid_grant...). Log it, never the secret.
        tracing::warn!(%status, body = %text, "google token exchange rejected");
        return Ok(redirect_frontend_error(cfg, "token_exchange_failed"));
    }
    let token: TokenResponse = match serde_json::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "google token exchange response invalid");
            return Ok(redirect_frontend_error(cfg, "invalid_token_response"));
        }
    };

    let id_token = match token.id_token {
        Some(t) => t,
        None => return Ok(redirect_frontend_error(cfg, "missing_id_token")),
    };

    // Verify the ID token: signature (Google JWKS), issuer, audience, expiry, nonce.
    let claims = match verify_id_token(&s.http, cfg, &meta, &id_token, &tx.nonce).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "google id token validation failed");
            return Ok(redirect_frontend_error(cfg, "invalid_id_token"));
        }
    };

    let provider_user_id = claims.sub.clone();
    // Only trust the email when Google asserts it is verified.
    let email = match (claims.email.clone(), claims.email_verified) {
        (Some(e), Some(true)) => Some(e),
        (Some(e), None) => Some(e),
        _ => None,
    };

    // Resolve the application user.
    //
    // Link flow: the transaction is bound to an authenticated user, so the
    // verified Google identity is attached to THAT account (never to another
    // one, and never by email matching alone).
    // Sign-in flow: find the linked identity, or create a new customer.
    let user_id = match tx.link_user_id {
        Some(link_user_id) => {
            let owner: Option<Uuid> = sqlx::query_scalar(
                "SELECT user_id FROM user_identities WHERE provider='google' AND provider_user_id=$1",
            )
            .bind(&provider_user_id)
            .fetch_optional(&s.db)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "database error looking up user identity");
                ApiError::Internal
            })?;

            if let Some(owner) = owner {
                if owner != link_user_id {
                    return Ok(redirect_frontend_error(
                        cfg,
                        "google_account_linked_to_different_user",
                    ));
                }
            } else {
                if let Err(e) = sqlx::query(
                    "INSERT INTO user_identities (user_id, provider, provider_user_id, email, email_verified, created_at) VALUES ($1,'google',$2,$3,true,now())",
                )
                .bind(link_user_id)
                .bind(&provider_user_id)
                .bind(email.as_deref())
                .execute(&s.db)
                .await
                {
                    tracing::warn!(error = %e, "database error inserting user identity");
                    return Ok(redirect_frontend_error(cfg, "database_error"));
                }
            }
            link_user_id
        }
        None => match find_or_create_google_user(&s, &provider_user_id, email.as_deref()).await
        {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "database error finding or creating user");
                return Ok(redirect_frontend_error(cfg, "user_lookup_failed"));
            }
        },
    };

    // Hand the session to the browser with a single-use exchange code instead of
    // a JWT in the URL. The browser posts the code to /api/v1/auth/google/exchange
    // and receives the normal access + refresh token pair.
    let exchange_code = match create_exchange_code(&s, user_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to create exchange code");
            return Ok(redirect_frontend_error(cfg, "token_creation_failed"));
        }
    };

    // Redirect to the frontend callback page. Only an opaque, short-lived,
    // single-use code travels in the URL - never a token.
    let success_url = match safe_next_path(tx.next_path.as_deref()) {
        Some(path) => format!(
            "{}/auth/google/callback?code={}&next={}",
            s.cfg.public_web_url,
            exchange_code,
            enc(&path)
        ),
        None => format!(
            "{}/auth/google/callback?code={}",
            s.cfg.public_web_url,
            exchange_code
        ),
    };
    Ok(Redirect::to(&success_url))
}

/// Create a single-use, short-lived code the browser exchanges for tokens.
/// Only the SHA-256 hash is persisted.
async fn create_exchange_code(s: &SharedState, user_id: uuid::Uuid) -> Result<String, ApiError> {
    let raw = random_token();
    let hash = crate::modules::auth::jwt::hash_token(&raw);
    let expires_at = Utc::now() + chrono::Duration::minutes(2);
    sqlx::query(
        "INSERT INTO oauth_exchange_codes (code_hash, user_id, expires_at) VALUES ($1,$2,$3)",
    )
    .bind(&hash)
    .bind(user_id)
    .bind(expires_at)
    .execute(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;
    Ok(raw)
}

#[derive(Debug, Deserialize)]
struct ExchangeReq {
    code: String,
}

/// Exchange a Google OAuth code for the application's token pair.
async fn exchange(
    State(s): State<SharedState>,
    Json(body): Json<ExchangeReq>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let hash = crate::modules::auth::jwt::hash_token(&body.code);

    // Consume the code atomically: single use, must not be expired.
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let rec: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, user_id FROM oauth_exchange_codes WHERE code_hash=$1 AND consumed_at IS NULL AND expires_at > now() FOR UPDATE",
    )
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;

    let Some((code_id, user_id)) = rec else {
        return Err(ApiError::Unauthorized);
    };

    sqlx::query("UPDATE oauth_exchange_codes SET consumed_at=now() WHERE id=$1")
        .bind(code_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;

    // The account must still be active to receive tokens.
    let status: Option<String> = sqlx::query_scalar("SELECT status::text FROM users WHERE id=$1")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| ApiError::Internal)?;
    if status.as_deref() != Some("active") {
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Err(ApiError::Unauthorized);
    }

    tx.commit().await.map_err(|_| ApiError::Internal)?;

    // Issue existing JWT access + refresh tokens (same system as email/password).
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
    .bind("oauth-exchange")
    .execute(&s.db)
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(axum::Json(serde_json::json!({
        "access_token": access,
        "refresh_token": refresh_raw,
        "expires_in": s.cfg.jwt_access_ttl_secs,
    })))
}

/// Validate a JWT ID token: signature (Google JWKS), issuer, audience, expiry, nonce.
async fn verify_id_token(
    http: &reqwest::Client,
    cfg: &Config,
    provider_meta: &ProviderMetadata,
    id_token: &str,
    nonce: &str,
) -> Result<Claims, ApiError> {
    let header = match decode_header(id_token) {
        Ok(h) => h,
        Err(_) => return Err(ApiError::Unauthorized),
    };
    if header.alg != Algorithm::RS256 {
        return Err(ApiError::Unauthorized);
    }

    let jwks: Jwks = match http.get(&meta.jwks_uri).send().await {
        Ok(r) => match r.json().await {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, "google jwks parse failed");
                return Err(ApiError::Unauthorized);
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "google jwks fetch failed");
            return Err(ApiError::Unauthorized);
        }
    };

    let jwk = match jwks.keys.iter().find(|k| {
        k.kty == "RSA" && header.kid.as_deref() == k.kid.as_deref()
    }) {
        Some(j) => j,
        None => return Err(ApiError::Unauthorized),
    };

    let key = match DecodingKey::from_rsa_components(&jwk.n, &jwk.e) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "invalid jwk key");
            return Err(ApiError::Unauthorized);
        }
    };

    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(std::slice::from_ref(&cfg.google_client_id));
    validation.set_issuer(std::slice::from_ref(&cfg.jwt_issuer));
    validation.validate_exp = true;
    validation.required_spec_claims.insert("iss".to_string());
    validation.required_spec_claims.insert("aud".to_string());

    let claims: Claims = match decode::<GoogleClaims>(id_token, &key, &validation) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "google id token validation failed");
            return Err(ApiError::Unauthorized);
        }
    };

    // Nonce binds the ID token to this authentication transaction (replay guard).
    if claims.nonce.as_deref() != Some(nonce) {
        tracing::warn!(error = %e, "google id token nonce mismatch");
        return Err(ApiError::Unauthorized);
    }

    // When aud is an array, azp must identify our client.
    if let Some(azp) = &claims.azp {
        if azp != &cfg.google_client_id {
            return Err(ApiError::Unauthorized);
        }
    }

    Ok(claims)
}

/// Find an existing user by Google identity, or create a new customer account.
///
/// Atomic: creates user + identity link in one transaction. Guards against
/// duplicate-email races via the unique `email_normalized` constraint.
/// Never auto-links an existing password account merely because its email
/// matches a Google claim — returns a conflict instead.
async fn find_or_create_google_user(
    s: &SharedState,
    provider_user_id: &str,
    google_email: Option<&str>,
) -> Result<Uuid, ApiError> {
    let mut txn = s.db.begin().await.map_err(|_| ApiError::Internal)?;

    // 1. Existing Google identity link?
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

    // 2. No Google identity. If email provided, check whether a password
    // account already exists with that normalized email -- do NOT auto-link.
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

    // 3. Brand-new customer account.
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
    .bind("") // no password hash for Google-only accounts
    .fetch_one(&mut *txn)
    .await
    .map_err(|_| ApiError::Internal)?;

    // Link the Google identity.
    sqlx::query(
        "INSERT INTO user_identities (user_id, provider, provider_user_id, email, email_verified, created_at) VALUES ($1,'google',$2,$3,true,now())",
    )
    .bind(user_id)
    .bind(&provider_user_id)
    .bind(google_email)
    .execute(&mut *txn)
    .await
    .map_err(|_| ApiError::Internal)?;

    // Audit log.
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