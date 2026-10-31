use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use serde_json::{Value, json};

use super::*;
use crate::friends::Activity;

struct Reply {
    path: String,
    status: u16,
    body: String,
    bearer: Option<&'static str>,
    delay_ms: u64,
}

fn reply(path: &str, status: u16, body: Value) -> Reply {
    Reply {
        path: path.into(),
        status,
        body: body.to_string(),
        bearer: None,
        delay_ms: 0,
    }
}

#[test]
fn a_game_joins_the_library_with_its_oldest_record() {
    let metadata = json!({ "libraryRecords": [
        { "acquisitionDate": "2022-05-20T10:00:00.000Z" },
        { "acquisitionDate": "2020-05-28T17:33:54.987Z" },
        { "acquisitionDate": "not a date" },
        {}
    ]});
    assert_eq!(added_at(&metadata), Some(1_590_687_234));
    assert_eq!(added_at(&json!({ "libraryRecords": [] })), None);
    assert_eq!(added_at(&json!({})), None);
}

struct Mock {
    api: Api,
    requests: Arc<StdMutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Mock {
    fn new(mut replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let fixture_base = base.clone();
        let handle = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture listener: {error}"),
                };
                // On macOS accepted sockets can inherit O_NONBLOCK.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                // Oversized responses can be rejected before the client drains
                // the socket. Bound the fixture writer so Drop cannot deadlock.
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                let mut body_length = None;
                while let Ok(count) = socket.read(&mut buffer) {
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(index) = bytes.windows(4).position(|slice| slice == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..index]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|length| length.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        body_length = Some(index + 4 + length);
                    }
                    if body_length.is_some_and(|length| bytes.len() >= length) {
                        break;
                    }
                }
                if bytes.is_empty() {
                    continue;
                }
                let request = String::from_utf8(bytes).unwrap();
                let target = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let path = target.split('?').next().unwrap();
                let index = replies
                    .iter()
                    .position(|reply| {
                        reply.path == path
                            && reply
                                .bearer
                                .is_none_or(|token| request.contains(&format!("Bearer {token}")))
                    })
                    .unwrap_or_else(|| panic!("unexpected fixture request {path}"));
                let mut response = replies.remove(index);
                response.body = response.body.replace("FIXTURE_BASE", &fixture_base);
                captured.lock().unwrap().push(request);
                thread::sleep(Duration::from_millis(response.delay_ms));
                let output = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    response.body.len(),
                    response.body
                );
                let _ = socket.write_all(output.as_bytes());
            }
        });
        let mut api = Api::new().unwrap();
        api.account = base.clone();
        api.store = base.clone();
        api.library = base.clone();
        api.launcher = base.clone();
        api.catalog = base.clone();
        api.ecommerce = base;
        Self {
            api,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn client(&self, expired: bool) -> Client {
        Client::from_session(
            self.api.clone(),
            Session {
                credentials: credentials(),
                access_token: "access-old".into(),
                expires_at: if expired {
                    Instant::now()
                } else {
                    Instant::now() + Duration::from_secs(3600)
                },
            },
        )
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.take().unwrap().join().unwrap();
    }
}

fn credentials() -> Credentials {
    Credentials {
        account_id: "account-a".into(),
        display_name: "Player".into(),
        refresh_token: "refresh-old".into(),
    }
}

fn token(account: &str) -> Value {
    json!({"access_token":"access-new", "refresh_token":"refresh-new", "account_id":account, "displayName":"Player", "expires_in":3600})
}

#[tokio::test]
async fn web_sign_in_spends_an_exchange_code_on_epics_site_only() {
    let mock = Mock::new(vec![reply(
        "/account/api/oauth/exchange",
        200,
        json!({"code":"single-use-code"}),
    )]);
    let client = mock.client(false);
    for elsewhere in [
        "http://store.epicgames.com/",
        "https://example.com/",
        "https://epicgames.com.example.com/",
        "https://user@store.epicgames.com/",
    ] {
        assert!(matches!(
            client.web_sign_in_url(elsewhere).await,
            Err(EpicError::Protocol)
        ));
    }
    let url = client
        .web_sign_in_url("https://store.epicgames.com/fr/")
        .await
        .unwrap();
    assert_eq!(
        url,
        "https://www.epicgames.com/id/exchange?exchangeCode=single-use-code&redirectUrl=https%3A%2F%2Fstore.epicgames.com%2Ffr%2F"
    );
    assert_eq!(mock.requests().len(), 1, "No code for another site");
}

#[tokio::test]
async fn supervisor_exchange_creates_a_distinct_session_without_rotating_parent_credentials() {
    let mock = Mock::new(vec![
        reply(
            "/account/api/oauth/exchange",
            200,
            json!({"code":"single-use-code"}),
        ),
        reply("/account/api/oauth/token", 200, token("account-a")),
    ]);
    let parent = mock.client(false);
    let code = parent.exchange_code().await.unwrap();
    let session = mock.api.exchange(&code, "account-a").await.unwrap();
    let supervisor = Client::from_session(mock.api.clone(), session);
    assert_eq!(
        parent.credentials().await.unwrap().refresh_token,
        "refresh-old"
    );
    assert_eq!(
        supervisor.credentials().await.unwrap().refresh_token,
        "refresh-new"
    );
    let requests = mock.requests();
    assert!(requests[0].contains("Bearer access-old"));
    assert!(requests[1].contains("grant_type=exchange_code"));
    assert!(requests[1].contains("exchange_code=single-use-code"));
    assert!(requests[1].contains("token_type=eg1"));
    assert!(!requests[1].contains("refresh-old"));
    assert!(!requests[1].contains("Bearer access-old"));
}

#[tokio::test]
async fn supervisor_exchange_rejects_and_revokes_an_unexpected_account() {
    let mock = Mock::new(vec![
        reply("/account/api/oauth/token", 200, token("account-b")),
        reply(
            "/account/api/oauth/sessions/kill/access-new",
            204,
            json!(null),
        ),
    ]);
    assert!(matches!(
        mock.api.exchange("single-use-code", "account-a").await,
        Err(EpicError::Unauthorized)
    ));
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("DELETE "));
    assert!(requests[1].contains("Bearer access-new"));
}

#[tokio::test]
async fn cloud_configuration_requires_ownership_and_reads_the_rules_of_the_platform_build() {
    for (platform, attributes, expected) in [
        (Platform::Mac, json!({}), "none"),
        (
            Platform::Mac,
            json!({"CloudSaveFolder_MAC":{"value":"{Home}/Game"},"CloudExcludeList":{"value":"*.log"}}),
            "some",
        ),
        (
            Platform::Mac,
            json!({"CloudSaveFolder_MAC":{"value":false}}),
            "error",
        ),
        (
            Platform::Mac,
            json!({"CloudSaveFolder":{"value":"{AppData}/Game"}}),
            "none",
        ),
        (
            Platform::Windows,
            json!({"CloudSaveFolder":{"value":"{AppData}/Game"}}),
            "some",
        ),
        (
            Platform::Windows,
            json!({"CloudSaveFolder_MAC":{"value":"{Home}/Game"}}),
            "none",
        ),
        (
            Platform::Windows,
            json!({"CloudSaveFolder":{"value":""}}),
            "error",
        ),
    ] {
        let mock = Mock::new(vec![
            reply(
                "/library/api/public/items",
                200,
                json!({"records":[item("app", "item")], "responseMetadata":{"nextCursor":null}}),
            ),
            reply(
                "/catalog/api/shared/namespace/space/bulk/items",
                200,
                json!({"item":{"customAttributes":attributes}}),
            ),
        ]);
        let result = mock
            .client(false)
            .cloud_configuration("space", "item", "app", platform)
            .await;
        match expected {
            "none" => assert!(result.unwrap().is_none()),
            "some" => assert!(result.unwrap().is_some()),
            _ => assert!(result.is_err()),
        }
    }
    let mock = Mock::new(vec![reply(
        "/library/api/public/items",
        200,
        json!({"records":[],"responseMetadata":{"nextCursor":null}}),
    )]);
    assert!(
        mock.client(false)
            .cloud_configuration("space", "item", "app", Platform::Mac)
            .await
            .is_err()
    );
    assert_eq!(mock.requests().len(), 1);
}

fn item(app: &str, catalog: &str) -> Value {
    json!({"appName":app,"namespace":"space","catalogItemId":catalog,"sandboxType":"PUBLIC"})
}

