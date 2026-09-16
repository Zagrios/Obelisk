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

#[tokio::test]
async fn game_details_preserve_epic_seconds_when_the_manifest_is_unavailable() {
    let mock = Mock::new(vec![
        reply(
            PLAYTIME_PATH,
            200,
            json!([
                {"accountId":"account-b","artifactId":"game","totalTime":999},
                {"accountId":"account-a","artifactId":"other","totalTime":123},
                {"accountId":"account-a","artifactId":"game","totalTime":68363}
            ]),
        ),
        reply(MANIFEST_PATH, 404, json!({})),
    ]);
    let details = mock
        .client(false)
        .game_details("space", "base", "game", "Mac")
        .await
        .unwrap();
    assert_eq!(details.playtime_seconds, Some(68363));
    assert_eq!(details.installation_bytes, None);
    assert!(
        mock.requests()
            .iter()
            .all(|request| request.contains("Bearer access-old"))
    );
}

#[tokio::test]
async fn missing_history_stays_unknown_but_explicit_zero_is_preserved() {
    for (body, expected) in [
        (json!([]), None),
        (
            json!([{"accountId":"account-a","artifactId":"game","totalTime":0}]),
            Some(0),
        ),
    ] {
        let mock = Mock::new(vec![
            reply(PLAYTIME_PATH, 200, body),
            reply(MANIFEST_PATH, 404, json!({})),
        ]);
        let details = mock
            .client(false)
            .game_details("space", "base", "game", "Mac")
            .await
            .unwrap();
        assert_eq!(details.playtime_seconds, expected);
    }
}

