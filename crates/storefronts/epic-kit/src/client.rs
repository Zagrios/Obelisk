//! Account-scoped library requests. Token refresh is serialized, while library
//! reads release the session lock so disconnect can invalidate them promptly.
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    sync::Arc,
    time::Instant,
};

use futures_util::{StreamExt, TryStreamExt, stream};
use obelisk_core::{
    platform::{Platform, Spelling},
    watch::Revisions,
};
use reqwest::Url;
use serde::{Deserialize, Serializer};
use tokio::sync::Mutex;

use crate::{
    EpicError,
    auth::{self, Api, Credentials, Session},
};

mod achievements;
mod cloud_api;
pub use achievements::Achievement;
mod download;
mod launch;
pub use download::{
    Installation, Prerequisite, TransferProgress, TransferState, installation_directory_name,
};
pub use launch::{InstalledLaunch, LaunchPlan, installed_catalog_item, installed_launch};

/// Epic's asset API labels for the two builds; also its receipts', folders'
/// and ids' spelling.
pub const PLATFORM: Spelling = Spelling::new("Mac", "Windows");

/// Writes a platform as Epic spells it.
pub fn write_platform<S: Serializer>(
    platform: &Platform,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    PLATFORM.serialize(*platform, serializer)
}

const MAX_LIBRARY_PAGES: usize = 1000;
const MAX_LIBRARY_RECORDS: usize = 100_000;

#[derive(Clone, Debug)]
pub struct Game {
    pub app_name: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub title: String,
    pub description: String,
    pub image_url: Option<String>,
    pub header_image_url: Option<String>,
    pub developer: String,
    pub windows: bool,
    pub macos: bool,
    /// When the game joined the account's library, in seconds since the
    /// Unix epoch.
    pub added_at: Option<u64>,
    /// Everything Epic returned for the game: `catalogItems` by catalog item
    /// ID, `libraryRecords`, and `assets` by platform.
    pub metadata: serde_json::Value,
}

/// What a game's page adds to its library entry: the build it shows before
/// any download, and its size.
#[derive(Clone, Debug, Default)]
pub struct GameDetails {
    /// The Mac build, unless a download found that its game does not run
    /// natively. Unknown when Epic could not tell.
    pub platform: Option<Platform>,
    /// Unknown when Epic serves no manifest of the build.
    pub installation_bytes: Option<u64>,
}

pub struct Client {
    downloads: download::Downloads,
    api: Api,
    session: Arc<Mutex<Option<Session>>>,
    credential_events: Revisions,
    social: crate::social::SocialService,
    installation_sizes: crate::details::InstallationSizes,
    /// Each Mac build's verdict, and the manifest the last native one read,
    /// for its download.
    build_choice: crate::details::BuildChoice,
}

impl Client {
    pub async fn sign_in(input: &str) -> Result<Self, EpicError> {
        let code = auth::authorization_code(input)?;
        let api = Api::new()?;
        let session = api.authorize(&code).await?;
        Ok(Self::from_session(api, session))
    }

    pub async fn restore(credentials: Credentials) -> Result<Self, EpicError> {
        let api = Api::new()?;
        let session = api.refresh(&credentials).await?;
        Ok(Self::from_session(api, session))
    }

    /// Creates an independent in-memory session for a detached supervisor.
    /// The exchange code is single-use and must never be persisted or logged.
    pub async fn from_exchange_code(code: &str, expected_account: &str) -> Result<Self, EpicError> {
        let api = Api::new()?;
        let session = api.exchange(code, expected_account).await?;
        Ok(Self::from_session(api, session))
    }

    pub async fn exchange_code(&self) -> Result<String, EpicError> {
        #[derive(Deserialize)]
        struct Exchange {
            code: String,
        }
        self.with_access_token(|token| async move {
            let url = endpoint(&self.api.account, &["account", "api", "oauth", "exchange"])?;
            let exchange: Exchange = self.api.get(url, &token).await?;
            if exchange.code.is_empty()
                || exchange.code.len() > 2048
                || !exchange
                    .code
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            {
                return Err(EpicError::Protocol);
            }
            Ok(exchange.code)
        })
        .await
    }