fn empty_assets() -> Vec<Reply> {
    vec![
        reply("/launcher/api/public/assets/Windows", 200, json!([])),
        reply("/launcher/api/public/assets/Mac", 200, json!([])),
    ]
}

const PLAYTIME_PATH: &str = "/library/api/public/playtime/account/account-a/all";
const MANIFEST_PATH: &str = "/launcher/api/public/assets/v2/platform/Mac/namespace/space/catalogItem/base/app/game/label/Live";
const WINDOWS_MANIFEST_PATH: &str = "/launcher/api/public/assets/v2/platform/Windows/namespace/space/catalogItem/base/app/game/label/Live";

fn live_asset(manifest: &str) -> Value {
    json!({"elements":[{"manifests":[{"uri":format!("FIXTURE_BASE/{manifest}"),"queryParams":[]}]}]})
}

#[tokio::test]
async fn game_details_name_the_build_a_download_installs_whose_manifest_may_be_unavailable() {
    // Epic has no Mac build, and the Windows build's manifest is gone.
    let mock = Mock::new(vec![
        reply(MANIFEST_PATH, 404, json!({})),
        reply(WINDOWS_MANIFEST_PATH, 200, live_asset("windows.manifest")),
        reply("/windows.manifest", 404, json!({})),
    ]);
    let details = mock
        .client(false)
        .game_details("space", "base", "game")
        .await
        .unwrap();
    assert_eq!(details.platform, Some(Platform::Windows));
    assert_eq!(details.installation_bytes, None);
    assert_eq!(
        mock.requests().len(),
        3,
        "the library already read every playtime"
    );
    assert!(
        mock.requests()
            .iter()
            .filter(|request| request.contains("/launcher/"))
            .all(|request| request.contains("Bearer access-old"))
    );
    // Without any build, nothing is installable.
    let mock = Mock::new(vec![
        reply(MANIFEST_PATH, 200, json!({"elements":[]})),
        reply(WINDOWS_MANIFEST_PATH, 404, json!({})),
    ]);
    let details = mock
        .client(false)
        .game_details("space", "base", "game")
        .await
        .unwrap();
    assert_eq!(details.platform, None);
    assert_eq!(details.installation_bytes, None);
}

/// A Mac build whose launch executable its manifest does not hold.
fn mac_build_without_program() -> Vec<Reply> {
    use sha1::{Digest, Sha1};
    let empty_hash: String = Sha1::digest([])
        .iter()
        .map(|byte| format!("{byte:03}"))
        .collect();
    vec![
        reply(MANIFEST_PATH, 200, live_asset("mac.manifest")),
        reply(
            "/mac.manifest",
            200,
            json!({
                "AppNameString": "game", "BuildVersionString": "1.0", "LaunchExeString":"Game.app",
                "FileManifestList":[{"Filename":"Game.app/Contents/empty", "FileHash":empty_hash, "FileChunkParts":[]}],
                "ChunkFilesizeList":{}, "ChunkHashList":{}, "ChunkShaList":{}, "DataGroupList":{}
            }),
        ),
    ]
}

