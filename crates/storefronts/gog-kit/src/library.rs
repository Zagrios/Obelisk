//! The account's games, as Heroic's `library.ts` reads them: the Galaxy
//! library names the products the account owns, gamesdb describes each one,
//! and `api.gog.com/products` the few gamesdb does not know.
use std::{collections::HashSet, time::Duration};

use futures_util::{StreamExt, TryStreamExt, stream};
use obelisk_core::Platform;
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    GogError,
    auth::Api,
    client::valid_product_id,
    http::{bounded_json, safe_https_url, send_retrying},
};

const MAX_LIBRARY_PAGES: usize = 1000;
/// Concurrent gamesdb requests while a library loads.
const DESCRIPTIONS: usize = 12;

#[derive(Clone, Debug)]
pub struct Game {
    pub product_id: String,
    pub title: String,
    /// The portrait cover.
    pub image_url: Option<String>,
    /// The wide artwork the hero and the game's page show.
    pub header_image_url: Option<String>,
    pub windows: bool,
    pub macos: bool,
    pub info: GameInfo,
    /// When the game joined the account's library, in seconds since the Unix
    /// epoch.
    pub added_at: Option<u64>,
    /// Everything GOG returned for the game: its Galaxy library record,
    /// `release`, and its gamesdb release or store product, `catalog`.
    pub metadata: Value,
}

/// What GOG tells of a game, for its page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GameInfo {
    /// In English: gamesdb has no other language.
    pub summary: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    /// Seconds since the Unix epoch.
    pub release_date: Option<u64>,
    pub genres: Vec<String>,
    /// The critics' rating, out of 100.
    pub rating: Option<u32>,
    /// The languages the game speaks, such as `fr-FR`.
    pub languages: Vec<String>,
}

impl GameInfo {
    /// What a gamesdb release tells of its game.
    fn of_release(catalog: &Value) -> Self {
        let game = &catalog["game"];
        let names = |list: &Value| -> Vec<String> {
            list.as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| match &item["name"] {
                    Value::String(name) => Some(name.clone()),
                    name => localized(name),
                })
                .filter(|name| !name.is_empty())
                .collect()
        };
        Self {
            summary: localized(&game["summary"])
                .or_else(|| localized(&catalog["summary"]))
                .unwrap_or_default(),
            developers: names(&game["developers"]),
            publishers: names(&game["publishers"]),
            release_date: [&catalog["first_release_date"], &game["first_release_date"]]
                .into_iter()
                .find_map(|date| date.as_str().and_then(obelisk_core::date::unix_seconds)),
            genres: names(&game["genres"]),
            rating: game["aggregated_rating"]
                .as_f64()
                .filter(|rating| *rating > 0.0 && *rating <= 100.0)
                .map(|rating| rating.round() as u32),
            languages: catalog["available_languages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|language| language["code"].as_str().map(str::to_owned))
                .collect(),
        }
    }
}

/// What a game's page adds to its library entry: the build it shows before
/// any download, and its size.
#[derive(Clone, Debug, Default)]
pub struct GameDetails {
    /// The Mac build, unless a download found that its game does not run
    /// natively. Unknown when GOG could not tell.
    pub platform: Option<Platform>,
    /// Unknown when GOG serves no build Obelisk installs.
    pub installation_bytes: Option<u64>,
}

