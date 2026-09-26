use std::{
    collections::HashMap,
    net::{TcpListener, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use super::*;
use crate::content_manifest::ChunkPart;

mod prefetch;

struct RequestGate {
    path: String,
    requests: usize,
    opened: AtomicBool,
}

struct Server {
    base: Url,
    requests: Arc<Mutex<Vec<String>>>,
    headers: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(responses: HashMap<String, Vec<u8>>, stalled: bool) -> Self {
        Self::with_gate(responses, stalled, None)
    }

    fn with_gate(
        responses: HashMap<String, Vec<u8>>,
        stalled: bool,
        gate: Option<Arc<RequestGate>>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let headers = Arc::new(Mutex::new(Vec::new()));
        let thread_headers = Arc::clone(&headers);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_stop = Arc::clone(&stop);
        let responses = Arc::new(responses);
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let requests = Arc::clone(&thread_requests);
                        let headers = Arc::clone(&thread_headers);
                        let stop = Arc::clone(&thread_stop);
                        let responses = Arc::clone(&responses);
                        let gate = gate.clone();
                        workers.push(thread::spawn(move || {
                            serve(socket, requests, headers, stop, responses, stalled, gate)
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            base,
            requests,
            headers,
            stop,
            thread: Some(thread),
        }
    }

    fn paths(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn serve(
    mut socket: TcpStream,
    requests: Arc<Mutex<Vec<String>>>,
    headers: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    responses: Arc<HashMap<String, Vec<u8>>>,
    stalled: bool,
    gate: Option<Arc<RequestGate>>,
) {
    // macOS accept inherits O_NONBLOCK. Wait for the HTTP request instead
    // of treating an early WouldBlock as a disconnected client.
    socket.set_nonblocking(false).unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let Ok(count) = socket.read(&mut buffer) else {
            return;
        };
        if count == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..count]);
        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&request);
    let path = request.split_whitespace().nth(1).unwrap().to_owned();
    requests.lock().unwrap().push(path.clone());
    headers.lock().unwrap().push(request.to_string());
    if let Some(gate) = gate
        && gate.path == path
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while requests.lock().unwrap().len() < gate.requests
            && !stop.load(Ordering::Acquire)
            && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(2));
        }
        gate.opened.store(
            requests.lock().unwrap().len() >= gate.requests,
            Ordering::Release,
        );
    }
    if stalled {
        while !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(5));
        }
        return;
    }
    if let Some(body) = responses.get(path.split('?').next().unwrap()) {
        let _ = write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(body);
    } else {
        let _ = socket
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
}

fn chunk(data: &[u8], index: u32, compressed: bool) -> (Chunk, Vec<u8>) {
    let guid = [1, 2, 3, index];
    let hash = rolling_hash(data);
    let sha1: [u8; 20] = Sha1::digest(data).into();
    let payload = if compressed {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    } else {
        data.to_vec()
    };
    let mut wire = Vec::new();
    for value in [0xB1FE3AA2, 3, 66, payload.len() as u32] {
        wire.extend_from_slice(&value.to_le_bytes());
    }
    for value in guid {
        wire.extend_from_slice(&value.to_le_bytes());
    }
    wire.extend_from_slice(&hash.to_le_bytes());
    wire.push(u8::from(compressed));
    wire.extend_from_slice(&sha1);
    wire.push(3);
    wire.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wire.extend_from_slice(&payload);
    (
        Chunk {
            guid,
            hash,
            sha1,
            group: 7,
            window_size: data.len() as u32,
            file_size: wire.len() as u64,
        },
        wire,
    )
}

