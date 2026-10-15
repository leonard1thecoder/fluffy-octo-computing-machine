use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::config::Config;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub iss: String,
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
    pub jti: String,
}

pub fn mint_access(user_id: Uuid, cfg: &Config) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: user_id,
        iss: cfg.jwt_issuer.clone(),
        aud: cfg.jwt_audience.clone(),
        exp: now + cfg.jwt_access_ttl_secs,
        iat: now,
        jti: Uuid::new_v4().to_string(),
    };
    Ok(encode(&Header::new(jsonwebtoken::Algorithm::HS256), &claims, &EncodingKey::from_secret(cfg.jwt_secret.as_bytes()))?)
}

pub fn verify_access(token: &str, cfg: &Config) -> anyhow::Result<Claims> {
    let mut v = Validation::new(jsonwebtoken::Algorithm::HS256);
    v.set_issuer(std::slice::from_ref(&cfg.jwt_issuer));
    v.set_audience(std::slice::from_ref(&cfg.jwt_audience));
    v.leeway = 30;
    let data = decode::<Claims>(token, &DecodingKey::from_secret(cfg.jwt_secret.as_bytes()), &v)?;
    Ok(data.claims)
}

/// Opaque refresh token: 32 random bytes, base64url. Only SHA-256 hash stored.
pub fn new_refresh_token() -> (String, String) {
    use rand::RngCore;
    use sha2::{Digest, Sha256};
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let token = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, raw);
    let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
    (token, hash)
}

pub fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