#[tokio::test]
async fn a_game_s_page_shows_its_mac_build_until_a_download_found_it_is_not_native() {
    use sha1::{Digest, Sha1};
    // A Mac build without its program, whose manifest the asset's hash names.
    let manifest = mac_build_without_program().remove(1).body;
    let hash: String = Sha1::digest(manifest.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut asset = live_asset("mac.manifest");
    asset["elements"][0]["hash"] = json!(hash);
    let mut found = reply("/mac.manifest", 200, Value::Null);
    found.body = manifest;
    let mock = Mock::new(vec![
        // The page: both builds, then the Mac build's size, unknown here.
        reply(MANIFEST_PATH, 200, asset.clone()),
        reply(WINDOWS_MANIFEST_PATH, 200, live_asset("windows.manifest")),
        reply("/mac.manifest", 404, json!({})),
        // The download's pick reads the Mac build's manifest.
        reply(MANIFEST_PATH, 200, asset.clone()),
        reply(WINDOWS_MANIFEST_PATH, 200, live_asset("windows.manifest")),
        found,
        // The page again.
        reply(MANIFEST_PATH, 200, asset),
        reply(WINDOWS_MANIFEST_PATH, 200, live_asset("windows.manifest")),
        reply("/windows.manifest", 404, json!({})),
    ]);
    let client = mock.client(false);
    let shown = client.game_details("space", "base", "game").await.unwrap();
    assert_eq!(shown.platform, Some(Platform::Mac));
    assert_eq!(
        client.platform("space", "base", "game").await.unwrap(),
        Platform::Windows
    );
    let shown = client.game_details("space", "base", "game").await.unwrap();
    assert_eq!(shown.platform, Some(Platform::Windows));
    assert_eq!(mock.requests().len(), 9);
}

#[tokio::test]
async fn a_mac_build_that_does_not_run_natively_leaves_the_download_to_the_windows_build() {
    let mut replies = mac_build_without_program();
    replies.push(reply(
        WINDOWS_MANIFEST_PATH,
        200,
        live_asset("windows.manifest"),
    ));
    let mock = Mock::new(replies);
    let client = mock.client(false);
    assert_eq!(
        client.platform("space", "base", "game").await.unwrap(),
        Platform::Windows
    );
    // Without a Windows build, the game cannot be installed.
    let mut replies = mac_build_without_program();
    replies.push(reply(WINDOWS_MANIFEST_PATH, 404, json!({})));
    let mock = Mock::new(replies);
    let client = mock.client(false);
    assert!(matches!(
        client.platform("space", "base", "game").await,
        Err(EpicError::Service { message }) if message == "backend.install.macBuildNotNative"
    ));
}

#[tokio::test]
async fn playtimes_read_every_game_of_the_account_in_one_request() {
    let mock = Mock::new(vec![reply(
        PLAYTIME_PATH,
        200,
        json!([
            {"accountId":"account-b","artifactId":"game","totalTime":999},
            {"accountId":"account-a","artifactId":"other","totalTime":123},
            {"accountId":"account-a","artifactId":"game","totalTime":68363},
            {"accountId":"account-a","artifactId":"idle","totalTime":0}
        ]),
    )]);
    let playtimes = mock.client(false).playtimes().await.unwrap();
    assert_eq!(
        playtimes.into_iter().collect::<Vec<_>>(),
        [
            ("game".to_string(), 68363),
            ("idle".to_string(), 0),
            ("other".to_string(), 123)
        ]
    );
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn game_details_retry_auth_once_and_keep_rotated_credentials() {
    for status in [404, 401] {
        let mut rejected = reply(MANIFEST_PATH, 401, json!({}));
        rejected.bearer = Some("access-old");
        let mut retried = reply(MANIFEST_PATH, status, json!({}));
        retried.bearer = Some("access-new");
        // Epic is asked for both builds at once.
        let mut windows = reply(WINDOWS_MANIFEST_PATH, 404, json!({}));
        windows.bearer = Some("access-old");
        let mut windows_retried = reply(WINDOWS_MANIFEST_PATH, 404, json!({}));
        windows_retried.bearer = Some("access-new");
        let mock = Mock::new(vec![
            reply("/account/api/oauth/token", 200, token("account-a")),
            rejected,
            retried,
            windows,
            windows_retried,
        ]);
        let client = mock.client(false);
        let result = client.game_details("space", "base", "game").await;
        if status == 404 {
            assert_eq!(result.unwrap().installation_bytes, None);
        } else {
            assert!(matches!(result, Err(EpicError::Unauthorized)));
        }
        assert_eq!(
            client.credentials().await.unwrap().refresh_token,
            "refresh-new"
        );
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn disconnect_discards_in_flight_game_details() {
    let mut slow = reply(MANIFEST_PATH, 404, json!({}));
    slow.delay_ms = 200;
    let mock = Mock::new(vec![
        slow,
        reply(WINDOWS_MANIFEST_PATH, 404, json!({})),
        reply(
            "/account/api/oauth/sessions/kill/access-old",
            204,
            Value::Null,
        ),
    ]);
    let client = Arc::new(mock.client(false));
    let reader = client.clone();
    let reading = tokio::spawn(async move { reader.game_details("space", "base", "game").await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while mock.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    client.disconnect().await.unwrap();
    assert!(matches!(
        reading.await.unwrap(),
        Err(EpicError::Unauthorized)
    ));
}

#[tokio::test]
async fn authorization_and_restore_use_form_grants_and_rotated_identity() {
    let mock = Mock::new(vec![
        reply("/account/api/oauth/token", 200, token("account-a")),
        reply("/account/api/oauth/token", 200, token("account-a")),
    ]);
    let session = mock.api.authorize("code_123").await.unwrap();
    assert_eq!(session.credentials.refresh_token, "refresh-new");
    mock.api.refresh(&credentials()).await.unwrap();
    let requests = mock.requests();
    assert!(requests[0].contains("grant_type=authorization_code&code=code_123&token_type=eg1"));
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: basic ")
    );
    assert!(
        requests[1].contains("grant_type=refresh_token&refresh_token=refresh-old&token_type=eg1")
    );
}

#[tokio::test]
async fn mismatched_restore_account_is_rejected_and_new_token_revoked() {
    let mock = Mock::new(vec![
        reply("/account/api/oauth/token", 200, token("account-b")),
        reply(
            "/account/api/oauth/sessions/kill/access-new",
            204,
            Value::Null,
        ),
    ]);
    assert!(matches!(
        mock.api.refresh(&credentials()).await,
        Err(EpicError::Unauthorized)
    ));
    assert!(mock.requests()[1].starts_with("DELETE "));
}

#[tokio::test]
async fn rotated_credentials_survive_a_failed_library_refresh() {
    let mut replies = empty_assets();
    replies.extend([
        reply("/account/api/oauth/token", 200, token("account-a")),
        reply(
            "/library/api/public/items",
            503,
            json!({"errorMessage":"secret-refresh-old"}),
        ),
    ]);
    let mock = Mock::new(replies);
    let client = mock.client(true);
    let error = client.games().await.unwrap_err();
    assert!(matches!(error, EpicError::Service { .. }));
    assert!(!error.to_string().contains("secret"));
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "refresh-new"
    );
}

#[tokio::test]
async fn rejected_access_token_is_refreshed_only_once() {
    let mock = Mock::new(vec![reply(
        "/account/api/oauth/token",
        200,
        token("account-a"),
    )]);
    let client = mock.client(false);
    assert_eq!(
        client.access_token(Some("access-old")).await.unwrap(),
        "access-new"
    );
    assert_eq!(
        client.access_token(Some("access-old")).await.unwrap(),
        "access-new"
    );
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn pages_merge_owned_assets_and_metadata_without_duplicate_games() {
    let mut private = item("private", "private");
    private["sandboxType"] = json!("PRIVATE");
    let mock = Mock::new(vec![
        reply(
            "/library/api/public/items",
            200,
            json!({"records":[item("game","base"),item("game","alternate"),item("dlc","dlc"),private,item("asset","asset"),item("mobile","mobile")],"responseMetadata":{"nextCursor":"page2"}}),
        ),
        reply(
            "/library/api/public/items",
            200,
            json!({"records":[item("game","base"),item("missing","missing")],"responseMetadata":{}}),
        ),
        reply(
            "/launcher/api/public/assets/Windows",
            200,
            json!([item("game", "base")]),
        ),
        reply(
            "/launcher/api/public/assets/Mac",
            200,
            json!([item("game", "alternate")]),
        ),
        reply(
            "/catalog/api/shared/namespace/space/bulk/items",
            200,
            json!({
                "base":{"title":"A game","description":"Game description","developer":"Studio","keyImages":[{"type":"DieselGameBoxTall","url":"file:///bad"},{"type":"OfferImageTall","url":"https://cdn.epicgames.com/cover.jpg"},{"type":"DieselStoreFrontWide","url":"https://cdn.epicgames.com/header.jpg"}],"customAttributes":{"CanRunOffline":{"type":"STRING","value":"true"}}},
                "alternate":{"title":"A game"},
                "dlc":{"title":"DLC","mainGameItem":{"id":"base"}},
                "asset":{"title":"Editor asset","categories":[{"path":"asset-format"}]},
                "mobile":{"title":"Mobile","releaseInfo":[{"appId":"mobile","platform":["Android"]}]}
            }),
        ),
    ]);
    let games = mock.client(false).games().await.unwrap();
    assert_eq!(games.len(), 2);
    assert_eq!(games[0].title, "A game");
    assert!(games[0].windows && games[0].macos);
    assert_eq!(
        games[0].image_url.as_deref(),
        Some("https://cdn.epicgames.com/cover.jpg")
    );
    assert_eq!(
        games[0].header_image_url.as_deref(),
        Some("https://cdn.epicgames.com/header.jpg")
    );
    // Everything Epic returned for the game stays available, unknown fields too.
    let metadata = &games[0].metadata;
    assert_eq!(metadata["catalogItems"]["base"]["developer"], "Studio");
    assert_eq!(
        metadata["catalogItems"]["base"]["customAttributes"]["CanRunOffline"]["value"],
        "true"
    );
    assert_eq!(metadata["catalogItems"]["alternate"]["title"], "A game");
    assert_eq!(
        metadata["libraryRecords"],
        json!([item("game", "base"), item("game", "alternate")]),
        "Each record once, though Epic listed one on both pages"
    );
    assert_eq!(metadata["assets"]["Windows"], json!([item("game", "base")]));
    assert_eq!(
        metadata["assets"]["Mac"],
        json!([item("game", "alternate")])
    );
    assert_eq!(games[1].app_name, "missing");
    assert!(games[1].image_url.is_none());
    assert!(games[1].header_image_url.is_none());
    assert!(
        games[1].metadata["catalogItems"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert_eq!(mock.requests().len(), 5);
    assert!(
        mock.requests()
            .iter()
            .any(|request| request.contains("cursor=page2"))
    );
    assert!(
        mock.requests()
            .iter()
            .all(|request| request.contains("Bearer access-old"))
    );
}

#[test]
fn cover_and_header_images_use_independent_priorities() {
    let details: CatalogItem = serde_json::from_value(json!({"keyImages":[
        {"type":"Thumbnail","url":"https://cdn.epicgames.com/thumbnail.jpg"},
        {"type":"OfferImageWide","url":"https://cdn.epicgames.com/offer-wide.jpg"},
        {"type":"DieselGameBoxTall","url":"https://cdn.epicgames.com/portrait.jpg"},
        {"type":"DieselStoreFrontWide","url":"https://cdn.epicgames.com/header.jpg"}
    ]}))
    .unwrap();
    assert_eq!(
        catalog_image(&details).as_deref(),
        Some("https://cdn.epicgames.com/portrait.jpg")
    );
    assert_eq!(
        catalog_header_image(&details).as_deref(),
        Some("https://cdn.epicgames.com/header.jpg")
    );
}

#[test]
fn header_images_fall_back_through_supported_types_without_using_unsafe_urls() {
    let kinds = [
        "DieselStoreFrontWide",
        "OfferImageWide",
        "DieselGameBox",
        "productFeatured",
        "Thumbnail",
    ];
    for (index, expected) in kinds.iter().enumerate() {
        let mut images = vec![
            json!({"type":"DieselStoreFrontWide","url":"file:///private/header.jpg"}),
            json!({"type":"DieselStoreFrontWide","url":"http://cdn.epicgames.com/header.jpg"}),
            json!({"type":"DieselStoreFrontWide","url":"https://user:secret@cdn.epicgames.com/header.jpg"}),
        ];
        images.extend(kinds[index..].iter().rev().map(|kind| {
            json!({
                "type":kind, "url":format!("https://cdn.epicgames.com/{kind}.jpg")
            })
        }));
        let details: CatalogItem = serde_json::from_value(json!({"keyImages":images})).unwrap();
        assert_eq!(
            catalog_header_image(&details),
            Some(format!("https://cdn.epicgames.com/{expected}.jpg"))
        );
    }
}

#[test]
fn missing_safe_landscape_keeps_portrait_available_for_ui_fallback() {
    let details: CatalogItem = serde_json::from_value(json!({"keyImages":[
        {"type":"DieselGameBoxTall","url":"https://cdn.epicgames.com/portrait.jpg"},
        {"type":"DieselStoreFrontWide","url":"file:///private/header.jpg"},
        {"type":"OfferImageWide","url":"https://user@cdn.epicgames.com/header.jpg"}
    ]}))
    .unwrap();
    assert!(catalog_header_image(&details).is_none());
    assert_eq!(
        catalog_image(&details).as_deref(),
        Some("https://cdn.epicgames.com/portrait.jpg")
    );
    assert!(catalog_header_image(&CatalogItem::default()).is_none());
    assert!(catalog_image(&CatalogItem::default()).is_none());
}

#[tokio::test]
async fn repeated_cursor_fails_without_returning_a_partial_library() {
    let mock = Mock::new(vec![
        reply(
            "/library/api/public/items",
            200,
            json!({"records":[],"responseMetadata":{"nextCursor":"same"}}),
        ),
        reply(
            "/library/api/public/items",
            200,
            json!({"records":[],"responseMetadata":{"nextCursor":"same"}}),
        ),
    ]);
    assert!(matches!(
        library_items(&mock.api, "access").await,
        Err(EpicError::Protocol)
    ));
    assert_eq!(mock.requests().len(), 2);
}

#[tokio::test]
async fn logout_clears_local_session_even_when_revocation_fails() {
    let mock = Mock::new(vec![reply(
        "/account/api/oauth/sessions/kill/access-old",
        503,
        json!({"errorMessage":"access-old"}),
    )]);
    let client = mock.client(false);
    let error = client.disconnect().await.unwrap_err();
    assert!(!error.to_string().contains("access-old"));
    assert!(matches!(
        client.credentials().await,
        Err(EpicError::Unauthorized)
    ));
    assert!(matches!(client.games().await, Err(EpicError::Unauthorized)));
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn oversized_token_response_is_rejected() {
    let mock = Mock::new(vec![Reply {
        path: "/account/api/oauth/token".into(),
        status: 200,
        body: " ".repeat(1024 * 1024 + 1),
        bearer: None,
        delay_ms: 0,
    }]);
    assert!(matches!(
        mock.api.authorize("code").await,
        Err(EpicError::Protocol)
    ));
}

#[tokio::test]
async fn prepare_returns_rotated_credentials_before_any_library_request() {
    let mock = Mock::new(vec![reply(
        "/account/api/oauth/token",
        200,
        token("account-a"),
    )]);
    let credentials = mock.client(true).prepare().await.unwrap();
    assert_eq!(credentials.refresh_token, "refresh-new");
    assert_eq!(mock.requests().len(), 1);
}

#[tokio::test]
async fn library_retries_unauthorized_once_and_preserves_the_new_refresh_token() {
    for retry_status in [200, 401] {
        let mut replies = vec![reply("/account/api/oauth/token", 200, token("account-a"))];
        for path in [
            "/library/api/public/items",
            "/launcher/api/public/assets/Windows",
            "/launcher/api/public/assets/Mac",
        ] {
            let mut rejected = reply(path, 401, json!({}));
            rejected.bearer = Some("access-old");
            replies.push(rejected);
            let body = if path.starts_with("/library") {
                json!({"records":[],"responseMetadata":{}})
            } else {
                json!([])
            };
            let mut retried = reply(path, retry_status, body);
            retried.bearer = Some("access-new");
            replies.push(retried);
        }
        let mock = Mock::new(replies);
        let client = mock.client(false);
        let result = client.games().await;
        if retry_status == 200 {
            assert!(result.unwrap().is_empty());
        } else {
            assert!(matches!(result, Err(EpicError::Unauthorized)));
        }
        assert_eq!(
            client.credentials().await.unwrap().refresh_token,
            "refresh-new"
        );
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|request| request.starts_with("POST /account/api/oauth/token "))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn disconnect_invalidates_a_library_read_while_http_is_still_in_flight() {
    let mut slow = reply(
        "/library/api/public/items",
        200,
        json!({"records":[],"responseMetadata":{}}),
    );
    slow.delay_ms = 300;
    let mut replies = empty_assets();
    replies.extend([
        slow,
        reply(
            "/account/api/oauth/sessions/kill/access-old",
            204,
            Value::Null,
        ),
    ]);
    let mock = Mock::new(replies);
    let client = Arc::new(mock.client(false));
    let reader = client.clone();
    let reading = tokio::spawn(async move { reader.games().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !mock
            .requests()
            .iter()
            .any(|request| request.starts_with("GET /library/"))
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let disconnecting = client.clone();
    let disconnect = tokio::spawn(async move { disconnecting.disconnect().await });
    tokio::time::timeout(Duration::from_millis(100), async {
        while client.credentials().await.is_ok() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("logout must not wait for the library HTTP response");
    disconnect.await.unwrap().unwrap();
    assert!(matches!(
        reading.await.unwrap(),
        Err(EpicError::Unauthorized)
    ));
}

#[tokio::test]
async fn malformed_game_record_cannot_silently_replace_the_cached_library() {
    let mock = Mock::new(vec![reply(
        "/library/api/public/items",
        200,
        json!({"records":[{"appName":"game"}],"responseMetadata":{}}),
    )]);
    assert!(matches!(
        library_items(&mock.api, "access").await,
        Err(EpicError::Protocol)
    ));
}

fn download_replies(app_name: &str) -> Vec<Reply> {
    use sha1::{Digest, Sha1};
    let empty_hash: String = Sha1::digest([])
        .iter()
        .map(|byte| format!("{byte:03}"))
        .collect();
    vec![
        reply(
            "/library/api/public/items",
            200,
            json!({"records":[item("game", "base")],"responseMetadata":{}}),
        ),
        reply(
            MANIFEST_PATH,
            200,
            json!({"elements":[{"manifests":[{"uri":"FIXTURE_BASE/game.manifest","queryParams":[{"name":"Signature","value":"manifest-only"}]}],"sidecar":{"config":json!({"deploymentId":"deployment-fixture"}).to_string()}}]}),
        ),
        reply(
            "/game.manifest",
            200,
            json!({
                "AppNameString": app_name, "BuildVersionString": "1.0", "LaunchExeString":"Game.app",
                "FileManifestList":[{"Filename":"Game.app/Contents/empty", "FileHash":empty_hash, "FileChunkParts":[]}],
                "ChunkFilesizeList":{}, "ChunkHashList":{}, "ChunkShaList":{}, "DataGroupList":{}
            }),
        ),
    ]
}

#[tokio::test]
async fn social_overlay_uses_the_public_windows_asset_and_verified_existing_files() {
    use sha1::{Digest, Sha1};
    let blob = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|byte| format!("{byte:03}"))
            .collect::<String>()
    };
    let data = b"overlay fixture";
    let guid = "1122AABB3344CCDD5566EEFF77880099";
    let files = [
        "EOSOVH-Win64-Shipping.dll",
        "EOSOverlayRenderer-Win64-Shipping.exe",
    ];
    let path = "/launcher/api/public/assets/v2/platform/Windows/namespace/302e5ede476149b1bc3e4fe6ae45e50e/catalogItem/cc15684f44d849e89e9bf4cec0508b68/app/98bc04bc842e4906993fd6d6644ffb8d/label/Live";
    let mock = Mock::new(vec![
        reply(
            path,
            200,
            json!({"elements":[{"manifests":[{"uri":"FIXTURE_BASE/overlay.manifest"}]}]}),
        ),
        reply(
            "/overlay.manifest",
            200,
            json!({
                "AppNameString":"EOSOverlay", "BuildVersionString":"1", "LaunchExeString":files[1],
                "FileManifestList": files.iter().map(|name| json!({
                    "Filename":name, "FileHash":blob(&Sha1::digest(data)),
                    "FileChunkParts":[{"Guid":guid,"Offset":blob(&0_u32.to_le_bytes()),"Size":blob(&(data.len() as u32).to_le_bytes())}]
                })).collect::<Vec<_>>(),
                "ChunkFilesizeList":{guid:blob(&100_u64.to_le_bytes())},
                "ChunkHashList":{guid:blob(&1_u64.to_le_bytes())},
            "ChunkShaList":{guid:obelisk_core::install::hex(&Sha1::digest(data))}, "DataGroupList":{guid:"000"}
            }),
        ),
    ]);
    let parent = tempfile::tempdir().unwrap();
    let overlay = parent.path().join("EOSOverlay");
    std::fs::create_dir(&overlay).unwrap();
    for file in files {
        std::fs::write(overlay.join(file), data).unwrap();
    }
    let installed = mock
        .client(false)
        .install_social_overlay(parent.path())
        .await
        .unwrap();
    assert_eq!(installed, overlay.canonicalize().unwrap());
    assert!(installed.join(".obelisk-install.json").is_file());
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        2,
        "Verified files need neither a library entitlement nor chunk downloads"
    );
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer access-old")
    );
    assert!(!requests[1].to_ascii_lowercase().contains("authorization:"));
}

#[tokio::test]
async fn an_incomplete_social_overlay_is_never_published_as_installed() {
    let mut replies = download_replies("EOSOverlay");
    replies.remove(0); // A launcher component has no library entitlement.
    replies[0].path = "/launcher/api/public/assets/v2/platform/Windows/namespace/302e5ede476149b1bc3e4fe6ae45e50e/catalogItem/cc15684f44d849e89e9bf4cec0508b68/app/98bc04bc842e4906993fd6d6644ffb8d/label/Live".into();
    let mock = Mock::new(replies);
    let parent = tempfile::tempdir().unwrap();
    assert!(matches!(
        mock.client(false)
            .install_social_overlay(parent.path())
            .await,
        Err(EpicError::UnsupportedContent)
    ));
    assert!(
        !parent
            .path()
            .join("EOSOverlay/.obelisk-install.json")
            .exists()
    );
}

#[tokio::test]
async fn download_checks_ownership_fetches_manifest_without_bearer_and_writes_receipt_only_on_success()
 {
    let mock = Mock::new(download_replies("game"));
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    let installed = client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    assert_eq!(installed.account_id, "account-a");
    assert_eq!(installed.app_name, "game");
    assert_eq!(installed.platform, Platform::Mac);
    let directory = std::path::Path::new(&installed.directory);
    assert!(directory.starts_with(parent.path().canonicalize().unwrap()));
    assert!(directory.join("Game.app/Contents/empty").is_file());
    let receipt: Installation =
        serde_json::from_slice(&std::fs::read(directory.join(".obelisk-install.json")).unwrap())
            .unwrap();
    assert_eq!(receipt.build_version, "1.0");
    assert_eq!(receipt.deployment_id.as_deref(), Some("deployment-fixture"));
    let progress = client.progress().unwrap();
    assert_eq!(progress.state, TransferState::Complete);
    assert!(progress.network_bytes > 0);
    assert_eq!(progress.content_bytes, 0);
    let requests = mock.requests();
    assert!(requests[0].contains("authorization: Bearer access-old"));
    assert!(requests[1].contains("authorization: Bearer access-old"));
    assert!(!requests[2].to_lowercase().contains("authorization:"));
    assert!(requests[2].contains("Signature=manifest-only"));
}

#[tokio::test]
async fn an_update_is_a_live_build_whose_version_differs_from_the_installed_one() {
    let live = |version: &str| {
        reply(
            MANIFEST_PATH,
            200,
            json!({"elements":[{"buildVersion": version, "manifests":[{"uri":"FIXTURE_BASE/game.manifest"}]}]}),
        )
    };
    let mut replies = download_replies("game");
    replies.extend([live("1.0"), live("1.1")]);
    let mock = Mock::new(replies);
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    let installed = client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    let directory = std::path::Path::new(&installed.directory);

    let update = |platform| client.update_available("space", "base", "game", platform, directory);
    assert!(
        !update(Platform::Mac).await.unwrap(),
        "the installed build is live"
    );
    assert!(update(Platform::Mac).await.unwrap(), "Epic published 1.1");
    assert!(
        update(Platform::Windows).await.unwrap(),
        "no build of that platform is installed there"
    );
    assert_eq!(mock.requests().len(), 5, "only the live build is asked for");
}

#[tokio::test]
async fn a_manifest_naming_another_app_still_installs() {
    let mock = Mock::new(download_replies("fd52e5b57c6d4468bfd6b39b5577436d"));
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    let installation = client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    assert_eq!(installation.app_name, "game");
}

#[tokio::test]
async fn unowned_games_never_create_an_installation() {
    let parent = tempfile::tempdir().unwrap();
    let mock = Mock::new(vec![reply(
        "/library/api/public/items",
        200,
        json!({"records":[],"responseMetadata":{}}),
    )]);
    let client = mock.client(false);
    assert!(
        matches!(client.download("space", "base", "game", Platform::Mac, parent.path()).await, Err(EpicError::Service {message}) if message == "backend.epic.game_not_owned")
    );
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn download_refreshes_rejected_session_once_before_fetching_content() {
    let mut replies = vec![
        reply("/library/api/public/items", 401, json!({})),
        reply("/account/api/oauth/token", 200, token("account-a")),
    ];
    replies.extend(download_replies("game"));
    let mock = Mock::new(replies);
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "refresh-new"
    );
    assert_eq!(
        mock.requests()
            .iter()
            .filter(|request| request.contains("POST /account/api/oauth/token"))
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_interrupts_preparation_rejects_concurrent_download_and_releases_slot() {
    let mut replies = download_replies("game");
    replies[0].delay_ms = 120;
    let mock = Mock::new(replies);
    let client = Arc::new(mock.client(false));
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().to_owned();
    let running = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .download("space", "base", "game", Platform::Mac, &path)
                .await
        }
    });
    for _ in 0..100 {
        if !mock.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(matches!(
        client
            .download("space", "base", "game", Platform::Mac, parent.path())
            .await,
        Err(EpicError::AlreadyDownloading)
    ));
    client.cancel_download();
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(50), running)
            .await
            .unwrap()
            .unwrap(),
        Err(EpicError::Cancelled)
    ));
    assert_eq!(client.progress().unwrap().state, TransferState::Cancelled);
    assert!(matches!(
        client
            .download("space", "base", "", Platform::Mac, parent.path())
            .await,
        Err(EpicError::Protocol)
    ));
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn dropping_download_future_cancels_the_operation_and_allows_retry() {
    let mut replies = download_replies("game");
    replies[0].delay_ms = 120;
    let mock = Mock::new(replies);
    let client = Arc::new(mock.client(false));
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().to_owned();
    let running = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .download("space", "base", "game", Platform::Mac, &path)
                .await
        }
    });
    for _ in 0..100 {
        if !mock.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_eq!(client.progress().unwrap().state, TransferState::Cancelled);
    assert!(matches!(
        client
            .download("space", "base", "", Platform::Mac, parent.path())
            .await,
        Err(EpicError::Protocol)
    ));
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
}

fn cached_download_replies(version: &str, filename: &str) -> Vec<Reply> {
    use sha1::{Digest, Sha1};
    let mut replies = download_replies("game");
    let mut manifest: Value = serde_json::from_str(&replies[2].body).unwrap();
    manifest["BuildVersionString"] = json!(version);
    manifest["FileManifestList"][0]["Filename"] = json!(filename);
    replies[2].body = manifest.to_string();
    let hash: String = Sha1::digest(replies[2].body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut asset: Value = serde_json::from_str(&replies[1].body).unwrap();
    asset["elements"][0]["hash"] = json!(hash);
    replies[1].body = asset.to_string();
    replies
}

fn files_containing(root: &std::path::Path, bytes: &[u8]) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            found.extend(files_containing(&entry.path(), bytes));
        } else if std::fs::read(entry.path()).unwrap() == bytes {
            found.push(entry.path());
        }
    }
    found
}

#[tokio::test]
async fn manifest_cache_survives_client_restart_but_rechecks_ownership_metadata_and_files() {
    let mut replies = cached_download_replies("1.0", "Game.app/Contents/empty");
    let manifest_bytes = replies[2].body.as_bytes().to_vec();
    let mut second = cached_download_replies("1.0", "Game.app/Contents/empty");
    let mut asset: Value = serde_json::from_str(&second[1].body).unwrap();
    asset["elements"][0]["manifests"][0]["uri"] = json!("FIXTURE_BASE/current/game.manifest");
    asset["elements"][0]["sidecar"]["config"] =
        json!(json!({"deploymentId":"fresh-deployment"}).to_string());
    second[1].body = asset.to_string();
    // No second CDN response exists: a cache hit must use fresh metadata only.
    replies.extend(second.into_iter().take(2));
    let mock = Mock::new(replies);
    let parent = tempfile::tempdir().unwrap();
    let first = mock
        .client(false)
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    let caches = files_containing(parent.path(), &manifest_bytes);
    assert_eq!(
        caches.len(),
        1,
        "the raw manifest must survive successful installation"
    );
    std::fs::write(
        std::path::Path::new(&first.directory).join("Game.app/Contents/empty"),
        b"corrupt",
    )
    .unwrap();

    let client = mock.client(false);
    let installed = client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    assert_eq!(installed.directory, first.directory);
    assert_eq!(installed.deployment_id.as_deref(), Some("fresh-deployment"));
    assert_eq!(
        std::fs::read(std::path::Path::new(&installed.directory).join("Game.app/Contents/empty"))
            .unwrap(),
        b""
    );
    assert_eq!(client.progress().unwrap().network_bytes, 0);
    assert_eq!(client.progress().unwrap().content_bytes, 0);
    assert_eq!(std::fs::read(&caches[0]).unwrap(), manifest_bytes);
    let requests = mock.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /library/"))
            .count(),
        2
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.starts_with("GET /launcher/"))
            .count(),
        2
    );
    assert!(!requests[2].to_lowercase().contains("authorization:"));
}

#[tokio::test]
async fn manifest_cache_uses_current_hash_to_fetch_and_install_new_build() {
    let mut replies = cached_download_replies("1.0", "Game.app/Contents/old");
    replies.extend(cached_download_replies("2.0", "Game.app/Contents/new"));
    let mock = Mock::new(replies);
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    let first = client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    let second = client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    assert_eq!(first.build_version, "1.0");
    assert_eq!(second.build_version, "2.0");
    assert!(
        std::path::Path::new(&second.directory)
            .join("Game.app/Contents/new")
            .is_file()
    );
    assert!(client.progress().unwrap().network_bytes > 0);
    assert_eq!(
        mock.requests()
            .iter()
            .filter(|request| request.starts_with("GET /game.manifest"))
            .count(),
        2
    );
}

#[tokio::test]
async fn manifest_cache_corruption_refetches_and_replaces_the_archive() {
    let mut replies = cached_download_replies("1.0", "Game.app/Contents/empty");
    let manifest_bytes = replies[2].body.as_bytes().to_vec();
    replies.extend(cached_download_replies("1.0", "Game.app/Contents/empty"));
    let mock = Mock::new(replies);
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    let cache = files_containing(parent.path(), &manifest_bytes)
        .pop()
        .expect("persisted manifest");
    let mut corrupt = manifest_bytes.clone();
    corrupt[0] ^= 1;
    std::fs::write(&cache, corrupt).unwrap();
    client
        .download("space", "base", "game", Platform::Mac, parent.path())
        .await
        .unwrap();
    assert_eq!(std::fs::read(cache).unwrap(), manifest_bytes);
    assert!(client.progress().unwrap().network_bytes > 0);
    assert_eq!(
        mock.requests()
            .iter()
            .filter(|request| request.starts_with("GET /game.manifest"))
            .count(),
        2
    );
}

#[tokio::test]
async fn manifest_cache_without_server_hash_always_fetches_current_manifest() {
    let mut replies = download_replies("game");
    let manifest_bytes = replies[2].body.as_bytes().to_vec();
    replies.extend(download_replies("game"));
    let mock = Mock::new(replies);
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        client
            .download("space", "base", "game", Platform::Mac, parent.path())
            .await
            .unwrap();
        assert!(client.progress().unwrap().network_bytes > 0);
    }
    assert!(files_containing(parent.path(), &manifest_bytes).is_empty());
    assert_eq!(mock.requests().len(), 6);
}

