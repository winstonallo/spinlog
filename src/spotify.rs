#![cfg(feature = "ssr")]

use crate::app::{AlbumDetail, SearchPage, SpotifyAlbum, Track};
use leptos::server_fn::error::ServerFnError;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use sqlx::{Row, SqlitePool};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

// Internal structs for deserializing Spotify API responses.

#[derive(Deserialize)]
struct SpotifySearchResponse {
    albums: SpotifyAlbumPage,
}

#[derive(Deserialize)]
struct SpotifyAlbumPage {
    items: Vec<SpotifyApiAlbum>,
    total: u32,
}

#[derive(Deserialize)]
struct SpotifyApiAlbum {
    id: String,
    name: String,
    artists: Vec<SpotifyApiArtist>,
    album_type: String,
    release_date: Option<String>,
    images: Vec<SpotifyImage>,
    // Present when fetching a single album; absent in search results.
    tracks: Option<SpotifyTrackPage>,
}

#[derive(Deserialize)]
struct SpotifyApiArtist {
    name: String,
}

#[derive(Deserialize)]
struct SpotifyTrackPage {
    items: Vec<SpotifyApiTrack>,
}

#[derive(Deserialize)]
struct SpotifyApiTrack {
    id: String,
    name: String,
    artists: Vec<SpotifyApiArtist>,
    disc_number: u32,
    track_number: u32,
    duration_ms: Option<u32>,
}

#[derive(Deserialize)]
struct SpotifyImage {
    url: String,
    width: Option<u32>,
}

// Internal struct for deserializing the Spotify token endpoint response.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

// Holds a cached bearer token with its expiry instant so we can avoid re-fetching
// on every request while still rotating before it actually expires.
struct CachedToken {
    token: String,
    expires_at: Instant,
}

// Database row shape matching the spotify_albums table columns we actually
// need when converting to SpotifyAlbum. cover_art_url is queried separately
// (via query_scalar) in refresh_album, so it is not included here.
struct DbSpotifyAlbum {
    spotify_id: String,
    title: String,
    artists: String, // JSON array of display-name strings
    album_type: Option<String>,
    release_date: Option<String>,
    cover_art: Option<Vec<u8>>,
}

/// Client for the Spotify Web API. Holds credentials, a shared HTTP client,
/// and a token cache so a single instance can be shared across the app via
/// Axum Extension without redundant token fetches.
///
/// All fields except the token cache are immutable after construction, so
/// deriving Clone is safe and cheap (Arc makes the Mutex clone O(1)).
#[derive(Clone)]
pub struct SpotifyClient {
    client_id: String,
    client_secret: String,
    http: reqwest::Client,
    token_cache: Arc<Mutex<Option<CachedToken>>>,
}

impl SpotifyClient {
    /// Returns a client with empty credentials for use in tests that only
    /// exercise the DB cache path. Any call that reaches the Spotify API will
    /// fail with an auth error, so tests must pre-seed the cache to avoid
    /// making real network requests.
    #[cfg(test)]
    pub fn new_test() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            http: reqwest::Client::new(),
            token_cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns a no-op client used when Spotify credentials are absent.
    /// Every method on this client will return a descriptive error rather than
    /// panicking, so the server starts up gracefully and surfaces a clear message
    /// to users instead of crashing.
    pub fn unconfigured() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            http: reqwest::Client::new(),
            token_cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Constructs a client from `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET`
    /// environment variables. A `User-Agent` default header is baked in so every
    /// request identifies the app to Spotify's servers.
    pub fn from_env() -> Result<Self, String> {
        let client_id = std::env::var("SPOTIFY_CLIENT_ID")
            .map_err(|_| "SPOTIFY_CLIENT_ID not set".to_string())?;
        let client_secret = std::env::var("SPOTIFY_CLIENT_SECRET")
            .map_err(|_| "SPOTIFY_CLIENT_SECRET not set".to_string())?;

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            USER_AGENT,
            HeaderValue::from_static("musicboxd/0.1"),
        );

        let http = reqwest::Client::builder()
            .default_headers(default_headers)
            .build()
            .map_err(|e| format!("failed to build HTTP client: {e}"))?;

