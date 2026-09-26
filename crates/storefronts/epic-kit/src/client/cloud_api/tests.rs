use super::*;
use crate::auth::{Api, Credentials, Session};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct State {
    objects: BTreeMap<String, Vec<u8>>,
    requests: Vec<(String, String, bool)>,
    fail_chunk_put: bool,
    missing_link: bool,
    corrupt_chunk: bool,
    redirect_manifest: bool,
    inventory_error: bool,
    list_count: usize,
    mutate_on_list: Option<(usize, String, Vec<u8>)>,
    delay_blob_ms: u64,
    nullable_link_metadata: bool,
    invalid_link_hash: bool,
    null_listing_field: Option<&'static str>,
}

struct Mock {
    api: Api,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Mock {
    fn new(objects: BTreeMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            objects,
            requests: vec![],
            fail_chunk_put: false,
            missing_link: false,
            corrupt_chunk: false,
            redirect_manifest: false,
            inventory_error: false,
            list_count: 0,
            mutate_on_list: None,
            delay_blob_ms: 0,
            nullable_link_metadata: false,
            invalid_link_hash: false,
            null_listing_field: None,
        }));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_stop = stopped.clone();
        let thread_state = state.clone();
        let thread_base = base.clone();
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 8192];
                let header_end = loop {
                    let count = socket.read(&mut buffer).unwrap_or(0);
                    if count == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                        break Some(end);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|v| v.parse().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + 4 + length {
                    let count = socket.read(&mut buffer).unwrap_or(0);
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let mut first = headers.lines().next().unwrap().split_whitespace();
                let method = first.next().unwrap();
                let path = first.next().unwrap().split('?').next().unwrap();
                let auth = headers.to_ascii_lowercase().contains("authorization:");
                let mut state = thread_state.lock().unwrap();
                state
                    .requests
                    .push((method.to_owned(), path.to_owned(), auth));
                let mut status = 200;
                let mut extra = String::new();
                let body;
                if path == "/library/api/public/items" && method == "GET" {
                    assert!(auth);
                    body = serde_json::to_vec(&json!({
                        "records": [{"namespace":"space", "catalogItemId":"item", "appName":"game", "sandboxType":"PUBLIC"}],
                        "responseMetadata": {"nextCursor":null},
                    })).unwrap();
                } else if path.ends_with("/manifests/") && method == "GET" {
                    assert!(auth);
                    state.list_count += 1;
                    if state
                        .mutate_on_list
                        .as_ref()
                        .is_some_and(|(count, _, _)| *count == state.list_count)
                    {
                        let (_, name, bytes) = state.mutate_on_list.take().unwrap();
                        state.objects.insert(name, bytes);
                    }
                    if state.inventory_error {
                        status = 503;
                        body = b"error".to_vec();
                    } else {
                        let files: serde_json::Map<_, _> = state.objects.iter().filter(|(name, _)| name.starts_with("manifests/")).map(|(name, bytes)| {
                            let mut entry = json!({"hash":hex(bytes),"lastModified":"2026-09-22T01:00:00Z","readLink":format!("{thread_base}/blob/{name}")});
                            if let Some(field) = state.null_listing_field { entry[field] = Value::Null; }
                            (format!("savesync/account/game/{name}"), entry)
                        }).collect();
                        body = serde_json::to_vec(&json!({"files":files})).unwrap();
                    }
                } else if path.ends_with("/game/") && method == "POST" {
                    assert!(auth);
                    let request: Value = serde_json::from_slice(&bytes[header_end + 4..]).unwrap();
                    let files: serde_json::Map<_, _> = request["files"].as_array().unwrap().iter().skip(usize::from(state.missing_link)).map(|name| {
                        let name = name.as_str().unwrap();
                        let mut entry = json!({"readLink":format!("{thread_base}/blob/{name}"),"writeLink":format!("{thread_base}/blob/{name}")});
                        if state.nullable_link_metadata { entry["hash"] = Value::Null; entry["lastModified"] = Value::Null; }
                        if state.invalid_link_hash { entry["hash"] = json!(false); }
                        (format!("savesync/account/game/{name}"), entry)
                    }).collect();
                    body = serde_json::to_vec(&json!({"files":files})).unwrap();
                } else if let Some(name) = path.strip_prefix("/blob/") {
                    assert!(!auth, "signed object requests must not carry OAuth");
                    if method == "PUT" {
                        if state.fail_chunk_put && !name.starts_with("manifests/") {
                            status = 503;
                        } else {
                            state
                                .objects
                                .insert(name.to_owned(), bytes[header_end + 4..].to_vec());
                        }
                        body = vec![];
                    } else if state.redirect_manifest && name.starts_with("manifests/") {
                        status = 302;
                        extra = format!("Location: {thread_base}/redirect-target\r\n");
                        body = vec![];
                    } else {
                        let delay = state.delay_blob_ms;
                        body = match state.objects.get(name) {
                            Some(bytes) => {
                                let mut bytes = bytes.clone();
                                if state.corrupt_chunk && !name.starts_with("manifests/") {
                                    *bytes.last_mut().unwrap() ^= 1;
                                }
                                bytes
                            }
                            None => {
                                status = 404;
                                vec![]
                            }
                        };
                        drop(state);
                        thread::sleep(Duration::from_millis(delay));
                        let output = format!(
                            "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(output.as_bytes());
                        let _ = socket.write_all(&body);
                        continue;
                    }
                } else {
                    panic!("unexpected {method} {path}");
                }
                drop(state);
                let output = format!(
                    "HTTP/1.1 {status} fixture\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(output.as_bytes());
                let _ = socket.write_all(&body);
            }
        });
        let mut api = Api::new().unwrap();
        api.account = base.clone();
        api.library = base.clone();
        api.cloud = base;
        Self {
            api,
            state,
            stop: stopped,
            handle: Some(handle),
        }
    }
    fn client(&self) -> Client {
        Client::from_session(
            self.api.clone(),
            Session {
                credentials: Credentials {
                    account_id: "account".into(),
                    display_name: "Player".into(),
                    refresh_token: "refresh".into(),
                },
                access_token: "access".into(),
                expires_at: Instant::now() + Duration::from_secs(3600),
            },
        )
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn hex(bytes: &[u8]) -> String {
    sailover_core::install::hex(&Sha1::digest(bytes))
}
fn rules() -> CloudRules {
    CloudRules {
        cloud_save_folder: None,
        cloud_save_folder_mac: Some("{Home}/saves".into()),
        include: vec![],
        exclude: vec![],
    }
}
fn fixture(files: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    let package = codec::package(
        game(),
        "account",
        &rules(),
        files,
        1_790_000_000,
        &CancelToken::new(),
    )
    .unwrap();
    let mut objects = package.chunks;
    objects.insert(package.manifest_path, package.manifest);
    objects
}

#[tokio::test]
async fn upload_download_and_all_deleted_generation_require_full_readback() {
    let mock = Mock::new(BTreeMap::new());
    let client = mock.client();
    let cancel = CancelToken::new();
    let empty = client.cloud_download(game(), &cancel).await.unwrap();
    assert!(empty.files.is_empty());
    let files = BTreeMap::from([
        ("slot.sav".into(), b"progress 1".to_vec()),
        ("empty.sav".into(), vec![]),
    ]);
    let uploaded = client
        .cloud_upload(game(), &rules(), &files, &empty.revision, &cancel)
        .await
        .unwrap();
    assert_eq!(uploaded.files, files);
    assert_ne!(uploaded.revision, empty.revision);
    assert_eq!(
        client.cloud_download(game(), &cancel).await.unwrap(),
        uploaded
    );
    let deleted = client
        .cloud_upload(
            game(),
            &rules(),
            &BTreeMap::new(),
            &uploaded.revision,
            &cancel,
        )
        .await
        .unwrap();
    assert!(deleted.files.is_empty());
    assert_ne!(deleted.revision, uploaded.revision);
    let state = mock.state.lock().unwrap();
    let first_put = state
        .requests
        .iter()
        .position(|(method, _, _)| method == "PUT")
        .unwrap();
    assert!(state.requests[first_put].1.contains("ChunksV4"));
    assert!(
        state
            .requests
            .iter()
            .any(|(method, path, _)| method == "GET" && path.contains("/blob/ChunksV4"))
    );
    assert!(
        state
            .requests
            .iter()
            .filter(|(_, path, _)| path.starts_with("/blob/"))
            .all(|(_, _, auth)| !auth)
    );
}

#[tokio::test]
async fn failed_chunk_upload_never_requests_or_publishes_manifest() {
    let mock = Mock::new(BTreeMap::new());
    let client = mock.client();
    let cancel = CancelToken::new();
    let before = client.cloud_download(game(), &cancel).await.unwrap();
    mock.state.lock().unwrap().fail_chunk_put = true;
    let files = BTreeMap::from([("slot.sav".into(), vec![7; 100])]);
    assert!(matches!(
        client
            .cloud_upload(game(), &rules(), &files, &before.revision, &cancel)
            .await,
        Err(EpicError::Http(503))
    ));
    let state = mock.state.lock().unwrap();
    assert!(state.objects.is_empty());
    assert!(
        !state
            .requests
            .iter()
            .any(|(method, path, _)| method == "PUT" && path.contains("manifests/"))
    );
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|(method, _, _)| method == "POST")
            .count(),
        1
    );
}

