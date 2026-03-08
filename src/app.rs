use leptos::prelude::*;
use leptos_meta::*;
use leptos_router::{
    components::{A, FlatRoutes, Route, Router},
    hooks::{use_navigate, use_params_map, use_query_map},
    ParamSegment, StaticSegment,
};
use serde::{Deserialize, Serialize};

/// A Spotify album as returned by search/lookup server fns.
/// Ungated so both the SSR binary and the WASM hydration bundle can
/// (de)serialize it across the server fn boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotifyAlbum {
    pub spotify_id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album_type: String,
    pub release_year: Option<u32>,
    /// True once the cover art BLOB has been stored in the DB. The first
    /// search response may be false; subsequent ones will be true after the
    /// background fetch completes.
    pub has_cover_art: bool,
}

/// A single track as returned by the album detail server fn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub track_id: String,
    pub disc_number: u32,
    pub track_number: u32,
    pub name: String,
    pub artists: Vec<String>,
    pub duration_ms: Option<u32>,
}

/// Album metadata combined with its full track listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumDetail {
    pub album: SpotifyAlbum,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub username: String,
    pub bio: Option<String>,
    pub follower_count: i64,
    pub following_count: i64,
    pub joined_at: String,
    pub is_self: bool,
    pub is_following: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRating {
    pub spotify_id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album_type: String,
    pub release_year: Option<u32>,
    pub has_cover_art: bool,
    pub rating: u8,
    pub rated_at: String,
}

/// A page of search results together with the total result count.
/// Ungated so both the SSR binary and WASM hydration can (de)serialize it.
/// `total` is the total number of cached results (≤50), used by the client
/// to compute the page count without an extra round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchPage {
    pub albums: Vec<SpotifyAlbum>,
    pub total: usize,
}

#[server]
pub async fn get_current_user() -> Result<Option<String>, ServerFnError> {
    use crate::auth::server::CurrentUser;
    use axum::Extension;
    let Extension(user): Extension<Option<CurrentUser>> = leptos_axum::extract().await?;
    Ok(user.map(|u| u.username))
}

#[server]
pub async fn search_music(query: String, page: u32) -> Result<SearchPage, ServerFnError> {
    use crate::spotify::SpotifyClient;
    use axum::Extension;
    use sqlx::SqlitePool;
    let page = page.max(1);
    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(spotify): Extension<SpotifyClient> = leptos_axum::extract().await?;
    spotify.search(&pool, &query, page).await
}

#[server]
pub async fn get_album_detail(spotify_id: String) -> Result<AlbumDetail, ServerFnError> {
    use crate::spotify::SpotifyClient;
    use axum::Extension;
    use sqlx::SqlitePool;
    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(spotify): Extension<SpotifyClient> = leptos_axum::extract().await?;
    spotify.get_album_detail(&pool, &spotify_id).await
}

#[server]
pub async fn get_user_profile(username: String) -> Result<Option<UserProfile>, ServerFnError> {
    use crate::auth::server::CurrentUser;
    use axum::Extension;
    use sqlx::{Row, SqlitePool};

    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(viewer): Extension<Option<CurrentUser>> = leptos_axum::extract().await?;

    let row = sqlx::query(
        "SELECT u.user_id, u.bio, u.created_at, \
         (SELECT COUNT(*) FROM follows WHERE followee_id = u.user_id) AS follower_count, \
         (SELECT COUNT(*) FROM follows WHERE follower_id = u.user_id) AS following_count \
         FROM users u WHERE u.username = ?",
    )
    .bind(&username)
    .fetch_optional(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    let Some(row) = row else { return Ok(None) };

    let profile_user_id: String = row.get("user_id");
    let bio: Option<String> = row.get("bio");
    let created_at: String = row.get("created_at");
    let follower_count: i64 = row.get("follower_count");
    let following_count: i64 = row.get("following_count");

    let (is_self, is_following) = if let Some(ref v) = viewer {
        let is_self = v.user_id == profile_user_id;
        let is_following = if is_self {
            false
        } else {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM follows WHERE follower_id = ? AND followee_id = ?",
            )
            .bind(&v.user_id)
            .bind(&profile_user_id)
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
            count > 0
        };
        (is_self, is_following)
    } else {
        (false, false)
    };

    Ok(Some(UserProfile {
        username,
        bio,
        follower_count,
        following_count,
        joined_at: created_at,
        is_self,
        is_following,
    }))
}

