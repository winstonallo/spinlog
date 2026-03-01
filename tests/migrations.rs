use sqlx::{Row, SqlitePool};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[sqlx::test(migrator = "MIGRATOR")]
async fn all_tables_exist(pool: SqlitePool) {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    for expected in &[
        "follows",
        "oauth_accounts",
        "oauth_states",
        "ratings",
        "release_group_external_ids",
        "release_groups",
        "sessions",
        "users",
    ] {
        assert!(
            tables.iter().any(|t| t == expected),
            "missing table: {expected}"
        );
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn users_insert_with_empty_password_hash(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES (?, ?, ?, '')",
    )
    .bind("u1")
    .bind("alice")
    .bind("alice@example.com")
    .execute(&pool)
    .await
    .expect("should insert user with empty password_hash");

    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE user_id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(email, "alice@example.com");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn users_username_unique(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u2', 'alice', 'b@x.com', '')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err(), "duplicate username should be rejected");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn users_email_unique(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'same@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u2', 'bob', 'same@x.com', '')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err(), "duplicate email should be rejected");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn oauth_accounts_links_to_user(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO oauth_accounts (user_id, provider, provider_uid) VALUES ('u1', 'google', 'g-123')",
    )
    .execute(&pool)
    .await
    .expect("should insert oauth account");

    let uid: String = sqlx::query_scalar(
        "SELECT user_id FROM oauth_accounts WHERE provider = 'google' AND provider_uid = 'g-123'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(uid, "u1");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn oauth_accounts_cascades_on_user_delete(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_accounts (user_id, provider, provider_uid) VALUES ('u1', 'google', 'g-123')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM users WHERE user_id = 'u1'")
        .execute(&pool)
        .await
        .unwrap();

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM oauth_accounts WHERE provider_uid = 'g-123'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0, "oauth_account should cascade-delete with user");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn oauth_accounts_provider_uid_unique_per_provider(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_accounts (user_id, provider, provider_uid) VALUES ('u1', 'google', 'g-123')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = sqlx::query(
        "INSERT INTO oauth_accounts (user_id, provider, provider_uid) VALUES ('u1', 'google', 'g-123')",
    )
    .execute(&pool)
    .await;

    assert!(result.is_err(), "duplicate (provider, provider_uid) should fail");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn oauth_states_insert_and_retrieve(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO oauth_states (csrf_token, pkce_verifier) VALUES ('csrf-abc', 'pkce-xyz')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let pkce: String = sqlx::query_scalar(
        "SELECT pkce_verifier FROM oauth_states \
         WHERE csrf_token = 'csrf-abc' \
         AND created_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-10 minutes')",
    )
    .fetch_one(&pool)
    .await
    .expect("recently inserted state should be found within TTL");

    assert_eq!(pkce, "pkce-xyz");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn oauth_states_expired_not_returned(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO oauth_states (csrf_token, pkce_verifier, created_at) \
         VALUES ('csrf-old', 'pkce-xyz', strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-11 minutes'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let row = sqlx::query_scalar::<_, String>(
        "SELECT pkce_verifier FROM oauth_states \
         WHERE csrf_token = 'csrf-old' \
         AND created_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-10 minutes')",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(row.is_none(), "state older than 10 minutes should not be returned");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn sessions_valid_session_returned(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO sessions (session_id, user_id, expires_at) \
         VALUES ('sess-1', 'u1', strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '+30 days'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let row = sqlx::query(
        "SELECT u.user_id, u.username \
         FROM sessions s JOIN users u ON s.user_id = u.user_id \
         WHERE s.session_id = 'sess-1' \
         AND s.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .fetch_optional(&pool)
    .await
    .unwrap()
    .expect("valid session should be found");

    assert_eq!(row.get::<String, _>("username"), "alice");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn sessions_expired_not_returned(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO sessions (session_id, user_id, expires_at) \
         VALUES ('sess-old', 'u1', strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 day'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    let row = sqlx::query(
        "SELECT u.user_id FROM sessions s JOIN users u ON s.user_id = u.user_id \
         WHERE s.session_id = 'sess-old' \
         AND s.expires_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(row.is_none(), "expired session should not be returned");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn sessions_cascade_delete_with_user(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sessions (session_id, user_id, expires_at) \
         VALUES ('sess-1', 'u1', strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '+30 days'))",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM users WHERE user_id = 'u1'")
        .execute(&pool)
        .await
        .unwrap();

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE session_id = 'sess-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0, "session should cascade-delete with user");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn follows_no_self_follow(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result =
        sqlx::query("INSERT INTO follows (follower_id, followee_id) VALUES ('u1', 'u1')")
            .execute(&pool)
            .await;

    assert!(result.is_err(), "self-follow should be rejected");
}