#[tokio::test]
async fn stale_revision_and_inventory_failure_cannot_start_writes() {
    let mock = Mock::new(BTreeMap::new());
    let client = mock.client();
    let cancel = CancelToken::new();
    let files = BTreeMap::from([("slot.sav".into(), b"save".to_vec())]);
    assert!(matches!(
        client
            .cloud_upload(game(), &rules(), &files, "stale", &cancel)
            .await,
        Err(EpicError::CloudConflict)
    ));
    mock.state.lock().unwrap().inventory_error = true;
    assert!(client.cloud_download(game(), &cancel).await.is_err());
    assert!(
        mock.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .all(|(method, _, _)| method == "GET")
    );
}

#[tokio::test]
async fn missing_links_corrupt_chunks_and_redirects_fail_closed() {
    let files = BTreeMap::from([("slot.sav".into(), vec![7; 500])]);
    for fault in [0, 1, 2] {
        let mock = Mock::new(fixture(&files));
        let client = mock.client();
        {
            let mut state = mock.state.lock().unwrap();
            match fault {
                0 => state.missing_link = true,
                1 => state.corrupt_chunk = true,
                _ => state.redirect_manifest = true,
            }
        }
        assert!(
            client
                .cloud_download(game(), &CancelToken::new())
                .await
                .is_err()
        );
        assert!(
            !mock
                .state
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|(_, path, _)| path == "/redirect-target")
        );
    }
}

