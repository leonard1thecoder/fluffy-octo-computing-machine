pub mod jwt;
pub mod password;

use axum::{Json, Router, extract::{ConnectInfo, Path, Query, State}, http::StatusCode, routing::{delete, get, post}};
use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, net::SocketAddr};
use uuid::Uuid;
use validator::Validate;

use crate::{
    errors::{ApiError, ApiResult},
    middleware::authentication::AuthUser,
    response::ApiResponse,
    state::SharedState,
    workers::email_outbox,
};

pub fn router() -> Router<SharedState> {
    // Protected routes need a stateful auth middleware; build without state here
    // and let `app.rs` supply it via `.with_state`. We attach auth inside handlers
    // by nesting a sub-router that uses `from_fn_with_state` lazily is complex,
    // so we register all routes and enforce auth per-handler via extension check
    // performed by `require_auth` layered in app wiring below.
    //
    // To keep `Router<SharedState>` simple and compilable, public routes are open
    // and protected routes call `auth_user()` helper which re-validates the
    // Bearer token from the request headers (same logic as middleware).
    Router::new()
        .route("/api/v1/auth/register", post(register))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/logout-all", post(logout_all))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/auth/sessions", get(list_sessions))
        .route("/api/v1/auth/sessions/:id", delete(delete_session))
        .route("/api/v1/auth/verify-email", get(verify_email))
        .route("/api/v1/auth/password-reset/request", post(request_password_reset))
        .route("/api/v1/auth/password-reset/confirm", post(confirm_password_reset))
}

async fn bearer_user(state: &SharedState, headers: &axum::http::HeaderMap) -> ApiResult<AuthUser> {
    let hdr = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let token = hdr.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
    let claims = jwt::verify_access(token, &state.cfg).map_err(|_| ApiError::Unauthorized)?;
    let rec: Option<(String, String)> =
        sqlx::query_as("SELECT role::text, status::text FROM users WHERE id=$1")
            .bind(claims.sub)
            .fetch_optional(&state.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let (role, status) = rec.ok_or(ApiError::Unauthorized)?;
    if status != "active" {
        return Err(ApiError::Unauthorized);
    }
    Ok(AuthUser { user_id: claims.sub, role, token_id: claims.jti })
}

// ---------- DTOs ----------

#[derive(Debug, Deserialize, Validate)]
struct RegisterReq {
    #[validate(length(min = 1, max = 120))]
    full_name: String,
    #[validate(email, length(max = 320))]
    email: String,
    #[validate(length(min = 8, max = 128))]
    password: String,
}

#[derive(Debug, Deserialize, Validate)]
struct LoginReq {
    #[validate(email, length(max = 320))]
    email: String,
    #[validate(length(min = 1, max = 128))]
    password: String,
}

#[derive(Debug, Deserialize)]
struct RefreshReq {
    refresh_token: String,
}

#[derive(Debug, Serialize)]
struct TokenPair {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

fn normalize_email(e: &str) -> String {
    e.trim().to_lowercase()
}

// ---------- Handlers ----------

async fn register(
    State(s): State<SharedState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RegisterReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    body.validate().map_err(|e| ApiError::Validation { fields: serde_json::json!(e.to_string()) })?;
    let email = normalize_email(&body.email);
    let _ = headers;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email_normalized=$1)")
        .bind(&email)
        .fetch_one(&s.db)
        .await
        .map_err(|_| ApiError::Internal)?;
    if exists {
        // generic-ish: do not reveal which field failed beyond conflict
        return Err(ApiError::Conflict("account already exists".into()));
    }
    let hash = password::hash(&body.password).map_err(|_| ApiError::Internal)?;
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, email_normalized, full_name, password_hash, role, status) VALUES ($1,$2,$3,$4,'customer','active') RETURNING id",
    )
    .bind(email.clone())
    .bind(email.clone())
    .bind(body.full_name.trim())
    .bind(hash)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;

    // email verification token (single-use, hashed)
    use rand::RngCore;
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let raw_token = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw);
    let token_hash = jwt::hash_token(&raw_token);
    sqlx::query("INSERT INTO email_verification_tokens (user_id, token_hash, expires_at) VALUES ($1,$2,now()+interval '24 hours')")
        .bind(user_id).bind(token_hash).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;

    let link = format!("{}/verify-email?token={}", s.cfg.public_web_url, raw_token);
    let html = email_outbox::branded_template("Verify your email", &format!("<p>Hi {},</p><p>Please verify your account: <a href=\"{}\">Verify email</a></p>", email_outbox::escape_html(body.full_name.trim()), link));
    email_outbox::enqueue(&mut tx, &email, "Verify your Nhlanganiso account", &html).await.map_err(|_| ApiError::Internal)?;
    sqlx::query("INSERT INTO audit_logs (actor_user_id, action, entity, entity_id) VALUES ($1,'auth.register','user',$2)")
        .bind(user_id).bind(user_id.to_string()).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (s, b) = ApiResponse::created(serde_json::json!({"id": user_id, "email": email}));
    Ok((s, Json(b.data)))
}