    /// Opens Epic's site signed in as the account, then goes on to
    /// `redirect`, as launchers open Epic's store: the page spends a new
    /// exchange code. The address is a secret until it loads.
    pub async fn web_sign_in_url(&self, redirect: &str) -> Result<String, EpicError> {
        let redirect = Url::parse(redirect).map_err(|_| EpicError::Protocol)?;
        if redirect.scheme() != "https"
            || !auth::credential_free(&redirect)
            || !redirect
                .host_str()
                .is_some_and(|host| host == "epicgames.com" || host.ends_with(".epicgames.com"))
        {
            return Err(EpicError::Protocol);
        }
        let code = self.exchange_code().await?;
        let mut url =
            Url::parse("https://www.epicgames.com/id/exchange").map_err(|_| EpicError::Protocol)?;
        url.query_pairs_mut()
            .append_pair("exchangeCode", &code)
            .append_pair("redirectUrl", redirect.as_str());
        Ok(url.into())
    }

    pub async fn credentials(&self) -> Result<Credentials, EpicError> {
        self.session
            .lock()
            .await
            .as_ref()
            .map(|session| session.credentials.clone())
            .ok_or(EpicError::Unauthorized)
    }

    /// Refresh before a long read so the caller can persist a rotated token
    /// without waiting for catalog enrichment to finish.
    pub async fn prepare(&self) -> Result<Credentials, EpicError> {
        self.access_token(None).await?;
        self.credentials().await
    }

    pub async fn games(&self) -> Result<Vec<Game>, EpicError> {
        self.with_access_token(|token| async move { library(&self.api, &token).await })
            .await
    }

    /// The build the game's page shows before any download, and its size:
    /// the Mac build, unless a download found that its game does not run
    /// natively. Only `platform` reads the build's files.
    pub async fn game_details(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
    ) -> Result<GameDetails, EpicError> {
        validate_identity(namespace, catalog_item_id, app_name)?;
        let shown = self
            .with_access_token(|token| async move {
                let shown = crate::details::shown_build(
                    &self.api,
                    &token,
                    namespace,
                    catalog_item_id,
                    app_name,
                    &self.build_choice,
                    &self.installation_sizes,
                )
                .await;
                // Re-raise so the funnel refreshes the token once; the funnel
                // also rejects the result if the session went away meanwhile.
                match shown {
                    Err(EpicError::Unauthorized) => Err(EpicError::Unauthorized),
                    shown => Ok(shown.ok()),
                }
            })
            .await?;
        Ok(match shown {
            Some((platform, size)) => GameDetails {
                platform: Some(platform),
                installation_bytes: size,
            },
            None => GameDetails::default(),
        })
    }

    /// The build a download of the game installs: the Mac build when its
    /// game runs natively on Apple silicon, else the Windows build.
    /// `cancel_download` stops it.
    pub async fn platform(
        &self,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
    ) -> Result<Platform, EpicError> {
        self.build_choice
            .pick(async |pick| {
                validate_identity(namespace, catalog_item_id, app_name)?;
                let pick = &pick;
                self.with_access_token(|token| async move {
                    let game = crate::details::LiveBuilds::new(
                        &self.api,
                        &token,
                        namespace,
                        catalog_item_id,
                        app_name,
                    );
                    pick.of(&game).await
                })
                .await
            })
            .await
    }

    /// Every game's playtime in seconds, by app name, in one request. A game
    /// the account never played has no entry.
    pub async fn playtimes(&self) -> Result<BTreeMap<String, u64>, EpicError> {
        let account_id = self.credentials().await?.account_id;
        let account_id = account_id.as_str();
        self.with_access_token(
            |token| async move { playtimes(&self.api, &token, account_id).await },
        )
        .await
    }

