//! Steam path checks and SHA-1 reads on the shared bounded verification scheduler.
use std::path::Path;

#[cfg(test)]
use forge_core::verification::worker_limit;
use forge_core::verification::{self, VerificationError, VerificationFile};

use super::{
    CancelToken, ContentError, checked_file_path, format::ManifestFile, invalid,
    safe_relative_path, verify_file,
};

impl VerificationFile for ManifestFile {
    fn name(&self) -> &str {
        &self.name
    }
    fn size(&self) -> u64 {
        self.size
    }
    fn is_directory(&self) -> bool {
        ManifestFile::is_directory(self)
    }
}

/// Preserve manifest order and count only complete verified files. Every reader
/// has stopped before this returns and the caller begins repairing game files.
pub(super) fn verify_files(
    root: &Path,
    files: &[ManifestFile],
    cancel: &CancelToken,
    progress: impl Fn(u64, u32, &str) + Sync,
) -> Result<Vec<bool>, ContentError> {
    verification::verify_files(
        files,
        cancel,
        |file| prepare(root, file),
        |path, file, cancel| verify_file(path, file, cancel),
        progress,
    )
    .map_err(map_error)
}

fn prepare(root: &Path, file: &ManifestFile) -> Result<std::path::PathBuf, ContentError> {
    Ok(checked_file_path(root, &safe_relative_path(&file.name)?)?)
}

#[cfg(test)]
fn verify_with_workers(
    root: &Path,
    files: &[ManifestFile],
    cancel: &CancelToken,
    progress: impl Fn(u64, u32, &str) + Sync,
    workers: usize,
    verify: impl Fn(&Path, &ManifestFile, &CancelToken) -> Result<bool, ContentError> + Sync,
) -> Result<Vec<bool>, ContentError> {
    verification::verify_with_workers(
        files,
        cancel,
        |file| prepare(root, file),
        |path, file, cancel| verify(path, file, cancel),
        progress,
        workers,
    )
    .map_err(map_error)
}

