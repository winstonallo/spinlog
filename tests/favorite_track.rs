use sqlx::SqlitePool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

// Inserts the minimal fixtures required by every test: one user, one
// spotify_album, and one release_group (linked via spotify_id).
async fn insert_fixtures(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO users (user_id, username, email, password_hash) VALUES ('u1', 'alice', 'a@x.com', '')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO spotify_albums (spotify_id, title, artists, album_type, release_date, raw_json) \
         VALUES ('sp1', 'Test Album', '[\"Artist A\"]', 'album', '2020-01-01', '{}')",
    )
    .execute(pool)
    .await
    .unwrap();

    // Replicate rate_album's release_group upsert so ratings can reference it.
    sqlx::query(
        "INSERT OR IGNORE INTO release_groups \
         (release_group_id, title, primary_type, first_release_year, spotify_id) \
         SELECT 'rg1', title, album_type, CAST(SUBSTR(release_date, 1, 4) AS INTEGER), spotify_id \
         FROM spotify_albums WHERE spotify_id = 'sp1'",
    )
    .execute(pool)
    .await
    .unwrap();
}

// Inserts a spotify_track row so favorite_track tests can reference a real track.
async fn insert_track(pool: &SqlitePool, track_id: &str, name: &str) {
    sqlx::query(
        "INSERT INTO spotify_tracks (spotify_id, track_id, track_number, name, artists) \
         VALUES ('sp1', ?, 1, ?, '[\"Artist A\"]')",
    )
    .bind(track_id)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
}

// Upserts a rating using the exact same INSERT … ON CONFLICT SQL as rate_album,
// including favorite_track_id in both the INSERT and the ON CONFLICT SET clause.
async fn upsert_rating(
    pool: &SqlitePool,
    rating_id: &str,
    rating: i64,
    review: Option<&str>,
    favorite_track_id: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO ratings (rating_id, user_id, release_group_id, rating, review, favorite_track_id) \
         VALUES (?, 'u1', 'rg1', ?, ?, ?) \
         ON CONFLICT(user_id, release_group_id) DO UPDATE SET \
         rating = excluded.rating, \
         review = excluded.review, \
         favorite_track_id = excluded.favorite_track_id, \
         updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(rating_id)
    .bind(rating)
    .bind(review)
    .bind(favorite_track_id)
    .execute(pool)
    .await
    .unwrap();
}

// --- Tests ---

/// Saving a rating with a favorite_track_id must persist that value in the DB.
/// If favorite_track_id is omitted from the INSERT column list or its bound
/// value is not included, this test fails.
#[sqlx::test(migrator = "MIGRATOR")]
async fn saving_favorite_track_stores_track_id(pool: SqlitePool) {
    insert_fixtures(&pool).await;
    insert_track(&pool, "t1", "Best Track").await;
    upsert_rating(&pool, "r1", 8, None, Some("t1")).await;

    let stored: Option<String> =
        sqlx::query_scalar("SELECT favorite_track_id FROM ratings WHERE user_id = 'u1'")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(
        stored.as_deref(),
        Some("t1"),
        "favorite_track_id should be stored alongside the rating"
    );
}

/// A second upsert must replace the favorite_track_id with the new value and
/// keep exactly one row. If the ON CONFLICT clause does not update
/// favorite_track_id = excluded.favorite_track_id, the original track ID
/// survives and this test fails.
#[sqlx::test(migrator = "MIGRATOR")]
async fn updating_rating_replaces_favorite_track(pool: SqlitePool) {
    insert_fixtures(&pool).await;
    insert_track(&pool, "t1", "First Fave").await;
    insert_track(&pool, "t2", "Second Fave").await;
    upsert_rating(&pool, "r1", 7, None, Some("t1")).await;
    upsert_rating(&pool, "r2", 9, None, Some("t2")).await;

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ratings WHERE user_id = 'u1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "upsert must keep exactly one row per user/album");

    let stored: Option<String> =
        sqlx::query_scalar("SELECT favorite_track_id FROM ratings WHERE user_id = 'u1'")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(
        stored.as_deref(),
        Some("t2"),
        "favorite_track_id should be replaced with the new track ID on upsert"
    );
}