fn fixture() -> (Manifest, HashMap<String, Vec<u8>>, Vec<u8>) {
    let (first, first_wire) = chunk(b"--hello", 10, true);
    let (second, second_wire) = chunk(b" world--", 11, false);
    let output = b"hello world".to_vec();
    let files = vec![ManifestFile {
        filename: "Game.app/Contents/MacOS/Game".into(),
        symlink_target: None,
        hash: Sha1::digest(&output).into(),
        flags: 4,
        parts: vec![
            ChunkPart {
                guid: first.guid,
                offset: 2,
                size: 5,
            },
            ChunkPart {
                guid: second.guid,
                offset: 0,
                size: 6,
            },
        ],
    }];
    let responses = [
        (format!("/{}", first.relative_path(18)), first_wire),
        (format!("/{}", second.relative_path(18)), second_wire),
    ]
    .into();
    let manifest = Manifest {
        custom_fields: Default::default(),
        app_name: "test-game".into(),
        build_version: "1.0".into(),
        launch_executable: files[0].filename.clone(),
        launch_command: String::new(),
        files,
        chunks: [(first.guid, first), (second.guid, second)].into(),
        feature_level: 18,
        prerequisite: None,
    };
    (manifest, responses, output)
}

#[tokio::test]
async fn installs_verified_parts_and_reuses_complete_files_without_network() {
    let (manifest, responses, expected) = fixture();
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&snapshots);
    let client = ContentClient::new(Client::new());
    let first = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            move |snapshot| captured.lock().unwrap().push(snapshot),
        )
        .await
        .unwrap();
    let target = temporary.path().join(&manifest.files[0].filename);
    assert_eq!(fs::read(&target).unwrap(), expected);
    assert_eq!(first.total_bytes, 11);
    assert_eq!(first.completed_files, 1);
    assert_eq!(first.reused_bytes, 0);
    assert!(first.content_bytes > 0);
    assert_eq!(server.paths().len(), 2);
    {
        let snapshots = snapshots.lock().unwrap();
        assert!(
            snapshots
                .windows(2)
                .all(|pair| pair[0].completed_bytes <= pair[1].completed_bytes
                    && pair[0].network_bytes <= pair[1].network_bytes)
        );
        assert_eq!(snapshots.last().unwrap().completed_bytes, 11);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    let second = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(second.reused_bytes, 11);
    assert_eq!(second.downloaded_bytes, 0);
    assert_eq!(server.paths().len(), 2);
    assert!(
        !temporary
            .path()
            .join(PARTIAL_DIRECTORY)
            .join("epic")
            .join(manifest_key(&manifest))
            .exists()
    );
}

#[tokio::test]
async fn cancellation_keeps_existing_file_and_verified_chunks_for_resume() {
    let (manifest, responses, expected) = fixture();
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join(&manifest.files[0].filename);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"previous installed game").unwrap();
    let cancel = CancelToken::new();
    let from_callback = cancel.clone();
    let client = ContentClient::new(Client::new());
    let result = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &cancel,
            move |status| {
                if status.completed_bytes >= 5 {
                    from_callback.cancel();
                }
            },
        )
        .await;
    assert!(matches!(result, Err(EpicError::Cancelled)));
    assert_eq!(fs::read(&target).unwrap(), b"previous installed game");
    let first_path = format!(
        "/{}",
        manifest.chunks[&manifest.files[0].parts[0].guid].relative_path(18)
    );
    assert_eq!(
        server
            .paths()
            .iter()
            .filter(|path| **path == first_path)
            .count(),
        1
    );
    let resumed = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), expected);
    assert_eq!(resumed.reused_bytes, 5);
    assert_eq!(
        server
            .paths()
            .iter()
            .filter(|path| **path == first_path)
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_interrupts_stalled_http_without_waiting_for_timeout() {
    let (manifest, responses, _) = fixture();
    let alternative = Server::new(responses.clone(), false);
    let server = Server::new(responses, true);
    let bases = [server.base.clone(), alternative.base.clone()];
    let temporary = tempfile::tempdir().unwrap();
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let observed = Arc::clone(&server.requests);
    let interrupter = tokio::spawn(async move {
        while observed.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        trigger.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        ContentClient::new(Client::new()).download(
            Arc::new(manifest.clone()),
            &bases,
            temporary.path(),
            &cancel,
            |_| {},
        ),
    )
    .await
    .unwrap();
    interrupter.await.unwrap();
    assert!(matches!(result, Err(EpicError::Cancelled)));
    assert!(alternative.paths().is_empty());
    assert!(!temporary.path().join(&manifest.files[0].filename).exists());
}

#[tokio::test]
async fn corrupt_download_never_replaces_existing_file() {
    let (manifest, mut responses, _) = fixture();
    for bytes in responses.values_mut() {
        *bytes.last_mut().unwrap() ^= 0xff;
    }
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join(&manifest.files[0].filename);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"original").unwrap();
    let result = ContentClient::new(Client::new())
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(EpicError::Integrity)), "{result:?}");
    assert_eq!(fs::read(target).unwrap(), b"original");
}