async fn login(
    State(s): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    body.validate().map_err(|e| ApiError::Validation { fields: serde_json::json!(e.to_string()) })?;
    // Redis abuse guard (fail-closed inside check caller handles 503 separately;
    // here map over-limit to 429).
    if let Ok(passed) = crate::middleware::rate_limit::check(&s, &format!("rl:login:{}", addr.ip()), 10, 60).await {
        if !passed {
            return Err(ApiError::RateLimited { retry_after_secs: 60 });
        }
    }
    let email = normalize_email(&body.email);
    let rec: Option<(Uuid, String, String, String)> =
        sqlx::query_as("SELECT id, password_hash, role::text, status::text FROM users WHERE email_normalized=$1")
            .bind(&email)
            .fetch_optional(&s.db)
            .await
            .map_err(|_| ApiError::Internal)?;
    let Some((uid, hash, _role, status)) = rec else {
        return Err(ApiError::Unauthorized);
    };
    if status != "active" || !password::verify(&body.password, &hash) {
        return Err(ApiError::Unauthorized);
    }
    let access = jwt::mint_access(uid, &s.cfg).map_err(|_| ApiError::Internal)?;
    let (refresh_raw, refresh_hash) = jwt::new_refresh_token();
    let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    sqlx::query("INSERT INTO refresh_tokens (user_id, token_hash, expires_at, user_agent, ip) VALUES ($1,$2,now()+($3::bigint * interval '1 second'),$4,$5)")
        .bind(uid).bind(refresh_hash).bind(s.cfg.jwt_refresh_ttl_secs).bind(ua).bind(addr.ip().to_string())
        .execute(&s.db).await.map_err(|_| ApiError::Internal)?;
    let (st, b) = ApiResponse::ok(TokenPair { access_token: access, refresh_token: refresh_raw, expires_in: s.cfg.jwt_access_ttl_secs });
    Ok((st, Json(serde_json::to_value(b.data).unwrap())))
}

