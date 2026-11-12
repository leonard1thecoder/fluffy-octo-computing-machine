use axum_test::TestServer;
use nhlanganiso_api::app::router;
use nhlanganiso_api::config::Config;
use nhlanganiso_api::state::AppState;
use serial_test::serial;
use sqlx::PgPool;
use std::env;
use uuid::Uuid;

// Every fixture carries a per-run token in its name, slug or email, so these tests can run
// against a shared database without seeing each other's rows. Cleanup runs at the end of each test.

async fn setup(database_url: &str) -> (TestServer, PgPool) {
    env::set_var("GOOGLE_CLIENT_ID", "test-client-id");
    env::set_var("GOOGLE_CLIENT_SECRET", "test-client-secret");
    env::set_var(
        "GOOGLE_REDIRECT_URI",
        "http://127.0.0.1:9/api/v1/auth/google/callback",
    );
    env::set_var("GOOGLE_ISSUER", "http://127.0.0.1:9/");
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
    let pool = PgPool::connect(database_url)
        .await
        .expect("database pool should connect");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("database migrations should run");
    let state = AppState::new(cfg, pool.clone())
        .await
        .expect("app state should init");
    (TestServer::new(router(state)), pool)
}

fn test_database_url() -> String {
    env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be configured for catalogue tests")
}

async fn insert_category(pool: &PgPool, name: &str, slug: &str, parent: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO categories (name, slug, parent_id) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(name)
    .bind(slug)
    .bind(parent)
    .fetch_one(pool)
    .await
    .expect("category should insert")
}

async fn insert_product(
    pool: &PgPool,
    name: &str,
    slug: &str,
    category: Option<Uuid>,
    active: bool,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO products (name, slug, description, image_url, price_cents, currency, active, category_id)
         VALUES ($1, $2, '', NULL, 1000, 'ZAR', $3, $4) RETURNING id",
    )
    .bind(name)
    .bind(slug)
    .bind(active)
    .bind(category)
    .fetch_one(pool)
    .await
    .expect("product should insert")
}

/// Gives a product `count` likes, each from a new shopper.
async fn add_likes(pool: &PgPool, product: Uuid, count: usize) {
    for _ in 0..count {
        let email = format!("{}@example.test", Uuid::new_v4());
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, email_normalized, full_name, password_hash)
             VALUES ($1, $1, 'Test Shopper', 'not-used') RETURNING id",
        )
        .bind(&email)
        .fetch_one(pool)
        .await
        .expect("user should insert");
        sqlx::query("INSERT INTO product_favorites (user_id, product_id) VALUES ($1, $2)")
            .bind(user)
            .bind(product)
            .execute(pool)
            .await
            .expect("favourite should insert");
    }
}

async fn cleanup(pool: &PgPool, run: &str) {
    let slug_pattern = format!("%-{run}");
    sqlx::query(
        "DELETE FROM product_favorites WHERE product_id IN (SELECT id FROM products WHERE slug LIKE $1)",
    )
    .bind(&slug_pattern)
    .execute(pool)
    .await
    .expect("favourites should clean up");
    sqlx::query("DELETE FROM products WHERE slug LIKE $1")
        .bind(&slug_pattern)
        .execute(pool)
        .await
        .expect("products should clean up");
    sqlx::query("DELETE FROM categories WHERE slug LIKE $1")
        .bind(&slug_pattern)
        .execute(pool)
        .await
        .expect("categories should clean up");
    sqlx::query("DELETE FROM delivery_zones WHERE name LIKE $1")
        .bind(format!("%{run}"))
        .execute(pool)
        .await
        .expect("delivery zones should clean up");
    sqlx::query("DELETE FROM users WHERE email LIKE '%@example.test'")
        .execute(pool)
        .await
        .expect("users should clean up");
}

/// GET /api/v1/products and return the item names in order, plus meta.total.
async fn list_products(server: &TestServer, query: &str) -> (Vec<String>, i64) {
    let response = server.get(&format!("/api/v1/products?{query}")).await;
    assert_eq!(
        response.status_code(),
        200,
        "list should succeed for {query}"
    );
    let body: serde_json::Value = response.json();
    let names = body["data"]
        .as_array()
        .expect("list data should be an array")
        .iter()
        .map(|item| item["name"].as_str().expect("name").to_owned())
        .collect();
    let total = body["meta"]["total"].as_i64().expect("meta.total");
    (names, total)
}

