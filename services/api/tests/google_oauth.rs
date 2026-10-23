use axum_test::TestServer;
use nhlanganiso_api::app::router;
use nhlanganiso_api::config::Config;
use nhlanganiso_api::modules::auth::google_oauth::{
    normalize_email, pkce_challenge_from_verifier, random_token,
};
use nhlanganiso_api::state::AppState;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::env;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mock Google OIDC provider: discovery document, token endpoint, JWKS.
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

/// Boot the API against a mock issuer + the test database.
async fn setup_test_app(mock_server: &MockServer) -> (TestServer, sqlx::PgPool) {
    let database_url =
        env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");

    env::set_var("GOOGLE_CLIENT_ID", "test-client-id");
    env::set_var("GOOGLE_CLIENT_SECRET", "test-client-secret");
    env::set_var(
        "GOOGLE_REDIRECT_URI",
        "http://localhost:8080/api/v1/auth/google/callback",
    );
    env::set_var("GOOGLE_ISSUER", mock_server.uri());
    env::set_var("APP_ENV", "test");
    env::set_var(
        "JWT_SECRET",
        "test-secret-key-min-32-chars-long-for-testing",
    );
    env::set_var("JWT_ISSUER", "nhlanganiso-api");
    env::set_var("JWT_AUDIENCE", "nhlanganiso-web");
    env::set_var("JWT_ACCESS_TTL_SECONDS", "900");
    env::set_var("JWT_REFRESH_TTL_SECONDS", "2592000");
    env::set_var("BIND_ADDR", "127.0.0.1:0");
    env::set_var("LOG_LEVEL", "error");

    let cfg = Config::from_env().expect("config should load from env");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await
        .expect("test database should be reachable");

    // Apply migrations so the schema matches the running service.
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("migrations should apply");

    let state = AppState::new(cfg, pool.clone())
        .await
        .expect("app state should init");
    let app = router(state);
    let server = TestServer::new(app).expect("test server should start");
    (server, pool)
}

#[test]
fn normalize_email_works() {
    assert_eq!(normalize_email("  User@Example.Com  "), "user@example.com");
    assert_eq!(normalize_email("USER@DOMAIN.CO.ZA"), "user@domain.co.za");
}

#[test]
fn pkce_challenge_is_deterministic() {
    let verifier = "test-verifier-string-for-pkce";
    let challenge = pkce_challenge_from_verifier(verifier);
    assert_eq!(challenge, pkce_challenge_from_verifier(verifier));
    assert!(!challenge.is_empty());
    assert_ne!(challenge, verifier);
    // S256 challenge is base64url of a 32-byte digest => 43 chars, no padding.
    assert_eq!(challenge.len(), 43);
    assert!(!challenge.contains('='));
}

#[test]
fn random_token_is_unique_and_url_safe() {
    let a = random_token();
    let b = random_token();
    assert_ne!(a, b);
    assert!(a
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '~'));
}

#[tokio::test]
async fn google_login_redirects_with_state_and_nonce() {
    let mock_server = setup_mock_issuer().await;
    let (server, _pool) = setup_test_app(&mock_server).await;

    let response = server.get("/api/v1/auth/google").await;

    // axum's Redirect::to() responds 303 See Other.
    assert_eq!(response.status_code(), 303);
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
        location.contains("scope=openid%20email%20profile"),
        "should request scopes: {location}"
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
    // Client secret must never appear in the browser-facing URL.
    assert!(
        !location.contains("test-client-secret"),
        "secret must not leak: {location}"
    );
}

