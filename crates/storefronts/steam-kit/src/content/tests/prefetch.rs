use super::*;

/// The disk side of depot 42's engine, for `transfer::run_engine`.
fn disk_install(
    destination: &Path,
    files: Vec<ManifestFile>,
    cancel: &CancelToken,
) -> (
    impl FnOnce(DepotChunks) -> Result<u64, ContentError> + Send + 'static,
    Arc<Reporter>,
) {
    let reporter = Reporter::new(
        DownloadProgress {
            total_bytes: files.iter().map(|file| file.size).sum(),
            total_files: files.iter().filter(|file| !file.is_directory()).count() as u32,
            ..Default::default()
        },
        |_| {},
    );
    let manifest = Arc::new(Manifest {
        depot_id: 42,
        manifest_id: 99,
        files,
    });
    let disk = DiskInstall::engine_side(
        destination.to_owned(),
        42,
        99,
        None,
        manifest,
        cancel.clone(),
        Arc::clone(&reporter),
    );
    (disk, reporter)
}

#[tokio::test]
async fn fetches_a_future_file_before_the_first_finishes_and_refills_the_window() {
    let directory = tempfile::tempdir().unwrap();
    let contents: Vec<_> = (0..12)
        .map(|index| format!("unique contents of file {index}").into_bytes())
        .collect();
    let mut files: Vec<_> = contents
        .iter()
        .enumerate()
        .map(|(index, bytes)| file(&format!("data/{index:02}.bin"), &[bytes]))
        .collect();
    let first = files[0].chunks[0].sha.clone();
    let second = files[1].chunks[0].sha.clone();
    let payloads: HashMap<_, _> = files
        .iter()
        .zip(&contents)
        .map(|(file, bytes)| (file.chunks[0].sha.clone(), bytes.clone()))
        .collect();
    // Neither a valid file nor a directory between two repairs should stop
    // requests for the next file. The empty file still needs publication.
    let valid = b"already installed";
    fs::create_dir(directory.path().join("data")).unwrap();
    fs::write(directory.path().join("data/valid.bin"), valid).unwrap();
    files.insert(1, file("data/valid.bin", &[valid]));
    files.insert(
        2,
        ManifestFile {
            name: "data".into(),
            flags: 64,
            ..Default::default()
        },
    );
    files.push(file("data/empty.bin", &[]));
    let cancel = CancelToken::new();
    let (disk, reporter) = disk_install(directory.path(), files, &cancel);
    let future_started = tokio::sync::Notify::new();
    let observed = AtomicBool::new(false);
    let fetched = Mutex::new(Vec::new());
    let installed =
        transfer::run_engine(pipeline(), &cancel, disk, |(place, chunk): ChunkRequest| {
            let first = &first;
            let second = &second;
            let payloads = &payloads;
            let future_started = &future_started;
            let observed = &observed;
            let fetched = &fetched;
            async move {
                fetched.lock().unwrap().push(chunk.sha.clone());
                if &chunk.sha == first {
                    // The old file-at-a-time scheduler cannot start file two
                    // while this first chunk is held, and fails this deadline.
                    tokio::time::timeout(Duration::from_secs(2), future_started.notified())
                        .await
                        .map_err(|_| ContentError::Network)?;
                } else if &chunk.sha == second {
                    observed.store(true, Ordering::Release);
                    future_started.notify_one();
                }
                Ok((place, payloads.get(&chunk.sha).unwrap().clone()))
            }
        })
        .await;
    assert_eq!(installed.unwrap(), valid.len() as u64);
    assert!(observed.load(Ordering::Acquire));
    for (index, bytes) in contents.iter().enumerate() {
        assert_eq!(
            fs::read(directory.path().join(format!("data/{index:02}.bin"))).unwrap(),
            *bytes
        );
    }
    assert!(directory.path().join("data/empty.bin").is_file());
    assert_eq!(
        fs::read(directory.path().join("data/valid.bin")).unwrap(),
        valid
    );
    assert_eq!(fetched.lock().unwrap().len(), contents.len());
    let status = reporter.snapshot();
    assert_eq!(status.completed_files, 14);
    assert_eq!(status.completed_bytes, status.total_bytes);
}

