use super::*;
use crate::auth::{Api, Credentials, Session};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
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
    /// An object whose read waits for the next link request, logged as
    /// `DONE` once answered.
    slow_object: Option<String>,
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
            slow_object: None,
        }));
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_stop = stopped.clone();
        let thread_state = state.clone();
        let thread_base = base.clone();
        let handle = thread::spawn(move || {
            let mut served = Vec::new();
            while !thread_stop.load(Ordering::Relaxed) {
                let (socket, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                let state = thread_state.clone();
                let base = thread_base.clone();
                // Transfers overlap, so each connection is answered on its own.
                served.push(thread::spawn(move || serve(socket, &state, &base)));
            }
            // A failed assertion while answering fails the test.
            for handler in served {
                handler.join().unwrap();
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

fn links(state: &Mutex<State>) -> usize {
    let state = state.lock().unwrap();
    state
        .requests
        .iter()
        .filter(|(method, _, _)| method == "POST")
        .count()
}

fn serve(mut socket: TcpStream, thread_state: &Mutex<State>, thread_base: &str) {
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
        return;
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
            let slow = state.slow_object.as_deref() == Some(name);
            drop(state);
            thread::sleep(Duration::from_millis(delay));
            if slow {
                // Answered once a second link request came, or after a while.
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline && links(thread_state) < 2 {
                    thread::sleep(Duration::from_millis(2));
                }
                let mut state = thread_state.lock().unwrap();
                state.requests.push(("DONE".into(), path.to_owned(), false));
            }
            let output = format!(
                "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(output.as_bytes());
            let _ = socket.write_all(&body);
            return;
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

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn hex(bytes: &[u8]) -> String {
    obelisk_core::install::hex(&Sha1::digest(bytes))
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

/// A complete read as a Cloud sync makes it: the listing, every file it names,
/// then the inventory again, which must not have changed.
#[derive(Debug, PartialEq, Eq)]
struct Generation {
    revision: String,
    files: BTreeMap<String, Vec<u8>>,
}
impl Generation {
    fn revision(&self) -> &str {
        &self.revision
    }
}
async fn download_all(
    client: &Client,
    game: CloudGame<'_>,
    cancel: &CancelToken,
) -> Result<Generation, EpicError> {
    let listing = client.cloud_list(game, cancel).await?;
    let names = listing.files().map(|(name, _)| name.to_owned()).collect();
    let files = client.cloud_fetch(game, &listing, &names, cancel).await?;
    if client.cloud_revision(game, &listing, cancel).await? != listing.revision() {
        return Err(EpicError::CloudConflict);
    }
    Ok(Generation {
        revision: listing.revision().to_owned(),
        files,
    })
}
/// Whether `listing` names exactly `files`, with their sizes and hashes.
fn lists(listing: &CloudListing, files: &BTreeMap<String, Vec<u8>>) -> bool {
    listing.files().eq(files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.len() as u64)))
        && files.iter().all(|(name, bytes)| listing.holds(name, bytes))
}

#[tokio::test]
async fn upload_download_and_all_deleted_generation_require_full_readback() {
    let mock = Mock::new(BTreeMap::new());
    let client = mock.client();
    let cancel = CancelToken::new();
    let empty = download_all(&client, game(), &cancel).await.unwrap();
    assert!(empty.files.is_empty());
    let files = BTreeMap::from([
        ("slot.sav".into(), b"progress 1".to_vec()),
        ("empty.sav".into(), vec![]),
    ]);
    let sent = mock.state.lock().unwrap().requests.len();
    let uploaded = client
        .cloud_upload(game(), &rules(), &files, empty.revision(), &cancel)
        .await
        .unwrap();
    assert!(lists(&uploaded, &files));
    assert_ne!(uploaded.revision(), empty.revision());
    {
        // The upload itself reads every chunk back after writing them.
        let state = mock.state.lock().unwrap();
        let upload = &state.requests[sent..];
        let last_put = upload
            .iter()
            .rposition(|(method, path, _)| method == "PUT" && path.contains("ChunksV4"))
            .unwrap();
        assert!(
            upload[last_put..]
                .iter()
                .any(|(method, path, _)| method == "GET" && path.contains("/blob/ChunksV4"))
        );
    }
    let read = download_all(&client, game(), &cancel).await.unwrap();
    assert_eq!(read.files, files);
    assert_eq!(read.revision, uploaded.revision());
    let deleted = client
        .cloud_upload(
            game(),
            &rules(),
            &BTreeMap::new(),
            uploaded.revision(),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(deleted.files().count(), 0);
    assert_ne!(deleted.revision(), uploaded.revision());
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
    let before = download_all(&client, game(), &cancel).await.unwrap();
    mock.state.lock().unwrap().fail_chunk_put = true;
    let files = BTreeMap::from([("slot.sav".into(), vec![7; 100])]);
    assert!(matches!(
        client
            .cloud_upload(game(), &rules(), &files, before.revision(), &cancel)
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
    assert!(download_all(&client, game(), &cancel).await.is_err());
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
            download_all(&client, game(), &CancelToken::new())
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
    let client = mock.client();
    let cancel = CancelToken::new();
    let listing = client.cloud_list(game(), &cancel).await.unwrap();
    assert_ne!(
        client
            .cloud_revision(game(), &listing, &cancel)
            .await
            .unwrap(),
        listing.revision()
    );
    let mock = Mock::new(BTreeMap::new());
    let client = mock.client();
    let cancel = CancelToken::new();
    let empty = download_all(&client, game(), &cancel).await.unwrap();
    mock.state.lock().unwrap().mutate_on_list =
        Some((4, manifest_name.clone(), manifest_bytes.clone()));
    assert!(matches!(
        client
            .cloud_upload(game(), &rules(), &files, empty.revision(), &cancel)
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
    let task = download_all(&client, game(), &cancel);
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
        client.session.discard().await;
    };
    let (result, _) = tokio::join!(download_all(&client, game(), &cancel), discard);
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
        let result = download_all(&mock.client(), game(), &CancelToken::new()).await;
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
        assert!(download_all(&client, requested, &cancel).await.is_err());
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
    let before = download_all(&client, game(), &cancel).await.unwrap();
    let mut rules = rules();
    rules.include = vec!["*.sav".into()];
    rules.exclude = vec!["cache/*".into()];
    let selected = BTreeMap::from([
        ("slot.sav".into(), b"managed".to_vec()),
        ("cache/remote.bin".into(), b"preserved remote".to_vec()),
    ]);
    let uploaded = client
        .cloud_upload(game(), &rules, &selected, before.revision(), &cancel)
        .await
        .unwrap();
    assert!(lists(&uploaded, &selected));
    let read = download_all(&client, game(), &cancel).await.unwrap();
    assert_eq!(read.files, selected);
    assert_eq!(read.revision, uploaded.revision());
}

fn game() -> CloudGame<'static> {
    CloudGame {
        namespace: "space",
        catalog_item_id: "item",
        app_name: "game",
    }
}

#[tokio::test]
async fn cloud_fetch_accepts_nullable_link_metadata() {
    let files = BTreeMap::from([("slot.sav".into(), b"synthetic progress".to_vec())]);
    let mock = Mock::new(fixture(&files));
    mock.state.lock().unwrap().nullable_link_metadata = true;
    let downloaded = download_all(&mock.client(), game(), &CancelToken::new())
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
    let before = download_all(&client, game(), &cancel).await.unwrap();
    let files = BTreeMap::from([("slot.sav".into(), b"synthetic progress".to_vec())]);
    let uploaded = client
        .cloud_upload(game(), &rules(), &files, before.revision(), &cancel)
        .await
        .unwrap();
    assert!(lists(&uploaded, &files));
}

#[tokio::test]
async fn nullable_inventory_metadata_and_scalar_link_metadata_still_fail() {
    let files = BTreeMap::from([("slot.sav".into(), b"synthetic progress".to_vec())]);
    for field in ["hash", "lastModified"] {
        let mock = Mock::new(fixture(&files));
        mock.state.lock().unwrap().null_listing_field = Some(field);
        assert!(matches!(
            download_all(&mock.client(), game(), &CancelToken::new()).await,
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
        download_all(&mock.client(), game(), &CancelToken::new()).await,
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

fn chunk_reads(state: &State) -> usize {
    state
        .requests
        .iter()
        .filter(|(method, path, _)| method == "GET" && path.contains("/blob/ChunksV4"))
        .count()
}

#[tokio::test]
async fn fetching_some_files_downloads_only_their_chunks() {
    // The first save fills a whole chunk, so the second starts its own.
    let files = BTreeMap::from([
        ("a.sav".into(), vec![1; 1024 * 1024]),
        ("b.sav".into(), b"small save".to_vec()),
    ]);
    let mock = Mock::new(fixture(&files));
    let client = mock.client();
    let cancel = CancelToken::new();
    let listing = client.cloud_list(game(), &cancel).await.unwrap();
    let wanted = BTreeSet::from(["b.sav".to_owned()]);
    let fetched = client
        .cloud_fetch(game(), &listing, &wanted, &cancel)
        .await
        .unwrap();
    assert_eq!(
        fetched,
        BTreeMap::from([("b.sav".into(), b"small save".to_vec())])
    );
    assert_eq!(chunk_reads(&mock.state.lock().unwrap()), 1);

    let unlisted = BTreeSet::from(["c.sav".to_owned()]);
    assert!(matches!(
        client
            .cloud_fetch(game(), &listing, &unlisted, &cancel)
            .await,
        Err(EpicError::Protocol)
    ));
    assert_eq!(chunk_reads(&mock.state.lock().unwrap()), 1);
}

#[tokio::test]
async fn chunk_transfers_refill_across_link_batches() {
    let files = BTreeMap::from([
        ("a.sav".into(), vec![0; 51 * 1024 * 1024]),
        ("b.sav".into(), vec![0; 51 * 1024 * 1024]),
    ]);
    let objects = fixture(&files);
    let (_, manifest) = objects
        .iter()
        .find(|(name, _)| name.starts_with("manifests/"))
        .unwrap();
    let manifest = codec::parse(manifest, game(), "account").unwrap();
    assert!(manifest.chunks.len() > LINK_BATCH);
    // The first chunk of the first batch is answered last.
    let first = manifest
        .chunks
        .values()
        .next()
        .unwrap()
        .relative_path(manifest.feature_level);
    let mock = Mock::new(objects);
    mock.state.lock().unwrap().slow_object = Some(first.clone());
    let client = mock.client();
    let read = download_all(&client, game(), &CancelToken::new())
        .await
        .unwrap();
    assert_eq!(read.files, files);
    let state = mock.state.lock().unwrap();
    let links: Vec<_> = state
        .requests
        .iter()
        .enumerate()
        .filter(|(_, (method, _, _))| method == "POST")
        .map(|(index, _)| index)
        .collect();
    assert_eq!(links.len(), manifest.chunks.len().div_ceil(LINK_BATCH));
    let done = state
        .requests
        .iter()
        .position(|(method, path, _)| method == "DONE" && path.ends_with(&first))
        .unwrap();
    // The next batch's links came while the slow chunk of the first was
    // still being read.
    assert!(links[1] < done);
}

#[tokio::test]
async fn a_listing_holds_only_bytes_with_its_size_and_sha1() {
    let files = BTreeMap::from([("slot.sav".into(), b"progress".to_vec())]);
    let mock = Mock::new(fixture(&files));
    let listing = mock
        .client()
        .cloud_list(game(), &CancelToken::new())
        .await
        .unwrap();
    assert!(lists(&listing, &files));
    assert!(listing.holds("slot.sav", b"progress"));
    assert!(!listing.holds("slot.sav", b"progresz"));
    assert!(!listing.holds("slot.sav", b"progress!"));
    assert!(!listing.holds("other.sav", b"progress"));
    let built = CloudListing::of("revision".into(), "account".into(), &files);
    assert!(lists(&built, &files));
    assert_eq!(built.revision(), "revision");
}