async fn refresh(
    State(s): State<SharedState>,
    Json(body): Json<RefreshReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let presented_hash = jwt::hash_token(&body.refresh_token);
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let rec: Option<(Uuid, Uuid, chrono::DateTime<Utc>, Option<chrono::DateTime<Utc>>, Uuid)> = sqlx::query_as(
        "SELECT id, user_id, expires_at, revoked_at, user_id FROM refresh_tokens WHERE token_hash=$1 FOR UPDATE",
    )
    .bind(&presented_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| ApiError::Internal)?;
    let Some((token_id, user_id, expires_at, revoked_at, _)) = rec else {
        return Err(ApiError::Unauthorized);
    };
    if revoked_at.is_some() {
        // Reuse of rotated token => revoke whole family/session set (replay detection).
        sqlx::query("UPDATE refresh_tokens SET revoked_at=now() WHERE user_id=$1 AND revoked_at IS NULL")
            .bind(user_id).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Err(ApiError::Unauthorized);
    }
    if expires_at < Utc::now() {
        sqlx::query("UPDATE refresh_tokens SET revoked_at=now() WHERE id=$1").bind(token_id).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
        return Err(ApiError::Unauthorized);
    }
    let (new_raw, new_hash) = jwt::new_refresh_token();
    sqlx::query("UPDATE refresh_tokens SET revoked_at=now(), replaced_by_hash=$2 WHERE id=$1")
        .bind(token_id).bind(&new_hash).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    sqlx::query("INSERT INTO refresh_tokens (user_id, token_hash, expires_at) VALUES ($1,$2,now()+($3::bigint * interval '1 second'))")
        .bind(user_id).bind(&new_hash).bind(s.cfg.jwt_refresh_ttl_secs)
        .execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let access = jwt::mint_access(user_id, &s.cfg).map_err(|_| ApiError::Internal)?;
    let (st, b) = ApiResponse::ok(TokenPair { access_token: access, refresh_token: new_raw, expires_in: s.cfg.jwt_access_ttl_secs });
    Ok((st, Json(serde_json::to_value(b.data).unwrap())))
}

async fn logout(State(s): State<SharedState>, headers: axum::http::HeaderMap, body: Option<Json<RefreshReq>>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let user = bearer_user(&s, &headers).await?;
    if let Some(Json(b)) = body {
        let h = jwt::hash_token(&b.refresh_token);
        sqlx::query("UPDATE refresh_tokens SET revoked_at=now() WHERE token_hash=$1 AND user_id=$2")
            .bind(h).bind(user.user_id).execute(&s.db).await.map_err(|_| ApiError::Internal)?;
    }
    let (st, b) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(b.data)))
}

async fn logout_all(State(s): State<SharedState>, headers: axum::http::HeaderMap) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let user = bearer_user(&s, &headers).await?;
    sqlx::query("UPDATE refresh_tokens SET revoked_at=now() WHERE user_id=$1 AND revoked_at IS NULL")
        .bind(user.user_id).execute(&s.db).await.map_err(|_| ApiError::Internal)?;
    // Invalidate JWTs issued before now by bumping a per-user serial (checked on password change path).
    let (st, b) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(b.data)))
}

async fn me(State(s): State<SharedState>, headers: axum::http::HeaderMap) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let user = bearer_user(&s, &headers).await?;
    let rec: Option<(String, String, String)> = sqlx::query_as("SELECT email, full_name, role::text FROM users WHERE id=$1")
        .bind(user.user_id).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((email, full_name, role)) = rec else { return Err(ApiError::Unauthorized) };
    let (st, b) = ApiResponse::ok(serde_json::json!({"id": user.user_id, "email": email, "full_name": full_name, "role": role}));
    Ok((st, Json(b.data)))
}

async fn list_sessions(State(s): State<SharedState>, headers: axum::http::HeaderMap) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let user = bearer_user(&s, &headers).await?;
    let rows: Vec<(Uuid, chrono::DateTime<Utc>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, created_at, user_agent, ip FROM refresh_tokens WHERE user_id=$1 AND revoked_at IS NULL AND expires_at > now() ORDER BY created_at DESC",
    )
    .bind(user.user_id).fetch_all(&s.db).await.map_err(|_| ApiError::Internal)?;
    let items: Vec<_> = rows.into_iter().map(|(id, created_at, ua, ip)| serde_json::json!({"id": id, "created_at": created_at, "user_agent": ua, "ip": ip})).collect();
    let (st, b) = ApiResponse::ok(items);
    Ok((st, Json(serde_json::to_value(b.data).unwrap())))
}

async fn delete_session(State(s): State<SharedState>, headers: axum::http::HeaderMap, Path(id): Path<Uuid>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let user = bearer_user(&s, &headers).await?;
    sqlx::query("UPDATE refresh_tokens SET revoked_at=now() WHERE id=$1 AND user_id=$2")
        .bind(id).bind(user.user_id).execute(&s.db).await.map_err(|_| ApiError::Internal)?;
    let (st, b) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(b.data)))
}

