//! Current platform manifests for installation estimates and verified
//! downloads, and the build a download installs.
use obelisk_core::{Platform, build_choice::GameBuilds, install::unhex};
use reqwest::Url;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, PoisonError},
    time::Duration,
};

use crate::{
    EpicError,
    auth::{self, Api, credential_free},
    client::{PLATFORM, validate_identity},
    content_manifest::Manifest,
    manifest,
};

mod build_choice;
pub(crate) mod diagnostic;
pub(crate) mod manifest_cache;

pub(crate) use build_choice::LiveBuilds;

/// What a build's manifest told, by game identity and live asset hash, which
/// names one immutable build: reopening a game's page skips its manifest.
pub(crate) struct ByBuild<T>(Mutex<HashMap<BuildKey, T>>);

/// Installation sizes.
pub(crate) type InstallationSizes = ByBuild<u64>;

type BuildKey = ([String; 4], String);

impl<T> Default for ByBuild<T> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<T: Copy> ByBuild<T> {
    fn get(&self, key: &BuildKey) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .copied()
    }

    fn insert(&self, key: BuildKey, value: T) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, value);
    }
}

/// The key of `asset`'s build of the game, when Epic names its hash.
fn build_key(identity: [&str; 4], asset: &Asset) -> Option<BuildKey> {
    asset
        .hash
        .clone()
        .map(|hash| (identity.map(str::to_owned), hash))
}

/// Each Mac build's verdict, and the manifest of the last native one, which
/// its download starts from.
pub(crate) type BuildChoice = obelisk_core::build_choice::BuildChoice<BuildKey, FetchedManifest>;

/// The build a game's page shows before any download, and its size: the
/// Mac build, unless a download found that its game does not run natively
/// and Epic has a Windows build of it. None of the build's files is read.
pub(crate) async fn shown_build(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    choice: &BuildChoice,
    sizes: &InstallationSizes,
) -> Result<(Platform, Option<u64>), EpicError> {
    let identity = |platform| [namespace, catalog_item_id, app_name, platform];
    let game = LiveBuilds::new(api, access_token, namespace, catalog_item_id, app_name);
    let offered = game.builds().await?;
    let (platform, asset) = choice
        .shown(&game, offered)
        .await
        .ok_or(EpicError::Protocol)?;
    let label = PLATFORM.name(platform);
    let mut trace = trace(namespace, catalog_item_id, app_name, label);
    let size = asset_size(api, &asset, identity(label), sizes, &mut trace).await;
    Ok((platform, reported(&mut trace, size).ok()))
}

/// The size of the build `asset` describes, from its manifest, read once
/// per build.
async fn asset_size(
    api: &Api,
    asset: &Asset,
    identity: [&str; 4],
    sizes: &InstallationSizes,
    trace: &mut diagnostic::Trace,
) -> Result<u64, EpicError> {
    let key = build_key(identity, asset);
    if let Some(size) = key.as_ref().and_then(|key| sizes.get(key)) {
        return Ok(size);
    }
    let [namespace, catalog_item_id, app_name, platform] = identity;
    let fetched = asset_manifest(
        api,
        asset,
        namespace,
        catalog_item_id,
        app_name,
        platform,
        None,
        trace,
    )
    .await?;
    let size = tokio::task::spawn_blocking(move || manifest::installation_size(&fetched.bytes))
        .await
        .map_err(|_| EpicError::Protocol)??;
    if let Some(key) = key {
        sizes.insert(key, size);
    }
    Ok(size)
}

pub(crate) struct FetchedManifest {
    pub bytes: Vec<u8>,
    pub base_urls: Vec<Url>,
    pub deployment_id: Option<String>,
    pub network_bytes: u64,
    pub manifest: Option<Manifest>,
    pub cache_key: Option<manifest_cache::ManifestCacheKey>,
}

