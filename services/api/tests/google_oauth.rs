use axum_test::TestServer;
use nhlanganiso_api::app::router;
use nhlanganiso_api::config::Config;
use nhlanganiso_api::modules::auth::google_oauth::{
    normalize_email, pkce_challenge_from_verifier, random_token,
};
use nhlanganiso_api::state::AppState;
use serde_json::json;
use std::env;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup_mock_issuer() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
            "jwks_uri": format!("{}/certs", server.uri()),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "scopes_supported": ["openid", "email", "profile"],
            "token_endpoint_auth_methods_supported": [
                "client_secret_post",
                "client_secret_basic",
            ],
        })))
        .mount(&server)
        .await;

    server
}

async fn mock_token_endpoint(server: &MockServer, id_token: &str) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "mock-access-token",
            "token_type": "Bearer",
            "expires_in": 3600,
            "id_token": id_token,
        })))
        .mount(server)
        .await;
}

async fn mock_jwks_endpoint(server: &MockServer, jwks: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/certs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(server)
        .await;
}

fn base64_url_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Builds a dummy RS256 JWT (signature not cryptographically valid, but structure
/// correct for testing the mock JWKS endpoint contract). The test verifier does
/// not actually check the signature — it only validates structure/claims — so a
/// well-formed placeholder is sufficient to exercise the callback logic.
fn dummy_id_token(
    issuer: &str,
    audience: &str,
    subject: &str,
    email: &str,
    nonce: &str,
    exp_offset_seconds: i64,
) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let claims = json!({
        "iss": issuer,
        "aud": audience,
        "sub": subject,
        "email": email,
        "email_verified": true,
        "exp": now + exp_offset_seconds,
        "iat": now,
        "nonce": nonce,
    });

    let header = json!({
        "alg": "RS256",
        "typ": "JWT",
        "kid": "test-key-id",
    });

    let header_b64 = base64_url_encode(serde_json::to_string(&header).unwrap().as_bytes());
    let claims_b64 = base64_url_encode(serde_json::to_string(&claims).unwrap().as_bytes());
    let sig_b64 = base64_url_encode(b"dummy-signature");

    format!("{header_b64}.{claims_b64}.{sig_b64}")
}

fn dummy_jwk() -> serde_json::Value {
    json!({
        "kty": "RSA",
        "kid": "test-key-id",
        "use": "sig",
        "alg": "RS256",
        "n": base64_url_encode(b"dummy-modulus"),
        "e": base64_url_encode(b"AQAB"),
    })
}

async fn setup_test_app(
    mock_server: &MockServer,
    database_url: &str,
    google_issuer_override: Option<&str>,
) -> (TestServer, sqlx::PgPool) {
    env::set_var("GOOGLE_CLIENT_ID", "test-client-id");
    env::set_var("GOOGLE_CLIENT_SECRET", "test-client-secret");
    env::set_var(
        "GOOGLE_REDIRECT_URI",
        &format!("{}/api/v1/auth/google/callback", mock_server.uri()),
    );
    env::set_var(
        "GOOGLE_ISSUER",
        google_issuer_override.unwrap_or(mock_server.uri()),
    );
    env::set_var("APP_ENV", "test");
    env::set_var(
        "JWT_SECRET",
        "test-secret-key-min-32-chars-long-for-testing",
    );
    env::set_var("JWT_ISSUER", "nhlanganiso-api");
    env::set_var("JWT_AUDIENCE", "nhlanganiso-web");
    env::set_var("JWT_ACCESS_TTL_SECONDS", "900");
    env::set_var("JWT_REFRESH_TTL_SECONDS", "2592000");
    env::set_var("DATABASE_URL", database_url);
    env::set_var("BIND_ADDR", "127.0.0.1:0");
    env::set_var("LOG_LEVEL", "error");

    let cfg = Config::from_env().expect("config should load from env");
    let db_pool = sqlx::PgPool::connect(database_url)
        .await
        .expect("database pool should connect");
    let state = AppState::new(cfg, db_pool.clone())
        .await
        .expect("app state should init");
    let app = router(state);
    let server = TestServer::new(app).expect("test server should start");
    (server, db_pool)
}

