#[cfg(feature = "ssr")]
pub mod server {
    use axum::{
        extract::Query,
        http::{header, StatusCode},
        response::{IntoResponse, Redirect, Response},
        Extension,
    };
    use oauth2::{
        basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
        EndpointNotSet, EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
        TokenResponse, TokenUrl,
    };
    use serde::Deserialize;
    use sqlx::{Row, SqlitePool};
    use uuid::Uuid;

    #[derive(Clone)]
    pub struct CurrentUser {
        pub user_id: String,
        pub username: String,
    }

    pub async fn session_auth(
        Extension(pool): Extension<SqlitePool>,
        mut request: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        let headers = request.headers().clone();
        let user = get_session_user(&pool, &headers)
            .await
            .map(|(user_id, username)| CurrentUser { user_id, username });
        request.extensions_mut().insert(user);
        next.run(request).await
    }

    #[derive(Clone)]
    pub struct OAuthConfig {
        pub google_client_id: String,
        pub google_client_secret: String,
        pub github_client_id: String,
        pub github_client_secret: String,
        pub base_url: String,
    }

    impl OAuthConfig {
        pub fn from_env(base_url: &str) -> Result<Self, String> {
            Ok(Self {
                google_client_id: std::env::var("GOOGLE_CLIENT_ID")
                    .map_err(|_| "GOOGLE_CLIENT_ID not set".to_string())?,
                google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET")
                    .map_err(|_| "GOOGLE_CLIENT_SECRET not set".to_string())?,
                github_client_id: std::env::var("GITHUB_CLIENT_ID")
                    .map_err(|_| "GITHUB_CLIENT_ID not set".to_string())?,
                github_client_secret: std::env::var("GITHUB_CLIENT_SECRET")
                    .map_err(|_| "GITHUB_CLIENT_SECRET not set".to_string())?,
                base_url: base_url.to_string(),
            })
        }
    }

    #[derive(Deserialize)]
    pub struct CallbackParams {
        code: String,
        state: String,
    }

    type ConfiguredClient =
        BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