#[tokio::test]
async fn corrupt_cached_chunk_is_downloaded_again() {
    let (manifest, responses, expected) = fixture();
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary
        .path()
        .join(PARTIAL_DIRECTORY)
        .join("epic")
        .join(manifest_key(&manifest))
        .join("chunks");
    fs::create_dir_all(&cache).unwrap();
    for chunk in manifest.chunks.values() {
        fs::write(
            cache.join(chunk_key(chunk)),
            vec![0; chunk.window_size as usize],
        )
        .unwrap();
    }
    ContentClient::new(Client::new())
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read(temporary.path().join(&manifest.files[0].filename)).unwrap(),
        expected
    );
    assert_eq!(server.paths().len(), 2);
}

#[tokio::test]
async fn rejects_unsafe_manifest_paths_before_writing() {
    let (mut manifest, responses, _) = fixture();
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("not-created");
    for name in [
        "../outside",
        "/absolute",
        "C:\\outside",
        "safe/../outside",
        ".sailover-content/lock",
        ".sailover-install.json",
    ] {
        manifest.files[0].filename = name.into();
        let result = ContentClient::new(Client::new())
            .download(
                Arc::new(manifest.clone()),
                std::slice::from_ref(&server.base),
                &destination,
                &CancelToken::new(),
                |_| {},
            )
            .await;
        assert!(matches!(result, Err(EpicError::UnsafePath(_))), "{name}");
        assert!(!destination.exists());
    }
    assert!(server.paths().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn refuses_symlink_parents_and_hardlinked_targets() {
    use std::os::unix::fs::symlink;
    let (mut manifest, responses, _) = fixture();
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    manifest.files[0].filename = "link/game".into();
    symlink(outside.path(), temporary.path().join("link")).unwrap();
    let client = ContentClient::new(Client::new());
    let result = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(EpicError::UnsafePath(_))));
    assert!(server.paths().is_empty());
    manifest.files[0].filename = "hardlink".into();
    fs::write(outside.path().join("original"), b"unchanged").unwrap();
    fs::hard_link(
        outside.path().join("original"),
        temporary.path().join("hardlink"),
    )
    .unwrap();
    let result = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(EpicError::UnsafePath(_))));
    assert_eq!(
        fs::read(outside.path().join("original")).unwrap(),
        b"unchanged"
    );
}

#[test]
fn chunk_checks_header_guid_hash_size_and_bounded_decompression() {
    let (expected, wire) = chunk(&vec![42; 1024], 10, true);
    assert_eq!(decode_chunk(&wire, &expected).unwrap(), vec![42; 1024]);
    for offset in [0, 8, 12, 16, 32, 41, 62] {
        let mut corrupted = wire.clone();
        corrupted[offset] ^= 1;
        assert!(
            decode_chunk(&corrupted, &expected).is_err(),
            "offset {offset}"
        );
    }
    for count in [0, 16, 40, 61, 65] {
        assert!(decode_chunk(&wire[..count], &expected).is_err());
    }
    let mut smaller = expected.clone();
    smaller.window_size = 8;
    assert!(decode_chunk(&wire, &smaller).is_err());
}