#[tokio::test]
async fn remote_change_during_read_or_chunk_upload_is_conflict() {
    let files = BTreeMap::from([("slot.sav".into(), b"save".to_vec())]);
    let remote = fixture(&files);
    let (manifest_name, manifest_bytes) = remote
        .iter()
        .find(|(name, _)| name.starts_with("manifests/"))
        .unwrap();
    let mock = Mock::new(BTreeMap::new());
    mock.state.lock().unwrap().mutate_on_list =
        Some((2, manifest_name.clone(), manifest_bytes.clone()));
    assert!(matches!(
        mock.client()
            .cloud_download(game(), &CancelToken::new())
            .await,
        Err(EpicError::CloudConflict)
    ));
    let mock = Mock::new(BTreeMap::new());
    let client = mock.client();
    let cancel = CancelToken::new();
    let empty = client.cloud_download(game(), &cancel).await.unwrap();
    mock.state.lock().unwrap().mutate_on_list =
        Some((4, manifest_name.clone(), manifest_bytes.clone()));
    assert!(matches!(
        client
            .cloud_upload(game(), &rules(), &files, &empty.revision, &cancel)
            .await,
        Err(EpicError::CloudConflict)
    ));
    assert!(
        !mock
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|(method, path, _)| method == "PUT" && path.contains("manifests/"))
    );
}

#[tokio::test]
async fn cancellation_interrupts_pending_signed_request_and_session_loss_discards_result() {
    let files = BTreeMap::from([("slot.sav".into(), b"save".to_vec())]);
    let mock = Mock::new(fixture(&files));
    mock.state.lock().unwrap().delay_blob_ms = 200;
    let client = mock.client();
    let cancel = CancelToken::new();
    let task = client.cloud_download(game(), &cancel);
    let cancel_later = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
    };
    let (result, _) = tokio::join!(task, cancel_later);
    assert!(matches!(result, Err(EpicError::Cancelled)));
    let mock = Mock::new(fixture(&files));
    mock.state.lock().unwrap().delay_blob_ms = 100;
    let client = mock.client();
    let cancel = CancelToken::new();
    let discard = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        *client.session.lock().await = None;
    };
    let (result, _) = tokio::join!(client.cloud_download(game(), &cancel), discard);
    assert!(matches!(result, Err(EpicError::Unauthorized)));
}