/// The manifest of the game's live build for `platform`: the one its pick
/// read when it is that build's, else the cached or downloaded one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_manifest(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: Platform,
    cache_directory: Option<&Path>,
    take: impl FnOnce(&BuildKey) -> Option<FetchedManifest>,
) -> Result<FetchedManifest, EpicError> {
    let platform = PLATFORM.name(platform);
    let mut trace = trace(namespace, catalog_item_id, app_name, platform);
    let result = async {
        let asset = live_asset(
            api,
            access_token,
            namespace,
            catalog_item_id,
            app_name,
            platform,
            &mut trace,
        )
        .await?;
        let identity = [namespace, catalog_item_id, app_name, platform];
        if let Some(mut fetched) = build_key(identity, &asset).and_then(|key| take(&key)) {
            fetched.cache_key = cache_directory.and_then(|_| {
                manifest_cache::ManifestCacheKey::new(
                    namespace,
                    catalog_item_id,
                    app_name,
                    platform,
                    asset.hash.as_deref(),
                )
            });
            return Ok(fetched);
        }
        asset_manifest(
            api,
            &asset,
            namespace,
            catalog_item_id,
            app_name,
            platform,
            cache_directory,
            &mut trace,
        )
        .await
    }
    .await;
    reported(&mut trace, result)
}

/// The version of the game's live build, as the launcher service names it.
pub(crate) async fn live_build_version(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: Platform,
) -> Result<String, EpicError> {
    let platform = PLATFORM.name(platform);
    let mut trace = trace(namespace, catalog_item_id, app_name, platform);
    let version = live_asset(
        api,
        access_token,
        namespace,
        catalog_item_id,
        app_name,
        platform,
        &mut trace,
    )
    .await
    .and_then(|asset| {
        asset
            .build_version
            .filter(|version| !version.is_empty())
            .ok_or(EpicError::Protocol)
    });
    reported(&mut trace, version)
}

fn trace(
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
) -> diagnostic::Trace {
    diagnostic::Trace {
        platform: platform.into(),
        namespace: namespace.into(),
        catalog_item_id: catalog_item_id.into(),
        app_name: app_name.into(),
        ..Default::default()
    }
}

/// Writes `trace` when `result` failed other than by the network or a
/// cancellation.
fn reported<T>(
    trace: &mut diagnostic::Trace,
    result: Result<T, EpicError>,
) -> Result<T, EpicError> {
    if let Err(error) = &result
        && !matches!(error, EpicError::Cancelled | EpicError::Network)
    {
        trace.error = format!("{error:?}");
        diagnostic::write(trace);
    }
    result
}

/// The game's live Mac and Windows builds; none for a platform Epic has no
/// build for.
async fn live_builds(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
) -> Result<(Option<Asset>, Option<Asset>), EpicError> {
    let (mac_label, windows_label) = (
        PLATFORM.name(Platform::Mac),
        PLATFORM.name(Platform::Windows),
    );
    let mut mac_trace = trace(namespace, catalog_item_id, app_name, mac_label);
    let mut windows_trace = trace(namespace, catalog_item_id, app_name, windows_label);
    let (mac, windows) = tokio::join!(
        live_build(
            api,
            access_token,
            namespace,
            catalog_item_id,
            app_name,
            mac_label,
            &mut mac_trace
        ),
        live_build(
            api,
            access_token,
            namespace,
            catalog_item_id,
            app_name,
            windows_label,
            &mut windows_trace,
        ),
    );
    Ok((
        reported(&mut mac_trace, mac)?,
        reported(&mut windows_trace, windows)?,
    ))
}

/// The game's live build as the launcher service describes it.
async fn live_asset(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
    trace: &mut diagnostic::Trace,
) -> Result<Asset, EpicError> {
    live_build(
        api,
        access_token,
        namespace,
        catalog_item_id,
        app_name,
        platform,
        trace,
    )
    .await?
    .ok_or(EpicError::Protocol)
}