#[tokio::test]
async fn cancellation_bounds_prepared_files_and_resumes_each_verified_partial() {
    let directory = tempfile::tempdir().unwrap();
    // More files than may be prepared at once, with fewer chunks than the window.
    let pieces: Vec<_> = (0..PREPARED_FILES + 8)
        .map(|index| {
            [
                format!("first chunk of file {index}").into_bytes(),
                format!("second chunk of file {index}").into_bytes(),
            ]
        })
        .collect();
    let files: Vec<_> = pieces
        .iter()
        .enumerate()
        .map(|(index, parts)| file(&format!("{index:02}.bin"), &[&parts[0], &parts[1]]))
        .collect();
    let mut responses = HashMap::from([(
        "/depot/42/manifest/99/5/123".into(),
        zip(&manifest(files.clone(), false)),
    )]);
    for bytes in pieces.iter().flatten() {
        responses.insert(
            format!("/depot/42/chunk/{}", hex(&Sha1::digest(bytes))),
            encrypt(&zip(bytes)),
        );
    }
    let original = b"old working version";
    for file in &files {
        fs::write(directory.path().join(&file.name), original).unwrap();
    }
    // A later file already has one valid partial chunk before prefetch begins.
    let partial_root = directory.path().join(PARTIAL_DIRECTORY).join("42/99");
    fs::create_dir_all(&partial_root).unwrap();
    let reused_path = partial_root.join(format!(
        "{}.part",
        hex(&Sha1::digest(files[3].name.as_bytes()))
    ));
    fs::write(reused_path, &pieces[3][0]).unwrap();
    let fixture = CdnFixture::new(responses);
    let client = fixture.client();
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let partial_count = Arc::new(Mutex::new(None));
    let count = Arc::clone(&partial_count);
    let counted_root = partial_root.clone();
    let reused = pieces[3][0].len() as u64;
    let result = client
        .download_depot(&plan(directory.path()), &cancel, move |status| {
            if status.completed_bytes > reused && !trigger.is_cancelled() {
                // The first chunk was written, but no replacement has been
                // published.
                *count.lock().unwrap() = Some(fs::read_dir(&counted_root).unwrap().count());
                trigger.cancel();
            }
        })
        .await;
    assert!(matches!(result, Err(ContentError::Cancelled)));
    assert_eq!(*partial_count.lock().unwrap(), Some(PREPARED_FILES));
    for file in &files {
        assert_eq!(
            fs::read(directory.path().join(&file.name)).unwrap(),
            original
        );
    }
    assert!(install::lock_engine(directory.path()).unwrap().is_some());
    let resumed = client
        .download_depot(&plan(directory.path()), &CancelToken::new(), |_| {})
        .await
        .unwrap()
        .summary;
    // The verified partial chunk, and the chunk written before cancellation.
    assert!(resumed.reused_bytes > reused);
    assert_eq!(resumed.completed_files, files.len() as u32);
    for (file, parts) in files.iter().zip(&pieces) {
        assert_eq!(
            fs::read(directory.path().join(&file.name)).unwrap(),
            parts.concat()
        );
    }
    let route = format!("/depot/42/chunk/{}", hex(&Sha1::digest(&pieces[3][0])));
    assert!(!fixture.requests.lock().unwrap().contains(&route));
}

#[tokio::test]
async fn a_refused_request_stops_preparing_and_returns_the_queued_cdn_error() {
    let directory = tempfile::tempdir().unwrap();
    let (kept, lost): (&[u8], &[u8]) = (b"kept chunk of file 00", b"lost chunk of file 00");
    let mut files = vec![file("00.bin", &[kept, lost])];
    files.extend((1..10).map(|index| file(&format!("{index:02}.bin"), &[b"new file content"])));
    // 00.bin's partial already holds its first chunk: only the second is requested.
    let partial_root = directory.path().join(PARTIAL_DIRECTORY).join("42/99");
    fs::create_dir_all(&partial_root).unwrap();
    let partial = format!("{}.part", hex(&Sha1::digest(b"00.bin")));
    fs::write(partial_root.join(&partial), kept).unwrap();
    let cancel = CancelToken::new();
    let (disk, _) = disk_install(directory.path(), files, &cancel);
    // The network side already stopped on a CDN error: its outcome is queued
    // and every request is refused.
    let (chunks, requests, deliveries) = DepotChunks::detached(WRITE_QUEUE);
    drop(requests);
    deliveries.send(Err(ContentError::Http(403))).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        tokio::task::spawn_blocking(move || disk(chunks)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(result, Err(ContentError::Http(403))), "{result:?}");
    assert!(
        !directory.path().join("00.bin").exists(),
        "a file missing a refused chunk is never published"
    );
    let prepared: Vec<_> = fs::read_dir(&partial_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(prepared, [partial.as_str()], "no file is prepared after it");
    assert!(install::lock_engine(directory.path()).unwrap().is_some());
}