#[test]
fn metadata_rejects_duplicates_and_unsigned_or_foreign_destinations() {
    assert!(serde_json::from_str::<Listing>(r#"{"files":{"a":{},"a":{}}}"#).is_err());
    for url in [
        "https://amazonaws.com.evil.test/key",
        "https://user@bucket.s3.amazonaws.com/key",
        "https://127.0.0.1/key",
        "ftp://bucket.s3.amazonaws.com/key",
        "https://bucket.s3.amazonaws.com/key#fragment",
    ] {
        assert!(signed_url(url).is_err(), "{url}");
    }
    assert!(signed_url("https://bucket.s3.amazonaws.com/key?signature=opaque").is_ok());
}

#[tokio::test]
async fn native_tuple_is_read_through_the_owned_game_transport() {
    for (name, accepted) in [
        ("space:item:game:account", true),
        ("foreign:item:game:account", false),
    ] {
        let manifest = serde_json::to_vec(&json!({
            "ManifestFileVersion":"017000000000",
            "AppNameString": name,
            "BuildVersionString":"2026.09.22-00.00.00",
            "FileManifestList":[], "ChunkHashList":{}, "ChunkShaList":{},
            "DataGroupList":{}, "ChunkFilesizeList":{},
        }))
        .unwrap();
        let mock = Mock::new(BTreeMap::from([(
            "manifests/2026.09.22-00.00.00.manifest".into(),
            manifest,
        )]));
        let result = mock
            .client()
            .cloud_download(game(), &CancelToken::new())
            .await;
        if accepted {
            assert!(result.unwrap().files.is_empty());
        } else {
            assert!(matches!(result, Err(EpicError::Protocol)));
        }
        let requests = &mock.state.lock().unwrap().requests;
        assert_eq!(requests[0].1, "/library/api/public/items");
        assert!(requests.iter().all(|(method, _, _)| method == "GET"));
    }
}

#[tokio::test]
async fn unowned_or_malformed_catalog_tuple_never_reaches_cloud_storage() {
    for requested in [
        CloudGame {
            namespace: "foreign",
            ..game()
        },
        CloudGame {
            catalog_item_id: "foreign",
            ..game()
        },
        CloudGame {
            app_name: "foreign",
            ..game()
        },
        CloudGame {
            namespace: "space:extra",
            ..game()
        },
    ] {
        let mock = Mock::new(BTreeMap::new());
        let client = mock.client();
        let cancel = CancelToken::new();
        assert!(client.cloud_download(requested, &cancel).await.is_err());
        assert!(
            client
                .cloud_upload(requested, &rules(), &BTreeMap::new(), "revision", &cancel)
                .await
                .is_err()
        );
        assert!(
            mock.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|(_, path, _)| path == "/library/api/public/items")
        );
    }
}

#[tokio::test]
async fn upload_preserves_remote_files_already_retained_outside_local_filters() {
    let mock = Mock::new(BTreeMap::new());
    let client = mock.client();
    let cancel = CancelToken::new();
    let before = client.cloud_download(game(), &cancel).await.unwrap();
    let mut rules = rules();
    rules.include = vec!["*.sav".into()];
    rules.exclude = vec!["cache/*".into()];
    let selected = BTreeMap::from([
        ("slot.sav".into(), b"managed".to_vec()),
        ("cache/remote.bin".into(), b"preserved remote".to_vec()),
    ]);
    let uploaded = client
        .cloud_upload(game(), &rules, &selected, &before.revision, &cancel)
        .await
        .unwrap();
    assert_eq!(uploaded.files, selected);
    assert_eq!(
        client.cloud_download(game(), &cancel).await.unwrap(),
        uploaded
    );
}

fn game() -> CloudGame<'static> {
    CloudGame {
        namespace: "space",
        catalog_item_id: "item",
        app_name: "game",
    }
}

#[tokio::test]
async fn cloud_download_accepts_nullable_link_metadata() {
    let files = BTreeMap::from([("slot.sav".into(), b"synthetic progress".to_vec())]);
    let mock = Mock::new(fixture(&files));
    mock.state.lock().unwrap().nullable_link_metadata = true;
    let downloaded = mock
        .client()
        .cloud_download(game(), &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(downloaded.files, files);
}

#[tokio::test]
async fn cloud_upload_readback_accepts_nullable_link_metadata() {
    let mock = Mock::new(BTreeMap::new());
    mock.state.lock().unwrap().nullable_link_metadata = true;
    let client = mock.client();
    let cancel = CancelToken::new();
    let before = client.cloud_download(game(), &cancel).await.unwrap();
    let files = BTreeMap::from([("slot.sav".into(), b"synthetic progress".to_vec())]);
    let uploaded = client
        .cloud_upload(game(), &rules(), &files, &before.revision, &cancel)
        .await
        .unwrap();
    assert_eq!(uploaded.files, files);
}

#[tokio::test]
async fn nullable_inventory_metadata_and_scalar_link_metadata_still_fail() {
    let files = BTreeMap::from([("slot.sav".into(), b"synthetic progress".to_vec())]);
    for field in ["hash", "lastModified"] {
        let mock = Mock::new(fixture(&files));
        mock.state.lock().unwrap().null_listing_field = Some(field);
        assert!(matches!(
            mock.client()
                .cloud_download(game(), &CancelToken::new())
                .await,
            Err(EpicError::Protocol)
        ));
        assert!(
            mock.state
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|(_, path, _)| !path.starts_with("/blob/"))
        );
    }
    let mock = Mock::new(fixture(&files));
    mock.state.lock().unwrap().invalid_link_hash = true;
    assert!(matches!(
        mock.client()
            .cloud_download(game(), &CancelToken::new())
            .await,
        Err(EpicError::Protocol)
    ));
    for field in ["hash", "lastModified"] {
        for invalid in [json!(false), json!(123), json!([]), json!({})] {
            let mut entry = json!({});
            entry[field] = invalid;
            assert!(serde_json::from_value::<Entry>(entry).is_err());
        }
    }
}