/// The game's live build for `platform`; none when Epic has no such build.
async fn live_build(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
    trace: &mut diagnostic::Trace,
) -> Result<Option<Asset>, EpicError> {
    trace.step = "identity";
    validate_identity(namespace, catalog_item_id, app_name)?;
    let mut url = Url::parse(&api.launcher).map_err(|_| EpicError::Protocol)?;
    url.path_segments_mut()
        .map_err(|_| EpicError::Protocol)?
        .clear()
        .extend([
            "launcher",
            "api",
            "public",
            "assets",
            "v2",
            "platform",
            platform,
            "namespace",
            namespace,
            "catalogItem",
            catalog_item_id,
            "app",
            app_name,
            "label",
            "Live",
        ]);
    trace.step = "assets request";
    let Some(response) = api
        .get_optional::<serde_json::Value>(url, access_token)
        .await?
    else {
        return Ok(None);
    };
    if cfg!(debug_assertions) {
        trace.assets = Some(diagnostic::redact(&response));
    }
    trace.step = "assets response";
    let response: AssetResponse =
        serde_json::from_value(response).map_err(|_| EpicError::Protocol)?;
    trace.step = "asset count";
    let asset = match <[Asset; 1]>::try_from(response.elements) {
        Ok([asset]) => asset,
        Err(elements) if elements.is_empty() => return Ok(None),
        Err(_) => return Err(EpicError::Protocol),
    };
    trace.asset_hash = asset.hash.clone();
    trace.step = "manifest count";
    if asset.manifests.is_empty() || asset.manifests.len() > 8 {
        return Err(EpicError::Protocol);
    }
    Ok(Some(asset))
}

#[allow(clippy::too_many_arguments)]
async fn asset_manifest(
    api: &Api,
    asset: &Asset,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
    cache_directory: Option<&Path>,
    trace: &mut diagnostic::Trace,
) -> Result<FetchedManifest, EpicError> {
    trace.step = "manifest cache";
    let cache_key = cache_directory.and_then(|_| {
        manifest_cache::ManifestCacheKey::new(
            namespace,
            catalog_item_id,
            app_name,
            platform,
            asset.hash.as_deref(),
        )
    });
    let mut cached = if let Some((directory, key)) = cache_directory.zip(cache_key.as_ref()) {
        let directory = directory.to_path_buf();
        let key = key.clone();
        tokio::task::spawn_blocking(move || manifest_cache::load(&directory, &key))
            .await
            .map_err(|_| EpicError::Protocol)??
    } else {
        None
    };
    let deployment_id = asset
        .sidecar
        .as_ref()
        .and_then(|sidecar| {
            sidecar
                .config
                .as_deref()
                .and_then(|config| serde_json::from_str::<serde_json::Value>(config).ok())
        })
        .and_then(|config| {
            config
                .get("deploymentId")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        });
    let mut last_error = EpicError::Protocol;
    trace.step = "manifests";
    for location in &asset.manifests {
        let attempt = trace.attempts.push_mut(diagnostic::Attempt {
            host: diagnostic::host(&location.uri),
            step: "request",
            ..Default::default()
        });
        let result = async {
            let request = manifest_request(api, location)?;
            // A signed manifest query applies to that manifest only. Chunk URLs
            // use its containing directory and never inherit account tokens.
            let base_url = request.url().join("./").map_err(|_| EpicError::Protocol)?;
            let base_urls = chunk_base_urls(api, &asset.manifests, base_url);
            if let Some(cached) = cached.take() {
                return Ok(FetchedManifest {
                    bytes: cached.bytes,
                    base_urls,
                    deployment_id: deployment_id.clone(),
                    network_bytes: 0,
                    manifest: Some(cached.manifest),
                    cache_key: cache_key.clone(),
                });
            }
            attempt.step = "download";
            let response = api
                .http
                .execute(request)
                .await
                .map_err(|_| EpicError::Network)?;
            attempt.status = Some(response.status().as_u16());
            // CDN errors do not invalidate the account's OAuth session.
            if !response.status().is_success() {
                return Err(EpicError::Protocol);
            }
            attempt.step = "body";
            let bytes = auth::bounded_bytes(response, manifest::MAX_MANIFEST_BYTES).await?;
            attempt.bytes = Some(bytes.len());
            attempt.step = "hash and parse";
            if cfg!(debug_assertions) {
                match crate::content_manifest::parse(&bytes) {
                    Ok(manifest) => attempt.manifest_app_name = Some(manifest.app_name),
                    Err(error) => attempt.parse_error = Some(format!("{error:?}")),
                }
                trace.manifest = Some(bytes.clone());
            }
            let hash = asset.hash.clone();
            let deployment_id = deployment_id.clone();
            let cache_key = cache_key.clone();
            let validate_content = cache_directory.is_some();
            tokio::task::spawn_blocking(move || {
                checked_hash(&bytes, hash.as_deref())?;
                let parsed = validate_content
                    .then(|| manifest_cache::validate(&bytes))
                    .transpose()?;
                Ok(FetchedManifest {
                    network_bytes: bytes.len() as u64,
                    bytes,
                    base_urls,
                    deployment_id,
                    manifest: parsed,
                    cache_key,
                })
            })
            .await
            .map_err(|_| EpicError::Protocol)?
        }
        .await;
        match result {
            Ok(fetched) => return Ok(fetched),
            Err(error) => {
                if let Some(attempt) = trace.attempts.last_mut() {
                    attempt.error = format!("{error:?}");
                }
                last_error = error;
            }
        }
    }
    Err(last_error)
}

