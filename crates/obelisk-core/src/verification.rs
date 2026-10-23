//! Bounded full-file verification shared by storefront installers.
//!
//! One call confines every manifest name under the installation root, reads the
//! file through its digest with at most eight disk readers, serializes progress, and
//! joins every reader before returning to the caller's repair loop. Storefronts
//! describe their manifest entries and map one error; they do not repeat the
//! path checks or the hashing.
use std::{
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

mod known;

pub use known::forget_verified;
use known::record_verified;

use crate::{
    install::{self, FileDigest, InstallError, Matched},
    transfer::{CancelToken, Cancelled, Reporter},
};

/// Measured on an M1 Pro: eight readers verified 2.5 GB of an app folder at
/// about 2.0 GB/s against 1.4–1.6 with four, and an external USB disk read
/// faster with eight too, while a download wrote to it.
const MAX_WORKERS: usize = 8;

/// What the scheduler needs from one manifest entry: where it goes, how big it
/// must be and what it must hash to. Directory entries remain in the result
/// vector as `false` without being passed to a reader.
pub trait VerificationFile {
    /// The manifest name, confined under the installation root before any read.
    fn name(&self) -> &str;
    fn size(&self) -> u64;
    /// What the fully installed file hashes to.
    fn digest(&self) -> FileDigest<'_>;
    fn is_directory(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub enum VerificationError {
    Cancelled,
    /// A manifest name escaped the installation root, or a read failed.
    Install(InstallError),
    /// A reader panicked, or a verified byte or file counter overflowed. The
    /// scheduler's own failure modes, opaque to callers.
    SchedulerFailed,
}

impl From<Cancelled> for VerificationError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<InstallError> for VerificationError {
    fn from(error: InstallError) -> Self {
        match error {
            InstallError::Cancelled => Self::Cancelled,
            other => Self::Install(other),
        }
    }
}

/// The outcome of one verification pass, in manifest order.
#[derive(Debug, Default)]
pub struct Verified {
    /// One flag per manifest entry. Directory entries are always `false`.
    pub valid: Vec<bool>,
    /// One count per manifest entry: its leading digest parts that matched,
    /// such as the chunks of a GOG file a repair can keep.
    pub matched_parts: Vec<usize>,
    /// Bytes already on disk and verified whole: what this installation reuses.
    pub reused_bytes: u64,
}

pub fn worker_limit() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_WORKERS)
}

/// Verify every file of the installation at `root` with the host's available
/// parallelism, bounded to eight disk readers. A file the last finished
/// Transfer recorded whole, whose disk still says the same of it, is not
/// read again; see [`record_verified`]. Every reader has stopped before
/// this returns, so the caller may begin repairing files immediately.
///
/// Inspection bytes, verified whole files and the current name are published
/// on `reporter` as they are counted. Whole-file validation stays separate from
/// inspection progress, so corrupt files never count as reusable. Updates run
/// inline under the reporter's lock; its callback must not reenter this verifier.
pub fn verify_files<T>(
    root: &Path,
    files: &[T],
    cancel: &CancelToken,
    reporter: &Reporter,
) -> Result<Verified, VerificationError>
where
    T: VerificationFile + Sync,
{
    verify_with_workers(root, files, cancel, reporter, worker_limit())
}

/// An explicit reader limit supports single-reader benchmarks and deterministic
/// overlap tests. Values are clamped to the production ceiling of eight.
pub fn verify_with_workers<T>(
    root: &Path,
    files: &[T],
    cancel: &CancelToken,
    reporter: &Reporter,
    workers: usize,
) -> Result<Verified, VerificationError>
where
    T: VerificationFile + Sync,
{
    cancel.check()?;
    reporter.update(|status| status.verification_bytes = Some(0));
    let known = known::Known::load(root);
    let verified = schedule(
        files,
        cancel,
        |file| confined_path(root, file),
        |path, file: &T, cancel| {
            let mut inspected = 0_u64;
            let mut report = |bytes: u64| {
                // The file can change while it is read. Its contribution must
                // never exceed the size in the manifest.
                let bytes = bytes.min(file.size().saturating_sub(inspected));
                inspected += bytes;
                reporter.update(|status| {
                    status.verification_bytes =
                        Some(status.verification_bytes.unwrap_or(0).saturating_add(bytes));
                    status.current_file.replace_range(.., file.name());
                });
            };
            let matched = if known.holds(path, file) {
                Matched {
                    whole: true,
                    parts: match file.digest() {
                        FileDigest::Sha1(_) => 1,
                        FileDigest::Md5Parts(parts) => parts.len(),
                    },
                }
            } else {
                install::verify_file_with_progress(
                    path,
                    file.size(),
                    file.digest(),
                    cancel,
                    &mut report,
                )?
            };
            // Missing and wrong-size files, and the files the record knows,
            // are inspected without being read.
            // Their full size still belongs to this pass's progress denominator.
            let remaining = file.size().saturating_sub(inspected);
            if remaining != 0 {
                reporter.update(|status| {
                    status.verification_bytes = Some(
                        status
                            .verification_bytes
                            .unwrap_or(0)
                            .saturating_add(remaining),
                    );
                    status.current_file.replace_range(.., file.name());
                });
            }
            Ok(matched)
        },
        |bytes, files, name| {
            reporter.update(|status| {
                status.completed_bytes = bytes;
                status.completed_files = files;
                status.current_file.replace_range(.., name);
            });
        },
        workers,
    )
    .map_err(|error| match error {
        ScheduleError::Cancelled => VerificationError::Cancelled,
        ScheduleError::File(error) => error.into(),
        ScheduleError::WorkerFailed
        | ScheduleError::InvalidTotalSize
        | ScheduleError::InvalidFileCount => VerificationError::SchedulerFailed,
    })?;
    // Repair uses validated bytes and may inspect partial chunks of its own.
    // Do not let the initial scan's numerator leak into that phase.
    reporter.update(|status| status.verification_bytes = None);
    Ok(verified)
}

/// Makes the installation at `root`, every file of it in place, durable with
/// one flush of its drive, then records `files` whole for the next Transfer
/// to skip; see [`record_verified`]. The record only saves time: a failure to
/// write it costs none.
pub fn complete<T: VerificationFile>(root: &Path, files: &[T]) -> std::io::Result<()> {
    install::make_durable(root)?;
    let _ = record_verified(root, files);
    Ok(())
}

/// Resolved before a file is reported, so an unsafe name never reaches a reader.
fn confined_path<T: VerificationFile>(root: &Path, file: &T) -> Result<PathBuf, InstallError> {
    install::checked_file_path(root, &install::safe_relative_path(file.name())?)
}

/// The scheduler's internal failures, before they are narrowed for callers.
#[derive(Debug)]
enum ScheduleError<E> {
    Cancelled,
    File(E),
    WorkerFailed,
    InvalidTotalSize,
    InvalidFileCount,
}

impl<E> From<Cancelled> for ScheduleError<E> {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

struct CheckedFiles {
    valid: Vec<bool>,
    matched_parts: Vec<usize>,
    bytes: u64,
    files: u32,
}

/// The reader pool itself. `prepare` and `verify` stay injectable so this
/// module's own tests can drive panics, read failures and counter overflow
/// without a filesystem; they are an internal seam, not part of the interface.
fn schedule<T, P, E>(
    files: &[T],
    cancel: &CancelToken,
    prepare: impl Fn(&T) -> Result<P, E> + Sync,
    verify: impl Fn(&P, &T, &CancelToken) -> Result<Matched, E> + Sync,
    progress: impl Fn(u64, u32, &str) + Sync,
    workers: usize,
) -> Result<Verified, ScheduleError<E>>
where
    T: VerificationFile + Sync,
    E: Send,
{
    cancel.check()?;
    let jobs: Vec<_> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.is_directory())
        .collect();
    let state = Mutex::new(CheckedFiles {
        valid: vec![false; files.len()],
        matched_parts: vec![0; files.len()],
        bytes: 0,
        files: 0,
    });
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let count = workers.clamp(1, MAX_WORKERS).min(jobs.len());
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(count);
        for _ in 0..count {
            handles.push(scope.spawn(|| {
                // A reader panic stops new work too; scoped threads are still
                // joined below before any caller can repair or replace a file.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    loop {
                        cancel.check()?;
                        if stopped.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        let job = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(index, file)) = jobs.get(job) else {
                            return Ok(());
                        };
                        let prepared = prepare(file).map_err(ScheduleError::File)?;
                        {
                            let checked = state.lock().map_err(|_| ScheduleError::WorkerFailed)?;
                            progress(checked.bytes, checked.files, file.name());
                        }
                        cancel.check()?;
                        let matched =
                            verify(&prepared, file, cancel).map_err(ScheduleError::File)?;
                        cancel.check()?;
                        let mut checked = state.lock().map_err(|_| ScheduleError::WorkerFailed)?;
                        checked.valid[index] = matched.whole;
                        checked.matched_parts[index] = matched.parts;
                        if matched.whole {
                            checked.bytes = checked
                                .bytes
                                .checked_add(file.size())
                                .ok_or(ScheduleError::InvalidTotalSize)?;
                            checked.files = checked
                                .files
                                .checked_add(1)
                                .ok_or(ScheduleError::InvalidFileCount)?;
                        }
                        progress(checked.bytes, checked.files, file.name());
                    }
                }))
                .unwrap_or(Err(ScheduleError::WorkerFailed));
                if result.is_err() {
                    stopped.store(true, Ordering::Release);
                }
                result
            }));
        }
        let mut error = None;
        for handle in handles {
            let result = handle.join().unwrap_or(Err(ScheduleError::WorkerFailed));
            if let Err(failure) = result {
                stopped.store(true, Ordering::Release);
                if error.is_none() {
                    error = Some(failure);
                }
            }
        }
        error.map_or(Ok(()), Err)
    })?;
    cancel.check()?;
    let checked = state
        .into_inner()
        .map_err(|_| ScheduleError::WorkerFailed)?;
    Ok(Verified {
        valid: checked.valid,
        matched_parts: checked.matched_parts,
        reused_bytes: checked.bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::DownloadProgress;
    use sha1::{Digest, Sha1};
    use std::{fs, sync::Arc, sync::Barrier};

    /// A reporter that keeps every snapshot it publishes, so the tests assert on
    /// what a caller actually observes rather than on a private callback.
    fn recording() -> (Arc<Reporter>, Arc<Mutex<Vec<DownloadProgress>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let reporter = Reporter::new(DownloadProgress::default(), move |snapshot| {
            sink.lock().unwrap().push(snapshot);
        });
        (reporter, log)
    }

    /// Discards every snapshot.
    fn silent() -> Arc<Reporter> {
        Reporter::new(DownloadProgress::default(), |_| {})
    }

    /// A manifest entry described the way both storefronts describe theirs.
    struct Entry {
        name: String,
        size: u64,
        sha: Vec<u8>,
        directory: bool,
    }

    impl VerificationFile for Entry {
        fn name(&self) -> &str {
            &self.name
        }
        fn size(&self) -> u64 {
            self.size
        }
        fn digest(&self) -> FileDigest<'_> {
            FileDigest::Sha1(&self.sha)
        }
        fn is_directory(&self) -> bool {
            self.directory
        }
    }

    fn entry(name: &str, bytes: &[u8]) -> Entry {
        Entry {
            name: name.into(),
            size: bytes.len() as u64,
            sha: Sha1::digest(bytes).to_vec(),
            directory: false,
        }
    }

    /// Only the reader pool, so panics and counter overflow stay reachable.
    struct Sized(u64);
    impl VerificationFile for Sized {
        fn name(&self) -> &str {
            "fixture"
        }
        fn size(&self) -> u64 {
            self.0
        }
        fn digest(&self) -> FileDigest<'_> {
            FileDigest::Sha1(&[])
        }
    }

    struct Reader<'a>(&'a AtomicUsize);
    impl Drop for Reader<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn hashes_valid_corrupt_missing_and_directory_entries_under_the_root() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("folder")).unwrap();
        let bytes = vec![0x5a; 192 * 1024 + 17];
        fs::write(root.path().join("folder/valid"), &bytes).unwrap();
        fs::write(root.path().join("empty"), []).unwrap();
        let mut corrupt = bytes.clone();
        corrupt[128 * 1024] ^= 1;
        fs::write(root.path().join("corrupt"), corrupt).unwrap();
        fs::write(root.path().join("truncated"), &bytes[..100]).unwrap();
        let mut directory = entry("folder", &[]);
        directory.directory = true;
        let files = vec![
            directory,
            entry("folder/valid", &bytes),
            entry("empty", &[]),
            entry("corrupt", &bytes),
            entry("truncated", &bytes),
            entry("missing", &bytes),
        ];
        let (reporter, events) = recording();
        let valid = verify_files(root.path(), &files, &CancelToken::new(), &reporter).unwrap();
        assert_eq!(valid.valid, [false, true, true, false, false, false]);
        assert_eq!(
            valid.reused_bytes,
            bytes.len() as u64,
            "the reused total is the scheduler's own count, not a second fold"
        );
        let events = events.lock().unwrap();
        let scanned: Vec<_> = events
            .iter()
            .filter_map(|event| event.verification_bytes)
            .collect();
        assert_eq!(scanned.first(), Some(&0));
        assert_eq!(
            scanned.last(),
            Some(&(4 * bytes.len() as u64)),
            "corrupt, truncated and missing files advance inspection without becoming reusable"
        );
        assert!(
            scanned.windows(2).all(|pair| pair[0] <= pair[1]),
            "concurrent readers must publish monotonic inspection progress"
        );
        // Counters only ever move forward, whichever reader published them.
        assert!(events.windows(2).all(|pair| {
            pair[0].completed_bytes <= pair[1].completed_bytes
                && pair[0].completed_files <= pair[1].completed_files
        }));
        let last = events.last().unwrap();
        assert_eq!(last.verification_bytes, None, "the scan ends before repair");
        assert_eq!(
            (last.completed_bytes, last.completed_files),
            (bytes.len() as u64, 2)
        );
        assert!(
            events.iter().all(|event| event.current_file != "folder"),
            "a directory entry is never handed to a reader"
        );
    }

    #[test]
    fn large_file_reports_partial_inspection_before_becoming_reusable() {
        let root = tempfile::tempdir().unwrap();
        let bytes = vec![0x7b; 6 * 1024 * 1024 + 73];
        fs::write(root.path().join("large.pak"), &bytes).unwrap();
        let total_bytes = bytes.len() as u64;
        let cancel = CancelToken::new();
        let callback_cancel = cancel.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let reporter = Reporter::new(
            DownloadProgress {
                total_bytes,
                ..Default::default()
            },
            move |snapshot| {
                if snapshot
                    .verification_bytes
                    .is_some_and(|bytes| bytes > 0 && bytes < total_bytes)
                {
                    callback_cancel.cancel();
                }
                sink.lock().unwrap().push(snapshot);
            },
        );

        let result = verify_with_workers(
            root.path(),
            &[entry("large.pak", &bytes)],
            &cancel,
            &reporter,
            1,
        );

        assert!(
            matches!(result, Err(VerificationError::Cancelled)),
            "inspection progress must be published while the file is still being hashed"
        );
        let events = events.lock().unwrap();
        let partial = events
            .iter()
            .find(|event| {
                event
                    .verification_bytes
                    .is_some_and(|bytes| bytes > 0 && bytes < total_bytes)
            })
            .expect("a large file must publish progress before its hash is complete");
        assert_eq!(partial.current_file, "large.pak");
        assert_eq!((partial.completed_bytes, partial.completed_files), (0, 0));
        assert!(
            events
                .iter()
                .all(|event| { event.completed_bytes == 0 && event.completed_files == 0 }),
            "cancelled inspection must never count the unfinished file as reusable"
        );
    }

    #[test]
    fn unsafe_names_are_rejected_before_any_read() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"unreadable").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let reporter = silent();
        for name in ["../outside", "/absolute", ".obelisk-content/lock"] {
            assert!(matches!(
                verify_files(
                    root.path(),
                    &[entry(name, b"x")],
                    &CancelToken::new(),
                    &reporter
                ),
                Err(VerificationError::Install(InstallError::UnsafePath(_)))
            ));
        }
        #[cfg(unix)]
        assert!(matches!(
            verify_files(
                root.path(),
                &[entry("link/secret", b"unreadable")],
                &CancelToken::new(),
                &reporter
            ),
            Err(VerificationError::Install(InstallError::UnsafePath(_)))
        ));
    }

    #[test]
    fn readers_overlap_up_to_the_eight_reader_ceiling() {
        let root = tempfile::tempdir().unwrap();
        let count = worker_limit();
        assert!((1..=MAX_WORKERS).contains(&count));
        let bytes = vec![7_u8; 64 * 1024];
        let files: Vec<_> = (0..count)
            .map(|index| {
                let name = format!("file-{index}");
                fs::write(root.path().join(&name), &bytes).unwrap();
                entry(&name, &bytes)
            })
            .collect();
        // Every reader must be able to run at once, or this never completes.
        let overlap = Barrier::new(count);
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let valid = schedule(
            &files,
            &CancelToken::new(),
            |file| confined_path(root.path(), file),
            |path, file: &Entry, cancel| {
                let running = active.fetch_add(1, Ordering::AcqRel) + 1;
                let _reader = Reader(&active);
                peak.fetch_max(running, Ordering::AcqRel);
                overlap.wait();
                install::verify_file_with_progress(path, file.size(), file.digest(), cancel, |_| {})
            },
            |_, _, _| {},
            count,
        )
        .unwrap();
        assert!(valid.valid.iter().all(|valid| *valid));
        assert_eq!(peak.load(Ordering::Acquire), count);
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert!(count <= MAX_WORKERS);
    }

    #[test]
    fn cancellation_joins_every_reader_and_skips_remaining_files() {
        let root = tempfile::tempdir().unwrap();
        let bytes = vec![3_u8; 1024];
        let files: Vec<_> = (0..8)
            .map(|index| {
                let name = format!("file-{index}");
                fs::write(root.path().join(&name), &bytes).unwrap();
                entry(&name, &bytes)
            })
            .collect();
        let cancel = CancelToken::new();
        let active = AtomicUsize::new(0);
        let started = AtomicUsize::new(0);
        let result = schedule(
            &files,
            &cancel,
            |file| confined_path(root.path(), file),
            |path, file: &Entry, inner: &CancelToken| {
                active.fetch_add(1, Ordering::AcqRel);
                let _reader = Reader(&active);
                started.fetch_add(1, Ordering::AcqRel);
                let matched = install::verify_file_with_progress(
                    path,
                    file.size(),
                    file.digest(),
                    inner,
                    |_| {},
                )?;
                // Cancelled once this file is read: the scheduler must stop
                // before handing the next one to a reader.
                cancel.cancel();
                Ok(matched)
            },
            |_, _, _| {},
            1,
        );
        assert!(matches!(result, Err(ScheduleError::Cancelled)));
        assert_eq!(active.load(Ordering::Acquire), 0, "every reader is joined");
        assert_eq!(
            started.load(Ordering::Acquire),
            1,
            "cancellation stops the remaining files"
        );
    }

    #[test]
    fn error_and_panic_join_every_reader_and_stop_pending_files() {
        for panic in [false, true] {
            let active = AtomicUsize::new(0);
            let started = AtomicUsize::new(0);
            let overlap = Barrier::new(4);
            let result = schedule(
                &[Sized(1), Sized(1), Sized(1), Sized(1), Sized(1), Sized(1)],
                &CancelToken::new(),
                |_| Ok(()),
                |_, _, _| {
                    active.fetch_add(1, Ordering::AcqRel);
                    let _reader = Reader(&active);
                    started.fetch_add(1, Ordering::AcqRel);
                    overlap.wait();
                    if panic {
                        panic!("fixture reader failed");
                    }
                    Err("fixture read failure")
                },
                |_, _, _| {},
                4,
            );
            if panic {
                assert!(matches!(result, Err(ScheduleError::WorkerFailed)));
            } else {
                assert!(matches!(
                    result,
                    Err(ScheduleError::File("fixture read failure"))
                ));
            }
            assert_eq!(active.load(Ordering::Acquire), 0);
            assert_eq!(started.load(Ordering::Acquire), 4);
        }
    }

    #[test]
    fn scheduler_failures_stay_opaque_to_callers() {
        let overflow = schedule(
            &[Sized(u64::MAX), Sized(1)],
            &CancelToken::new(),
            |_| Ok::<_, ()>(()),
            |_, _, _| {
                Ok(Matched {
                    whole: true,
                    parts: 1,
                })
            },
            |_, _, _| {},
            1,
        );
        assert!(matches!(overflow, Err(ScheduleError::InvalidTotalSize)));
        let panic = schedule(
            &[Sized(1)],
            &CancelToken::new(),
            |_| Ok::<_, ()>(()),
            |_, _, _| {
                Ok(Matched {
                    whole: true,
                    parts: 1,
                })
            },
            |_, _, _| panic!("fixture callback failed"),
            1,
        );
        assert!(matches!(panic, Err(ScheduleError::WorkerFailed)));
        // Both reach the caller as one variant, not as five.
        for error in [
            ScheduleError::<()>::WorkerFailed,
            ScheduleError::InvalidTotalSize,
            ScheduleError::InvalidFileCount,
        ] {
            let narrowed = match error {
                ScheduleError::Cancelled => VerificationError::Cancelled,
                ScheduleError::File(()) => VerificationError::SchedulerFailed,
                ScheduleError::WorkerFailed
                | ScheduleError::InvalidTotalSize
                | ScheduleError::InvalidFileCount => VerificationError::SchedulerFailed,
            };
            assert!(matches!(narrowed, VerificationError::SchedulerFailed));
        }
    }

    /// Whether `verify_files` finds each of `files` whole under `root`.
    fn whole(root: &Path, files: &[Entry]) -> Vec<bool> {
        verify_files(root, files, &CancelToken::new(), &silent())
            .unwrap()
            .valid
    }

    /// Overwrites `path` in place, keeping its inode and its modification time.
    fn tamper(path: &Path, bytes: &[u8]) {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn a_recorded_file_is_not_read_again_until_the_player_verifies() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("game.bin");
        fs::write(&path, b"original").unwrap();
        let files = [entry("game.bin", b"original")];
        record_verified(root.path(), &files).unwrap();
        // Only a disk that lies about its file, same inode, size and time,
        // shows that the file was not read.
        tamper(&path, b"tampered");
        assert_eq!(whole(root.path(), &files), [true]);
        forget_verified(root.path()).unwrap();
        assert_eq!(whole(root.path(), &files), [false]);
        forget_verified(root.path()).unwrap();
    }

    #[test]
    fn a_file_whose_disk_says_otherwise_is_hashed_again() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("game.bin");
        fs::write(&path, b"original").unwrap();
        let files = [entry("game.bin", b"original")];

        // Written again: another modification time.
        record_verified(root.path(), &files).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"tampered").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified + std::time::Duration::from_nanos(1))
            .unwrap();
        assert_eq!(whole(root.path(), &files), [false]);

        // Replaced by a rename, as an installer does: another inode, even
        // with the same size and time.
        fs::write(&path, b"original").unwrap();
        record_verified(root.path(), &files).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let replacement = root.path().join("replacement");
        fs::write(&replacement, b"tampered").unwrap();
        fs::File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert_eq!(whole(root.path(), &files), [false]);

        // Another build's digest for the same file.
        fs::write(&path, b"original").unwrap();
        record_verified(root.path(), &files).unwrap();
        tamper(&path, b"tampered");
        assert_eq!(
            whole(root.path(), &[entry("game.bin", b"tampered")]),
            [true]
        );
        assert_eq!(
            whole(root.path(), &[entry("game.bin", b"anothers")]),
            [false]
        );

        // Gone.
        fs::remove_file(&path).unwrap();
        assert_eq!(whole(root.path(), &files), [false]);
    }

    #[test]
    fn the_record_keeps_other_files_drops_vanished_ones_and_survives_damage() {
        let root = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            fs::write(root.path().join(name), name).unwrap();
        }
        // Two depots of one installation, recorded one after the other.
        record_verified(root.path(), &[entry("a", b"a"), entry("b", b"b")]).unwrap();
        record_verified(root.path(), &[entry("c", b"c")]).unwrap();
        fs::remove_file(root.path().join("b")).unwrap();
        record_verified(root.path(), &[entry("c", b"c")]).unwrap();
        let record = root
            .path()
            .join(install::PARTIAL_DIRECTORY)
            .join("verified.json");
        let text = fs::read_to_string(&record).unwrap();
        assert!(text.contains("\"a\"") && text.contains("\"c\"") && !text.contains("\"b\""));
        for name in ["a", "c"] {
            tamper(&root.path().join(name), b"x");
        }
        assert_eq!(
            whole(root.path(), &[entry("a", b"a"), entry("c", b"c")]),
            [true, true]
        );
        // A damaged record is no record.
        fs::write(&record, b"{not json").unwrap();
        assert_eq!(whole(root.path(), &[entry("a", b"a")]), [false]);
    }
}