#[derive(Deserialize)]
struct ReleasesPage {
    #[serde(default)]
    items: Vec<Value>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// One product of the account's Galaxy library.
struct Release {
    platform_id: String,
    external_id: String,
    certificate: Option<String>,
}

impl Release {
    /// The record's product, whose id Galaxy may spell as a number.
    fn of(record: &Value) -> Self {
        let text = |key: &str| match &record[key] {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            _ => String::new(),
        };
        Self {
            platform_id: text("platform_id"),
            external_id: text("external_id"),
            certificate: record["certificate"].as_str().map(str::to_owned),
        }
    }
}

/// The account's games, in the order Galaxy lists them.
pub(crate) async fn games(
    api: &Api,
    access_token: &str,
    user_id: &str,
) -> Result<Vec<Game>, GogError> {
    let releases = releases(api, access_token, user_id).await?;
    let descriptions =
        releases
            .into_iter()
            .enumerate()
            .map(|(index, (release, record))| async move {
                Ok::<_, GogError>((index, describe(api, access_token, release, record).await?))
            });
    // A slow product never holds back the others' requests.
    let mut described: Vec<(usize, Option<Game>)> = stream::iter(descriptions)
        .buffer_unordered(DESCRIPTIONS)
        .try_collect()
        .await?;
    described.sort_unstable_by_key(|(index, _)| *index);
    Ok(described.into_iter().filter_map(|(_, game)| game).collect())
}

/// Every GOG product of the account's Galaxy library, once each, with its
/// record as Galaxy returned it.
async fn releases(
    api: &Api,
    access_token: &str,
    user_id: &str,
) -> Result<Vec<(Release, Value)>, GogError> {
    let mut seen = HashSet::new();
    let mut releases = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_LIBRARY_PAGES {
        let mut url = endpoint(&api.library, &["users", user_id, "releases"])?;
        if let Some(token) = &token {
            url.query_pairs_mut().append_pair("page_token", token);
        }
        let page: ReleasesPage = api.get(url, access_token).await?;
        for mut record in page.items {
            let release = Release::of(&record);
            if release.platform_id != "gog"
                || !valid_product_id(&release.external_id)
                || !seen.insert(release.external_id.clone())
            {
                continue;
            }
            // The ownership certificate stays in memory: it reaches gamesdb
            // only.
            if let Some(record) = record.as_object_mut() {
                record.remove("certificate");
            }
            releases.push((release, record));
        }
        match page.next_page_token.filter(|next| !next.is_empty()) {
            Some(next) if Some(&next) != token.as_ref() => token = Some(next),
            _ => return Ok(releases),
        }
    }
    Err(GogError::Protocol)
}

/// The game a library record names, or `None` for a product the library does
/// not show, such as a DLC.
async fn describe(
    api: &Api,
    access_token: &str,
    release: Release,
    record: Value,
) -> Result<Option<Game>, GogError> {
    let id = &release.external_id;
    let added_at = added_at(&record);
    if let Some(catalog) = gamesdb(api, access_token, &release).await? {
        let kind = catalog["type"].as_str().unwrap_or_default();
        let visible = catalog["game"]["visible_in_library"].as_bool() != Some(false);
        if !matches!(kind, "game" | "mod") || !visible {
            return Ok(None);
        }
        let systems: Vec<&str> = catalog["supported_operating_systems"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|system| system["slug"].as_str())
            .collect();
        return Ok(Some(Game {
            product_id: id.clone(),
            title: localized(&catalog["title"])
                .or_else(|| localized(&catalog["game"]["title"]))
                .unwrap_or_else(|| id.clone()),
            image_url: image(&catalog["game"]["vertical_cover"], "_glx_vertical_cover"),
            header_image_url: image(&catalog["game"]["background"], "")
                .or_else(|| image(&catalog["game"]["horizontal_artwork"], "")),
            windows: systems.contains(&"windows"),
            macos: systems.contains(&"osx"),
            info: GameInfo::of_release(&catalog),
            added_at,
            metadata: metadata(record, catalog),
        }));
    }
    // gamesdb does not know this product: the store tells what it is.
    let product = product(api, id).await?;
    if product["game_type"].as_str() != Some("game") {
        return Ok(None);
    }
    let compatibility = &product["content_system_compatibility"];
    Ok(Some(Game {
        product_id: id.clone(),
        title: product["title"].as_str().unwrap_or(id).to_owned(),
        image_url: None,
        header_image_url: product["images"]["background"]
            .as_str()
            .and_then(safe_https_url),
        windows: compatibility["windows"].as_bool() == Some(true),
        macos: compatibility["osx"].as_bool() == Some(true),
        info: GameInfo {
            release_date: product["release_date"]
                .as_str()
                .and_then(obelisk_core::date::unix_seconds),
            ..GameInfo::default()
        },
        added_at,
        metadata: metadata(record, product),
    }))
}

/// When a Galaxy library record says the account got its product: the
/// earlier of its creation and its ownership, in seconds since the Unix epoch.
fn added_at(record: &Value) -> Option<u64> {
    [&record["date_created"], &record["owned_since"]]
        .into_iter()
        .filter_map(Value::as_u64)
        .filter(|seconds| *seconds > 0)
        .min()
}

/// What the library cache keeps of a game beyond what it shows: its Galaxy
/// record and its catalog data.
fn metadata(record: Value, catalog: Value) -> Value {
    Value::Object(serde_json::Map::from_iter([
        ("release".into(), record),
        ("catalog".into(), catalog),
    ]))
}

/// The product's gamesdb release, `None` when gamesdb does not know it. A
/// failed request is tried again, as Heroic does.
async fn gamesdb(
    api: &Api,
    access_token: &str,
    release: &Release,
) -> Result<Option<Value>, GogError> {
    let url = endpoint(
        &api.gamesdb,
        &[
            "platforms",
            "gog",
            "external_releases",
            &release.external_id,
        ],
    )?;
    let request = || {
        let request = api.http.get(url.clone()).bearer_auth(access_token);
        match &release.certificate {
            Some(certificate) => request.header("X-GOG-Library-Cert", certificate),
            None => request,
        }
    };
    match send_retrying(request, 3, Duration::from_millis(500)).await? {
        Some(response) => bounded_json(response, 4 * 1024 * 1024).await.map(Some),
        None => Ok(None),
    }
}

/// The store's product, which describes every product, DLC included.
async fn product(api: &Api, id: &str) -> Result<Value, GogError> {
    let mut url = endpoint(&api.api, &["products", id])?;
    url.query_pairs_mut().append_pair("locale", "en-US");
    api.get_public(url).await
}

/// A gamesdb localized text: every language's, else English.
fn localized(value: &Value) -> Option<String> {
    ["*", "en-US"]
        .iter()
        .find_map(|key| value[key].as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A gamesdb image in `formatter`'s size, as JPEG.
fn image(value: &Value, formatter: &str) -> Option<String> {
    let format = value["url_format"].as_str()?;
    safe_https_url(
        &format
            .replace("{formatter}", formatter)
            .replace("{ext}", "jpg"),
    )
}

pub(crate) fn endpoint(base: &str, segments: &[&str]) -> Result<Url, GogError> {
    let mut url = Url::parse(base).map_err(|_| GogError::Protocol)?;
    url.path_segments_mut()
        .map_err(|_| GogError::Protocol)?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

#[cfg(test)]
mod tests;