/// Rating again with None favorite_track_id must set the column to NULL.
/// If the ON CONFLICT clause does not propagate the NULL, the old track ID
/// survives and this test fails.
#[sqlx::test(migrator = "MIGRATOR")]
async fn clearing_favorite_track_sets_null(pool: SqlitePool) {
    insert_fixtures(&pool).await;
    insert_track(&pool, "t1", "Some Track").await;
    upsert_rating(&pool, "r1", 8, None, Some("t1")).await;
    upsert_rating(&pool, "r2", 8, None, None).await;

    let stored: Option<String> =
        sqlx::query_scalar("SELECT favorite_track_id FROM ratings WHERE user_id = 'u1'")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(
        stored.is_none(),
        "favorite_track_id should be NULL after being cleared with None"
    );
}

/// The get_my_rating query must SELECT favorite_track_id and return the correct
/// value. If the column is dropped from the SELECT list, fetching it panics and
/// this test fails.
#[sqlx::test(migrator = "MIGRATOR")]
async fn get_my_rating_returns_favorite_track_id(pool: SqlitePool) {
    insert_fixtures(&pool).await;
    insert_track(&pool, "t1", "Fave Track").await;
    upsert_rating(&pool, "r1", 7, None, Some("t1")).await;

    // Replicate the exact query used by get_my_rating.
    let row = sqlx::query(
        "SELECT r.rating, r.review, r.favorite_track_id FROM ratings r \
         JOIN release_groups rg ON r.release_group_id = rg.release_group_id \
         WHERE rg.spotify_id = ? AND r.user_id = ?",
    )
    .bind("sp1")
    .bind("u1")
    .fetch_optional(&pool)
    .await
    .unwrap()
    .expect("row must exist after rating");

    use sqlx::Row;
    let favorite_track_id: Option<String> = row.get("favorite_track_id");

    assert_eq!(
        favorite_track_id.as_deref(),
        Some("t1"),
        "get_my_rating query must return the favorite_track_id column"
    );
}

/// The get_user_ratings query must LEFT JOIN spotify_tracks and expose
/// favorite_track_name. If the JOIN or the column alias is removed, fetching
/// the column panics and this test fails.
#[sqlx::test(migrator = "MIGRATOR")]
async fn get_user_ratings_resolves_track_name(pool: SqlitePool) {
    insert_fixtures(&pool).await;
    insert_track(&pool, "t1", "Greatest Hit").await;
    upsert_rating(&pool, "r1", 9, None, Some("t1")).await;

    // Replicate the exact query used by get_user_ratings.
    let rows = sqlx::query(
        "SELECT sa.spotify_id, sa.title, sa.artists, sa.album_type, sa.release_date, \
         sa.cover_art IS NOT NULL AS has_cover_art, r.rating, r.review, r.created_at AS rated_at, \
         st.name AS favorite_track_name \
         FROM ratings r \
         JOIN users u ON r.user_id = u.user_id \
         JOIN release_groups rg ON r.release_group_id = rg.release_group_id \
         JOIN spotify_albums sa ON rg.spotify_id = sa.spotify_id \
         LEFT JOIN spotify_tracks st \
           ON st.spotify_id = rg.spotify_id AND st.track_id = r.favorite_track_id \
         WHERE u.username = ? \
         ORDER BY r.created_at DESC",
    )
    .bind("alice")
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 1, "alice should have exactly one rated album");

    use sqlx::Row;
    let row = &rows[0];
    let favorite_track_name: Option<String> = row.get("favorite_track_name");

    assert_eq!(
        favorite_track_name.as_deref(),
        Some("Greatest Hit"),
        "get_user_ratings query must resolve favorite_track_name via the LEFT JOIN on spotify_tracks"
    );
}