#[tokio::test]
async fn manifest_cache_never_bypasses_current_ownership_or_authentication() {
    for status in [200, 401] {
        let mut replies = cached_download_replies("1.0", "Game.app/Contents/empty");
        replies.push(reply(
            "/library/api/public/items",
            status,
            json!({"records":[],"responseMetadata":{}}),
        ));
        if status == 401 {
            replies.push(reply("/account/api/oauth/token", 200, token("account-a")));
            replies.push(reply("/library/api/public/items", 401, json!({})));
        }
        let mock = Mock::new(replies);
        let client = mock.client(false);
        let parent = tempfile::tempdir().unwrap();
        let installed = client
            .download("space", "base", "game", Platform::Mac, parent.path())
            .await
            .unwrap();
        let receipt = std::path::Path::new(&installed.directory).join(".obelisk-install.json");
        let before = std::fs::read(&receipt).unwrap();
        let result = client
            .download("space", "base", "game", Platform::Mac, parent.path())
            .await;
        match status {
            200 => assert!(
                matches!(result, Err(EpicError::Service { message }) if message == "backend.epic.game_not_owned")
            ),
            _ => assert!(matches!(result, Err(EpicError::Unauthorized))),
        }
        assert_eq!(std::fs::read(receipt).unwrap(), before);
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|request| request.starts_with("GET /launcher/"))
                .count(),
            1
        );
        assert_eq!(
            mock.requests()
                .iter()
                .filter(|request| request.starts_with("GET /game.manifest"))
                .count(),
            1
        );
    }
}

