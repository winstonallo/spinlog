use sqlx::Row;
use sqlx::SqlitePool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Exact SQL used by the `get_followers` server function.
/// Parameters: (target_username, viewer_id, query, like_pattern, offset).
const FOLLOWERS_SQL: &str = "\
    SELECT u.user_id, u.username, u.bio, \
           (SELECT COUNT(*) FROM follows WHERE followee_id = u.user_id) AS follower_count, \
           CASE WHEN vf.follower_id IS NOT NULL THEN 1 ELSE 0 END AS is_following \
    FROM users u \
    INNER JOIN follows f ON f.follower_id = u.user_id \
                         AND f.followee_id = (SELECT user_id FROM users WHERE username = ?) \
    LEFT JOIN follows vf ON vf.follower_id = ? AND vf.followee_id = u.user_id \
    WHERE (? = '' OR u.username LIKE ? ESCAPE '\\') \
    ORDER BY u.username \
    LIMIT 21 OFFSET ?";

/// Exact SQL used by the `get_following` server function.
/// Parameters: (target_username, viewer_id, query, like_pattern, offset).
const FOLLOWING_SQL: &str = "\
    SELECT u.user_id, u.username, u.bio, \
           (SELECT COUNT(*) FROM follows WHERE followee_id = u.user_id) AS follower_count, \
           CASE WHEN vf.follower_id IS NOT NULL THEN 1 ELSE 0 END AS is_following \
    FROM users u \
    INNER JOIN follows f ON f.followee_id = u.user_id \
                         AND f.follower_id = (SELECT user_id FROM users WHERE username = ?) \
    LEFT JOIN follows vf ON vf.follower_id = ? AND vf.followee_id = u.user_id \
    WHERE (? = '' OR u.username LIKE ? ESCAPE '\\') \
    ORDER BY u.username \
    LIMIT 21 OFFSET ?";

async fn insert_user(pool: &SqlitePool, id: &str, username: &str) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES (?, ?, ?, '')",
    )
    .bind(id)
    .bind(username)
    .bind(format!("{id}@x.com"))
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_follow(pool: &SqlitePool, follower_id: &str, followee_id: &str) {
    sqlx::query("INSERT INTO follows (follower_id, followee_id) VALUES (?, ?)")
        .bind(follower_id)
        .bind(followee_id)
        .execute(pool)
        .await
        .unwrap();
}