#[server]
pub async fn update_profile(new_username: String, new_bio: String) -> Result<(), ServerFnError> {
    use crate::auth::server::CurrentUser;
    use axum::Extension;
    use sqlx::SqlitePool;

    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(viewer): Extension<Option<CurrentUser>> = leptos_axum::extract().await?;

    let viewer = viewer.ok_or_else(|| ServerFnError::new("Not logged in".to_string()))?;

    if new_username.len() < 3 || new_username.len() > 32 {
        return Err(ServerFnError::new("Username must be 3–32 characters".to_string()));
    }
    if !new_username.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(ServerFnError::new(
            "Username may only contain letters, digits, and underscores".to_string(),
        ));
    }

    let taken: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE username = ? AND user_id != ?",
    )
    .bind(&new_username)
    .bind(&viewer.user_id)
    .fetch_one(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    if taken > 0 {
        return Err(ServerFnError::new("Username is already taken".to_string()));
    }

    let bio_val: Option<String> = if new_bio.is_empty() { None } else { Some(new_bio) };

    sqlx::query(
        "UPDATE users SET username = ?, bio = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
         WHERE user_id = ?",
    )
    .bind(&new_username)
    .bind(&bio_val)
    .bind(&viewer.user_id)
    .execute(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(())
}

#[server]
pub async fn get_user_ratings(username: String) -> Result<Vec<UserRating>, ServerFnError> {
    use axum::Extension;
    use sqlx::{Row, SqlitePool};

    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;

    let rows = sqlx::query(
        "SELECT sa.spotify_id, sa.title, sa.artists, sa.album_type, sa.release_date, \
         sa.cover_art IS NOT NULL AS has_cover_art, r.rating, r.created_at AS rated_at \
         FROM ratings r \
         JOIN users u ON r.user_id = u.user_id \
         JOIN release_groups rg ON r.release_group_id = rg.release_group_id \
         JOIN spotify_albums sa ON rg.spotify_id = sa.spotify_id \
         WHERE u.username = ? \
         ORDER BY r.created_at DESC",
    )
    .bind(&username)
    .fetch_all(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    let ratings = rows
        .into_iter()
        .map(|row| {
            let artists_json: String = row.get("artists");
            let artists: Vec<String> =
                serde_json::from_str(&artists_json).unwrap_or_default();
            let release_date: Option<String> = row.get("release_date");
            let release_year = release_date
                .as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok());
            let has_cover_art: bool = row.get::<i64, _>("has_cover_art") != 0;
            UserRating {
                spotify_id: row.get("spotify_id"),
                title: row.get("title"),
                artists,
                album_type: row.get::<Option<String>, _>("album_type").unwrap_or_default(),
                release_year,
                has_cover_art,
                rating: row.get::<i64, _>("rating") as u8,
                rated_at: row.get("rated_at"),
            }
        })
        .collect();

    Ok(ratings)
}

#[server]
pub async fn rate_album(spotify_id: String, rating: u8) -> Result<(), ServerFnError> {
    use crate::auth::server::CurrentUser;
    use axum::Extension;
    use sqlx::SqlitePool;
    use uuid::Uuid;

    if rating < 1 || rating > 10 {
        return Err(ServerFnError::new("Rating must be between 1 and 10".to_string()));
    }

    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(viewer): Extension<Option<CurrentUser>> = leptos_axum::extract().await?;
    let viewer = viewer.ok_or_else(|| ServerFnError::new("Not logged in".to_string()))?;

    // Ensure a release_groups row exists for this spotify_id.
    let new_rg_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT OR IGNORE INTO release_groups (release_group_id, title, primary_type, first_release_year, spotify_id) \
         SELECT ?, title, album_type, CAST(SUBSTR(release_date, 1, 4) AS INTEGER), spotify_id \
         FROM spotify_albums WHERE spotify_id = ?",
    )
    .bind(&new_rg_id)
    .bind(&spotify_id)
    .execute(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    let rg_id: String = sqlx::query_scalar(
        "SELECT release_group_id FROM release_groups WHERE spotify_id = ?",
    )
    .bind(&spotify_id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?
    .ok_or_else(|| ServerFnError::new("Album not found in cache".to_string()))?;

    let rating_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ratings (rating_id, user_id, release_group_id, rating) \
         VALUES (?, ?, ?, ?) \
         ON CONFLICT(user_id, release_group_id) DO UPDATE SET \
         rating = excluded.rating, \
         updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
    )
    .bind(&rating_id)
    .bind(&viewer.user_id)
    .bind(&rg_id)
    .bind(rating as i64)
    .execute(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(())
}

#[server]
pub async fn get_my_rating(spotify_id: String) -> Result<Option<u8>, ServerFnError> {
    use crate::auth::server::CurrentUser;
    use axum::Extension;
    use sqlx::SqlitePool;

    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(viewer): Extension<Option<CurrentUser>> = leptos_axum::extract().await?;

    let Some(viewer) = viewer else { return Ok(None) };

    let rating: Option<i64> = sqlx::query_scalar(
        "SELECT r.rating FROM ratings r \
         JOIN release_groups rg ON r.release_group_id = rg.release_group_id \
         WHERE rg.spotify_id = ? AND r.user_id = ?",
    )
    .bind(&spotify_id)
    .bind(&viewer.user_id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(rating.map(|r| r as u8))
}

#[server]
pub async fn follow_user(target_username: String) -> Result<(), ServerFnError> {
    use crate::auth::server::CurrentUser;
    use axum::Extension;
    use sqlx::SqlitePool;

    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(viewer): Extension<Option<CurrentUser>> = leptos_axum::extract().await?;
    let viewer = viewer.ok_or_else(|| ServerFnError::new("Not logged in".to_string()))?;

    sqlx::query(
        "INSERT OR IGNORE INTO follows (follower_id, followee_id) \
         SELECT ?, user_id FROM users WHERE username = ?",
    )
    .bind(&viewer.user_id)
    .bind(&target_username)
    .execute(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(())
}

#[server]
pub async fn unfollow_user(target_username: String) -> Result<(), ServerFnError> {
    use crate::auth::server::CurrentUser;
    use axum::Extension;
    use sqlx::SqlitePool;

    let Extension(pool): Extension<SqlitePool> = leptos_axum::extract().await?;
    let Extension(viewer): Extension<Option<CurrentUser>> = leptos_axum::extract().await?;
    let viewer = viewer.ok_or_else(|| ServerFnError::new("Not logged in".to_string()))?;

    sqlx::query(
        "DELETE FROM follows WHERE follower_id = ? \
         AND followee_id = (SELECT user_id FROM users WHERE username = ?)",
    )
    .bind(&viewer.user_id)
    .bind(&target_username)
    .execute(&pool)
    .await
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(())
}

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <link rel="stylesheet" href="/style.css"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    view! {
        <Title text="Musicboxd"/>
        <Router>
            <main>
                <FlatRoutes fallback=|| "Page not found.".into_view()>
                    <Route path=StaticSegment("") view=HomePage/>
                    <Route path=(StaticSegment("album"), ParamSegment("id")) view=AlbumPage/>
                    <Route path=(StaticSegment("user"), ParamSegment("username")) view=ProfilePage/>
                </FlatRoutes>
            </main>
        </Router>
    }
}

#[component]
fn HomePage() -> impl IntoView {
    let query_map = use_query_map();
    let navigate = use_navigate();

    // Query lives in the URL so back-navigation and refresh restore it.
    let url_q = move || query_map.read().get("q").unwrap_or_default();

    // Text box mirrors the URL query; kept in sync by the Effect below.
    let (input, set_input) = signal(url_q());
    Effect::new(move |_| set_input.set(url_q()));

    let current_user = Resource::new(|| (), |_| get_current_user());

    // Infinite scroll: page number is in-memory only — scrolling is a session
    // gesture, not something to encode in the URL.
    let (page, set_page) = signal(1u32);
    let (albums, set_albums) = signal(Vec::<SpotifyAlbum>::new());

    // When the query changes, reset accumulated results and restart from page 1.
    Effect::new(move |prev_q: Option<String>| {
        let q = url_q();
        if prev_q.as_deref() != Some(&q) {
            set_page.set(1);
            set_albums.set(vec![]);
        }
        q
    });

    let page_result = Resource::new(
        move || (url_q(), page.get()),
        |(q, p)| async move {
            if q.trim().is_empty() {
                return Ok(SearchPage { albums: vec![], total: 0 });
            }
            search_music(q, p).await
        },
    );

    // Accumulate pages: replace on page 1 (fresh query), append on later pages.
    Effect::new(move |_| {
        let Some(Ok(result)) = page_result.get() else { return };
        let fetched = result.albums;
        if page.get_untracked() == 1 {
            set_albums.set(fetched);
        } else {
            set_albums.update(|a| a.extend(fetched));
        }
    });


    view! {
        <header class="site-header">
            <span class="logo">"Musicboxd"</span>
            <div class="header-auth">
                <Suspense fallback=|| ()>
                    {move || current_user.get().map(|res| {
                        match res {
                            Ok(Some(username)) => view! {
                                <A href=format!("/user/{}", username) attr:class="auth-user">{username.clone()}</A>
                                <a class="auth-link" rel="external" href="/auth/logout">"Sign out"</a>
                            }.into_any(),
                            _ => view! {
                                <a class="auth-link oauth-btn" rel="external" href="/auth/google">
                                    <img class="oauth-icon" src="/google-icon.svg" alt="" width="14" height="14"/>
                                    "Sign in with Google"
                                </a>
                                <a class="auth-link oauth-btn" rel="external" href="/auth/github">
                                    <img class="oauth-icon" src="/github-icon.svg" alt="" width="14" height="14"/>
                                    "Sign in with GitHub"
                                </a>
                            }.into_any(),
                        }
                    })}
                </Suspense>
            </div>
        </header>
        <form class="search-form" on:submit=move |ev| {
            ev.prevent_default();
            let q = input.get_untracked();
            let dest = if q.trim().is_empty() {
                "/".to_string()
            } else {
                format!("/?q={}", url_encode_query(&q))
            };
            navigate(&dest, Default::default());
        }>
            <input
                class="search-input"
                type="text"
                placeholder="Search for music..."
                prop:value=move || input.get()
                on:input=move |ev| set_input.set(event_target_value(&ev))
            />
            <button class="search-btn" type="submit">"Search"</button>
        </form>
        {move || {
            if url_q().trim().is_empty() {
                return None;
            }
            Some(view! {
                <ul class="results-list">
                    {move || albums.get().into_iter().map(|album| {
                        let cover_src = format!("/album-art/{}", album.spotify_id);
                        let href = format!("/album/{}", album.spotify_id);
                        let artists = album.artists.join(", ");
                        let year = album
                            .release_year
                            .map(|y| y.to_string())
                            .unwrap_or_else(|| "????".to_string());
                        view! {
                            <li class="result-card">
                                <A href=href attr:class="result-card-link">
                                    <img class="result-cover" src=cover_src alt="Album cover" width="72" height="72"/>
                                    <div class="result-info">
                                        <span class="result-title">{album.title}</span>
                                        <span class="result-artist">{artists}</span>
                                        <div class="result-meta">
                                            <span class="result-type">{album.album_type}</span>
                                            <span class="result-year">{year}</span>
                                        </div>
                                    </div>
                                </A>
                            </li>
                        }
                    }).collect_view()}
                </ul>
                <Suspense fallback=move || {
                    if albums.with(Vec::is_empty) {
                        view! { <p class="status-msg">"Searching..."</p> }.into_any()
                    } else {
                        view! { <p class="status-msg">"Loading..."</p> }.into_any()
                    }
                }>
                    {move || page_result.get().map(|res| match res {
                        Err(e) => view! {
                            <p class="status-msg">"Error: " {e.to_string()}</p>
                        }.into_any(),
                        Ok(r) if r.albums.is_empty() && page.get() == 1 => view! {
                            <p class="status-msg">"No results found."</p>
                        }.into_any(),
                        Ok(_) => view! {
                            <div class="load-more-bar">
                                <button class="load-more-btn"
                                    on:click=move |_| set_page.update(|p| *p += 1)>
                                    "Load more"
                                </button>
                            </div>
                        }.into_any(),
                    })}
                </Suspense>
            })
        }}
    }
}

#[component]
fn AlbumPage() -> impl IntoView {
    let params = use_params_map();
    let spotify_id = move || params.read().get("id").unwrap_or_default();

    let detail = Resource::new(spotify_id, |id| async move { get_album_detail(id).await });
    let my_rating = Resource::new(spotify_id, |id| get_my_rating(id));
    let current_user_res = Resource::new(|| (), |_| get_current_user());

    view! {
        <header class="site-header">
            <A href="/" attr:class="logo">"Musicboxd"</A>
        </header>
        <Suspense fallback=move || view! { <p class="status-msg">"Loading..."</p> }>
            {move || detail.get().map(|res| match res {
                Err(e) => view! {
                    <p class="status-msg">"Error: " {e.to_string()}</p>
                }.into_any(),
                Ok(d) => {
                    let cover_src = format!("/album-art/{}", d.album.spotify_id);
                    let artists = d.album.artists.join(", ");
                    let year = d.album.release_year.map(|y| y.to_string()).unwrap_or_else(|| "????".to_string());
                    view! {
                        <div class="album-detail">
                            <div class="album-header">
                                <img class="album-cover" src=cover_src alt="Album cover" width="200" height="200"/>
                                <div class="album-meta">
                                    <h1 class="album-title">{d.album.title}</h1>
                                    <p class="album-artists">{artists}</p>
                                    <p class="album-info">
                                        <span class="album-type">{d.album.album_type}</span>
                                        " · "
                                        <span class="album-year">{year}</span>
                                    </p>
                                </div>
                            </div>
                            <ul class="track-list">
                                {d.tracks.into_iter().map(|track| {
                                    let duration = format_duration(track.duration_ms);
                                    let track_artists = track.artists.join(", ");
                                    view! {
                                        <li class="track-row">
                                            <span class="track-num">{track.track_number}</span>
                                            <div class="track-info">
                                                <div class="track-name">{track.name}</div>
                                                {(!track_artists.is_empty()).then(|| view! {
                                                    <div class="track-artists">{track_artists}</div>
                                                })}
                                            </div>
                                            <span class="track-duration">{duration}</span>
                                        </li>
                                    }
                                }).collect_view()}
                            </ul>
                            <Suspense fallback=|| ()>
                                {move || {
                                    let user = current_user_res.get().and_then(|r| r.ok()).flatten();
                                    let rating = my_rating.get().and_then(|r| r.ok()).flatten();
                                    if user.is_none() {
                                        return view! { <p class="status-msg">"Sign in to rate this album."</p> }.into_any();
                                    }
                                    let sid = spotify_id();
                                    view! {
                                        <div class="rating-widget">
                                            <span class="rating-label">"Your rating: "</span>
                                            {(1u8..=10).map(|dot| {
                                                let active = rating.map(|r| r >= dot).unwrap_or(false);
                                                let sid2 = sid.clone();
                                                view! {
                                                    <button
                                                        class=move || if active { "rating-dot active" } else { "rating-dot" }
                                                        on:click=move |_| {
                                                            let s = sid2.clone();
                                                            leptos::task::spawn_local(async move {
                                                                let _ = rate_album(s, dot).await;
                                                                my_rating.refetch();
                                                            });
                                                        }
                                                    >{dot}</button>
                                                }
                                            }).collect_view()}
                                        </div>
                                    }.into_any()
                                }}
                            </Suspense>
                        </div>
                    }.into_any()
                }
            })}
        </Suspense>
    }
}

#[component]
fn ProfilePage() -> impl IntoView {
    let params = use_params_map();
    let username = move || params.read().get("username").unwrap_or_default();

    let profile = Resource::new(username, |u| get_user_profile(u));
    let ratings = Resource::new(username, |u| get_user_ratings(u));

    let (edit_mode, set_edit_mode) = signal(false);
    let (edit_username, set_edit_username) = signal(String::new());
    let (edit_bio, set_edit_bio) = signal(String::new());
    let (edit_error, set_edit_error) = signal(Option::<String>::None);

    view! {
        <header class="site-header">
            <A href="/" attr:class="logo">"Musicboxd"</A>
        </header>
        <Suspense fallback=move || view! { <p class="status-msg">"Loading..."</p> }>
            {move || profile.get().map(|res| match res {
                Err(e) => view! { <p class="status-msg">"Error: " {e.to_string()}</p> }.into_any(),
                Ok(None) => view! { <p class="status-msg">"User not found."</p> }.into_any(),
                Ok(Some(p)) => {
                    let initial = p.username.chars().next().unwrap_or('?').to_uppercase().to_string();
                    let follower_count = p.follower_count;
                    let following_count = p.following_count;
                    let is_self = p.is_self;
                    let is_following = p.is_following;
                    let bio = p.bio.clone();
                    let joined = p.joined_at.get(..10).unwrap_or("").to_string();
                    let profile_username = p.username.clone();
                    view! {
                        <div class="profile-page">
                            <div class="profile-header">
                                <div class="profile-avatar">{initial}</div>
                                <div class="profile-info">
                                    <h1 class="profile-username">{profile_username.clone()}</h1>
                                    {bio.as_deref().filter(|b| !b.is_empty()).map(|b| view! {
                                        <p class="profile-bio">{b.to_string()}</p>
                                    })}
                                    <p class="profile-joined">"Joined " {joined}</p>
                                </div>
                            </div>
                            <div class="profile-stats">
                                <span class="profile-stat"><strong>{follower_count}</strong>" followers"</span>
                                <span class="profile-stat"><strong>{following_count}</strong>" following"</span>
                            </div>
                            <div class="profile-actions">
                                {if is_self {
                                    view! {
                                        <button class="follow-btn" on:click=move |_| {
                                            set_edit_username.set(username());
                                            set_edit_bio.set(bio.clone().unwrap_or_default());
                                            set_edit_error.set(None);
                                            set_edit_mode.set(true);
                                        }>"Edit profile"</button>
                                    }.into_any()
                                } else if is_following {
                                    let uname = profile_username.clone();
                                    view! {
                                        <button class="follow-btn following" on:click=move |_| {
                                            let u = uname.clone();
                                            leptos::task::spawn_local(async move {
                                                let _ = unfollow_user(u).await;
                                                profile.refetch();
                                            });
                                        }>"Unfollow"</button>
                                    }.into_any()
                                } else {
                                    let uname = profile_username.clone();
                                    view! {
                                        <button class="follow-btn" on:click=move |_| {
                                            let u = uname.clone();
                                            leptos::task::spawn_local(async move {
                                                let _ = follow_user(u).await;
                                                profile.refetch();
                                            });
                                        }>"Follow"</button>
                                    }.into_any()
                                }}
                            </div>
                            {move || edit_mode.get().then(|| {
                                view! {
                                    <form class="edit-profile-form" on:submit=move |ev| {
                                        ev.prevent_default();
                                        let u = edit_username.get_untracked();
                                        let b = edit_bio.get_untracked();
                                        leptos::task::spawn_local(async move {
                                            match update_profile(u, b).await {
                                                Ok(()) => {
                                                    set_edit_mode.set(false);
                                                    profile.refetch();
                                                }
                                                Err(e) => set_edit_error.set(Some(e.to_string())),
                                            }
                                        });
                                    }>
                                        <label>"Username"
                                            <input class="search-input"
                                                prop:value=move || edit_username.get()
                                                on:input=move |ev| set_edit_username.set(event_target_value(&ev))
                                            />
                                        </label>
                                        <label>"Bio"
                                            <textarea class="bio-input"
                                                prop:value=move || edit_bio.get()
                                                on:input=move |ev| set_edit_bio.set(event_target_value(&ev))
                                            />
                                        </label>
                                        {move || edit_error.get().map(|e| view! {
                                            <p class="status-msg">{e}</p>
                                        })}
                                        <div class="edit-form-actions">
                                            <button class="search-btn" type="submit">"Save"</button>
                                            <button class="follow-btn" type="button" on:click=move |_| set_edit_mode.set(false)>"Cancel"</button>
                                        </div>
                                    </form>
                                }
                            })}
                        </div>
                        <Suspense fallback=move || view! { <p class="status-msg">"Loading ratings..."</p> }>
                            {move || ratings.get().map(|res| match res {
                                Err(e) => view! { <p class="status-msg">"Error: " {e.to_string()}</p> }.into_any(),
                                Ok(rs) if rs.is_empty() => view! { <p class="status-msg">"No ratings yet."</p> }.into_any(),
                                Ok(rs) => view! {
                                    <ul class="results-list">
                                        {rs.into_iter().map(|r| {
                                            let cover_src = format!("/album-art/{}", r.spotify_id);
                                            let href = format!("/album/{}", r.spotify_id);
                                            let artists = r.artists.join(", ");
                                            let year = r.release_year.map(|y| y.to_string()).unwrap_or_else(|| "????".to_string());
                                            let rating = r.rating;
                                            view! {
                                                <li class="result-card">
                                                    <A href=href attr:class="result-card-link">
                                                        <div class="result-cover-wrap">
                                                            <img class="result-cover" src=cover_src alt="Album cover" width="72" height="72"/>
                                                            <span class="rating-badge">{rating}</span>
                                                        </div>
                                                        <div class="result-info">
                                                            <span class="result-title">{r.title}</span>
                                                            <span class="result-artist">{artists}</span>
                                                            <div class="result-meta">
                                                                <span class="result-type">{r.album_type}</span>
                                                                <span class="result-year">{year}</span>
                                                            </div>
                                                        </div>
                                                    </A>
                                                </li>
                                            }
                                        }).collect_view()}
                                    </ul>
                                }.into_any(),
                            })}
                        </Suspense>
                    }.into_any()
                }
            })}
        </Suspense>
    }
}

/// Encodes a search query for use in a URL query string.
/// Converts spaces to `+` and percent-encodes the characters that are
/// structurally significant in a URL (`%`, `&`, `#`, `+`).
fn url_encode_query(s: &str) -> String {
    s.replace('%', "%25")
        .replace('&', "%26")
        .replace('#', "%23")
        .replace('+', "%2B")
        .replace(' ', "+")
}

fn format_duration(ms: Option<u32>) -> String {
    let ms = match ms {
        Some(v) => v,
        None => return String::new(),
    };
    let total_secs = ms / 1000;
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}