#[tokio::test]
async fn google_callback_missing_code_returns_422() {
    let mock_server = setup_mock_issuer().await;
    let (server, _pool) = setup_test_app(&mock_server).await;

    let response = server
        .get("/api/v1/auth/google/callback?state=some-state")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[tokio::test]
async fn google_callback_missing_state_returns_422() {
    let mock_server = setup_mock_issuer().await;
    let (server, _pool) = setup_test_app(&mock_server).await;

    let response = server
        .get("/api/v1/auth/google/callback?code=some-code")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[tokio::test]
async fn google_callback_invalid_state_returns_422() {
    let mock_server = setup_mock_issuer().await;
    let (server, _pool) = setup_test_app(&mock_server).await;

    let response = server
        .get("/api/v1/auth/google/callback?code=some-code&state=nonexistent-state")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[tokio::test]
async fn google_callback_google_error_redirects_to_frontend() {
    let mock_server = setup_mock_issuer().await;
    let (server, _pool) = setup_test_app(&mock_server).await;

    let response = server
        .get("/api/v1/auth/google/callback?error=access_denied&error_description=User+denied+the+request&state=ignored")
        .await;

    // axum's Redirect::to() responds 303 See Other.
    assert_eq!(response.status_code(), 303);
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

#[tokio::test]
async fn google_callback_rejects_invalid_id_token_signature() {
    let mock_server = setup_mock_issuer().await;
    let (server, _pool) = setup_test_app(&mock_server).await;

    // Start a real login so a valid state/nonce pair exists.
    let login = server.get("/api/v1/auth/google").await;
    assert_eq!(login.status_code(), 303);
    let location = login.headers().get("location").unwrap().to_str().unwrap();
    let state = location
        .split("state=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .expect("state param in redirect")
        .to_string();

    // Token endpoint returns a structurally valid but unsigned ID token.
    mock_token_endpoint(&mock_server, "not-a-jwt").await;

    let response = server
        .get(&format!(
            "/api/v1/auth/google/callback?code=auth-code&state={state}"
        ))
        .await;

    assert_eq!(
        response.status_code(),
        401,
        "an invalid ID token must be rejected"
    );
}

#[tokio::test]
async fn google_exchange_rejects_unknown_code() {
    let mock_server = setup_mock_issuer().await;
    let (server, _pool) = setup_test_app(&mock_server).await;

    let response = server
        .post("/api/v1/auth/google/exchange")
        .json(&serde_json::json!({ "code": "not-a-real-code" }))
        .await;

    assert_eq!(
        response.status_code(),
        401,
        "an unknown exchange code must be rejected"
    );
}

#[tokio::test]
async fn google_exchange_code_is_single_use() {
    let mock_server = setup_mock_issuer().await;
    let (server, pool) = setup_test_app(&mock_server).await;

    // Create a user and an exchange code directly, then try to use it twice.
    let user_id: uuid::Uuid = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, email, email_normalized, full_name, password_hash, role, status) VALUES ($1,$2,$2,'Google User','','customer','active')",
    )
    .bind(user_id)
    .bind("exchange.user@example.com")
    .execute(&pool)
    .await
    .expect("insert user");

    let raw = "one-time-exchange-code-for-test";
    let hash = {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(raw.as_bytes()))
    };
    sqlx::query(
        "INSERT INTO oauth_exchange_codes (code_hash, user_id, expires_at) VALUES ($1,$2,now()+interval '2 minutes')",
    )
    .bind(&hash)
    .bind(user_id)
    .execute(&pool)
    .await
    .expect("insert exchange code");

    let first = server
        .post("/api/v1/auth/google/exchange")
        .json(&serde_json::json!({ "code": raw }))
        .await;
    assert_eq!(first.status_code(), 200, "first exchange should succeed");
    let body: serde_json::Value = first.json();
    assert!(
        body.get("access_token").is_some(),
        "should return an access token"
    );
    assert!(
        body.get("refresh_token").is_some(),
        "should return a refresh token"
    );

    let second = server
        .post("/api/v1/auth/google/exchange")
        .json(&serde_json::json!({ "code": raw }))
        .await;
    assert_eq!(
        second.status_code(),
        401,
        "an exchange code must not be reusable"
    );
}
