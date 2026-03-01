#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::{routing::get, Extension, Router};
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};
    use musicboxd::app::{shell, App};
    use musicboxd::auth::server::{
        github_callback, github_login, google_callback, google_login, logout, OAuthConfig,
    };
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::SqlitePool;

    let conf = get_configuration(None).unwrap();
    let addr = conf.leptos_options.site_addr;
    let leptos_options = conf.leptos_options;

    let routes = generate_route_list(App);

    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "musicboxd.db".to_string());

    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .expect("failed to open database");

    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("failed to run migrations");

    let base_url =
        std::env::var("BASE_URL").unwrap_or_else(|_| format!("http://{}", addr));

    let oauth_config = OAuthConfig::from_env(&base_url).unwrap_or_else(|e| {
        eprintln!("Warning: OAuth not configured ({e}). Sign-in will be unavailable.");
        OAuthConfig {
            google_client_id: String::new(),
            google_client_secret: String::new(),
            github_client_id: String::new(),
            github_client_secret: String::new(),
            base_url: base_url.clone(),
        }
    });

    let app = Router::new()
        .route("/auth/google", get(google_login))
        .route("/auth/google/callback", get(google_callback))
        .route("/auth/github", get(github_login))
        .route("/auth/github/callback", get(github_callback))
        .route("/auth/logout", get(logout))
        .leptos_routes_with_context(
            &leptos_options,
            routes,
            {
                let pool = pool.clone();
                move || provide_context(pool.clone())
            },
            {
                let leptos_options = leptos_options.clone();
                move || shell(leptos_options.clone())
            },
        )
        .fallback(leptos_axum::file_and_error_handler(shell))
        .with_state(leptos_options)
        .layer(Extension(oauth_config))
        .layer(Extension(pool));

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("Listening on http://{addr}");
    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}

#[cfg(not(feature = "ssr"))]
pub fn main() {}