const LAUNCH_CATALOG_PATH: &str = "/catalog/api/shared/namespace/space/bulk/items";
const EXCHANGE_PATH: &str = "/account/api/oauth/exchange";
const OWNERSHIP_PATH: &str =
    "/ecommerceintegration/api/public/platforms/EPIC/identities/account-a/ownershipToken";

fn launch_installation(directory: &std::path::Path) -> Installation {
    let directory = directory.canonicalize().unwrap();
    let executable = "Game.app/Contents/MacOS/Game";
    std::fs::create_dir_all(directory.join("Game.app/Contents/MacOS")).unwrap();
    std::fs::write(directory.join(executable), b"fixture executable, never run").unwrap();
    let installation = Installation {
        app_name: "game".into(),
        namespace: "space".into(),
        catalog_item_id: "base".into(),
        account_id: "account-a".into(),
        directory: directory.to_str().unwrap().into(),
        platform: Platform::Mac,
        total_bytes: 29,
        build_version: "1.0".into(),
        launch_executable: executable.into(),
        launch_command: "--title \"My game\" --literal '$HOME'".into(),
        deployment_id: Some("deployment-fixture".into()),
        prerequisite: None,
    };
    write_launch_receipt(&installation);
    installation
}

fn write_launch_receipt(installation: &Installation) {
    std::fs::write(
        std::path::Path::new(&installation.directory).join(".obelisk-install.json"),
        serde_json::to_vec(installation).unwrap(),
    )
    .unwrap();
}