// ── get_followers ──────────────────────────────────────────────────────────────

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_returns_users_who_follow_target(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "carol").await;
    // bob and carol follow alice
    insert_follow(&pool, "u2", "u1").await;
    insert_follow(&pool, "u3", "u1").await;

    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("alice") // target
        .bind("")      // no viewer
        .bind("")      // no query filter
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    let names: Vec<String> = rows.iter().map(|r| r.get("username")).collect();
    assert_eq!(rows.len(), 2, "alice has two followers");
    assert!(names.contains(&"bob".to_string()));
    assert!(names.contains(&"carol".to_string()));
    assert!(!names.contains(&"alice".to_string()), "target should not appear in own follower list");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_excludes_users_not_following_target(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "carol").await;
    // only bob follows alice
    insert_follow(&pool, "u2", "u1").await;

    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1, "only one follower");
    let name: String = rows[0].get("username");
    assert_eq!(name, "bob");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_query_filter_matches_substring(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "barbara").await;
    // bob and barbara both follow alice
    insert_follow(&pool, "u2", "u1").await;
    insert_follow(&pool, "u3", "u1").await;

    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("alice")
        .bind("")
        .bind("barb")    // query
        .bind("%barb%")  // pattern
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1, "only barbara matches 'barb'");
    let name: String = rows[0].get("username");
    assert_eq!(name, "barbara");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_is_following_reflects_viewer_state(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "carol").await;
    // bob and carol follow alice; viewer (bob) follows carol
    insert_follow(&pool, "u2", "u1").await;
    insert_follow(&pool, "u3", "u1").await;
    insert_follow(&pool, "u2", "u3").await;

    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("alice") // target
        .bind("u2")    // viewer = bob
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 2);
    for row in &rows {
        let username: String = row.get("username");
        let is_following: i64 = row.get("is_following");
        match username.as_str() {
            "carol" => assert_eq!(is_following, 1, "bob follows carol"),
            "bob" => assert_eq!(is_following, 0, "bob cannot follow himself"),
            other => panic!("unexpected user {other}"),
        }
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_has_more_true_when_over_20(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    // Insert 21 users who all follow alice
    for i in 0..21_u32 {
        let uid = format!("u{}", i + 2);
        let uname = format!("follower{i:02}");
        insert_user(&pool, &uid, &uname).await;
        insert_follow(&pool, &uid, "u1").await;
    }

    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64) // page 1
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 21, "query returns 21 rows so caller knows has_more = true");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_has_more_false_when_leq_20(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    for i in 0..5_u32 {
        let uid = format!("u{}", i + 2);
        let uname = format!("follower{i:02}");
        insert_user(&pool, &uid, &uname).await;
        insert_follow(&pool, &uid, "u1").await;
    }

    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert!(rows.len() <= 20, "≤20 rows means has_more = false");
    assert_eq!(rows.len(), 5);
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_empty_for_nonexistent_target(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_follow(&pool, "u2", "u1").await;

    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("nobody") // does not exist → subquery returns NULL → INNER JOIN produces 0 rows
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 0);
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn followers_second_page_returns_remainder(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    for i in 0..21_u32 {
        let uid = format!("u{}", i + 2);
        let uname = format!("follower{i:02}");
        insert_user(&pool, &uid, &uname).await;
        insert_follow(&pool, &uid, "u1").await;
    }

    // Page 2: offset 20, expects 1 row
    let rows = sqlx::query(FOLLOWERS_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(20_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1, "21st follower appears on page 2");
}

// ── get_following ──────────────────────────────────────────────────────────────

#[sqlx::test(migrator = "MIGRATOR")]
async fn following_returns_users_target_follows(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "carol").await;
    // alice follows bob and carol
    insert_follow(&pool, "u1", "u2").await;
    insert_follow(&pool, "u1", "u3").await;

    let rows = sqlx::query(FOLLOWING_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    let names: Vec<String> = rows.iter().map(|r| r.get("username")).collect();
    assert_eq!(rows.len(), 2);
    assert!(names.contains(&"bob".to_string()));
    assert!(names.contains(&"carol".to_string()));
    assert!(!names.contains(&"alice".to_string()), "target should not appear in own following list");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn following_excludes_users_target_does_not_follow(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "carol").await;
    // alice only follows bob
    insert_follow(&pool, "u1", "u2").await;

    let rows = sqlx::query(FOLLOWING_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    let name: String = rows[0].get("username");
    assert_eq!(name, "bob");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn following_query_filter_matches_substring(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "barbara").await;
    insert_follow(&pool, "u1", "u2").await;
    insert_follow(&pool, "u1", "u3").await;

    let rows = sqlx::query(FOLLOWING_SQL)
        .bind("alice")
        .bind("")
        .bind("barb")
        .bind("%barb%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1);
    let name: String = rows[0].get("username");
    assert_eq!(name, "barbara");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn following_is_following_reflects_viewer_state(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_user(&pool, "u3", "carol").await;
    // alice follows bob and carol; viewer (alice) also follows carol
    insert_follow(&pool, "u1", "u2").await;
    insert_follow(&pool, "u1", "u3").await;

    let rows = sqlx::query(FOLLOWING_SQL)
        .bind("alice") // target = alice's following list
        .bind("u1")    // viewer = alice herself
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 2);
    for row in &rows {
        let username: String = row.get("username");
        let is_following: i64 = row.get("is_following");
        // alice follows both bob and carol, so viewer (alice) should see is_following=1 for both
        assert_eq!(is_following, 1, "alice follows {username}");
    }
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn following_has_more_true_when_over_20(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    for i in 0..21_u32 {
        let uid = format!("u{}", i + 2);
        let uname = format!("followed{i:02}");
        insert_user(&pool, &uid, &uname).await;
        insert_follow(&pool, "u1", &uid).await;
    }

    let rows = sqlx::query(FOLLOWING_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 21, "21 rows means has_more = true");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn following_second_page_returns_remainder(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    for i in 0..21_u32 {
        let uid = format!("u{}", i + 2);
        let uname = format!("followed{i:02}");
        insert_user(&pool, &uid, &uname).await;
        insert_follow(&pool, "u1", &uid).await;
    }

    let rows = sqlx::query(FOLLOWING_SQL)
        .bind("alice")
        .bind("")
        .bind("")
        .bind("%")
        .bind(20_i64) // page 2
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 1, "21st followed user appears on page 2");
}

#[sqlx::test(migrator = "MIGRATOR")]
async fn following_empty_for_nonexistent_target(pool: SqlitePool) {
    insert_user(&pool, "u1", "alice").await;
    insert_user(&pool, "u2", "bob").await;
    insert_follow(&pool, "u1", "u2").await;

    let rows = sqlx::query(FOLLOWING_SQL)
        .bind("nobody")
        .bind("")
        .bind("")
        .bind("%")
        .bind(0_i64)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(rows.len(), 0);
}