        Ok(Self {
            client_id,
            client_secret,
            http,
            token_cache: Arc::new(Mutex::new(None)),
        })
    }

    /// Returns a valid bearer token, fetching a new one from Spotify's token
    /// endpoint only when the cached one has less than 60 seconds of lifetime
    /// remaining. The 60-second buffer prevents races where a token expires
    /// between retrieval and use.
    pub async fn token(&self) -> Result<String, ServerFnError> {
        let mut cache = self.token_cache.lock().await;

        if let Some(ref cached) = *cache {
            if cached.expires_at > Instant::now() + Duration::from_secs(60) {
                return Ok(cached.token.clone());
            }
        }

        let response = self
            .http
            .post("https://accounts.spotify.com/api/token")
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .map_err(|e| {
                eprintln!("spotify token request error: {e}");
                ServerFnError::new("internal server error")
            })?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let secs = retry_after_secs(&response);
            eprintln!("spotify token rate limited — retry after {secs}s");
            return Err(ServerFnError::new("internal server error"));
        }

        if !response.status().is_success() {
            let status = response.status();
            eprintln!("spotify token endpoint: HTTP {status}");
            return Err(ServerFnError::new("internal server error"));
        }

        let body: TokenResponse = response
            .json()
            .await
            .map_err(|e| {
                eprintln!("spotify token parse error: {e}");
                ServerFnError::new("internal server error")
            })?;

        let expires_at = Instant::now() + Duration::from_secs(body.expires_in.saturating_sub(60));
        *cache = Some(CachedToken {
            token: body.access_token.clone(),
            expires_at,
        });

        Ok(body.access_token)
    }

    /// Cache-first album search. Normalises the query, checks the
    /// `spotify_search_cache` table (24-hour TTL) for the requested page's
    /// offset, and falls back to the Spotify API when the cache misses. Results
    /// are upserted into `spotify_albums`; every 100th hit triggers a background
    /// metadata refresh, and cover art is fetched in the background for any
    /// album that doesn't have it yet.
    ///
    /// Each page is cached independently by `(query, offset)`, so navigating to
    /// page 3 triggers an API call with `offset=20` — we never need to fetch all
    /// results upfront. Spotify's `total` field is stored in each cache row so
    /// the UI can compute the page count from any cached page.
    pub async fn search(
        &self,
        pool: &SqlitePool,
        query: &str,
        page: u32,
    ) -> Result<SearchPage, ServerFnError> {
        const PAGE_SIZE: u32 = 10;

        let normalized = query
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        let offset = page.saturating_sub(1) * PAGE_SIZE;

        // Cache hit: load IDs and total from spotify_search_cache for this offset.
        let cache_row = sqlx::query(
            "SELECT spotify_ids, total FROM spotify_search_cache \
             WHERE query = ? AND result_offset = ? \
             AND datetime(cached_at, '+1 day') > datetime('now')",
        )
        .bind(&normalized)
        .bind(offset as i64)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            eprintln!("spotify search cache read DB error: {e}");
            ServerFnError::new("internal server error")
        })?;

        if let Some(row) = cache_row {
            let ids_json: String = row.get("spotify_ids");
            let total: i64 = row.get("total");
            let ids: Vec<String> = serde_json::from_str(&ids_json)
                .map_err(|e| {
                    eprintln!("spotify search cache parse error: {e}");
                    ServerFnError::new("internal server error")
                })?;
            let albums = self.load_albums_by_ids_and_bump(pool, &ids).await?;
            return Ok(SearchPage { albums, total: total as usize });
        }

        // Cache miss: hit the Spotify API with the appropriate offset.
        let (api_albums, spotify_total) = self.search_api(&normalized, offset).await?;

        let mut result_ids: Vec<String> = Vec::with_capacity(api_albums.len());
        let mut albums: Vec<SpotifyAlbum> = Vec::with_capacity(api_albums.len());

        for api_album in api_albums {
            let spotify_id = api_album.id.clone();
            let album = spotify_album_from_api(&api_album);

            if relevance_score(&normalized, &album.title, &album.artists) < 0.5 {
                continue;
            }

            let cover_art_url = best_image_url(&api_album.images);
            let raw_json = serde_json::to_string(&AlbumRaw {
                id: &api_album.id,
                name: &api_album.name,
            })
            .unwrap_or_default();
            let artists_json = serde_json::to_string(
                &api_album
                    .artists
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| {
                eprintln!("spotify search artists serialize error: {e}");
                ServerFnError::new("internal server error")
            })?;

            upsert_album(
                pool,
                &AlbumInsert {
                    spotify_id: &spotify_id,
                    title: &album.title,
                    artists_json: &artists_json,
                    album_type: &album.album_type,
                    release_date: api_album.release_date.as_deref(),
                    cover_art_url: cover_art_url.as_deref(),
                    raw_json: &raw_json,
                },
            )
            .await?;

            // Increment hit count and check whether a background refresh is due.
            let new_count: i64 = sqlx::query_scalar(
                "UPDATE spotify_albums SET search_hit_count = search_hit_count + 1 \
                 WHERE spotify_id = ? \
                 RETURNING search_hit_count",
            )
            .bind(&spotify_id)
            .fetch_one(pool)
            .await
            .map_err(|e| {
                eprintln!("spotify search hit count update DB error: {e}");
                ServerFnError::new("internal server error")
            })?;

            if new_count % 100 == 0 {
                let self_clone = self.clone();
                let pool_clone = pool.clone();
                let id_clone = spotify_id.clone();
                tokio::spawn(async move {
                    self_clone.refresh_album(pool_clone, id_clone).await;
                });
            }

            // Spawn cover-art fetch in the background for albums without art yet.
            if !album.has_cover_art {
                if let Some(url) = cover_art_url {
                    let self_clone = self.clone();
                    let pool_clone = pool.clone();
                    let id_clone = spotify_id.clone();
                    tokio::spawn(async move {
                        self_clone
                            .fetch_and_store_cover_art(pool_clone, id_clone, url)
                            .await;
                    });
                }
            }

            result_ids.push(spotify_id);
            albums.push(album);
        }

        // Cache this page by (query, offset).
        let ids_json = serde_json::to_string(&result_ids)
            .map_err(|e| {
                eprintln!("spotify search ids serialize error: {e}");
                ServerFnError::new("internal server error")
            })?;
        sqlx::query(
            "INSERT OR REPLACE INTO spotify_search_cache \
             (query, result_offset, spotify_ids, total, cached_at) \
             VALUES (?, ?, ?, ?, datetime('now'))",
        )
        .bind(&normalized)
        .bind(offset as i64)
        .bind(&ids_json)
        .bind(spotify_total as i64)
        .execute(pool)
        .await
        .map_err(|e| {
            eprintln!("spotify search cache write DB error: {e}");
            ServerFnError::new("internal server error")
        })?;

        Ok(SearchPage {
            albums,
            total: spotify_total as usize,
        })
    }

    /// Fetches a single album by Spotify ID, checking the local DB first.
    /// Falls back to the Spotify API and upserts the result when not cached.
    pub async fn get_album(
        &self,
        pool: &SqlitePool,
        spotify_id: &str,
    ) -> Result<SpotifyAlbum, ServerFnError> {
        // Check DB first.
        if let Some(db_row) = fetch_db_album(pool, spotify_id).await? {
            return Ok(spotify_album_from_db(&db_row));
        }

        let token = self.token().await?;

        let response = self
            .http
            .get(format!(
                "https://api.spotify.com/v1/albums/{}",
                spotify_id
            ))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| {
                eprintln!("spotify get album request error: {e}");
                ServerFnError::new("internal server error")
            })?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let secs = retry_after_secs(&response);
            eprintln!("spotify get album rate limited — retry after {secs}s");
            return Err(ServerFnError::new("internal server error"));
        }

        if !response.status().is_success() {
            let status = response.status();
            eprintln!("spotify get album failed: HTTP {status}");
            return Err(ServerFnError::new("internal server error"));
        }

        let api_album: SpotifyApiAlbum = response
            .json()
            .await
            .map_err(|e| {
                eprintln!("spotify get album parse error: {e}");
                ServerFnError::new("internal server error")
            })?;

        let album = spotify_album_from_api(&api_album);
        let cover_art_url = best_image_url(&api_album.images);
        let artists_json = serde_json::to_string(
            &api_album
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| {
            eprintln!("spotify get album artists serialize error: {e}");
            ServerFnError::new("internal server error")
        })?;
        let raw_json = serde_json::to_string(&AlbumRaw {
            id: &api_album.id,
            name: &api_album.name,
        })
        .unwrap_or_default();

        upsert_album(
            pool,
            &AlbumInsert {
                spotify_id: &api_album.id,
                title: &album.title,
                artists_json: &artists_json,
                album_type: &album.album_type,
                release_date: api_album.release_date.as_deref(),
                cover_art_url: cover_art_url.as_deref(),
                raw_json: &raw_json,
            },
        )
        .await?;

        if !album.has_cover_art {
            if let Some(url) = cover_art_url {
                let self_clone = self.clone();
                let pool_clone = pool.clone();
                let id_clone = api_album.id.clone();
                tokio::spawn(async move {
                    self_clone
                        .fetch_and_store_cover_art(pool_clone, id_clone, url)
                        .await;
                });
            }
        }

        Ok(album)
    }

    /// Cache-first album detail fetch: returns metadata and the full track listing.
    /// Checks `spotify_tracks` first; if rows exist the method skips the Spotify
    /// API entirely. On a miss it calls `GET /v1/albums/{id}`, upserts metadata,
    /// inserts tracks, and returns both. Only the first page of tracks (≤50) is
    /// stored, which covers virtually all albums.
    pub async fn get_album_detail(
        &self,
        pool: &SqlitePool,
        spotify_id: &str,
    ) -> Result<AlbumDetail, ServerFnError> {
        if let Some(tracks) = self.load_cached_tracks(pool, spotify_id).await? {
            let album = fetch_db_album(pool, spotify_id)
                .await?
                .map(|r| spotify_album_from_db(&r))
                .ok_or_else(|| {
                    eprintln!("spotify_albums row missing for {spotify_id} despite cached tracks");
                    ServerFnError::new("internal server error")
                })?;
            return Ok(AlbumDetail { album, tracks });
        }

        let token = self.token().await?;

        let response = self
            .http
            .get(format!("https://api.spotify.com/v1/albums/{}", spotify_id))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| {
                eprintln!("spotify album detail request error: {e}");
                ServerFnError::new("internal server error")
            })?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let secs = retry_after_secs(&response);
            eprintln!("spotify album detail rate limited — retry after {secs}s");
            return Err(ServerFnError::new("internal server error"));
        }

        if !response.status().is_success() {
            let status = response.status();
            eprintln!("spotify album detail failed: HTTP {status}");
            return Err(ServerFnError::new("internal server error"));
        }

        let api_album: SpotifyApiAlbum = response
            .json()
            .await
            .map_err(|e| {
                eprintln!("spotify album detail parse error: {e}");
                ServerFnError::new("internal server error")
            })?;

        let album = spotify_album_from_api(&api_album);
        let cover_art_url = best_image_url(&api_album.images);
        let artists_json = serde_json::to_string(
            &api_album
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
        )
        .map_err(|e| {
            eprintln!("spotify album detail artists serialize error: {e}");
            ServerFnError::new("internal server error")
        })?;
        let raw_json = serde_json::to_string(&AlbumRaw {
            id: &api_album.id,
            name: &api_album.name,
        })
        .unwrap_or_default();

        upsert_album(
            pool,
            &AlbumInsert {
                spotify_id: &api_album.id,
                title: &album.title,
                artists_json: &artists_json,
                album_type: &album.album_type,
                release_date: api_album.release_date.as_deref(),
                cover_art_url: cover_art_url.as_deref(),
                raw_json: &raw_json,
            },
        )
        .await?;

        if !album.has_cover_art {
            if let Some(url) = cover_art_url {
                let self_clone = self.clone();
                let pool_clone = pool.clone();
                let id_clone = api_album.id.clone();
                tokio::spawn(async move {
                    self_clone
                        .fetch_and_store_cover_art(pool_clone, id_clone, url)
                        .await;
                });
            }
        }

        let api_tracks = api_album.tracks.map(|p| p.items).unwrap_or_default();
        let mut tracks: Vec<Track> = Vec::with_capacity(api_tracks.len());

        for t in &api_tracks {
            let track_artists_json = serde_json::to_string(
                &t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            )
            .map_err(|e| {
                eprintln!("spotify track artists serialize error: {e}");
                ServerFnError::new("internal server error")
            })?;

            sqlx::query(
                "INSERT OR IGNORE INTO spotify_tracks \
                 (spotify_id, track_id, disc_number, track_number, name, artists, duration_ms) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(spotify_id)
            .bind(&t.id)
            .bind(t.disc_number)
            .bind(t.track_number)
            .bind(&t.name)
            .bind(&track_artists_json)
            .bind(t.duration_ms)
            .execute(pool)
            .await
            .map_err(|e| {
                eprintln!("spotify track insert DB error: {e}");
                ServerFnError::new("internal server error")
            })?;

            tracks.push(track_from_api(t));
        }

        Ok(AlbumDetail { album, tracks })
    }

    /// Loads cached tracks from `spotify_tracks` ordered by disc and track
    /// number. Returns `None` on a cache miss (no rows for this album), which
    /// the caller uses to decide whether to hit the Spotify API. An empty result
    /// set is treated as a miss because real albums always have at least one track.
    async fn load_cached_tracks(
        &self,
        pool: &SqlitePool,
        spotify_id: &str,
    ) -> Result<Option<Vec<Track>>, ServerFnError> {
        let rows = sqlx::query(
            "SELECT track_id, disc_number, track_number, name, artists, duration_ms \
             FROM spotify_tracks WHERE spotify_id = ? \
             ORDER BY disc_number, track_number",
        )
        .bind(spotify_id)
        .fetch_all(pool)
        .await
        .map_err(|e| {
            eprintln!("spotify tracks DB fetch error: {e}");
            ServerFnError::new("internal server error")
        })?;

        if rows.is_empty() {
            return Ok(None);
        }

        let tracks = rows
            .iter()
            .map(|r| {
                let artists_json: String = r.get("artists");
                let artists: Vec<String> =
                    serde_json::from_str(&artists_json).unwrap_or_default();
                Track {
                    track_id: r.get("track_id"),
                    disc_number: r.get::<i64, _>("disc_number") as u32,
                    track_number: r.get::<i64, _>("track_number") as u32,
                    name: r.get("name"),
                    artists,
                    duration_ms: r.get::<Option<i64>, _>("duration_ms").map(|v| v as u32),
                }
            })
            .collect();

        Ok(Some(tracks))
    }

    /// Calls the Spotify search API with a specific offset and returns the items
    /// together with the total result count Spotify reports for the query.
    /// Limit is fixed at 10 (the current Spotify maximum as of February 2026).
    /// 429 and non-2xx responses are turned into descriptive errors.
    async fn search_api(
        &self,
        query: &str,
        offset: u32,
    ) -> Result<(Vec<SpotifyApiAlbum>, u32), ServerFnError> {
        let token = self.token().await?;
        let offset_str = offset.to_string();

        let response = self
            .http
            .get("https://api.spotify.com/v1/search")
            .query(&[
                ("q", query),
                ("type", "album"),
                ("limit", "10"),
                ("offset", &offset_str),
            ])
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| {
                eprintln!("spotify search request error: {e}");
                ServerFnError::new("internal server error")
            })?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let secs = retry_after_secs(&response);
            eprintln!("spotify search rate limited — retry after {secs}s");
            return Err(ServerFnError::new("internal server error"));
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            eprintln!("spotify search failed: HTTP {status} — {body}");
            return Err(ServerFnError::new("internal server error"));
        }

        let body: SpotifySearchResponse = response
            .json()
            .await
            .map_err(|e| {
                eprintln!("spotify search parse error: {e}");
                ServerFnError::new("internal server error")
            })?;

        Ok((body.albums.items, body.albums.total))
    }

    /// Downloads cover art bytes from a public Spotify CDN URL and stores them
    /// in the `spotify_albums.cover_art` column. This is fire-and-forget: the
    /// caller spawns it with `tokio::spawn` and does not await the result.
    /// Errors are logged to stderr rather than propagated, because a missing
    /// thumbnail should never block the user-facing response.
    async fn fetch_and_store_cover_art(&self, pool: SqlitePool, spotify_id: String, url: String) {
        let response = match self.http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("spotify cover art fetch error for {spotify_id}: {e}");
                return;
            }
        };

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let secs = retry_after_secs(&response);
            eprintln!("spotify cover art rate limited for {spotify_id}, retry after {secs}s");
            return;
        }

        if !response.status().is_success() {
            let status = response.status();
            eprintln!("spotify cover art HTTP {status} for {spotify_id}");
            return;
        }

        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("spotify cover art read error for {spotify_id}: {e}");
                return;
            }
        };

        if let Err(e) = sqlx::query(
            "UPDATE spotify_albums SET cover_art = ? WHERE spotify_id = ?",
        )
        .bind(bytes.as_ref())
        .bind(&spotify_id)
        .execute(&pool)
        .await
        {
            eprintln!("spotify cover art store error for {spotify_id}: {e}");
        }
    }

    /// Re-fetches album metadata from the Spotify API and updates all non-count
    /// fields in `spotify_albums`. If the cover art URL has changed, re-fetches
    /// the image too. Called in the background every 100 search hits to keep
    /// cached metadata reasonably fresh without adding latency to searches.
    async fn refresh_album(&self, pool: SqlitePool, spotify_id: String) {
        let token = match self.token().await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("spotify refresh token error for {spotify_id}: {e}");
                return;
            }
        };

        let response = match self
            .http
            .get(format!(
                "https://api.spotify.com/v1/albums/{}",
                spotify_id
            ))
            .bearer_auth(&token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("spotify refresh request error for {spotify_id}: {e}");
                return;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            eprintln!("spotify refresh HTTP {status} for {spotify_id}");
            return;
        }

        let api_album: SpotifyApiAlbum = match response.json().await {
            Ok(a) => a,
            Err(e) => {
                eprintln!("spotify refresh parse error for {spotify_id}: {e}");
                return;
            }
        };

        let new_cover_url = best_image_url(&api_album.images);
        let artists_json = match serde_json::to_string(
            &api_album
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
        ) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("spotify refresh artists serialize error for {spotify_id}: {e}");
                return;
            }
        };
        let raw_json = serde_json::to_string(&AlbumRaw {
            id: &api_album.id,
            name: &api_album.name,
        })
        .unwrap_or_default();

        // Read the old cover_art_url so we know if it changed.
        let old_cover_url: Option<String> = sqlx::query_scalar(
            "SELECT cover_art_url FROM spotify_albums WHERE spotify_id = ?",
        )
        .bind(&spotify_id)
        .fetch_optional(&pool)
        .await
        .unwrap_or(None)
        .flatten();

        if let Err(e) = sqlx::query(
            "UPDATE spotify_albums \
             SET title = ?, artists = ?, album_type = ?, release_date = ?, \
                 cover_art_url = ?, raw_json = ?, cached_at = datetime('now') \
             WHERE spotify_id = ?",
        )
        .bind(&api_album.name)
        .bind(&artists_json)
        .bind(&api_album.album_type)
        .bind(api_album.release_date.as_deref())
        .bind(new_cover_url.as_deref())
        .bind(&raw_json)
        .bind(&spotify_id)
        .execute(&pool)
        .await
        {
            eprintln!("spotify refresh DB update error for {spotify_id}: {e}");
            return;
        }

        // Re-fetch cover art only if the URL changed.
        if new_cover_url.is_some() && new_cover_url != old_cover_url {
            if let Some(url) = new_cover_url {
                self.fetch_and_store_cover_art(pool, spotify_id, url).await;
            }
        }
    }

    /// Loads albums from the `spotify_albums` DB table in the order given by
    /// `ids`, incrementing each album's `search_hit_count` and triggering a
    /// background refresh every 100 hits. IDs missing from the table are
    /// skipped defensively — this should not happen in normal operation.
    ///
    /// We count cache hits too, not just API misses, because the refresh
    /// cadence should reflect how often users actually see an album, not how
    /// often we had to phone Spotify for it.
    async fn load_albums_by_ids_and_bump(
        &self,
        pool: &SqlitePool,
        ids: &[String],
    ) -> Result<Vec<SpotifyAlbum>, ServerFnError> {
        let mut albums = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = fetch_db_album(pool, id).await? {
                let new_count: i64 = sqlx::query_scalar(
                    "UPDATE spotify_albums SET search_hit_count = search_hit_count + 1 \
                     WHERE spotify_id = ? \
                     RETURNING search_hit_count",
                )
                .bind(id)
                .fetch_one(pool)
                .await
                .map_err(|e| {
                    eprintln!("spotify hit count update DB error: {e}");
                    ServerFnError::new("internal server error")
                })?;

                if new_count % 100 == 0 {
                    let self_clone = self.clone();
                    let pool_clone = pool.clone();
                    let id_clone = id.clone();
                    tokio::spawn(async move {
                        self_clone.refresh_album(pool_clone, id_clone).await;
                    });
                }

                albums.push(spotify_album_from_db(&row));
            }
        }
        Ok(albums)
    }
}

