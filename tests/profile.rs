use sqlx::SqlitePool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[sqlx::test(migrator = "MIGRATOR")]
async fn rate_album_upserts_release_group(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO spotify_albums (spotify_id, title, artists, album_type, release_date, raw_json) \
         VALUES ('sp1', 'Test Album', '[\"Artist A\"]', 'album', '2020-01-01', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Replicate the rate_album logic: upsert release_group
    sqlx::query(
        "INSERT OR IGNORE INTO release_groups (release_group_id, title, primary_type, first_release_year, spotify_id) \
         SELECT 'rg1', title, album_type, CAST(SUBSTR(release_date, 1, 4) AS INTEGER), spotify_id \
         FROM spotify_albums WHERE spotify_id = 'sp1'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let rg_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM release_groups WHERE spotify_id = 'sp1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rg_count, 1, "exactly one release_groups row should exist");

    let year: Option<i64> = sqlx::query_scalar(
        "SELECT first_release_year FROM release_groups WHERE spotify_id = 'sp1'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(year, Some(2020));

    sqlx::query(
        "INSERT INTO ratings (rating_id, user_id, release_group_id, rating) VALUES ('r1', 'u1', 'rg1', 8)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let rating: i64 = sqlx::query_scalar(
        "SELECT rating FROM ratings WHERE user_id = 'u1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rating, 8);
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn rating_update_on_duplicate(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO spotify_albums (spotify_id, title, artists, album_type, release_date, raw_json) \
         VALUES ('sp1', 'Test Album', '[\"Artist A\"]', 'album', '2020', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT OR IGNORE INTO release_groups (release_group_id, title, primary_type, first_release_year, spotify_id) \
         SELECT 'rg1', title, album_type, CAST(SUBSTR(release_date, 1, 4) AS INTEGER), spotify_id \
         FROM spotify_albums WHERE spotify_id = 'sp1'",
    )
    .execute(&pool)
    .await
    .unwrap();

    // First rating
    sqlx::query(
        "INSERT INTO ratings (rating_id, user_id, release_group_id, rating) VALUES ('r1', 'u1', 'rg1', 7) \
         ON CONFLICT(user_id, release_group_id) DO UPDATE SET rating = excluded.rating, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Second rating (upsert)
    sqlx::query(
        "INSERT INTO ratings (rating_id, user_id, release_group_id, rating) VALUES ('r2', 'u1', 'rg1', 9) \
         ON CONFLICT(user_id, release_group_id) DO UPDATE SET rating = excluded.rating, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ratings WHERE user_id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "only one rating row should exist after upsert");

    let rating: i64 = sqlx::query_scalar("SELECT rating FROM ratings WHERE user_id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rating, 9, "rating should be updated to 9");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn follow_and_unfollow(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u2', 'bob', 'b@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Follow
    sqlx::query("INSERT OR IGNORE INTO follows (follower_id, followee_id) SELECT 'u1', user_id FROM users WHERE username = 'bob'")
        .execute(&pool)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM follows WHERE follower_id = 'u1' AND followee_id = 'u2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "follow row should exist");

    // Unfollow
    sqlx::query("DELETE FROM follows WHERE follower_id = 'u1' AND followee_id = (SELECT user_id FROM users WHERE username = 'bob')")
        .execute(&pool)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM follows WHERE follower_id = 'u1' AND followee_id = 'u2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "follow row should be gone after unfollow");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn no_self_follow_db_constraint(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = sqlx::query("INSERT INTO follows (follower_id, followee_id) VALUES ('u1', 'u1')")
        .execute(&pool)
        .await;

    assert!(result.is_err(), "self-follow should be rejected by DB constraint");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn update_profile_rejects_taken_username(pool: SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u2', 'bob', 'b@x.com', '')",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Simulate the uniqueness check in update_profile: count users with target username excluding self
    let taken: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE username = ? AND user_id != ?",
    )
    .bind("bob")     // alice wants to take bob's username
    .bind("u1")      // alice's user_id
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(taken, 1, "should detect that 'bob' is already taken by another user");

    // Also verify that a direct UPDATE would fail the UNIQUE constraint
    let result = sqlx::query("UPDATE users SET username = 'bob' WHERE user_id = 'u1'")
        .execute(&pool)
        .await;
    assert!(result.is_err(), "direct update to taken username should fail");
}