    pub(crate) fn from_session(api: Api, session: Session) -> Self {
        let social = crate::social::SocialService::new(session.credentials.account_id.clone());
        Self {
            downloads: download::Downloads::default(),
            api,
            session: Arc::new(Mutex::new(Some(session))),
            credential_events: Revisions::new(),
            social,
            installation_sizes: Default::default(),
            build_choice: Default::default(),
        }
    }

    async fn access_token(&self, rejected: Option<&str>) -> Result<String, EpicError> {
        self.social_auth().access_token(rejected).await
    }

    /// Runs `operation` with a current access token. When Epic rejects that
    /// token, it is refreshed once and the operation runs again. A result
    /// computed for a session the account discarded while the request was in
    /// flight is never returned: sign-out clears the session concurrently.
    pub(crate) async fn with_access_token<T, Fut>(
        &self,
        operation: impl Fn(String) -> Fut,
    ) -> Result<T, EpicError>
    where
        Fut: Future<Output = Result<T, EpicError>>,
    {
        let token = self.access_token(None).await?;
        let result = match operation(token.clone()).await {
            Err(EpicError::Unauthorized) => {
                let token = self.access_token(Some(&token)).await?;
                operation(token).await
            }
            result => result,
        };
        if self.session.lock().await.is_none() {
            return Err(EpicError::Unauthorized);
        }
        result
    }

    /// The catalog is presentation data. Check the current account's paginated
    /// library before asking a content or launch service about a game.
    pub(crate) async fn ensure_owned(
        &self,
        access_token: &str,
        namespace: &str,
        catalog_item_id: &str,
        app_name: &str,
    ) -> Result<(), EpicError> {
        let owned = library_items(&self.api, access_token).await?;
        if owned.iter().any(|item| {
            item.usable()
                && item.namespace == namespace
                && item.catalog_item_id == catalog_item_id
                && item.app_name == app_name
        }) {
            Ok(())
        } else {
            Err(EpicError::Service {
                message: "backend.epic.game_not_owned".into(),
            })
        }
    }

    fn social_auth(&self) -> SocialAuth {
        SocialAuth {
            api: self.api.clone(),
            session: self.session.clone(),
            credential_events: self.credential_events.clone(),
        }
    }

    pub fn start_friends(&self) {
        self.social.start(self.social_auth(), false, None);
    }
    /// Advertise the native game owned by this client's detached supervisor.
    /// The activity ID is the catalog PresenceId or namespace, not the app name.
    /// Stop the connection when that process exits or its account is revoked.
    pub fn start_game_presence(&self, activity: crate::friends::Activity) -> Result<(), EpicError> {
        if !activity.publishable() {
            return Err(EpicError::Protocol);
        }
        self.social.start(self.social_auth(), true, Some(activity));
        Ok(())
    }
    pub fn reconnect_friends(&self) {
        self.social.start(self.social_auth(), true, None);
    }
    pub fn stop_friends(&self) {
        self.social.stop();
    }
    #[cfg(test)]
    pub(crate) fn friends(&self) -> crate::friends::Snapshot {
        self.social.snapshot()
    }
    pub async fn next_friends(&self, after_revision: u64) -> crate::friends::Snapshot {
        self.social.next(after_revision).await
    }

    /// The revision of the credentials now, which grows with each rotation.
    pub fn credentials_revision(&self) -> u64 {
        self.credential_events.current()
    }

    /// A separate watch reports refresh immediately, even if the next HTTP request fails.
    pub async fn next_credentials_revision(&self, after_revision: u64) -> Option<u64> {
        self.credential_events.next(after_revision).await
    }

    /// Synchronous cancellation crosses UniFFI even when a Swift task is suspended.
    /// This does not revoke OAuth or affect a running content transfer.
    pub fn shutdown_observers(&self) {
        self.social.stop();
        self.credential_events.stop();
    }