fn launch_replies(ownership: bool) -> Vec<Reply> {
    let mut replies = vec![
        reply(
            "/library/api/public/items",
            200,
            json!({"records":[item("game", "base")],"responseMetadata":{}}),
        ),
        reply(
            LAUNCH_CATALOG_PATH,
            200,
            json!({"base":{"id":"base", "namespace":"space", "title":"A fixture game", "customAttributes":{
                "AdditionalCommandLine":{"value":"--publisher \"Epic build\" --literal '$(id)'"},
                "OwnershipToken":{"value":if ownership { "TrUe" } else { "false" }}
            }}}),
        ),
        reply(
            EXCHANGE_PATH,
            200,
            json!({"code":"exchange-fixture", "expiresInSeconds":300}),
        ),
    ];
    if ownership {
        let mut raw = reply(OWNERSHIP_PATH, 200, Value::Null);
        raw.body = "ownership\0binary\u{1}".into();
        replies.push(raw);
    }
    replies
}

/// Plan a launch whose catalog reply body has been patched by `patch`.
async fn launch_plan_with_catalog(
    directory: &std::path::Path,
    patch: impl FnOnce(&mut Value),
) -> LaunchPlan {
    launch_installation(directory);
    let mut replies = launch_replies(false);
    let catalog = replies
        .iter_mut()
        .find(|reply| reply.path == LAUNCH_CATALOG_PATH)
        .unwrap();
    let mut body: Value = serde_json::from_str(&catalog.body).unwrap();
    patch(&mut body);
    catalog.body = body.to_string();
    let mock = Mock::new(replies);
    mock.client(false)
        .launch_plan("space", "base", "game", Platform::Mac, directory, "en")
        .await
        .unwrap()
}

fn request_url(request: &str) -> reqwest::Url {
    let target = request
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    reqwest::Url::parse(&format!("http://fixture{target}")).unwrap()
}

#[tokio::test]
async fn launch_tolerates_catalog_items_without_a_display_title() {
    for title in [
        None,
        Some(Value::Null),
        Some(json!("  ")),
        Some(json!(42)),
        Some(json!("bad\u{0000}title")),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let plan = launch_plan_with_catalog(temporary.path(), |body| match title {
            Some(title) => body["base"]["title"] = title,
            None => {
                body["base"].as_object_mut().unwrap().remove("title");
            }
        })
        .await;
        // An unusable title still advertises the game, without a display name.
        assert_eq!(
            plan.activity,
            Some(Activity {
                id: "space".into(),
                name: None
            })
        );
        assert_eq!(plan.app_name, "game");
    }
}

