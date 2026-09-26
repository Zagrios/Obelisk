//! Current platform manifests for installation estimates and verified downloads.
use reqwest::Url;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::{path::Path, time::Duration};

use crate::{
    EpicError,
    auth::{self, Api, credential_free},
    client::validate_identity,
    content_manifest::Manifest,
    manifest,
};

pub(crate) mod diagnostic;
pub(crate) mod manifest_cache;

pub(crate) async fn installation_size(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
) -> Result<u64, EpicError> {
    let fetched = fetch_manifest(
        api,
        access_token,
        namespace,
        catalog_item_id,
        app_name,
        platform,
        None,
    )
    .await?;
    tokio::task::spawn_blocking(move || manifest::installation_size(&fetched.bytes))
        .await
        .map_err(|_| EpicError::Protocol)?
}

pub(crate) struct FetchedManifest {
    pub bytes: Vec<u8>,
    pub base_urls: Vec<Url>,
    pub deployment_id: Option<String>,
    pub network_bytes: u64,
    pub manifest: Option<Manifest>,
    pub cache_key: Option<manifest_cache::ManifestCacheKey>,
}

pub(crate) async fn fetch_manifest(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
    cache_directory: Option<&Path>,
) -> Result<FetchedManifest, EpicError> {
    let mut trace = diagnostic::Trace {
        platform: platform.into(),
        namespace: namespace.into(),
        catalog_item_id: catalog_item_id.into(),
        app_name: app_name.into(),
        ..Default::default()
    };
    let result = fetch_traced(
        api,
        access_token,
        namespace,
        catalog_item_id,
        app_name,
        platform,
        cache_directory,
        &mut trace,
    )
    .await;
    if let Err(error) = &result
        && !matches!(error, EpicError::Cancelled | EpicError::Network)
    {
        trace.error = format!("{error:?}");
        diagnostic::write(&trace);
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn fetch_traced(
    api: &Api,
    access_token: &str,
    namespace: &str,
    catalog_item_id: &str,
    app_name: &str,
    platform: &str,
    cache_directory: Option<&Path>,
    trace: &mut diagnostic::Trace,
) -> Result<FetchedManifest, EpicError> {
    trace.step = "identity";
    validate_identity(namespace, catalog_item_id, app_name, platform)?;
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
    let response: serde_json::Value = api.get(url, access_token).await?;
    if cfg!(debug_assertions) {
        trace.assets = Some(diagnostic::redact(&response));
    }
    trace.step = "assets response";
    let response: AssetResponse =
        serde_json::from_value(response).map_err(|_| EpicError::Protocol)?;
    trace.step = "asset count";
    let [asset] = response.elements.as_slice() else {
        return Err(EpicError::Protocol);
    };
    trace.asset_hash = asset.hash.clone();
    trace.step = "manifest count";
    if asset.manifests.is_empty() || asset.manifests.len() > 8 {
        return Err(EpicError::Protocol);
    }
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
            let app_name = app_name.to_owned();
            tokio::task::spawn_blocking(move || {
                checked_hash(&bytes, hash.as_deref())?;
                let parsed = validate_content
                    .then(|| manifest_cache::validate(&bytes, &app_name))
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
struct Asset {
    hash: Option<String>,
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
        && (hash.len() != 40
            || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            || Sha1::digest(bytes).iter().enumerate().any(|(index, byte)| {
                u8::from_str_radix(&hash[index * 2..index * 2 + 2], 16) != Ok(*byte)
            }))
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
                "Mac",
                Some(directory.path()),
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
        let result =
            installation_size(&api, "account-token", "ns", "catalog", "game/name", "Mac").await;
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
