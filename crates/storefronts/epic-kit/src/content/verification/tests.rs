use std::{
    fs,
    sync::{
        Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::*;
use crate::content_manifest::ChunkPart;
use sha1::{Digest, Sha1};

fn file(name: &str, bytes: &[u8]) -> ManifestFile {
    ManifestFile {
        filename: name.into(),
        symlink_target: None,
        hash: Sha1::digest(bytes).into(),
        flags: 0,
        parts: if bytes.is_empty() {
            vec![]
        } else {
            vec![ChunkPart {
                guid: [1, 2, 3, 4],
                offset: 0,
                size: bytes.len() as u32,
            }]
        },
    }
}

fn verify_with_workers(
    root: &Path,
    files: &[ManifestFile],
    cancel: &CancelToken,
    workers: usize,
    verify: impl Fn(&Path, &ManifestFile, &CancelToken) -> Result<bool, EpicError> + Sync,
) -> Result<Vec<bool>, EpicError> {
    verification::verify_with_workers(
        files,
        cancel,
        |file| prepare(root, file),
        |path, file, cancel| verify(path, file, cancel),
        |_, _, _| {},
        workers,
    )
    .map_err(map_error)
}

#[test]
fn parallel_sha1_reuses_only_complete_valid_files_and_reports_monotone_progress() {
    let root = tempfile::tempdir().unwrap();
    let bytes = vec![0x5a; 192 * 1024 + 17];
    fs::create_dir(root.path().join("folder")).unwrap();
    fs::write(root.path().join("folder/valid"), &bytes).unwrap();
    fs::write(root.path().join("empty"), []).unwrap();
    let mut corrupt = bytes.clone();
    corrupt[128 * 1024] ^= 1;
    fs::write(root.path().join("corrupt"), &corrupt).unwrap();
    fs::write(root.path().join("truncated"), &bytes[..100]).unwrap();
    let files = vec![
        file("folder/valid", &bytes),
        file("empty", &[]),
        file("corrupt", &bytes),
        file("truncated", &bytes),
        file("missing", &bytes),
    ];
    let cancel = CancelToken::new();
    let sequential: Vec<_> = files
        .iter()
        .map(|file| {
            verify_file(
                &root.path().join(&file.filename),
                file.size(),
                &file.hash,
                &cancel,
            )
            .unwrap()
        })
        .collect();
    let events = Mutex::new(Vec::new());
    let parallel = verify_files(root.path(), &files, &cancel, |bytes, count, name| {
        events.lock().unwrap().push((bytes, count, name.to_owned()));
    })
    .unwrap();
    assert_eq!(parallel, sequential);
    assert_eq!(parallel, [true, true, false, false, false]);
    let events = events.into_inner().unwrap();
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].0 <= pair[1].0 && pair[0].1 <= pair[1].1)
    );
    let last = events.last().unwrap();
    assert_eq!((last.0, last.1), (bytes.len() as u64, 2));
    assert_eq!(fs::read(root.path().join("corrupt")).unwrap(), corrupt);
    assert!(!root.path().join("missing").exists());
}

struct ActiveReader<'a>(&'a AtomicUsize);
impl Drop for ActiveReader<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[test]
fn epic_sha1_readers_overlap_with_the_same_four_reader_limit_as_steam() {
    let root = tempfile::tempdir().unwrap();
    let files: Vec<_> = (0..12)
        .map(|index| {
            let name = format!("file-{index}");
            fs::write(root.path().join(&name), b"valid content").unwrap();
            file(&name, b"valid content")
        })
        .collect();
    let overlap = Barrier::new(4);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let result = verify_with_workers(
        root.path(),
        &files,
        &CancelToken::new(),
        100,
        |path, file, cancel| {
            let readers = active.fetch_add(1, Ordering::AcqRel) + 1;
            let _reader = ActiveReader(&active);
            peak.fetch_max(readers, Ordering::AcqRel);
            overlap.wait();
            verify_file(path, file.size(), &file.hash, cancel)
        },
    )
    .unwrap();
    assert!(result.into_iter().all(|valid| valid));
    assert_eq!(peak.load(Ordering::Acquire), 4);
    assert_eq!(active.load(Ordering::Acquire), 0);
    assert!((1..=4).contains(&verification::worker_limit()));
}

#[test]
fn cancellation_waits_for_all_epic_readers_and_skips_remaining_jobs() {
    let root = tempfile::tempdir().unwrap();
    let files: Vec<_> = (0..12)
        .map(|index| {
            let name = format!("file-{index}");
            fs::write(root.path().join(&name), b"content").unwrap();
            file(&name, b"content")
        })
        .collect();
    let active = AtomicUsize::new(0);
    let started = AtomicUsize::new(0);
    let overlap = Barrier::new(4);
    let cancel = CancelToken::new();
    let result = verify_with_workers(root.path(), &files, &cancel, 4, |path, file, cancel| {
        active.fetch_add(1, Ordering::AcqRel);
        let _reader = ActiveReader(&active);
        started.fetch_add(1, Ordering::AcqRel);
        let result = verify_file(path, file.size(), &file.hash, cancel);
        overlap.wait();
        cancel.cancel();
        result
    });
    assert!(matches!(result, Err(EpicError::Cancelled)));
    assert_eq!(active.load(Ordering::Acquire), 0);
    assert_eq!(started.load(Ordering::Acquire), 4);
    let callbacks = AtomicUsize::new(0);
    assert!(matches!(
        verify_files(root.path(), &files, &cancel, |_, _, _| {
            callbacks.fetch_add(1, Ordering::Relaxed);
        }),
        Err(EpicError::Cancelled)
    ));
    assert_eq!(callbacks.load(Ordering::Relaxed), 0);
}

#[cfg(unix)]
#[test]
fn unsafe_epic_paths_and_existing_links_never_reach_the_hash_reader() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("target"), b"content").unwrap();
    symlink(outside.path(), root.path().join("linked")).unwrap();
    fs::hard_link(outside.path().join("target"), root.path().join("hardlink")).unwrap();
    for name in [
        "../target",
        "/etc/passwd",
        ".forge-content/cache",
        "linked/target",
        "hardlink",
    ] {
        let reads = AtomicUsize::new(0);
        let result = verify_with_workers(
            root.path(),
            &[file(name, b"content")],
            &CancelToken::new(),
            4,
            |_, _, _| {
                reads.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            },
        );
        assert!(matches!(result, Err(EpicError::UnsafePath(_))), "{name}");
        assert_eq!(reads.load(Ordering::Relaxed), 0);
    }
}