#[test]
fn rejects_conflicting_paths_and_unsupported_symlinks() {
    let (mut manifest, _, _) = fixture();
    let mut duplicate = manifest.files[0].clone();
    duplicate.filename = duplicate.filename.to_uppercase();
    manifest.files.push(duplicate);
    assert!(matches!(
        validate_manifest(&manifest),
        Err(EpicError::Protocol)
    ));
    manifest.files.pop();
    manifest.files[0].symlink_target = Some("../../outside".into());
    assert!(matches!(
        validate_manifest(&manifest),
        Err(EpicError::UnsupportedContent)
    ));
}

#[test]
fn rolling_hash_matches_independent_known_vector() {
    // Fixed output computed with Legendary's Python get_hash implementation:
    // https://github.com/derrod/legendary/blob/master/legendary/utils/rolling_hash.py
    assert_eq!(rolling_hash(b"hello world"), 0xd92e71f59dd35ed9);
}

#[test]
fn accepts_legacy_and_plain_v4_headers_but_rejects_encryption() {
    let (mut expected, wire) = chunk(b"uncompressed bytes", 25, false);
    for (version, header_size) in [(1_u32, 41_usize), (2, 62), (4, 98)] {
        let mut variant = wire[..header_size.min(66)].to_vec();
        variant[4..8].copy_from_slice(&version.to_le_bytes());
        variant[8..12].copy_from_slice(&(header_size as u32).to_le_bytes());
        variant.resize(header_size, 0);
        variant.extend_from_slice(&wire[66..]);
        expected.file_size = variant.len() as u64;
        assert_eq!(
            decode_chunk(&variant, &expected).unwrap(),
            b"uncompressed bytes"
        );
        variant[40] = 2;
        assert!(matches!(
            decode_chunk(&variant, &expected),
            Err(EpicError::UnsupportedContent)
        ));
    }
}

#[test]
fn a_chunk_decodes_whatever_size_the_manifest_estimated_but_never_torn() {
    let (mut expected, wire) = chunk(b"uncompressed bytes", 25, false);
    // Older manifests list a version 2 header's size for a version 3 chunk.
    expected.file_size = wire.len() as u64 - 4;
    assert_eq!(
        decode_chunk(&wire, &expected).unwrap(),
        b"uncompressed bytes"
    );
    assert!(matches!(
        decode_chunk(&wire[..wire.len() - 1], &expected),
        Err(EpicError::Protocol)
    ));
    let mut longer = wire.clone();
    longer.push(0);
    assert!(matches!(
        decode_chunk(&longer, &expected),
        Err(EpicError::Protocol)
    ));
}