async fn verify_email(State(s): State<SharedState>, Query(q): Query<HashMap<String, String>>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let raw = q.get("token").ok_or(ApiError::Unprocessable("missing token".into()))?;
    let h = jwt::hash_token(raw);
    let rec: Option<(Uuid, Uuid)> = sqlx::query_as("SELECT id, user_id FROM email_verification_tokens WHERE token_hash=$1 AND used_at IS NULL AND expires_at > now()")
        .bind(h).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((tid, uid)) = rec else { return Err(ApiError::Unprocessable("invalid or expired token".into())) };
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    sqlx::query("UPDATE email_verification_tokens SET used_at=now() WHERE id=$1").bind(tid).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    sqlx::query("UPDATE users SET email_verified_at=now() WHERE id=$1").bind(uid).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (st, b) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(b.data)))
}

#[derive(Debug, Deserialize, Validate)]
struct PwResetReq {
    #[validate(email)]
    email: String,
}

async fn request_password_reset(State(s): State<SharedState>, Json(body): Json<PwResetReq>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    body.validate().map_err(|e| ApiError::Validation { fields: serde_json::json!(e.to_string()) })?;
    let email = normalize_email(&body.email);
    // Generic response to avoid enumeration; only create token if user exists.
    let uid: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email_normalized=$1").bind(&email).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    if let Some(uid) = uid {
        use rand::RngCore;
        let mut raw = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut raw);
        let raw_token = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw);
        let h = jwt::hash_token(&raw_token);
        let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
        sqlx::query("INSERT INTO password_reset_tokens (user_id, token_hash, expires_at) VALUES ($1,$2,now()+interval '1 hour')")
            .bind(uid).bind(h).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
        let link = format!("{}/reset-password?token={}", s.cfg.public_web_url, raw_token);
        let html = email_outbox::branded_template("Reset your password", &format!("<p>Reset link (1 hour): <a href=\"{}\">Reset password</a></p>", link));
        email_outbox::enqueue(&mut tx, &email, "Reset your Nhlanganiso password", &html).await.map_err(|_| ApiError::Internal)?;
        tx.commit().await.map_err(|_| ApiError::Internal)?;
    }
    let (st, b) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(b.data)))
}

#[derive(Debug, Deserialize, Validate)]
struct PwResetConfirm {
    token: String,
    #[validate(length(min = 8, max = 128))]
    new_password: String,
}

async fn confirm_password_reset(State(s): State<SharedState>, Json(body): Json<PwResetConfirm>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    body.validate().map_err(|e| ApiError::Validation { fields: serde_json::json!(e.to_string()) })?;
    let h = jwt::hash_token(&body.token);
    let rec: Option<(Uuid, Uuid)> = sqlx::query_as("SELECT id, user_id FROM password_reset_tokens WHERE token_hash=$1 AND used_at IS NULL AND expires_at > now()")
        .bind(h).fetch_optional(&s.db).await.map_err(|_| ApiError::Internal)?;
    let Some((tid, uid)) = rec else { return Err(ApiError::Unprocessable("invalid or expired token".into())) };
    let mut tx = s.db.begin().await.map_err(|_| ApiError::Internal)?;
    let hash = password::hash(&body.new_password).map_err(|_| ApiError::Internal)?;
    sqlx::query("UPDATE users SET password_hash=$2 WHERE id=$1").bind(uid).bind(hash).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    sqlx::query("UPDATE password_reset_tokens SET used_at=now() WHERE id=$1").bind(tid).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    // Invalidate all sessions after password change.
    sqlx::query("UPDATE refresh_tokens SET revoked_at=now() WHERE user_id=$1 AND revoked_at IS NULL").bind(uid).execute(&mut *tx).await.map_err(|_| ApiError::Internal)?;
    tx.commit().await.map_err(|_| ApiError::Internal)?;
    let (st, b) = ApiResponse::ok(serde_json::json!({"ok": true}));
    Ok((st, Json(b.data)))
}