#[tokio::test]
#[serial]
async fn product_list_sorts_by_most_liked_and_respects_search_in_total() {
    let (server, pool) = setup(&test_database_url()).await;
    let run = Uuid::new_v4().simple().to_string();
    let token = format!("zz{run}");

    let alpha = insert_product(
        &pool,
        &format!("Alpha {token}"),
        &format!("alpha-{run}"),
        None,
        true,
    )
    .await;
    let bravo = insert_product(
        &pool,
        &format!("Bravo {token}"),
        &format!("bravo-{run}"),
        None,
        true,
    )
    .await;
    insert_product(
        &pool,
        &format!("Charlie {token}"),
        &format!("charlie-{run}"),
        None,
        true,
    )
    .await; // No likes.
    let delta = insert_product(
        &pool,
        &format!("Delta {token}"),
        &format!("delta-{run}"),
        None,
        true,
    )
    .await;
    let echo = insert_product(
        &pool,
        &format!("Echo {token}"),
        &format!("echo-{run}"),
        None,
        false,
    )
    .await;

    add_likes(&pool, alpha, 3).await;
    add_likes(&pool, delta, 2).await;
    add_likes(&pool, bravo, 1).await;
    add_likes(&pool, echo, 5).await; // Inactive, so it must not appear even with the most likes.

    let (names, total) = list_products(&server, &format!("search={token}&sort=most_liked")).await;
    assert_eq!(
        names,
        vec![
            format!("Alpha {token}"),
            format!("Delta {token}"),
            format!("Bravo {token}"),
            format!("Charlie {token}"),
        ],
        "most_liked orders by likes descending"
    );
    assert_eq!(total, 4, "total counts the matching active rows only");

    let (names, _) = list_products(&server, &format!("search={token}")).await;
    assert_eq!(
        names,
        vec![
            format!("Alpha {token}"),
            format!("Bravo {token}"),
            format!("Charlie {token}"),
            format!("Delta {token}"),
        ],
        "default sort is by name"
    );

    let (names, total) = list_products(
        &server,
        &format!("search={token}&sort=most_liked&per_page=2&page=2"),
    )
    .await;
    assert_eq!(
        names,
        vec![format!("Bravo {token}"), format!("Charlie {token}")]
    );
    assert_eq!(total, 4, "total is unaffected by the page size");

    let detail = server.get(&format!("/api/v1/products/alpha-{run}")).await;
    assert_eq!(detail.status_code(), 200);
    let body: serde_json::Value = detail.json();
    assert_eq!(body["favorite_count"], 3, "detail carries the like count");

    let inactive = server.get(&format!("/api/v1/products/echo-{run}")).await;
    assert_eq!(inactive.status_code(), 404, "inactive products stay hidden");

    cleanup(&pool, &run).await;
}

#[tokio::test]
#[serial]
async fn product_list_rejects_unknown_sort_and_filters_by_category() {
    let (server, pool) = setup(&test_database_url()).await;
    let run = Uuid::new_v4().simple().to_string();
    let token = format!("zz{run}");

    let parent = insert_category(&pool, &format!("Cake {run}"), &format!("cake-{run}"), None).await;
    let child = insert_category(
        &pool,
        &format!("Cupcake {run}"),
        &format!("cake-child-{run}"),
        Some(parent),
    )
    .await;
    let other = insert_category(
        &pool,
        &format!("Other {run}"),
        &format!("other-{run}"),
        None,
    )
    .await;

    insert_product(
        &pool,
        &format!("Alpha {token}"),
        &format!("alpha-{run}"),
        Some(child),
        true,
    )
    .await;
    insert_product(
        &pool,
        &format!("Bravo {token}"),
        &format!("bravo-{run}"),
        Some(parent),
        true,
    )
    .await;
    insert_product(
        &pool,
        &format!("Charlie {token}"),
        &format!("charlie-{run}"),
        Some(other),
        true,
    )
    .await;

    let bogus = server
        .get(&format!("/api/v1/products?search={token}&sort=price_desc"))
        .await;
    assert_eq!(bogus.status_code(), 422, "unknown sort values are refused");

    // A top-level category includes its sub-categories.
    let (names, total) =
        list_products(&server, &format!("search={token}&category=cake-{run}")).await;
    assert_eq!(
        names,
        vec![format!("Alpha {token}"), format!("Bravo {token}")]
    );
    assert_eq!(total, 2);

    // A sub-category matches only its own products.
    let (names, total) = list_products(
        &server,
        &format!("search={token}&category=cake-child-{run}"),
    )
    .await;
    assert_eq!(names, vec![format!("Alpha {token}")]);
    assert_eq!(total, 1);

    // An unknown category returns no rows rather than an error.
    let (names, total) =
        list_products(&server, &format!("search={token}&category=no-such-{run}")).await;
    assert!(names.is_empty());
    assert_eq!(total, 0);

    cleanup(&pool, &run).await;
}

#[tokio::test]
#[serial]
async fn delivery_zones_lists_only_active_zones_by_fee() {
    let (server, pool) = setup(&test_database_url()).await;
    let run = Uuid::new_v4().simple().to_string();

    for (name, fee, active) in [
        (format!("Zone far {run}"), 9000_i64, true),
        (format!("Zone near {run}"), 2000_i64, true),
        (format!("Zone closed {run}"), 500_i64, false),
    ] {
        sqlx::query("INSERT INTO delivery_zones (name, fee_cents, active) VALUES ($1, $2, $3)")
            .bind(name)
            .bind(fee)
            .bind(active)
            .execute(&pool)
            .await
            .expect("zone should insert");
    }

    let response = server.get("/api/v1/delivery-zones").await;
    assert_eq!(response.status_code(), 200);
    let zones: Vec<serde_json::Value> = response.json();
    let ours: Vec<String> = zones
        .iter()
        .map(|z| z["name"].as_str().expect("name").to_owned())
        .filter(|name| name.ends_with(&run))
        .collect();
    assert_eq!(
        ours,
        vec![format!("Zone near {run}"), format!("Zone far {run}")],
        "active zones only, cheapest first"
    );
    let near = zones
        .iter()
        .find(|z| z["name"] == format!("Zone near {run}"))
        .expect("near zone present");
    assert_eq!(near["fee_cents"], 2000);
    assert!(near["id"].is_string(), "zones carry an id for checkout");

    cleanup(&pool, &run).await;
}