    pub async fn disconnect(&self) -> Result<(), EpicError> {
        self.shutdown_observers();
        self.cancel_download();
        let session = self.session.lock().await.take();
        if let Some(session) = session {
            self.api.revoke(&session.access_token).await?;
        }
        Ok(())
    }
}

/// Catalog identity strings are interpolated into service URLs and hashed into
/// cache and installation paths: reject empty, oversized or NUL-bearing values.
pub(crate) fn validate_identity(
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
) -> Result<(), EpicError> {
    if [namespace, catalog_item_id, app_name]
        .iter()
        .any(|value| value.is_empty() || value.len() > 1024 || value.contains('\0'))
    {
        return Err(EpicError::Protocol);
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct SocialAuth {
    pub(crate) api: Api,
    session: Arc<Mutex<Option<Session>>>,
    credential_events: Revisions,
}
impl SocialAuth {
    pub(crate) async fn access_token(&self, rejected: Option<&str>) -> Result<String, EpicError> {
        // The renewing task owns the refresh, not the caller awaiting it. Social
        // reconnection may cancel that caller after Epic has already rotated the token.
        // Dropping a JoinHandle detaches it; the bounded OAuth request still publishes
        // credentials, and concurrent consumers reuse the session under the same lock.
        let auth = self.clone();
        let rejected = rejected.map(str::to_owned);
        tokio::spawn(async move {
            let mut guard = auth.session.lock().await;
            let session = guard.as_mut().ok_or(EpicError::Unauthorized)?;
            if session.expires_at <= Instant::now()
                || rejected.as_deref() == Some(&session.access_token)
            {
                *session = auth.api.refresh(&session.credentials).await?;
                auth.credential_events.advance();
            }
            Ok(session.access_token.clone())
        })
        .await
        .map_err(|_| EpicError::Cancelled)?
    }

    pub(crate) async fn expires_at(&self) -> Result<Instant, EpicError> {
        self.session
            .lock()
            .await
            .as_ref()
            .map(|s| s.expires_at)
            .ok_or(EpicError::Unauthorized)
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryItem {
    #[serde(default)]
    app_name: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    catalog_item_id: String,
    #[serde(default)]
    sandbox_type: String,
    /// The record as Epic returned it.
    #[serde(skip)]
    raw: serde_json::Value,
}

impl LibraryItem {
    fn parse(raw: serde_json::Value) -> Result<Self, EpicError> {
        let mut item = Self::deserialize(&raw).map_err(|_| EpicError::Protocol)?;
        item.raw = raw;
        Ok(item)
    }

    fn usable(&self) -> bool {
        !self.app_name.is_empty()
            && self.app_name != "1"
            && !self.namespace.is_empty()
            && self.namespace != "ue"
            && !self.catalog_item_id.is_empty()
            && self.sandbox_type != "PRIVATE"
    }
    fn key(&self) -> (String, String, String) {
        (
            self.namespace.clone(),
            self.catalog_item_id.clone(),
            self.app_name.clone(),
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryPage {
    records: Vec<serde_json::Value>,
    response_metadata: PageMetadata,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageMetadata {
    next_cursor: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    developer: String,
    #[serde(default)]
    key_images: Vec<CatalogImage>,
    #[serde(default)]
    categories: Vec<Category>,
    main_game_item: Option<serde_json::Value>,
    #[serde(default)]
    release_info: Vec<ReleaseInfo>,
}

#[derive(Deserialize)]
struct CatalogImage {
    #[serde(rename = "type")]
    kind: String,
    url: String,
}
#[derive(Deserialize)]
struct Category {
    path: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseInfo {
    #[serde(default)]
    app_id: String,
    #[serde(default)]
    platform: Vec<String>,
}

async fn library(api: &Api, access_token: &str) -> Result<Vec<Game>, EpicError> {
    let (windows_label, mac_label) = (
        PLATFORM.name(Platform::Windows),
        PLATFORM.name(Platform::Mac),
    );
    let (owned, windows, macos) = tokio::try_join!(
        library_items(api, access_token),
        assets(api, access_token, windows_label),
        assets(api, access_token, mac_label),
    )?;
    let excluded: HashSet<_> = owned
        .iter()
        .filter(|item| !item.usable())
        .map(LibraryItem::key)
        .collect();
    let windows_keys: HashSet<_> = windows.iter().map(LibraryItem::key).collect();
    let macos_keys: HashSet<_> = macos.iter().map(LibraryItem::key).collect();
    let mut items = BTreeMap::new();
    // Every record Epic returned for each game, by where it came from.
    let mut sources: BTreeMap<(String, String), BTreeMap<&str, Vec<serde_json::Value>>> =
        BTreeMap::new();
    let tagged = (owned.into_iter().map(|item| ("library", item)))
        .chain(windows.into_iter().map(|item| (windows_label, item)))
        .chain(macos.into_iter().map(|item| (mac_label, item)));
    for (source, mut item) in tagged {
        if item.usable() && !excluded.contains(&item.key()) {
            let records = sources
                .entry((item.namespace.clone(), item.app_name.clone()))
                .or_default()
                .entry(source)
                .or_default();
            // `sources` keeps the record: nothing reads it from the item again.
            let raw = std::mem::take(&mut item.raw);
            if !records.contains(&raw) {
                records.push(raw);
            }
            items.insert(item.key(), item);
        }
    }
    let mut namespaces: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for item in items.values() {
        namespaces
            .entry(item.namespace.clone())
            .or_default()
            .insert(item.catalog_item_id.clone());
    }
    let mut requests = Vec::new();
    for (namespace, ids) in namespaces {
        let mut ids: Vec<_> = ids.into_iter().collect();
        ids.sort();
        for batch in ids.chunks(50) {
            requests.push((namespace.clone(), batch.to_vec()));
        }
    }
    let metadata: Vec<_> = stream::iter(requests)
        .map(|(namespace, ids)| async move {
            let mut url = endpoint(
                &api.catalog,
                &[
                    "catalog",
                    "api",
                    "shared",
                    "namespace",
                    &namespace,
                    "bulk",
                    "items",
                ],
            )?;
            {
                let mut query = url.query_pairs_mut();
                query
                    .append_pair("includeDLCDetails", "true")
                    .append_pair("includeMainGameDetails", "true")
                    .append_pair("country", "US")
                    .append_pair("locale", "en");
                for id in ids {
                    query.append_pair("id", &id);
                }
            }
            let result: BTreeMap<String, serde_json::Value> = api.get(url, access_token).await?;
            let result = result
                .into_iter()
                .map(|(id, raw)| {
                    let item = CatalogItem::deserialize(&raw).map_err(|_| EpicError::Protocol)?;
                    Ok((id, (item, raw)))
                })
                .collect::<Result<BTreeMap<_, _>, EpicError>>()?;
            Ok::<_, EpicError>((namespace, result))
        })
        .buffer_unordered(6)
        .try_collect()
        .await?;
    let metadata: BTreeMap<_, _> = metadata
        .into_iter()
        .flat_map(|(namespace, catalog)| {
            catalog
                .into_iter()
                .map(move |(id, item)| ((namespace.clone(), id), item))
        })
        .collect();

    let mut games: BTreeMap<(String, String), Game> = BTreeMap::new();
    for (key, item) in items {
        let entry = metadata.get(&(item.namespace.clone(), item.catalog_item_id.clone()));
        let details = entry.map(|(details, _)| details);
        if details.is_some_and(|details| {
            details.main_game_item.is_some()
                || details.categories.iter().any(|category| {
                    matches!(
                        category.path.as_str(),
                        "mods" | "assets" | "plugins" | "projects" | "asset-format"
                    ) || category.path.starts_with("asset-format/")
                })
        }) {
            continue;
        }
        let mut windows = windows_keys.contains(&key);
        let mut macos = macos_keys.contains(&key);
        if let Some(details) = details {
            for release in &details.release_info {
                if release.app_id.is_empty() || release.app_id == item.app_name {
                    windows |= release
                        .platform
                        .iter()
                        .any(|platform| matches!(platform.as_str(), "Windows" | "Win32"));
                    macos |= release
                        .platform
                        .iter()
                        .any(|platform| matches!(platform.as_str(), "Mac" | "MacOS"));
                }
            }
            // Do not list mobile-only entries as desktop games. An empty
            // platform list is unknown and keeps older third-party titles.
            if !windows
                && !macos
                && details
                    .release_info
                    .iter()
                    .any(|release| !release.platform.is_empty())
            {
                continue;
            }
        }
        let metadata = game_metadata(
            &item,
            entry.map(|(_, raw)| raw),
            sources.get(&(item.namespace.clone(), item.app_name.clone())),
        );
        let game = Game {
            title: details
                .map(|details| details.title.as_str())
                .filter(|title| !title.is_empty())
                .unwrap_or(&item.app_name)
                .to_owned(),
            description: details
                .map(|details| details.description.clone())
                .unwrap_or_default(),
            developer: details
                .map(|details| details.developer.clone())
                .unwrap_or_default(),
            image_url: details.and_then(catalog_image),
            header_image_url: details.and_then(catalog_header_image),
            added_at: added_at(&metadata),
            metadata,
            app_name: item.app_name,
            namespace: item.namespace,
            catalog_item_id: item.catalog_item_id,
            windows,
            macos,
        };
        let identity = (game.namespace.clone(), game.app_name.clone());
        if let Some(existing) = games.get_mut(&identity) {
            existing.windows |= game.windows;
            existing.macos |= game.macos;
            if existing.title == existing.app_name && game.title != game.app_name {
                existing.title = game.title;
                existing.catalog_item_id = game.catalog_item_id;
            }
            if existing.description.is_empty() {
                existing.description = game.description;
            }
            if existing.developer.is_empty() {
                existing.developer = game.developer;
            }
            if existing.image_url.is_none() {
                existing.image_url = game.image_url;
            }
            if existing.header_image_url.is_none() {
                existing.header_image_url = game.header_image_url;
            }
            existing.added_at = existing.added_at.into_iter().chain(game.added_at).min();
            // Each catalog item of the game, under its own ID.
            if let (Some(all), Some(added)) = (
                existing.metadata["catalogItems"].as_object_mut(),
                game.metadata["catalogItems"].as_object(),
            ) {
                for (id, raw) in added {
                    all.entry(id.clone()).or_insert_with(|| raw.clone());
                }
            }
        } else {
            games.insert(identity, game);
        }
    }
    let mut games: Vec<_> = games.into_values().collect();
    games.sort_by_cached_key(|game| (game.title.to_lowercase(), game.app_name.clone()));
    Ok(games)
}

/// Everything Epic returned for a game: its catalog item, its library records
/// and its assets by platform.
fn game_metadata(
    item: &LibraryItem,
    catalog: Option<&serde_json::Value>,
    sources: Option<&BTreeMap<&str, Vec<serde_json::Value>>>,
) -> serde_json::Value {
    let records = |source: &str| {
        sources
            .and_then(|sources| sources.get(source))
            .cloned()
            .unwrap_or_default()
    };
    let mut catalog_items = serde_json::Map::new();
    if let Some(catalog) = catalog {
        catalog_items.insert(item.catalog_item_id.clone(), catalog.clone());
    }
    serde_json::json!({
        "catalogItems": catalog_items,
        "libraryRecords": records("library"),
        "assets": { "Windows": records("Windows"), "Mac": records("Mac") },
    })
}

/// When a game joined the account's library: its oldest library record's
/// acquisition, in seconds since the Unix epoch.
fn added_at(metadata: &serde_json::Value) -> Option<u64> {
    metadata["libraryRecords"]
        .as_array()?
        .iter()
        .filter_map(|record| record["acquisitionDate"].as_str())
        .filter_map(obelisk_core::date::unix_seconds)
        .min()
}

fn catalog_image(item: &CatalogItem) -> Option<String> {
    catalog_image_by_kind(
        item,
        &[
            "DieselGameBoxTall",
            "OfferImageTall",
            "DieselStoreFrontTall",
            "Thumbnail",
            "DieselGameBox",
            "OfferImageWide",
        ],
    )
}

fn catalog_header_image(item: &CatalogItem) -> Option<String> {
    catalog_image_by_kind(
        item,
        &[
            "DieselStoreFrontWide",
            "OfferImageWide",
            "DieselGameBox",
            "productFeatured",
            "Thumbnail",
        ],
    )
}

fn catalog_image_by_kind(item: &CatalogItem, kinds: &[&str]) -> Option<String> {
    kinds
        .iter()
        .flat_map(|kind| {
            item.key_images
                .iter()
                .filter(move |image| image.kind == *kind)
        })
        .find_map(|image| auth::safe_https_url(image.url.clone()))
}

async fn library_items(api: &Api, access_token: &str) -> Result<Vec<LibraryItem>, EpicError> {
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut records = Vec::new();
    for _ in 0..MAX_LIBRARY_PAGES {
        let mut url = endpoint(&api.library, &["library", "api", "public", "items"])?;
        url.query_pairs_mut().append_pair("includeMetadata", "true");
        if let Some(cursor) = &cursor {
            url.query_pairs_mut().append_pair("cursor", cursor);
        }
        let page: LibraryPage = api.get(url, access_token).await?;
        let page_records = page
            .records
            .into_iter()
            .map(LibraryItem::parse)
            .collect::<Result<Vec<_>, _>>()?;
        if page_records.iter().any(|item| {
            !item.app_name.is_empty()
                && (item.namespace.is_empty() || item.catalog_item_id.is_empty())
        }) {
            return Err(EpicError::Protocol);
        }
        records.extend(page_records);
        if records.len() > MAX_LIBRARY_RECORDS {
            return Err(EpicError::Protocol);
        }
        cursor = page
            .response_metadata
            .next_cursor
            .filter(|cursor| !cursor.is_empty());
        match &cursor {
            None => return Ok(records),
            Some(cursor) if cursor.len() > 8192 || !cursors.insert(cursor.clone()) => {
                return Err(EpicError::Protocol);
            }
            _ => {}
        }
    }
    Err(EpicError::Protocol)
}

async fn assets(
    api: &Api,
    access_token: &str,
    platform: &str,
) -> Result<Vec<LibraryItem>, EpicError> {
    let mut url = endpoint(
        &api.launcher,
        &["launcher", "api", "public", "assets", platform],
    )?;
    url.query_pairs_mut().append_pair("label", "Live");
    let assets: Vec<serde_json::Value> = api.get(url, access_token).await?;
    assets.into_iter().map(LibraryItem::parse).collect()
}

fn endpoint(base: &str, segments: &[&str]) -> Result<Url, EpicError> {
    let mut url = Url::parse(base).map_err(|_| EpicError::Protocol)?;
    url.path_segments_mut()
        .map_err(|_| EpicError::Protocol)?
        .clear()
        .extend(segments.iter().copied());
    Ok(url)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaytimeItem {
    account_id: String,
    artifact_id: String,
    total_time: u64,
}

/// The account's playtime of every game it played, by app name.
async fn playtimes(
    api: &Api,
    access_token: &str,
    account_id: &str,
) -> Result<BTreeMap<String, u64>, EpicError> {
    let url = endpoint(
        &api.library,
        &[
            "library", "api", "public", "playtime", "account", account_id, "all",
        ],
    )?;
    let records: Vec<PlaytimeItem> = api.get(url, access_token).await?;
    Ok(records
        .into_iter()
        .filter(|record| record.account_id == account_id)
        .map(|record| (record.artifact_id, record.total_time))
        .collect())
}

#[cfg(test)]
mod tests;