#[derive(Deserialize)]
struct AssetResponse {
    elements: Vec<Asset>,
}

#[derive(Deserialize)]
pub(crate) struct Asset {
    hash: Option<String>,
    /// The build's version, as its manifest records it too.
    #[serde(default, rename = "buildVersion")]
    build_version: Option<String>,
    sidecar: Option<Sidecar>,
    manifests: Vec<ManifestLocation>,
}

#[derive(Deserialize)]
struct Sidecar {
    config: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestLocation {
    uri: String,
    #[serde(default)]
    query_params: Vec<QueryParameter>,
}

#[derive(Deserialize)]
struct QueryParameter {
    name: String,
    value: String,
}

fn chunk_base_urls(api: &Api, locations: &[ManifestLocation], selected: Url) -> Vec<Url> {
    let mut urls = vec![selected];
    for location in locations {
        if let Ok(request) = manifest_request(api, location)
            && let Ok(base_url) = request.url().join("./")
            && !urls.contains(&base_url)
        {
            urls.push(base_url);
        }
    }
    urls
}

fn manifest_request(api: &Api, location: &ManifestLocation) -> Result<reqwest::Request, EpicError> {
    if location.uri.len() > 16_384 || location.query_params.len() > 64 {
        return Err(EpicError::Protocol);
    }
    let mut url = Url::parse(&location.uri).map_err(|_| EpicError::Protocol)?;
    // Some launcher responses still advertise these CDNs with HTTP. Always
    // upgrade known legacy hosts; never send metadata requests in plaintext.
    if url.scheme() == "http"
        && matches!(
            url.host_str(),
            Some(
                "download.epicgames.com"
                    | "download2.epicgames.com"
                    | "download3.epicgames.com"
                    | "download4.epicgames.com"
                    | "epicgames-download1.akamaized.net"
            )
        )
    {
        if url.port().is_some_and(|port| port != 80) {
            return Err(EpicError::Protocol);
        }
        url.set_port(None).map_err(|_| EpicError::Protocol)?;
        url.set_scheme("https").map_err(|_| EpicError::Protocol)?;
    }
    let permitted_scheme = url.scheme() == "https"
        || cfg!(test) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if !permitted_scheme || !credential_free(&url) {
        return Err(EpicError::Protocol);
    }
    if !location.query_params.is_empty() {
        let mut query = url.query_pairs_mut();
        for parameter in &location.query_params {
            if parameter.name.is_empty()
                || parameter.name.len() > 1024
                || parameter.value.len() > 8192
            {
                return Err(EpicError::Protocol);
            }
            query.append_pair(&parameter.name, &parameter.value);
        }
    }
    if url.as_str().len() > 32_768 {
        return Err(EpicError::Protocol);
    }
    // Api's client disables redirects and stores no bearer/default credentials.
    api.http
        .get(url)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| EpicError::Protocol)
}