// --- DB helpers ---

/// Data needed to insert or update a row in `spotify_albums`.
struct AlbumInsert<'a> {
    spotify_id: &'a str,
    title: &'a str,
    artists_json: &'a str,
    album_type: &'a str,
    release_date: Option<&'a str>,
    cover_art_url: Option<&'a str>,
    raw_json: &'a str,
}

/// Fetches a single album row from `spotify_albums` by ID. Returns None when
/// the album is not yet cached locally.
async fn fetch_db_album(
    pool: &SqlitePool,
    spotify_id: &str,
) -> Result<Option<DbSpotifyAlbum>, ServerFnError> {
    let row = sqlx::query(
        "SELECT spotify_id, title, artists, album_type, release_date, cover_art \
         FROM spotify_albums WHERE spotify_id = ?",
    )
    .bind(spotify_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| {
        eprintln!("spotify db fetch error: {e}");
        ServerFnError::new("internal server error")
    })?;

    Ok(row.map(|r| DbSpotifyAlbum {
        spotify_id: r.get("spotify_id"),
        title: r.get("title"),
        artists: r.get("artists"),
        album_type: r.get("album_type"),
        release_date: r.get("release_date"),
        cover_art: r.get("cover_art"),
    }))
}

/// Inserts or updates an album row. ON CONFLICT preserves `search_hit_count`
/// (which is managed separately) while refreshing all metadata columns. The
/// `cached_at` column is updated only on insert so the 24-hour cache TTL
/// reflects when we first saw this album from search, not every upsert.
async fn upsert_album(pool: &SqlitePool, data: &AlbumInsert<'_>) -> Result<(), ServerFnError> {
    sqlx::query(
        "INSERT INTO spotify_albums \
             (spotify_id, title, artists, album_type, release_date, cover_art_url, raw_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(spotify_id) DO UPDATE SET \
             title         = excluded.title, \
             artists       = excluded.artists, \
             album_type    = excluded.album_type, \
             release_date  = excluded.release_date, \
             cover_art_url = excluded.cover_art_url, \
             raw_json      = excluded.raw_json",
    )
    .bind(data.spotify_id)
    .bind(data.title)
    .bind(data.artists_json)
    .bind(data.album_type)
    .bind(data.release_date)
    .bind(data.cover_art_url)
    .bind(data.raw_json)
    .execute(pool)
    .await
    .map_err(|e| {
        eprintln!("spotify album upsert DB error: {e}");
        ServerFnError::new("internal server error")
    })?;

    Ok(())
}

