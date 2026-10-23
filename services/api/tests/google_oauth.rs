use axum_test::TestServer;
use nhlanganiso_api::app::router;
use nhlanganiso_api::config::Config;
use nhlanganiso_api::state::AppState;
use serde_json::json;
use serial_test::serial;
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

async fn setup_test_app(
    mock_server: &MockServer,
    database_url: &str,
    google_issuer_override: Option<&str>,
) -> (TestServer, sqlx::PgPool) {
    env::set_var("GOOGLE_CLIENT_ID", "test-client-id");
    env::set_var("GOOGLE_CLIENT_SECRET", "test-client-secret");
    env::set_var(
        "GOOGLE_REDIRECT_URI",
        format!("{}/api/v1/auth/google/callback", mock_server.uri()),
    );
    let issuer = google_issuer_override
        .map(str::to_owned)
        .unwrap_or_else(|| mock_server.uri());
    env::set_var("GOOGLE_ISSUER", issuer);
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
    sqlx::migrate!("./migrations")
        .run(&db_pool)
        .await
        .expect("database migrations should run");
    let state = AppState::new(cfg, db_pool.clone())
        .await
        .expect("app state should init");
    let app = router(state);
    let server = TestServer::new(app);
    (server, db_pool)
}

fn test_database_url() -> String {
    env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be configured for OAuth tests")
}

#[serial]
#[tokio::test]
async fn google_login_redirects_with_state_and_nonce() {
    let mock_server = setup_mock_issuer().await;
    let database_url = test_database_url();
    let (server, _db_pool) = setup_test_app(&mock_server, &database_url, None).await;

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

#[serial]
#[tokio::test]
async fn google_callback_missing_code_returns_422() {
    let mock_server = setup_mock_issuer().await;
    let database_url = test_database_url();
    let (server, _db_pool) = setup_test_app(&mock_server, &database_url, None).await;

    let response = server
        .get("/api/v1/auth/google/callback?state=some-state")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[serial]
#[tokio::test]
async fn google_callback_missing_state_returns_422() {
    let mock_server = setup_mock_issuer().await;
    let database_url = test_database_url();
    let (server, _db_pool) = setup_test_app(&mock_server, &database_url, None).await;

    let response = server
        .get("/api/v1/auth/google/callback?code=some-code")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[serial]
#[tokio::test]
async fn google_callback_invalid_state_returns_422() {
    let mock_server = setup_mock_issuer().await;
    let database_url = test_database_url();
    let (server, _db_pool) = setup_test_app(&mock_server, &database_url, None).await;

    let response = server
        .get("/api/v1/auth/google/callback?code=some-code&state=nonexistent-state")
        .await;

    assert_eq!(response.status_code(), 422);
}

#[serial]
#[tokio::test]
async fn google_callback_google_error_redirects_to_frontend() {
    let mock_server = setup_mock_issuer().await;
    let database_url = test_database_url();
    let (server, _db_pool) = setup_test_app(&mock_server, &database_url, None).await;

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
