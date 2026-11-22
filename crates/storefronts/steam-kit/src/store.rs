//! What a game's public Steam store page tells of it, as its page in Obelisk
//! shows it. Store pages never require, receive or persist account
//! credentials.
use std::collections::HashMap;

use crate::client::{ClientError, read_bounded};
use crate::news::{shared_client, store_text};
use obelisk_core::transfer::BodyError;
use serde::Deserialize;

/// A store page carries its full descriptions and system requirements as
/// HTML, of which Obelisk reads none.
const MAX_STORE_PAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorePage {
    /// The page's short description, as plain text.
    pub summary: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    /// The release date as the store writes it in the page's language, such
    /// as "24 feb 2017", or what it says instead before the release.
    pub release_date: Option<String>,
    pub genres: Vec<String>,
    /// Metacritic's score, out of 100.
    pub metacritic: Option<u32>,
    /// How the game plays, as the store names it in the page's language,
    /// such as "Online Co-op" or "Full controller support", in its order.
    pub features: Vec<String>,
    pub screenshots: Vec<Screenshot>,
    pub trailers: Vec<Trailer>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    /// About 600 pixels wide.
    pub thumbnail: String,
    /// The full image, usually 1920 pixels wide.
    pub full: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trailer {
    pub name: String,
    pub thumbnail: String,
    /// An HLS playlist AVFoundation plays.
    pub stream: String,
}

/// The store's categories a game page shows, as Steam numbers them: how many
/// play and how, controllers, and the Workshop. The others, such as trading
/// cards or Steam Cloud, tell of Steam rather than of the game.
const FEATURES: &[u32] = &[
    2,  // Single-player
    1,  // Multi-player
    49, // PvP
    36, // Online PvP
    9,  // Co-op
    38, // Online Co-op
    39, // Shared/Split Screen Co-op
    24, // Shared/Split Screen
    27, // Cross-Platform Multiplayer
    20, // MMO
    28, // Full controller support
    18, // Partial Controller Support
    30, // Steam Workshop
];

/// The store page of `app_id` in `language`, a Steam language code such as
/// `italian`; none when Steam has no page for it, as for a game removed from
/// the store or one not sold in the player's region.
pub async fn page(app_id: u32, language: &str) -> Result<Option<StorePage>, ClientError> {
    let response = shared_client()?
        .get("https://store.steampowered.com/api/appdetails")
        .query(&[("appids", app_id.to_string()), ("l", language.into())])
        .send()
        .await
        .map_err(|_| load_failed())?
        .error_for_status()
        .map_err(|_| load_failed())?;
    let bytes =
        read_bounded(response, MAX_STORE_PAGE_BYTES)
            .await
            .map_err(|error| match error {
                BodyError::Size => incompatible(),
                BodyError::Network | BodyError::Cancelled => load_failed(),
            })?;
    parse(app_id, &bytes)
}

fn parse(app_id: u32, bytes: &[u8]) -> Result<Option<StorePage>, ClientError> {
    let mut envelope: HashMap<String, Entry> =
        serde_json::from_slice(bytes).map_err(|_| incompatible())?;
    // Steam answers for the game it was asked about, under its app ID.
    let entry = envelope
        .remove(&app_id.to_string())
        .ok_or_else(incompatible)?;
    let Some(data) = entry.data.filter(|_| entry.success) else {
        return Ok(None);
    };
    let names = |values: Vec<String>| -> Vec<String> {
        values
            .iter()
            .map(|value| store_text(value))
            .filter(|value| !value.is_empty())
            .collect()
    };
    Ok(Some(StorePage {
        summary: store_text(&data.short_description),
        developers: names(data.developers),
        publishers: names(data.publishers),
        release_date: data
            .release_date
            .map(|date| store_text(&date.date))
            .filter(|date| !date.is_empty()),
        genres: names(
            data.genres
                .into_iter()
                .map(|genre| genre.description)
                .collect(),
        ),
        metacritic: data
            .metacritic
            .map(|metacritic| metacritic.score)
            .filter(|score| (1..=100).contains(score)),
        features: names(
            data.categories
                .into_iter()
                .filter(|category| FEATURES.contains(&category.id))
                .map(|category| category.description)
                .collect(),
        ),
        screenshots: data
            .screenshots
            .into_iter()
            .filter_map(|shot| {
                Some(Screenshot {
                    thumbnail: secure(shot.path_thumbnail)?,
                    full: secure(shot.path_full)?,
                })
            })
            .collect(),
        trailers: data
            .movies
            .into_iter()
            .filter_map(|movie| {
                Some(Trailer {
                    name: store_text(&movie.name),
                    thumbnail: secure(movie.thumbnail)?,
                    stream: secure(movie.hls_h264?)?,
                })
            })
            .collect(),
    }))
}

/// A remote image or stream the page may load: HTTPS, without credentials.
fn secure(url: String) -> Option<String> {
    let parsed = reqwest::Url::parse(&url).ok()?;
    (parsed.scheme() == "https" && parsed.username().is_empty() && parsed.password().is_none())
        .then_some(url)
}

fn load_failed() -> ClientError {
    ClientError::Failure("backend.steam.store.loadFailed".into())
}

fn incompatible() -> ClientError {
    ClientError::Failure("backend.steam.store.incompatibleFormat".into())
}

#[derive(Deserialize)]
struct Entry {
    success: bool,
    data: Option<Data>,
}
#[derive(Deserialize)]
struct Data {
    #[serde(default)]
    short_description: String,
    #[serde(default)]
    developers: Vec<String>,
    #[serde(default)]
    publishers: Vec<String>,
    release_date: Option<ReleaseDate>,
    #[serde(default)]
    genres: Vec<Genre>,
    metacritic: Option<Metacritic>,
    #[serde(default)]
    categories: Vec<Category>,
    #[serde(default)]
    screenshots: Vec<WireScreenshot>,
    #[serde(default)]
    movies: Vec<Movie>,
}
#[derive(Deserialize)]
struct Category {
    id: u32,
    description: String,
}
#[derive(Deserialize)]
struct WireScreenshot {
    path_thumbnail: String,
    path_full: String,
}
#[derive(Deserialize)]
struct Movie {
    #[serde(default)]
    name: String,
    thumbnail: String,
    hls_h264: Option<String>,
}
#[derive(Deserialize)]
struct ReleaseDate {
    #[serde(default)]
    date: String,
}
#[derive(Deserialize)]
struct Genre {
    description: String,
}
#[derive(Deserialize)]
struct Metacritic {
    score: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_page_tells_in_plain_text() {
        let body = br#"{"367520":{"success":true,"data":{
            "type":"game","name":"Hollow Knight","steam_appid":367520,
            "short_description":"Forgia la tua strada in <b>Hollow Knight</b>! Un&#39;epica avventura &amp; [non BBCode].",
            "developers":["Team Cherry",""],"publishers":["Team Cherry"],
            "release_date":{"coming_soon":false,"date":"24 feb 2017"},
            "genres":[{"id":"1","description":"Azione"},{"id":"25","description":"Avventura"}],
            "metacritic":{"score":87,"url":"https://www.metacritic.com/game/pc/hollow-knight"},
            "categories":[{"id":2,"description":"Giocatore singolo"},{"id":29,"description":"Carte collezionabili di Steam"},
                {"id":28,"description":"Supporto completo per i controller"}],
            "screenshots":[{"id":0,"path_thumbnail":"https://shared.steamstatic.com/ss_1.600x338.jpg","path_full":"https://shared.steamstatic.com/ss_1.1920x1080.jpg"},
                {"id":1,"path_thumbnail":"http://insecure.example/ss_2.jpg","path_full":"https://shared.steamstatic.com/ss_2.jpg"}],
            "movies":[{"id":1,"name":"Launch &amp; Trailer","thumbnail":"https://shared.steamstatic.com/movie.jpg",
                "hls_h264":"https://video.steamstatic.com/hls_264_master.m3u8"},{"id":2,"name":"No stream","thumbnail":"https://shared.steamstatic.com/m2.jpg"}],
            "detailed_description":"<h1>Ignored</h1>"}}}"#;
        assert_eq!(
            parse(367520, body).unwrap(),
            Some(StorePage {
                summary:
                    "Forgia la tua strada in Hollow Knight! Un’epica avventura & [non BBCode]."
                        .into(),
                developers: vec!["Team Cherry".into()],
                publishers: vec!["Team Cherry".into()],
                release_date: Some("24 feb 2017".into()),
                genres: vec!["Azione".into(), "Avventura".into()],
                metacritic: Some(87),
                features: vec![
                    "Giocatore singolo".into(),
                    "Supporto completo per i controller".into()
                ],
                screenshots: vec![Screenshot {
                    thumbnail: "https://shared.steamstatic.com/ss_1.600x338.jpg".into(),
                    full: "https://shared.steamstatic.com/ss_1.1920x1080.jpg".into(),
                }],
                trailers: vec![Trailer {
                    name: "Launch & Trailer".into(),
                    thumbnail: "https://shared.steamstatic.com/movie.jpg".into(),
                    stream: "https://video.steamstatic.com/hls_264_master.m3u8".into(),
                }],
            })
        );
    }

    #[test]
    fn a_page_steam_does_not_have_is_none() {
        assert_eq!(parse(10, br#"{"10":{"success":false}}"#).unwrap(), None);
    }

    #[test]
    fn a_page_missing_what_it_may_omit_still_reads() {
        let page = parse(
            10,
            br#"{"10":{"success":true,"data":{"release_date":{"coming_soon":true,"date":""}}}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(page.summary, "");
        assert!(page.developers.is_empty() && page.genres.is_empty());
        assert_eq!((page.release_date, page.metacritic), (None, None));
    }

    #[test]
    fn an_answer_about_another_game_is_refused() {
        assert!(parse(10, br#"{"20":{"success":true,"data":{}}}"#).is_err());
        assert!(parse(10, b"<html>").is_err());
    }

    #[tokio::test]
    #[ignore = "contacts the public Steam store"]
    async fn live_page() {
        let page = page(620, "italian").await.unwrap().unwrap();
        assert!(!page.summary.is_empty());
        assert!(page.developers.iter().any(|developer| developer == "Valve"));
        assert!(
            !page.screenshots.is_empty() && !page.trailers.is_empty() && !page.features.is_empty()
        );
    }
}