#[tokio::test]
async fn game_details_retry_auth_once_and_keep_rotated_credentials() {
    for status in [200, 401] {
        let mut replies = vec![reply("/account/api/oauth/token", 200, token("account-a"))];
        for (path, success_status, body) in [
            (
                PLAYTIME_PATH,
                status,
                json!([{"accountId":"account-a","artifactId":"game","totalTime":120}]),
            ),
            (MANIFEST_PATH, 404, json!({})),
        ] {
            let mut rejected = reply(path, 401, json!({}));
            rejected.bearer = Some("access-old");
            replies.push(rejected);
            let mut retried = reply(path, success_status, body);
            retried.bearer = Some("access-new");
            replies.push(retried);
        }
        let mock = Mock::new(replies);
        let client = mock.client(false);
        let result = client.game_details("space", "base", "game", "Mac").await;
        if status == 200 {
            assert_eq!(result.unwrap().playtime_seconds, Some(120));
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
    let mut slow = reply(
        PLAYTIME_PATH,
        200,
        json!([{"accountId":"account-a","artifactId":"game","totalTime":120}]),
    );
    slow.delay_ms = 200;
    let mock = Mock::new(vec![
        slow,
        reply(MANIFEST_PATH, 404, json!({})),
        reply(
            "/account/api/oauth/sessions/kill/access-old",
            204,
            Value::Null,
        ),
    ]);
    let client = Arc::new(mock.client(false));
    let reader = client.clone();
    let reading =
        tokio::spawn(async move { reader.game_details("space", "base", "game", "Mac").await });
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
                "base":{"title":"A game","description":"Game description","developer":"Studio","keyImages":[{"type":"DieselGameBoxTall","url":"file:///bad"},{"type":"OfferImageTall","url":"https://cdn.epicgames.com/cover.jpg"},{"type":"DieselStoreFrontWide","url":"https://cdn.epicgames.com/header.jpg"}]},
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
    assert_eq!(games[1].app_name, "missing");
    assert!(games[1].image_url.is_none());
    assert!(games[1].header_image_url.is_none());
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
async fn download_checks_ownership_fetches_manifest_without_bearer_and_writes_receipt_only_on_success()
 {
    let mock = Mock::new(download_replies("game"));
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    let installed = client
        .download("space", "base", "game", "Mac", parent.path())
        .await
        .unwrap();
    assert_eq!(installed.account_id, "account-a");
    assert_eq!(installed.app_name, "game");
    assert_eq!(installed.platform, "Mac");
    let directory = std::path::Path::new(&installed.directory);
    assert!(directory.starts_with(parent.path().canonicalize().unwrap()));
    assert!(directory.join("Game.app/Contents/empty").is_file());
    let receipt: Installation =
        serde_json::from_slice(&std::fs::read(directory.join(".forge-install.json")).unwrap())
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
async fn manifest_identity_mismatch_and_unowned_games_never_create_an_installation() {
    let mock = Mock::new(download_replies("another-game"));
    let client = mock.client(false);
    let parent = tempfile::tempdir().unwrap();
    assert!(matches!(
        client
            .download("space", "base", "game", "Mac", parent.path())
            .await,
        Err(EpicError::Protocol)
    ));
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    assert_eq!(client.progress().unwrap().state, TransferState::Failed);
    let mock = Mock::new(vec![reply(
        "/library/api/public/items",
        200,
        json!({"records":[],"responseMetadata":{}}),
    )]);
    let client = mock.client(false);
    assert!(
        matches!(client.download("space", "base", "game", "Mac", parent.path()).await, Err(EpicError::Service {message}) if message == "backend.epic.game_not_owned")
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
        .download("space", "base", "game", "Mac", parent.path())
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
        async move { client.download("space", "base", "game", "Mac", &path).await }
    });
    for _ in 0..100 {
        if !mock.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(matches!(
        client
            .download("space", "base", "game", "Mac", parent.path())
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
            .download("space", "base", "game", "Unsupported", parent.path())
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
        async move { client.download("space", "base", "game", "Mac", &path).await }
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
            .download("space", "base", "game", "Unsupported", parent.path())
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
        .download("space", "base", "game", "Mac", parent.path())
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
        .download("space", "base", "game", "Mac", parent.path())
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
        .download("space", "base", "game", "Mac", parent.path())
        .await
        .unwrap();
    let second = client
        .download("space", "base", "game", "Mac", parent.path())
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
        .download("space", "base", "game", "Mac", parent.path())
        .await
        .unwrap();
    let cache = files_containing(parent.path(), &manifest_bytes)
        .pop()
        .expect("persisted manifest");
    let mut corrupt = manifest_bytes.clone();
    corrupt[0] ^= 1;
    std::fs::write(&cache, corrupt).unwrap();
    client
        .download("space", "base", "game", "Mac", parent.path())
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
            .download("space", "base", "game", "Mac", parent.path())
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
            .download("space", "base", "game", "Mac", parent.path())
            .await
            .unwrap();
        let receipt = std::path::Path::new(&installed.directory).join(".forge-install.json");
        let before = std::fs::read(&receipt).unwrap();
        let result = client
            .download("space", "base", "game", "Mac", parent.path())
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
        platform: "Mac".into(),
        total_bytes: 29,
        build_version: "1.0".into(),
        launch_executable: executable.into(),
        launch_command: "--title \"My game\" --literal '$HOME'".into(),
        deployment_id: Some("deployment-fixture".into()),
    };
    write_launch_receipt(&installation);
    installation
}

fn write_launch_receipt(installation: &Installation) {
    std::fs::write(
        std::path::Path::new(&installation.directory).join(".forge-install.json"),
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
            json!({"base":{"id":"base", "namespace":"space", "customAttributes":{
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
async fn launch_builds_authenticated_literal_arguments_from_verified_installation_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let installation = launch_installation(temporary.path());
    let mock = Mock::new(launch_replies(false));
    let plan = mock
        .client(false)
        .launch_plan("space", "base", "game", "Mac", temporary.path(), "fr-FR")
        .await
        .unwrap();
    assert_eq!(plan.app_name, "game");
    assert_eq!(plan.platform, "Mac");
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
            .launch_plan("space", "base", "game", "Mac", temporary.path(), "en")
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

#[tokio::test]
async fn launch_rejects_missing_mismatched_and_escaping_receipts_before_network() {
    let mock = Mock::new(Vec::new());
    let client = mock.client(false);
    let temporary = tempfile::tempdir().unwrap();
    assert!(
        client
            .launch_plan("space", "base", "game", "Mac", temporary.path(), "en")
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
            temporary.path().join(".forge-install.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert!(
            client
                .launch_plan("space", "base", "game", "Mac", temporary.path(), "en")
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
            .launch_plan("space", "base", "game", "Mac", temporary.path(), "en")
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
            .launch_plan("space", "base", "game", "Mac", temporary.path(), "en")
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
            .launch_plan("space", "base", "game", "Mac", &directory, "en")
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
    let _download_lock = download::lock_installation(temporary.path()).unwrap();
    let mock = Mock::new(Vec::new());
    let result = mock
        .client(false)
        .launch_plan("space", "base", "game", "Mac", temporary.path(), "en")
        .await;
    assert!(matches!(result, Err(EpicError::AlreadyDownloading)));
    assert!(mock.requests().is_empty());
}