#[cfg(test)]
fn checked_size(bytes: &[u8], hash: Option<&str>) -> Result<u64, EpicError> {
    checked_hash(bytes, hash)?;
    manifest::installation_size(bytes)
}

fn checked_hash(bytes: &[u8], hash: Option<&str>) -> Result<(), EpicError> {
    if let Some(hash) = hash
        && !unhex::<20>(hash).is_some_and(|expected| Sha1::digest(bytes)[..] == expected)
    {
        return Err(EpicError::Protocol);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };

    #[test]
    fn cdn_manifest_requests_use_https_signed_queries_and_no_account_credentials() {
        let api = Api::new().unwrap();
        for uri in [
            "https://download.epicgames.com/game.manifest?existing=1",
            "http://download.epicgames.com/game.manifest?existing=1",
            "http://download4.epicgames.com/game.manifest?existing=1",
            "http://epicgames-download1.akamaized.net/game.manifest?existing=1",
        ] {
            let location = ManifestLocation {
                uri: uri.into(),
                query_params: vec![QueryParameter {
                    name: "Signature".into(),
                    value: "a+b/c=".into(),
                }],
            };
            let request = manifest_request(&api, &location).unwrap();
            assert_eq!(request.url().scheme(), "https");
            assert!(request.headers().get("authorization").is_none());
            assert!(request.headers().get("cookie").is_none());
            let query: std::collections::BTreeMap<_, _> = request.url().query_pairs().collect();
            assert_eq!(
                query.get("Signature").map(|value| value.as_ref()),
                Some("a+b/c=")
            );
            assert_eq!(query.get("existing").map(|value| value.as_ref()), Some("1"));
        }
        for uri in [
            "file:///tmp/game",
            "http://example.com/game",
            "https://user:password@example.com/game",
            "https://example.com/game#fragment",
        ] {
            let location = ManifestLocation {
                uri: uri.into(),
                query_params: vec![],
            };
            assert!(manifest_request(&api, &location).is_err());
        }
    }

    #[test]
    fn manifest_sha1_from_asset_response_is_verified_before_using_the_size() {
        let bytes = br#"{"FileManifestList":[{"FileChunkParts":[{"Size":"000016000000"}]}]}"#;
        let hash = "97b66a6f9bb438f612c2feff8de4b80456a6fd5b";
        assert_eq!(checked_size(bytes, Some(hash)).unwrap(), 4096);
        assert_eq!(checked_size(bytes, None).unwrap(), 4096);
        assert!(checked_size(bytes, Some("0000000000000000000000000000000000000000")).is_err());
        assert!(checked_size(bytes, Some("broken")).is_err());
    }

    #[tokio::test]
    async fn cached_manifest_uses_current_cdn_directory_and_sidecar_only() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = manifest_cache::tests::fixture("game", "game.exe");
        let hash: String = Sha1::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let key =
            manifest_cache::ManifestCacheKey::new("ns", "catalog", "game", "Mac", Some(&hash))
                .unwrap();
        manifest_cache::store(directory.path(), &key, &bytes).unwrap();
        for valid_location in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut api = Api::new().unwrap();
            api.launcher = format!("http://{}", listener.local_addr().unwrap());
            let current_directory = format!("{}/fresh/release/", api.launcher);
            let uri = if valid_location {
                format!("{current_directory}game.manifest?old-in-query=current")
            } else {
                "file:///invalid/game.manifest".into()
            };
            let mut locations = vec![serde_json::json!({"uri": uri, "queryParams": [
                {"name": "Signature", "value": "fresh-signed-secret"}
            ]})];
            if valid_location {
                locations.extend([
                    serde_json::json!({"uri": "file:///invalid/alternative.manifest"}),
                    serde_json::json!({"uri": "https://second.example.com/fresh/game.manifest?Signature=second-secret"}),
                    serde_json::json!({"uri": format!("{current_directory}duplicate.manifest?Signature=duplicate-secret")}),
                ]);
            }
            let response = serde_json::to_vec(&serde_json::json!({"elements": [{
                "hash": hash,
                "sidecar": {"config": "{\"deploymentId\":\"fresh-deployment\"}"},
                "manifests": locations,
            }]}))
            .unwrap();
            let server = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "asset request never arrived");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
                socket.write_all(&response).unwrap();
                String::from_utf8(request).unwrap()
            });
            let result = fetch_manifest(
                &api,
                "account-token",
                "ns",
                "catalog",
                "game",
                Platform::Mac,
                Some(directory.path()),
                |_| None,
            )
            .await;
            let request = server.join().unwrap();
            assert!(request.starts_with("GET /launcher/api/public/assets/v2/"));
            if valid_location {
                let fetched = result.unwrap();
                assert_eq!(fetched.network_bytes, 0);
                assert_eq!(fetched.base_urls.len(), 2);
                assert_eq!(fetched.base_urls[0].as_str(), current_directory);
                assert_eq!(
                    fetched.base_urls[1].as_str(),
                    "https://second.example.com/fresh/"
                );
                assert!(fetched.base_urls.iter().all(|url| url.query().is_none()));
                assert_eq!(fetched.deployment_id.as_deref(), Some("fresh-deployment"));
                assert_eq!(fetched.bytes, bytes);
                assert_eq!(fetched.manifest.unwrap().app_name, "game");
            } else {
                assert!(matches!(result, Err(EpicError::Protocol)));
            }
        }
    }

    #[tokio::test]
    async fn a_download_starts_from_the_manifest_its_pick_read_and_caches_it() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut api = Api::new().unwrap();
        api.launcher = format!("http://{}", listener.local_addr().unwrap());
        // Only the live asset is served: the manifest is never asked for.
        let response = serde_json::to_vec(&serde_json::json!({"elements": [{
            "hash": "0123456789abcdef0123456789abcdef01234567",
            "manifests": [{"uri": "https://download.epicgames.com/game.manifest"}],
        }]}))
        .unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "asset request never arrived");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
            socket.write_all(&response).unwrap();
        });
        let expected = (
            ["ns", "catalog", "game", "Mac"].map(str::to_owned),
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
        );
        let mut picked = Some(FetchedManifest {
            bytes: b"picked manifest".to_vec(),
            base_urls: vec![Url::parse("https://download.epicgames.com/").unwrap()],
            deployment_id: None,
            network_bytes: 15,
            manifest: None,
            cache_key: None,
        });
        let mut asked = None;
        let directory = tempfile::tempdir().unwrap();
        let fetched = fetch_manifest(
            &api,
            "account-token",
            "ns",
            "catalog",
            "game",
            Platform::Mac,
            Some(directory.path()),
            |key| {
                asked = Some(key.clone());
                picked.take()
            },
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(asked, Some(expected), "it asks for the live build's");
        assert_eq!(fetched.bytes, b"picked manifest");
        assert_eq!(fetched.network_bytes, 15, "its download counts it");
        assert!(fetched.cache_key.is_some(), "the installation caches it");
    }

    #[tokio::test]
    async fn asset_request_uses_platform_and_identity_and_preserves_unauthorized() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut api = Api::new().unwrap();
        api.launcher = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "asset request never arrived");
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut bytes = [0; 1024];
                let count = socket.read(&mut bytes).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&bytes[..count]);
            }
            socket
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let result = fetch_manifest(
            &api,
            "account-token",
            "ns",
            "catalog",
            "game/name",
            Platform::Mac,
            None,
            |_| None,
        )
        .await;
        assert!(matches!(result, Err(EpicError::Unauthorized)));
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /launcher/api/public/assets/v2/platform/Mac/namespace/ns/catalogItem/catalog/app/game%2Fname/label/Live HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer account-token\r\n")
        );
    }
}
