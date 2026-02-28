use leptos::prelude::*;
use leptos_meta::*;
use leptos_router::{
    components::{FlatRoutes, Route, Router},
    StaticSegment,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseGroup {
    pub id: String,
    pub title: String,
    #[serde(rename = "artist-credit", default)]
    pub artist_credit: Vec<ArtistCredit>,
    #[serde(rename = "first-release-date", default)]
    pub first_release_date: Option<String>,
    #[serde(rename = "primary-type")]
    pub primary_type: Option<String>,
    #[serde(rename = "secondary-types", default)]
    pub secondary_types: Vec<String>,
    pub score: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtistCredit {
    pub artist: Artist,
    pub name: Option<String>,
    #[serde(default)]
    pub joinphrase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub name: String,
}

#[cfg(feature = "ssr")]
#[derive(Debug, Deserialize)]
struct MbResponse {
    #[serde(rename = "release-groups")]
    release_groups: Vec<ReleaseGroup>,
}

#[server]
pub async fn search_music(query: String) -> Result<Vec<ReleaseGroup>, ServerFnError> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://musicbrainz.org/ws/2/release-group")
        .query(&[("query", &query), ("limit", &"10".to_string()), ("fmt", &"json".to_string())])
        .header("User-Agent", "musicboxd/0.1 (https://github.com/musicboxd)")
        .send()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let mb_response: MbResponse = response
        .json()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    Ok(mb_response.release_groups)
}

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
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
                </FlatRoutes>
            </main>
        </Router>
    }
}

fn format_artist_credit(credits: &[ArtistCredit]) -> String {
    let mut result = String::new();
    for credit in credits {
        let name = credit.name.as_deref().unwrap_or(&credit.artist.name);
        result.push_str(name);
        result.push_str(&credit.joinphrase);
    }
    result
}

fn format_type(primary: &Option<String>, secondary: &[String]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(p) = primary {
        parts.push(p.as_str());
    }
    for s in secondary {
        parts.push(s.as_str());
    }
    if parts.is_empty() {
        "Unknown".to_string()
    } else {
        parts.join(" + ")
    }
}

fn format_year(date: &Option<String>) -> String {
    match date {
        Some(d) if d.len() >= 4 => d[..4].to_string(),
        _ => "????".to_string(),
    }
}

#[component]
fn HomePage() -> impl IntoView {
    let (input, set_input) = signal(String::new());
    let (query, set_query) = signal(String::new());

    let results = Resource::new(
        move || query.get(),
        |q| async move {
            if q.trim().is_empty() {
                Ok(vec![])
            } else {
                search_music(q).await
            }
        },
    );

    view! {
        <h1>"Musicboxd"</h1>
        <form on:submit=move |ev| {
            ev.prevent_default();
            set_query.set(input.get_untracked());
        }>
            <input
                type="text"
                placeholder="Search for music..."
                prop:value=move || input.get()
                on:input=move |ev| set_input.set(event_target_value(&ev))
            />
            <button type="submit">"Search"</button>
        </form>
        <Suspense fallback=move || view! { <p>"Searching..."</p> }>
            {move || {
                if query.get().trim().is_empty() {
                    return Some(view! { <></> }.into_any());
                }
                results.get().map(|res| {
                    match res {
                        Ok(groups) if groups.is_empty() => {
                            view! { <p>"No results found."</p> }.into_any()
                        }
                        Ok(groups) => {
                            view! {
                                <ul>
                                    {groups.into_iter().map(|rg| {
                                        let cover_url = format!(
                                            "https://coverartarchive.org/release-group/{}/front-250",
                                            rg.id
                                        );
                                        let artist = format_artist_credit(&rg.artist_credit);
                                        let year = format_year(&rg.first_release_date);
                                        let release_type = format_type(&rg.primary_type, &rg.secondary_types);
                                        let score = rg.score.unwrap_or(0);
                                        view! {
                                            <li>
                                                <img src=cover_url alt="Album cover" width="250" height="250"/>
                                                <div>
                                                    <strong>{rg.title}</strong>
                                                    <span>" — "</span>
                                                    <span>{artist}</span>
                                                    <span>" ("</span>
                                                    <span>{year}</span>
                                                    <span>") ["</span>
                                                    <span>{release_type}</span>
                                                    <span>"] ["</span>
                                                    <span>{score}"%]"</span>
                                                </div>
                                            </li>
                                        }
                                    }).collect_view()}
                                </ul>
                            }.into_any()
                        }
                        Err(e) => {
                            view! { <p>"Error: " {e.to_string()}</p> }.into_any()
                        }
                    }
                })
            }}
        </Suspense>
    }
}
