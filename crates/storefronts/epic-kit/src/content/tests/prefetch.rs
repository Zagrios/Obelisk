use super::*;

fn files_fixture(count: usize) -> (Manifest, HashMap<String, Vec<u8>>, Vec<Vec<u8>>) {
    let mut manifest = fixture().0;
    manifest.files.clear();
    manifest.chunks.clear();
    let mut responses = HashMap::new();
    let mut outputs = Vec::new();
    for index in 0..count {
        let bytes = format!("content of file {index:02}").into_bytes();
        let (chunk, wire) = chunk(&bytes, index as u32 + 100, true);
        manifest.files.push(ManifestFile {
            filename: format!("Game/file-{index:02}"),
            symlink_target: None,
            hash: Sha1::digest(&bytes).into(),
            flags: 0,
            parts: vec![ChunkPart {
                guid: chunk.guid,
                offset: 0,
                size: bytes.len() as u32,
            }],
        });
        responses.insert(format!("/{}", chunk.relative_path(18)), wire);
        manifest.chunks.insert(chunk.guid, chunk);
        outputs.push(bytes);
    }
    (manifest, responses, outputs)
}

#[tokio::test]
async fn later_files_download_before_the_first_response_and_window_keeps_advancing() {
    let (manifest, responses, outputs) = files_fixture(FILE_PREFETCH + 3);
    let first = &manifest.chunks[&manifest.files[0].parts[0].guid];
    // Each fixture file has one chunk. The blocked first response must leave
    // room for other files, up to the smaller file-window and network limits.
    let overlapping_files = manifest
        .files
        .len()
        .min(FILE_PREFETCH)
        .min(CHUNK_CONCURRENCY);
    assert!(
        overlapping_files > 1,
        "this test requires overlap across files"
    );
    let gate = Arc::new(RequestGate {
        path: format!("/{}", first.relative_path(18)),
        requests: overlapping_files,
        opened: AtomicBool::new(false),
    });
    let server = Server::with_gate(responses, false, Some(Arc::clone(&gate)));
    let temporary = tempfile::tempdir().unwrap();
    let summary = tokio::time::timeout(
        Duration::from_secs(8),
        ContentClient::new(Client::new()).download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        gate.opened.load(Ordering::Acquire),
        "later files must be requested while file 0 is blocked"
    );
    assert_eq!(summary.completed_files as usize, outputs.len());
    assert_eq!(server.paths().len(), outputs.len());
    for (file, expected) in manifest.files.iter().zip(outputs) {
        assert_eq!(
            fs::read(temporary.path().join(&file.filename)).unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn shared_chunks_cross_the_window_and_survive_cancellation_with_cached_repairs() {
    let (mut manifest, responses, mut outputs) = files_fixture(FILE_PREFETCH + 3);
    let shared = manifest.files[0].parts[0].clone();
    manifest.files[0].parts.push(shared.clone());
    let original_shared = outputs[0].clone();
    outputs[0].extend_from_slice(&original_shared);
    manifest.files[0].hash = Sha1::digest(&outputs[0]).into();
    for index in [3, FILE_PREFETCH + 2] {
        manifest.files[index].parts.insert(0, shared.clone());
        outputs[index] = [original_shared.as_slice(), outputs[index].as_slice()].concat();
        manifest.files[index].hash = Sha1::digest(&outputs[index]).into();
    }
    let server = Server::new(responses, false);
    let temporary = tempfile::tempdir().unwrap();
    let cache = temporary
        .path()
        .join(PARTIAL_DIRECTORY)
        .join("epic")
        .join(manifest_key(&manifest))
        .join("chunks");
    fs::create_dir_all(&cache).unwrap();
    let cached = &manifest.chunks[&manifest.files[2].parts[0].guid];
    fs::write(cache.join(chunk_key(cached)), &outputs[2]).unwrap();
    let corrupt = &manifest.chunks[&manifest.files[4].parts[0].guid];
    fs::write(
        cache.join(chunk_key(corrupt)),
        vec![0; corrupt.window_size as usize],
    )
    .unwrap();
    fs::create_dir_all(temporary.path().join("Game")).unwrap();
    fs::write(
        temporary.path().join(&manifest.files[1].filename),
        &outputs[1],
    )
    .unwrap();
    let old_target = temporary.path().join(&manifest.files[3].filename);
    fs::write(&old_target, b"previous installed file").unwrap();
    let cancel = CancelToken::new();
    let stop = cancel.clone();
    let client = ContentClient::new(Client::new());
    let result = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &cancel,
            move |progress| {
                // One complete file was already present. Stop after publishing file 0.
                if progress.completed_files == 2 && progress.content_bytes > 0 {
                    stop.cancel();
                }
            },
        )
        .await;
    assert!(matches!(result, Err(EpicError::Cancelled)));
    assert_eq!(fs::read(&old_target).unwrap(), b"previous installed file");
    assert_eq!(
        fs::read(temporary.path().join(&manifest.files[0].filename)).unwrap(),
        outputs[0]
    );
    let shared_chunk = &manifest.chunks[&shared.guid];
    assert_eq!(
        fs::read(cache.join(chunk_key(shared_chunk))).unwrap(),
        original_shared
    );
    let summary = client
        .download(
            Arc::new(manifest.clone()),
            std::slice::from_ref(&server.base),
            temporary.path(),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(summary.completed_files as usize, outputs.len());
    assert_eq!(
        summary.reused_bytes,
        (original_shared.len() * 4 + outputs[1].len() + outputs[2].len()) as u64
    );
    for (file, expected) in manifest.files.iter().zip(outputs) {
        assert_eq!(
            fs::read(temporary.path().join(&file.filename)).unwrap(),
            expected
        );
    }
    let paths = server.paths();
    let shared_path = format!("/{}", shared_chunk.relative_path(18));
    assert_eq!(paths.iter().filter(|path| **path == shared_path).count(), 1);
    for index in [1, 2] {
        let chunk = &manifest.chunks[&manifest.files[index].parts[0].guid];
        assert!(!paths.contains(&format!("/{}", chunk.relative_path(18))));
    }
    assert!(!cache.exists());
}

#[tokio::test]
async fn prefetch_window_is_bounded_and_closed_requests_preserve_the_ordered_error() {
    let (manifest, _, outputs) = files_fixture(FILE_PREFETCH + 3);
    let manifest = Arc::new(manifest);
    let temporary = tempfile::tempdir().unwrap();
    // This test plays the network side.
    let (chunks, mut requests, deliveries) = Chunks::detached(PIPELINE.queue);
    let disk = DiskInstall::engine_side(
        temporary.path().to_owned(),
        Arc::clone(&manifest),
        CancelToken::new(),
        Reporter::new(DownloadProgress::default(), |_| {}),
    );
    let disk = tokio::task::spawn_blocking(move || disk(chunks));
    // One chunk per file: the chunks requested ahead bound the window.
    for file in &manifest.files[..FILE_PREFETCH.min(QUEUED_CHUNKS)] {
        let chunk = tokio::time::timeout(Duration::from_secs(2), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chunk.guid, file.parts[0].guid);
    }
    // The disk side waits for file 0's chunk before it plans another file.
    assert!(matches!(
        requests.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    // File 1's chunk fails for good: the network side stops after delivering
    // file 0's chunk and that error, so the refill after file 0 is refused.
    // It must not replace the ordered error with Protocol.
    drop(requests);
    deliveries.send(Ok(outputs[0].clone())).await.unwrap();
    deliveries.send(Err(EpicError::Http(404))).await.unwrap();
    drop(deliveries);
    let result = tokio::time::timeout(Duration::from_secs(8), disk)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(EpicError::Http(404))));
    let root = temporary.path();
    assert_eq!(
        fs::read(root.join(&manifest.files[0].filename)).unwrap(),
        outputs[0]
    );
    assert!(!root.join(&manifest.files[1].filename).exists());
    assert!(
        root.join(PARTIAL_DIRECTORY)
            .join("epic")
            .join(manifest_key(&manifest))
            .is_dir()
    );
    assert!(install::lock_engine(root).unwrap().is_some());
}