// --- Conversion helpers ---

/// Converts a DB row into the public `SpotifyAlbum` type that server fns return.
/// `release_year` is extracted from the first four characters of `release_date`,
/// which Spotify supplies as YYYY, YYYY-MM, or YYYY-MM-DD.
fn spotify_album_from_db(row: &DbSpotifyAlbum) -> SpotifyAlbum {
    let artists: Vec<String> =
        serde_json::from_str(&row.artists).unwrap_or_default();

    let release_year = row
        .release_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<u32>().ok());

    SpotifyAlbum {
        spotify_id: row.spotify_id.clone(),
        title: row.title.clone(),
        artists,
        album_type: row.album_type.clone().unwrap_or_default(),
        release_year,
        has_cover_art: row.cover_art.is_some(),
    }
}

/// Converts a live Spotify API album object into the public `SpotifyAlbum` type.
/// `has_cover_art` is always false here because we only store art asynchronously
/// after the API call returns.
fn spotify_album_from_api(api: &SpotifyApiAlbum) -> SpotifyAlbum {
    let artists: Vec<String> = api.artists.iter().map(|a| a.name.clone()).collect();

    let release_year = api
        .release_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<u32>().ok());

    SpotifyAlbum {
        spotify_id: api.id.clone(),
        title: api.name.clone(),
        artists,
        album_type: api.album_type.clone(),
        release_year,
        has_cover_art: false,
    }
}