#[tokio::test]
async fn dropping_download_cancels_disk_worker_and_releases_install_lock() {
    let (manifest, responses, _) = fixture();
    let server = Server::new(responses, true);
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().to_owned();
    let cancel = CancelToken::new();
    let task_cancel = cancel.clone();
    let base = server.base.clone();
    let task = tokio::spawn(async move {
        ContentClient::new(Client::new())
            .download(
                Arc::new(manifest.clone()),
                std::slice::from_ref(&base),
                &destination,
                &task_cancel,
                |_| {},
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while server.paths().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(cancel.is_cancelled());
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            temporary
                .path()
                .join(PARTIAL_DIRECTORY)
                .join("install.lock"),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while lock.try_lock().is_err() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn shared_chunks_are_requested_once_and_kept_until_last_file() {
    let (mut manifest, responses, _) = fixture();
    let mut second = manifest.files[0].clone();
    second.filename = "shared.txt".into();
    second.parts.truncate(1);
    second.hash = Sha1::digest(b"hello").into();
    manifest.files.push(second);
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let summary = ContentClient::new(Client::new())
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(summary.completed_files, 2);
    assert_eq!(summary.total_bytes, 16);
    assert_eq!(
        fs::read(temporary.path().join("shared.txt")).unwrap(),
        b"hello"
    );
    assert_eq!(server.paths().len(), 2);
}

#[tokio::test]
async fn repairs_corrupt_files_using_another_mirror_without_manifest_credentials() {
    for corrupt_response in [false, true] {
        let (manifest, responses, expected) = fixture();
        let first_responses = if corrupt_response {
            responses
                .iter()
                .map(|(path, bytes)| {
                    let mut corrupted = bytes.clone();
                    *corrupted.last_mut().unwrap() ^= 0xff;
                    (path.clone(), corrupted)
                })
                .collect()
        } else {
            HashMap::new()
        }; // This mirror returns 404 for every chunk.
        let first = Server::new(first_responses, false);
        let second = Server::new(responses, false);
        let mut bases = vec![first.base.clone(), second.base.clone()];
        for base in &mut bases {
            base.set_query(Some("Signature=manifest-only"));
        }
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join(&manifest.files[0].filename);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"corrupt local game").unwrap();
        let summary = ContentClient::new(Client::new())
            .download(
                Arc::new(manifest.clone()),
                &bases,
                temporary.path(),
                &CancelToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(fs::read(&target).unwrap(), expected);
        assert_eq!(summary.completed_files, 1);
        assert_eq!(first.paths().len(), if corrupt_response { 6 } else { 2 });
        assert_eq!(second.paths().len(), 2);
        for server in [&first, &second] {
            assert!(server.paths().iter().all(|path| !path.contains('?')));
            assert!(
                server
                    .headers
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|request| !request.to_ascii_lowercase().contains("authorization:"))
            );
        }
    }
}

#[tokio::test]
async fn abandoned_account_transfer_keeps_disk_admission_until_worker_exits() {
    use crate::client::{TransferProgress, TransferState};
    use sailover_core::transfer::Transfers;
    fn initial() -> TransferProgress {
        TransferProgress::new("test-game")
    }
    let (manifest, responses, expected) = fixture();
    let manifest = Arc::new(manifest);
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let target = temporary.path().join(&manifest.files[0].filename);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"original").unwrap();
    let transfers = Arc::new(Transfers::default());
    let owner = Arc::clone(&transfers);
    let destination = temporary.path().to_owned();
    let task_manifest = Arc::clone(&manifest);
    let filename = manifest.files[0].filename.clone();
    let base = server.base.clone();
    let (reached, waiting) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let gate = Mutex::new(Some((reached, blocked)));
    let task = tokio::spawn(async move {
        owner
            .run(initial(), |transfer| async move {
                let progress = transfer.clone();
                ContentClient::new(Client::new())
                    .download(
                        task_manifest,
                        &[base],
                        &destination,
                        transfer.cancel_token(),
                        move |snapshot| {
                            progress.update(|status| {
                                status.state = snapshot.phase.into();
                                status.completed_bytes = snapshot.completed_bytes;
                            });
                            // This report comes from the disk worker before it asks for chunks.
                            if snapshot.current_file == filename
                                && let Some((reached, blocked)) = gate.lock().unwrap().take()
                            {
                                reached.send(()).unwrap();
                                blocked.recv_timeout(Duration::from_secs(5)).unwrap();
                            }
                        },
                    )
                    .await
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(matches!(
        transfers
            .run(initial(), |_| async { Ok::<_, EpicError>(()) })
            .await,
        Err(EpicError::AlreadyDownloading)
    ));
    assert!(install::lock_engine(temporary.path()).unwrap().is_none());
    assert_eq!(fs::read(&target).unwrap(), b"original");
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while transfers.snapshot().unwrap().state != TransferState::Cancelled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(install::lock_engine(temporary.path()).unwrap().is_some());
    transfers
        .run(initial(), |transfer| async move {
            ContentClient::new(Client::new())
                .download(
                    manifest,
                    std::slice::from_ref(&server.base),
                    temporary.path(),
                    transfer.cancel_token(),
                    |_| {},
                )
                .await?;
            assert_eq!(fs::read(&target).unwrap(), expected);
            Ok::<_, EpicError>(())
        })
        .await
        .unwrap();
    assert_eq!(transfers.snapshot().unwrap().state, TransferState::Complete);
}