#[tokio::test]
async fn normalize_email_works() {
    assert_eq!(normalize_email("  User@Example.Com  "), "user@example.com");
    assert_eq!(normalize_email("USER@DOMAIN.CO.ZA"), "user@domain.co.za");
}

#[tokio::test]
async fn pkce_challenge_is_deterministic() {
    let verifier = "test-verifier-string-for-pkce";
    let challenge = pkce_challenge_from_verifier(verifier);
    assert_eq!(challenge, pkce_challenge_from_verifier(verifier));
    assert!(!challenge.is_empty());
    assert_ne!(challenge, verifier);
}

#[tokio::test]
async fn random_token_is_unique_and_url_safe() {
    let a = random_token();
    let b = random_token();
    assert_ne!(a, b);
    assert!(a
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '~'));
}

#[tokio::test]
async fn google_login_redirects_with_state_and_nonce(database_url: &str) {
    let mock_server = setup_mock_issuer().await;
    let (server, _db_pool) = setup_test_app(&mock_server, database_url, None).await;

    let response = server.get("/api/v1/auth/google").await;

    assert_eq!(response.status_code(), 302);
    let location = response
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        location.starts_with("http"),
        "location should be a URL: {location}"
    );
    assert!(
        location.contains("response_type=code"),
        "should request code flow: {location}"
    );
    assert!(
        location.contains("client_id=test-client-id"),
        "should include client id: {location}"
    );
    assert!(
        location.contains("scope=openid"),
        "should request openid scope: {location}"
    );
    assert!(
        location.contains("scope=email"),
        "should request email scope: {location}"
    );
    assert!(
        location.contains("scope=profile"),
        "should request profile scope: {location}"
    );
    assert!(
        location.contains("redirect_uri="),
        "should include redirect uri: {location}"
    );
    assert!(
        location.contains("state="),
        "should include state param: {location}"
    );
    assert!(
        location.contains("nonce="),
        "should include nonce param: {location}"
    );
    assert!(
        location.contains("code_challenge="),
        "should include pkce challenge: {location}"
    );
    assert!(
        location.contains("code_challenge_method=S256"),
        "should use S256: {location}"
    );
}

#[tokio::test]
async fn google_callback_missing_code_returns_422(database_url: &str) {
    let mock_server = setup_mock_issuer().await;
    let (server, _db_pool) = setup_test_app(&mock_server, database_url, None).await;

    let response = server
        .get("/api/v1/auth/google/callback?state=some-state")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[tokio::test]
async fn google_callback_missing_state_returns_422(database_url: &str) {
    let mock_server = setup_mock_issuer().await;
    let (server, _db_pool) = setup_test_app(&mock_server, database_url, None).await;

    let response = server
        .get("/api/v1/auth/google/callback?code=some-code")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[tokio::test]
async fn google_callback_invalid_state_returns_422(database_url: &str) {
    let mock_server = setup_mock_issuer().await;
    let (server, _db_pool) = setup_test_app(&mock_server, database_url, None).await;

    let response = server
        .get("/api/v1/auth/google/callback?code=some-code&state=nonexistent-state")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[tokio::test]
async fn google_callback_google_error_redirects_to_frontend(database_url: &str) {
    let mock_server = setup_mock_issuer().await;
    let (server, _db_pool) = setup_test_app(&mock_server, database_url, None).await;

    let response = server
        .get("/api/v1/auth/google/callback?error=access_denied&error_description=User+denied+the+request&state=ignored")
        .await;

    assert_eq!(response.status_code(), 302);
    let location = response
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        location.contains("auth=error"),
        "should redirect to frontend error: {location}"
    );
    assert!(
        location.contains("reason=google_auth_failed"),
        "should include reason: {location}"
    );
}