/// Converts a Spotify API track object into the public `Track` type.
fn track_from_api(t: &SpotifyApiTrack) -> Track {
    Track {
        track_id: t.id.clone(),
        disc_number: t.disc_number,
        track_number: t.track_number,
        name: t.name.clone(),
        artists: t.artists.iter().map(|a| a.name.clone()).collect(),
        duration_ms: t.duration_ms,
    }
}

/// Picks the best cover art URL from Spotify's images array. Spotify returns
/// images sorted largest first; we prefer the largest available for quality,
/// letting the UI resize as needed. Falls back to None if no images exist.
fn best_image_url(images: &[SpotifyImage]) -> Option<String> {
    // Prefer the image closest to 640px wide (high quality without being huge).
    // Spotify typically provides 640, 300, and 64px variants.
    images
        .iter()
        .max_by_key(|img| img.width.unwrap_or(0))
        .map(|img| img.url.clone())
}

/// Computes what fraction of the query's tokens appear in the combined token
/// set of `title` and `artists`. Both sides are normalised by lowercasing and
/// splitting on non-alphanumeric characters so punctuation differences (e.g.
/// "mac's" vs "mac") do not create spurious mismatches.
///
/// Spotify requests `limit=50` to give the ranking algorithm room, but the
/// tail of a 50-item response is often unrelated to the query. Filtering on
/// this score before adding results to the cache and DB keeps the stored data
/// relevant and avoids surfacing noise in the UI.
fn relevance_score(query: &str, title: &str, artists: &[String]) -> f64 {
    fn tokenize(s: &str) -> Vec<String> {
        s.chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return 1.0;
    }

    let mut target: std::collections::HashSet<String> = tokenize(title).into_iter().collect();
    for artist in artists {
        target.extend(tokenize(artist));
    }

    let matches = query_tokens.iter().filter(|t| target.contains(*t)).count();
    matches as f64 / query_tokens.len() as f64
}