#[tokio::test]
async fn launch_uses_catalog_presence_identity_and_ignores_unpublishable_values() {
    for (attribute, expected) in [
        (json!({"value":"published-game"}), Some("published-game")),
        (json!({"value":"p-private-game"}), None),
        (json!({"value":""}), None),
        (json!({"value":"bad\u{0000}id"}), None),
        (json!({"value":"x".repeat(1025)}), None),
        (json!({"value":42}), None),
        (Value::Null, None),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let plan = launch_plan_with_catalog(temporary.path(), |body| {
            body["base"]["customAttributes"]["PresenceId"] = attribute;
        })
        .await;
        assert_eq!(
            plan.activity.as_ref().map(|activity| activity.id.as_str()),
            expected
        );
        assert_eq!(plan.app_name, "game");
    }
}

#[tokio::test]
async fn launch_builds_authenticated_literal_arguments_from_verified_installation_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let installation = launch_installation(temporary.path());
    let mock = Mock::new(launch_replies(false));
    let plan = mock
        .client(false)
        .launch_plan(
            "space",
            "base",
            "game",
            Platform::Mac,
            temporary.path(),
            "fr-FR",
        )
        .await
        .unwrap();
    assert_eq!(plan.app_name, "game");
    assert_eq!(
        plan.activity,
        Some(Activity {
            id: "space".into(),
            name: Some("A fixture game".into())
        })
    );
    assert_eq!(plan.platform, Platform::Mac);
    assert!(plan.ownership_token.is_none());
    assert!(!plan.target.is_application_bundle);
    assert_eq!(
        std::path::Path::new(&plan.target.executable),
        std::path::Path::new(&installation.directory).join(&installation.launch_executable)
    );
    assert_eq!(
        std::path::Path::new(&plan.target.working_directory),
        std::path::Path::new(&installation.directory).join("Game.app/Contents/MacOS")
    );
    assert_eq!(
        &plan.target.arguments[..8],
        [
            "--title",
            "My game",
            "--literal",
            "$HOME",
            "--publisher",
            "Epic build",
            "--literal",
            "$(id)"
        ]
    );
    let expected = [
        "-AUTH_LOGIN=unused",
        "-AUTH_PASSWORD=exchange-fixture",
        "-AUTH_TYPE=exchangecode",
        "-epicapp=game",
        "-epicenv=Prod",
        "-EpicPortal",
        "-epicusername=Player",
        "-epicuserid=account-a",
        "-epiclocale=fr-FR",
        "-epicsandboxid=space",
        "-epicdeploymentid=deployment-fixture",
    ];
    assert_eq!(&plan.target.arguments[8..], expected);
    assert!(
        !plan
            .target
            .arguments
            .iter()
            .any(|argument| argument.contains("refresh-old") || argument.contains("access-old"))
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.contains("authorization: Bearer access-old"))
    );
    let catalog = requests
        .iter()
        .find(|request| request_url(request).path() == LAUNCH_CATALOG_PATH)
        .unwrap();
    assert!(
        request_url(catalog)
            .query_pairs()
            .any(|(key, value)| key == "id" && value == "base")
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /account/api/oauth/exchange "))
    );
}

#[tokio::test]
async fn launch_fetches_required_ownership_token_as_raw_bytes_and_never_swallows_failure() {
    for status in [200, 403] {
        let temporary = tempfile::tempdir().unwrap();
        launch_installation(temporary.path());
        let mut replies = launch_replies(true);
        replies.last_mut().unwrap().status = status;
        let mock = Mock::new(replies);
        let result = mock
            .client(false)
            .launch_plan(
                "space",
                "base",
                "game",
                Platform::Mac,
                temporary.path(),
                "en",
            )
            .await;
        if status == 200 {
            let plan = result.unwrap();
            assert_eq!(
                plan.ownership_token.as_deref(),
                Some(b"ownership\0binary\x01".as_slice())
            );
        } else {
            assert!(result.is_err());
        }
        let requests = mock.requests();
        let ownership = requests
            .iter()
            .find(|request| request_url(request).path() == OWNERSHIP_PATH)
            .unwrap();
        assert!(ownership.starts_with("POST "));
        assert!(ownership.contains("authorization: Bearer access-old"));
        assert!(
            ownership
                .to_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        let body = ownership.split_once("\r\n\r\n").unwrap().1;
        let parsed = reqwest::Url::parse(&format!("http://fixture/?{body}")).unwrap();
        assert_eq!(
            parsed.query_pairs().collect::<Vec<_>>(),
            [("nsCatalogItemId".into(), "space:base".into())]
        );
    }
}

#[test]
fn an_installation_names_its_game_and_prerequisite_from_its_own_receipt_only() {
    let temp = tempfile::tempdir().unwrap();
    let mut installation = launch_installation(temp.path());
    let directory = std::path::PathBuf::from(&installation.directory);
    let read = |app: &str, platform: Platform| {
        super::installed_launch("space", "base", app, platform, &directory)
    };
    let installed = read("game", Platform::Mac).unwrap();
    assert_eq!(installed.prerequisite, None);
    assert_eq!(
        std::path::Path::new(&installed.target.executable),
        directory.join(&installation.launch_executable)
    );
    installation.prerequisite = Some(Prerequisite {
        ids: vec!["ue".into()],
        name: "Prerequisites".into(),
        path: "Engine\\Extras\\Redist\\en-us\\setup.exe".into(),
        arguments: "/quiet".into(),
    });
    write_launch_receipt(&installation);
    assert_eq!(
        read("game", Platform::Mac).unwrap().prerequisite,
        installation.prerequisite
    );
    assert!(read("other", Platform::Mac).is_err());
    assert!(read("game", Platform::Windows).is_err());
}

#[test]
fn an_installation_tells_its_catalog_item_from_its_own_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let installation = launch_installation(temp.path());
    let directory = std::path::PathBuf::from(&installation.directory);
    assert_eq!(super::installed_catalog_item(&directory).unwrap(), "base");
    let incomplete = temp.path().join("incomplete");
    std::fs::create_dir_all(&incomplete).unwrap();
    assert!(super::installed_catalog_item(&incomplete).is_err());
}