    fn build_google_client(config: &OAuthConfig) -> Result<ConfiguredClient, String> {
        let auth_url = AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())
            .map_err(|e| format!("invalid Google auth URL: {e}"))?;
        let token_url = TokenUrl::new("https://oauth2.googleapis.com/token".to_string())
            .map_err(|e| format!("invalid Google token URL: {e}"))?;
        let redirect_url =
            RedirectUrl::new(format!("{}/auth/google/callback", config.base_url))
                .map_err(|e| format!("invalid Google redirect URL: {e}"))?;
        Ok(BasicClient::new(ClientId::new(config.google_client_id.clone()))
            .set_client_secret(ClientSecret::new(config.google_client_secret.clone()))
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url))
    }

    fn build_github_client(config: &OAuthConfig) -> Result<ConfiguredClient, String> {
        let auth_url = AuthUrl::new("https://github.com/login/oauth/authorize".to_string())
            .map_err(|e| format!("invalid GitHub auth URL: {e}"))?;
        let token_url =
            TokenUrl::new("https://github.com/login/oauth/access_token".to_string())
                .map_err(|e| format!("invalid GitHub token URL: {e}"))?;
        let redirect_url =
            RedirectUrl::new(format!("{}/auth/github/callback", config.base_url))
                .map_err(|e| format!("invalid GitHub redirect URL: {e}"))?;
        Ok(BasicClient::new(ClientId::new(config.github_client_id.clone()))
            .set_client_secret(ClientSecret::new(config.github_client_secret.clone()))
            .set_auth_uri(auth_url)
            .set_token_uri(token_url)
            .set_redirect_uri(redirect_url))
    }

    pub async fn google_login(
        Extension(config): Extension<OAuthConfig>,
        Extension(pool): Extension<SqlitePool>,
    ) -> impl IntoResponse {
        let client = match build_google_client(&config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("google_login: failed to build OAuth client: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "OAuth misconfigured").into_response();
            }
        };

        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("email".to_string()))
            .add_scope(Scope::new("profile".to_string()))
            .set_pkce_challenge(challenge)
            .url();

        let result = sqlx::query(
            "INSERT INTO oauth_states (csrf_token, pkce_verifier) VALUES (?, ?)",
        )
        .bind(csrf.secret())
        .bind(verifier.secret())
        .execute(&pool)
        .await;

        match result {
            Ok(_) => Redirect::to(url.as_str()).into_response(),
            Err(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Failed to initiate login").into_response()
            }
        }
    }

    pub async fn google_callback(
        Extension(config): Extension<OAuthConfig>,
        Extension(pool): Extension<SqlitePool>,
        Query(params): Query<CallbackParams>,
    ) -> Response {
        let state_row = sqlx::query(
            "SELECT pkce_verifier FROM oauth_states \
             WHERE csrf_token = ? \
             AND created_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-10 minutes')",
        )
        .bind(&params.state)
        .fetch_optional(&pool)
        .await;

        let pkce_verifier: String = match state_row {
            Ok(Some(row)) => row.get("pkce_verifier"),
            _ => return (StatusCode::BAD_REQUEST, "Invalid or expired state").into_response(),
        };

        let _ = sqlx::query("DELETE FROM oauth_states WHERE csrf_token = ?")
            .bind(&params.state)
            .execute(&pool)
            .await;

        let client = match build_google_client(&config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("google_callback: failed to build OAuth client: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "OAuth misconfigured").into_response();
            }
        };

        let http_client = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build reqwest client");

        let token = client
            .exchange_code(AuthorizationCode::new(params.code))
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier))
            .request_async(&http_client)
            .await;

        let token = match token {
            Ok(t) => t,
            Err(_) => return (StatusCode::BAD_REQUEST, "Token exchange failed").into_response(),
        };

        #[derive(Deserialize)]
        struct GoogleUserInfo {
            sub: String,
            email: String,
        }

        let resp = match http_client
            .get("https://openidconnect.googleapis.com/v1/userinfo")
            .bearer_auth(token.access_token().secret())
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch user info")
                    .into_response()
            }
        };

        let info: GoogleUserInfo = match resp.json().await {
            Ok(i) => i,
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to parse user info")
                    .into_response()
            }
        };

        match complete_login(&pool, &config, "google", &info.sub, &info.email, &info.email).await {
            Ok(resp) => resp,
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response(),
        }
    }

    pub async fn github_login(
        Extension(config): Extension<OAuthConfig>,
        Extension(pool): Extension<SqlitePool>,
    ) -> impl IntoResponse {
        let client = match build_github_client(&config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("github_login: failed to build OAuth client: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "OAuth misconfigured").into_response();
            }
        };

        let (url, csrf) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new("user:email".to_string()))
            .url();

        let result = sqlx::query(
            "INSERT INTO oauth_states (csrf_token, pkce_verifier) VALUES (?, ?)",
        )
        .bind(csrf.secret())
        .bind("")
        .execute(&pool)
        .await;

        match result {
            Ok(_) => Redirect::to(url.as_str()).into_response(),
            Err(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Failed to initiate login").into_response()
            }
        }
    }

    pub async fn github_callback(
        Extension(config): Extension<OAuthConfig>,
        Extension(pool): Extension<SqlitePool>,
        Query(params): Query<CallbackParams>,
    ) -> Response {
        let state_exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM oauth_states \
             WHERE csrf_token = ? \
             AND created_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-10 minutes')",
        )
        .bind(&params.state)
        .fetch_one(&pool)
        .await
        .unwrap_or(0);

        if state_exists == 0 {
            return (StatusCode::BAD_REQUEST, "Invalid or expired state").into_response();
        }

        let _ = sqlx::query("DELETE FROM oauth_states WHERE csrf_token = ?")
            .bind(&params.state)
            .execute(&pool)
            .await;

        let client = match build_github_client(&config) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("github_callback: failed to build OAuth client: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "OAuth misconfigured").into_response();
            }
        };

        let http_client = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build reqwest client");

        let token = client
            .exchange_code(AuthorizationCode::new(params.code))
            .request_async(&http_client)
            .await;

        let token = match token {
            Ok(t) => t,
            Err(_) => return (StatusCode::BAD_REQUEST, "Token exchange failed").into_response(),
        };

        #[derive(Deserialize)]
        struct GitHubUser {
            id: i64,
            login: String,
            email: Option<String>,
        }

        let user: GitHubUser = match http_client
            .get("https://api.github.com/user")
            .bearer_auth(token.access_token().secret())
            .header("User-Agent", "musicboxd/0.1")
            .send()
            .await
        {
            Ok(resp) => match resp.json().await {
                Ok(u) => u,
                Err(_) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to parse GitHub user")
                        .into_response()
                }
            },
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to fetch GitHub user")
                    .into_response()
            }
        };

        let email = if let Some(e) = user.email.filter(|e| !e.is_empty()) {
            e
        } else {
            #[derive(Deserialize)]
            struct GitHubEmail {
                email: String,
                primary: bool,
            }

            let emails: Vec<GitHubEmail> = match http_client
                .get("https://api.github.com/user/emails")
                .bearer_auth(token.access_token().secret())
                .header("User-Agent", "musicboxd/0.1")
                .send()
                .await
            {
                Ok(resp) => resp.json().await.unwrap_or_else(|_| vec![]),
                Err(_) => vec![],
            };

            match emails.into_iter().find(|e| e.primary).map(|e| e.email) {
                Some(e) => e,
                None => {
                    return (StatusCode::BAD_REQUEST, "No public email on GitHub account")
                        .into_response()
                }
            }
        };

        let provider_uid = user.id.to_string();
        match complete_login(&pool, &config, "github", &provider_uid, &email, &user.login).await {
            Ok(resp) => resp,
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response(),
        }
    }

    pub async fn logout(
        Extension(pool): Extension<SqlitePool>,
        Extension(config): Extension<OAuthConfig>,
        headers: axum::http::HeaderMap,
    ) -> impl IntoResponse {
        if let Some(session_id) = extract_session_id(&headers) {
            let _ = sqlx::query("DELETE FROM sessions WHERE session_id = ?")
                .bind(&session_id)
                .execute(&pool)
                .await;
        }
        let secure_flag = if config.base_url.starts_with("https") {
            "; Secure"
        } else {
            ""
        };
        Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/")
            .header(
                header::SET_COOKIE,
                format!("session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0{}", secure_flag),
            )
            .body(axum::body::Body::empty())
            .unwrap()
    }

    pub fn extract_session_id(headers: &axum::http::HeaderMap) -> Option<String> {
        let cookie_str = headers
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())?;
        for part in cookie_str.split(';') {
            let part = part.trim();
            if let Some(val) = part.strip_prefix("session=") {
                let val = val.trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
        None
    }

    pub async fn get_session_user(
        pool: &SqlitePool,
        headers: &axum::http::HeaderMap,
    ) -> Option<(String, String)> {
        let session_id = extract_session_id(headers)?;
        let row = sqlx::query(
            "SELECT u.user_id, u.username \
             FROM sessions s JOIN users u ON s.user_id = u.user_id \
             WHERE s.session_id = ? \
             AND s.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        )
        .bind(&session_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()?;
        Some((row.get("user_id"), row.get("username")))
    }

    async fn complete_login(
        pool: &SqlitePool,
        config: &OAuthConfig,
        provider: &str,
        provider_uid: &str,
        email: &str,
        username_hint: &str,
    ) -> Result<Response, sqlx::Error> {
        let user_id = upsert_user(pool, provider, provider_uid, email, username_hint).await?;

        let session_id = Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO sessions (session_id, user_id, expires_at) \
             VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '+30 days'))",
        )
        .bind(&session_id)
        .bind(&user_id)
        .execute(pool)
        .await?;

        let max_age = 30u64 * 24 * 60 * 60;
        let expires = httpdate::fmt_http_date(
            std::time::SystemTime::now() + std::time::Duration::from_secs(max_age),
        );
        let secure_flag = if config.base_url.starts_with("https") {
            "; Secure"
        } else {
            ""
        };
        Ok(Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/")
            .header(
                header::SET_COOKIE,
                format!(
                    "session={}; HttpOnly; SameSite=Lax; Path=/; Max-Age={}; Expires={}{}",
                    session_id, max_age, expires, secure_flag
                ),
            )
            .body(axum::body::Body::empty())
            .unwrap())
    }

    async fn upsert_user(
        pool: &SqlitePool,
        provider: &str,
        provider_uid: &str,
        email: &str,
        username_hint: &str,
    ) -> Result<String, sqlx::Error> {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT user_id FROM oauth_accounts WHERE provider = ? AND provider_uid = ?",
        )
        .bind(provider)
        .bind(provider_uid)
        .fetch_optional(pool)
        .await?;

        if let Some(user_id) = existing {
            return Ok(user_id);
        }

        let user_id = Uuid::new_v4().to_string();
        let username = generate_username(pool, username_hint).await?;

        sqlx::query(
            "INSERT OR IGNORE INTO users (user_id, username, email) VALUES (?, ?, ?)",
        )
        .bind(&user_id)
        .bind(&username)
        .bind(email)
        .execute(pool)
        .await?;

        let actual_user_id: String =
            sqlx::query_scalar("SELECT user_id FROM users WHERE email = ?")
                .bind(email)
                .fetch_one(pool)
                .await?;

        sqlx::query(
            "INSERT OR IGNORE INTO oauth_accounts \
             (user_id, provider, provider_uid) VALUES (?, ?, ?)",
        )
        .bind(&actual_user_id)
        .bind(provider)
        .bind(provider_uid)
        .execute(pool)
        .await?;

        Ok(actual_user_id)
    }

    async fn generate_username(pool: &SqlitePool, hint: &str) -> Result<String, sqlx::Error> {
        let base: String = hint
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .take(32)
            .collect();
        let base = if base.is_empty() {
            "user".to_string()
        } else {
            base
        };

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
                .bind(&base)
                .fetch_one(pool)
                .await?;

        if count == 0 {
            return Ok(base);
        }

        for i in 1..=99 {
            let candidate = format!("{}_{}", base, i);
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ?")
                    .bind(&candidate)
                    .fetch_one(pool)
                    .await?;
            if count == 0 {
                return Ok(candidate);
            }
        }

        Ok(format!("{}_{}", base, Uuid::new_v4().simple()))
    }
}