/// Reads the `Retry-After` header from a 429 response and parses it as a
/// number of seconds. Defaults to 5 when the header is absent or unparseable,
/// which is a reasonable back-off that avoids hammering the API.
fn retry_after_secs(response: &reqwest::Response) -> u64 {
    response
        .headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5)
}

// Minimal struct used when serialising `raw_json` — we store the album ID and
// name at minimum so the field is not empty, without pulling in the full
// deserialized object which would require re-serialisation logic.
#[derive(serde::Serialize)]
struct AlbumRaw<'a> {
    id: &'a str,
    name: &'a str,
}

#[cfg(test)]
mod tests {
    use super::relevance_score;

    #[test]
    fn exact_title_match() {
        assert_eq!(relevance_score("rumours", "Rumours", &["Fleetwood Mac".to_string()]), 1.0);
    }

    #[test]
    fn artist_only_match() {
        // Query is purely an artist name; the album title is unrelated.
        assert_eq!(
            relevance_score("fleetwood mac", "Rumours", &["Fleetwood Mac".to_string()]),
            1.0
        );
    }

    #[test]
    fn multi_word_title_match() {
        assert_eq!(
            relevance_score(
                "dark side of the moon",
                "The Dark Side of the Moon",
                &["Pink Floyd".to_string()]
            ),
            1.0
        );
    }

    #[test]
    fn partial_match_above_threshold() {
        // 2 of 3 query tokens present → 0.67, above 0.5.
        let score = relevance_score(
            "ok computer radiohead",
            "OK Computer",
            &["Radiohead".to_string()],
        );
        assert!(score >= 0.5, "expected ≥ 0.5, got {score}");
    }

    #[test]
    fn unrelated_result_below_threshold() {
        let score = relevance_score(
            "dark side of the moon",
            "Wish You Were Here",
            &["Pink Floyd".to_string()],
        );
        assert!(score < 0.5, "expected < 0.5, got {score}");
    }

    #[test]
    fn punctuation_normalised() {
        // Apostrophe in artist name should not prevent matching.
        assert_eq!(
            relevance_score("guns n roses", "Appetite for Destruction", &["Guns N' Roses".to_string()]),
            1.0
        );
    }

    #[test]
    fn empty_query_returns_full_score() {
        assert_eq!(relevance_score("", "Anything", &[]), 1.0);
    }

}