#[tokio::test]
async fn launch_rejects_missing_mismatched_and_escaping_receipts_before_network() {
    let mock = Mock::new(Vec::new());
    let client = mock.client(false);
    let temporary = tempfile::tempdir().unwrap();
    assert!(
        client
            .launch_plan(
                "space",
                "base",
                "game",
                Platform::Mac,
                temporary.path(),
                "en"
            )
            .await
            .is_err()
    );
    let installation = launch_installation(temporary.path());
    let baseline = serde_json::to_value(&installation).unwrap();
    for (field, value) in [
        ("app_name", "another-game"),
        ("namespace", "another-space"),
        ("catalog_item_id", "another-catalog"),
        ("platform", "Windows"),
        ("account_id", "account-b"),
        ("directory", "/another/directory"),
        ("launch_executable", "../outside"),
    ] {
        let mut receipt = baseline.clone();
        receipt[field] = json!(value);
        std::fs::write(
            temporary.path().join(".obelisk-install.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert!(
            client
                .launch_plan(
                    "space",
                    "base",
                    "game",
                    Platform::Mac,
                    temporary.path(),
                    "en"
                )
                .await
                .is_err(),
            "accepted mismatched {field}"
        );
    }
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn launch_rechecks_ownership_before_issuing_an_exchange_code() {
    let temporary = tempfile::tempdir().unwrap();
    launch_installation(temporary.path());
    let mock = Mock::new(vec![reply(
        "/library/api/public/items",
        200,
        json!({"records":[item("other", "base")],"responseMetadata":{}}),
    )]);
    assert!(
        mock.client(false)
            .launch_plan(
                "space",
                "base",
                "game",
                Platform::Mac,
                temporary.path(),
                "en"
            )
            .await
            .is_err()
    );
    assert_eq!(mock.requests().len(), 1);
    assert!(!mock.requests()[0].contains("oauth/exchange"));
}

#[tokio::test]
async fn launch_retries_rejected_exchange_once_and_keeps_rotated_credentials() {
    for retry_status in [200, 401] {
        let temporary = tempfile::tempdir().unwrap();
        launch_installation(temporary.path());
        let mut replies = launch_replies(false);
        for reply in &mut replies {
            reply.bearer = Some("access-old");
            if reply.path == EXCHANGE_PATH {
                reply.status = 401;
            }
        }
        replies.push(reply("/account/api/oauth/token", 200, token("account-a")));
        let mut retry = launch_replies(false);
        for reply in &mut retry {
            reply.bearer = Some("access-new");
            if reply.path == EXCHANGE_PATH {
                reply.status = retry_status;
            }
        }
        replies.extend(retry);
        let mock = Mock::new(replies);
        let client = mock.client(false);
        let result = client
            .launch_plan(
                "space",
                "base",
                "game",
                Platform::Mac,
                temporary.path(),
                "en",
            )
            .await;
        if retry_status == 200 {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(EpicError::Unauthorized)));
        }
        assert_eq!(
            client.credentials().await.unwrap().refresh_token,
            "refresh-new"
        );
        let requests = mock.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST /account/api/oauth/token "))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("GET /account/api/oauth/exchange "))
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn launch_discards_exchange_code_when_user_disconnects_during_http() {
    let temporary = tempfile::tempdir().unwrap();
    launch_installation(temporary.path());
    let mut replies = launch_replies(false);
    replies
        .iter_mut()
        .find(|reply| reply.path == EXCHANGE_PATH)
        .unwrap()
        .delay_ms = 200;
    replies.push(reply(
        "/account/api/oauth/sessions/kill/access-old",
        204,
        Value::Null,
    ));
    let mock = Mock::new(replies);
    let client = Arc::new(mock.client(false));
    let directory = temporary.path().to_owned();
    let launcher = client.clone();
    let running = tokio::spawn(async move {
        launcher
            .launch_plan("space", "base", "game", Platform::Mac, &directory, "en")
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !mock
            .requests()
            .iter()
            .any(|request| request.starts_with("GET /account/api/oauth/exchange "))
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    client.disconnect().await.unwrap();
    assert!(matches!(
        running.await.unwrap(),
        Err(EpicError::Unauthorized)
    ));
}

#[tokio::test]
async fn launch_cannot_read_installation_while_download_holds_its_lock() {
    let temporary = tempfile::tempdir().unwrap();
    launch_installation(temporary.path());
    let _download_lock = obelisk_core::installation::lock(temporary.path()).unwrap();
    let mock = Mock::new(Vec::new());
    let result = mock
        .client(false)
        .launch_plan(
            "space",
            "base",
            "game",
            Platform::Mac,
            temporary.path(),
            "en",
        )
        .await;
    assert!(matches!(result, Err(EpicError::AlreadyDownloading)));
    assert!(mock.requests().is_empty());
}

fn achievement_definitions() -> Value {
    let definitions: Vec<_> = [("earned", false), ("locked", false), ("secret", true)]
        .into_iter().map(|(name, hidden)| json!({"achievement": {
            "name":name,"hidden":hidden,"unlockedDisplayName":"Earned title",
            "lockedDisplayName":"Locked title","unlockedDescription":"Earned description",
            "lockedDescription":"Locked description","unlockedIconLink":"https://example.com/earned.png",
            "lockedIconLink":"https://example.com/locked.png"
        }})).collect();
    json!({"data":{"Achievement":{"productAchievementsRecordBySandbox":{
        "sandboxId":"space","totalAchievements":3,"achievements":definitions
    }}}})
}

fn achievement_player(account: &str) -> Value {
    json!({"data":{"PlayerAchievement":{"playerAchievementGameRecordsBySandbox":{"records":[{
        "playerAchievements":[{"playerAchievement":{
            "sandboxId":"space","epicAccountId":account,"achievementName":"earned",
            "unlocked":true,"unlockDate":"2026-09-21T12:00:00.000Z"
        }}]
    }]}}}})
}

#[tokio::test]
async fn achievements_merge_player_state_and_keep_hidden_locked_details_private() {
    let mock = Mock::new(vec![
        reply("/graphql", 200, achievement_definitions()),
        reply("/graphql", 200, achievement_player("account-a")),
    ]);
    let items = mock
        .client(false)
        .achievements("space", "fr")
        .await
        .unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].id, "earned");
    assert!(items[0].unlocked);
    assert_eq!(items[0].name, "Earned title");
    assert_eq!(
        items[0].unlock_date.as_deref(),
        Some("2026-09-21T12:00:00.000Z")
    );
    assert_eq!(items[1].name, "Locked title");
    assert!(!items[1].unlocked);
    assert!(items[2].hidden);
    assert!(
        items[2].name.is_empty() && items[2].description.is_empty() && items[2].icon_url.is_empty()
    );
    let requests = mock.requests();
    let bodies: Vec<Value> = requests
        .iter()
        .map(|r| serde_json::from_str(r.split_once("\r\n\r\n").unwrap().1).unwrap())
        .collect();
    assert_eq!(
        bodies[0]["variables"],
        json!({"sandboxId":"space","locale":"fr"})
    );
    assert_eq!(
        bodies[1]["variables"],
        json!({"sandboxId":"space","epicAccountId":"account-a"})
    );
    assert!(
        bodies
            .iter()
            .all(|body| body["query"].as_str().unwrap().starts_with("query "))
    );
}

#[tokio::test]
async fn achievements_distinguish_no_support_from_graphql_and_incomplete_responses() {
    let mock = Mock::new(vec![reply(
        "/graphql",
        200,
        json!({"data":{"Achievement":{"productAchievementsRecordBySandbox":null}}}),
    )]);
    assert!(
        mock.client(false)
            .achievements("space", "fr")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(mock.requests().len(), 1);
    let mock = Mock::new(vec![reply(
        "/graphql",
        200,
        json!({"data":{"Achievement":{"productAchievementsRecordBySandbox":
            {"sandboxId":null,"totalAchievements":null,"achievements":null}}}}),
    )]);
    assert!(
        mock.client(false)
            .achievements("space", "fr")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(mock.requests().len(), 1);
    for response in [
        json!({"errors":[{"message":"denied"}],"data":null}),
        json!({"data":{}}),
    ] {
        let mock = Mock::new(vec![reply("/graphql", 200, response)]);
        assert!(
            mock.client(false)
                .achievements("space", "fr")
                .await
                .is_err()
        );
    }
    let mut incomplete = achievement_definitions();
    incomplete["data"]["Achievement"]["productAchievementsRecordBySandbox"]["totalAchievements"] =
        json!(4);
    let mock = Mock::new(vec![reply("/graphql", 200, incomplete)]);
    assert!(matches!(
        mock.client(false).achievements("space", "fr").await,
        Err(EpicError::Protocol)
    ));
}

#[tokio::test]
async fn achievements_reject_player_state_from_another_account() {
    let mock = Mock::new(vec![
        reply("/graphql", 200, achievement_definitions()),
        reply("/graphql", 200, achievement_player("account-b")),
    ]);
    assert!(matches!(
        mock.client(false).achievements("space", "fr").await,
        Err(EpicError::Protocol)
    ));
}

#[tokio::test]
async fn achievements_retry_unauthorized_with_refreshed_credentials() {
    let mock = Mock::new(vec![
        reply("/graphql", 401, json!({})),
        reply("/account/api/oauth/token", 200, token("account-a")),
        reply("/graphql", 200, achievement_definitions()),
        reply("/graphql", 200, achievement_player("account-a")),
    ]);
    let client = mock.client(false);
    assert_eq!(client.achievements("space", "fr").await.unwrap().len(), 3);
    assert_eq!(
        client.credentials().await.unwrap().refresh_token,
        "refresh-new"
    );
    let requests = mock.requests();
    assert!(requests[2].contains("Bearer access-new"));
    assert!(requests[3].contains("Bearer access-new"));
}

#[test]
fn an_installation_folder_is_named_after_its_identity_hash() {
    assert_eq!(
        super::installation_directory_name("namespace", "catalog", "app/name", Platform::Mac),
        "epic-c5d3f0bff0a8491d3095fdd858bc870fe8416f60-Mac"
    );
    assert_eq!(
        super::installation_directory_name("namespace", "catalog", "app/name", Platform::Windows),
        "epic-c5d3f0bff0a8491d3095fdd858bc870fe8416f60-Windows"
    );
}

#[test]
fn an_installation_receipt_keeps_epic_s_spelling() {
    let temp = tempfile::tempdir().unwrap();
    let installation = launch_installation(temp.path());
    let mut written = serde_json::to_value(&installation).unwrap();
    assert_eq!(written["platform"], "Mac");
    written["platform"] = json!("Windows");
    let read: Installation = serde_json::from_value(written).unwrap();
    assert_eq!(read.platform, Platform::Windows);
}