fn map_error(error: VerificationError<ContentError>) -> ContentError {
    match error {
        VerificationError::Cancelled => ContentError::Cancelled,
        VerificationError::File(error) => error,
        VerificationError::WorkerFailed => invalid("backend.steam.verification.workerFailed"),
        VerificationError::InvalidTotalSize => {
            invalid("backend.steam.verification.invalidTotalSize")
        }
        VerificationError::InvalidFileCount => {
            invalid("backend.steam.verification.invalidFileCount")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha1::{Digest, Sha1};
    use std::{
        fs,
        sync::{
            Barrier, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    fn file(name: &str, bytes: &[u8]) -> ManifestFile {
        ManifestFile {
            name: name.into(),
            size: bytes.len() as u64,
            sha: Sha1::digest(bytes).to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn parallel_checks_match_sequential_sha1_for_valid_corrupt_and_missing_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("folder")).unwrap();
        let bytes = vec![0x5a; 192 * 1024 + 17];
        fs::write(root.path().join("folder/valid"), &bytes).unwrap();
        fs::write(root.path().join("empty"), []).unwrap();
        let mut corrupt = bytes.clone();
        corrupt[128 * 1024] ^= 1;
        fs::write(root.path().join("corrupt"), corrupt).unwrap();
        fs::write(root.path().join("truncated"), &bytes[..100]).unwrap();
        let mut directory = file("folder", &[]);
        directory.flags = 64;
        let files = vec![
            directory,
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
                !file.is_directory()
                    && verify_file(&root.path().join(&file.name), file, &cancel).unwrap()
            })
            .collect();
        let events = Mutex::new(Vec::new());
        let parallel = verify_files(root.path(), &files, &cancel, |bytes, files, name| {
            events.lock().unwrap().push((bytes, files, name.to_owned()));
        })
        .unwrap();
        assert_eq!(parallel, sequential);
        assert_eq!(parallel, [false, true, true, false, false, false]);
        let events = events.into_inner().unwrap();
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].0 <= pair[1].0 && pair[0].1 <= pair[1].1)
        );
        let last = events.last().unwrap();
        assert_eq!((last.0, last.1), (bytes.len() as u64, 2));
        assert!(events.iter().all(|event| event.2 != "folder"));
    }

    struct ActiveReader<'a>(&'a AtomicUsize);
    impl Drop for ActiveReader<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn readers_are_bounded_by_available_parallelism_and_four() {
        let root = tempfile::tempdir().unwrap();
        let count = worker_limit();
        assert!((1..=4).contains(&count));
        let files: Vec<_> = (0..count * 3)
            .map(|index| {
                let name = format!("file-{index}");
                fs::write(root.path().join(&name), b"verified").unwrap();
                file(&name, b"verified")
            })
            .collect();
        let barrier = Barrier::new(count);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let reads = AtomicUsize::new(0);
        let result = verify_with_workers(
            root.path(),
            &files,
            &CancelToken::new(),
            |_, _, _| {},
            count,
            |path, expected, cancel| {
                let current = active.fetch_add(1, Ordering::AcqRel) + 1;
                let _reader = ActiveReader(&active);
                peak.fetch_max(current, Ordering::AcqRel);
                // Force all readers to overlap in each batch without timing assumptions.
                barrier.wait();
                let result = verify_file(path, expected, cancel);
                reads.fetch_add(1, Ordering::AcqRel);
                result
            },
        )
        .unwrap();
        assert!(result.into_iter().all(|valid| valid));
        assert_eq!(reads.load(Ordering::Acquire), files.len());
        assert_eq!(peak.load(Ordering::Acquire), count);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cancellation_joins_all_readers_and_does_not_start_remaining_jobs() {
        let root = tempfile::tempdir().unwrap();
        let count = worker_limit();
        let files: Vec<_> = (0..count * 3)
            .map(|index| file(&format!("file-{index}"), b"content"))
            .collect();
        let barrier = Barrier::new(count);
        let cancel = CancelToken::new();
        let active = AtomicUsize::new(0);
        let started = AtomicUsize::new(0);
        let result = verify_with_workers(
            root.path(),
            &files,
            &cancel,
            |_, _, _| {},
            count,
            |_, _, cancel| {
                active.fetch_add(1, Ordering::AcqRel);
                let _reader = ActiveReader(&active);
                started.fetch_add(1, Ordering::AcqRel);
                // Every worker has entered its read when cancellation becomes visible.
                barrier.wait();
                cancel.cancel();
                cancel.check()?;
                Ok(false)
            },
        );
        assert!(matches!(result, Err(ContentError::Cancelled)));
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert_eq!(started.load(Ordering::Acquire), count);
        assert!(matches!(
            verify_files(root.path(), &files, &cancel, |_, _, _| {}),
            Err(ContentError::Cancelled)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_paths_and_existing_symlinks_are_rejected_before_reading() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("target"), b"content").unwrap();
        symlink(outside.path(), root.path().join("linked")).unwrap();
        for name in ["../target", "/etc/passwd", "linked/target"] {
            let reads = AtomicUsize::new(0);
            let result = verify_with_workers(
                root.path(),
                &[file(name, b"content")],
                &CancelToken::new(),
                |_, _, _| {},
                1,
                |_, _, _| {
                    reads.fetch_add(1, Ordering::Relaxed);
                    Ok(true)
                },
            );
            assert!(matches!(result, Err(ContentError::UnsafePath(_))), "{name}");
            assert_eq!(reads.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    #[ignore = "Reads an existing installation selected by FORGE_VERIFY_BENCH_DIRECTORY; no writes or network"]
    fn benchmark_installed_files() {
        use std::{fs::File, io::Read, time::Instant};

        let root = std::env::var_os("FORGE_VERIFY_BENCH_DIRECTORY")
            .map(std::path::PathBuf::from)
            .expect("set FORGE_VERIFY_BENCH_DIRECTORY to an existing game installation")
            .canonicalize()
            .unwrap();
        assert!(root.is_dir());
        let mut directories = vec![root.clone()];
        let mut paths = Vec::new();
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name().to_string_lossy().starts_with(".forge") {
                    continue;
                }
                let kind = entry.file_type().unwrap();
                assert!(
                    !kind.is_symlink(),
                    "benchmark does not follow symbolic links"
                );
                if kind.is_dir() {
                    directories.push(entry.path());
                } else {
                    assert!(kind.is_file(), "benchmark only reads regular game files");
                    paths.push(entry.path());
                }
            }
        }
        paths.sort();
        assert!(!paths.is_empty(), "installation contains no game files");

        // Build expected hashes before either timer. Both passes receive the same
        // complete-file expectations and run the actual production verifier.
        let mut files = Vec::with_capacity(paths.len());
        let mut buffer = [0_u8; 64 * 1024];
        for path in paths {
            let relative = path.strip_prefix(&root).unwrap();
            let name = relative.to_str().expect("game path must be UTF-8");
            let checked = checked_file_path(&root, &safe_relative_path(name).unwrap()).unwrap();
            let mut input = File::open(checked).unwrap();
            let mut hasher = Sha1::new();
            let mut size = 0_u64;
            loop {
                let count = input.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                size += count as u64;
                hasher.update(&buffer[..count]);
            }
            files.push(ManifestFile {
                name: name.into(),
                size,
                sha: hasher.finalize().to_vec(),
                ..Default::default()
            });
        }
        let bytes: u64 = files.iter().map(|file| file.size).sum();
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        for workers in [1, worker_limit()] {
            let start = Instant::now();
            let result = verify_with_workers(
                &root,
                &files,
                &CancelToken::new(),
                |_, _, _| {},
                workers,
                verify_file,
            )
            .unwrap();
            let duration = start.elapsed();
            let all_valid = result.iter().all(|valid| *valid);
            println!(
                "verification_benchmark profile={profile} workers={workers} files={} bytes={bytes} seconds={:.6} all_valid={all_valid}",
                files.len(),
                duration.as_secs_f64()
            );
            assert!(all_valid, "a file changed during the read-only benchmark");
        }
    }
}
