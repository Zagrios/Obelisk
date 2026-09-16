//! Bounded full-file verification shared by storefront installers.
//!
//! Storefronts provide path validation and their checksum reader. This scheduler
//! serializes progress and joins every reader before returning to the repair loop.
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use crate::transfer::{CancelToken, Cancelled};

const MAX_WORKERS: usize = 4;

/// Metadata needed to schedule a file and count verified bytes. Directory entries
/// remain in the result vector as `false` without being passed to a reader.
pub trait VerificationFile {
    fn name(&self) -> &str;
    fn size(&self) -> u64;
    fn is_directory(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub enum VerificationError<E> {
    Cancelled,
    File(E),
    WorkerFailed,
    InvalidTotalSize,
    InvalidFileCount,
}

impl<E> From<Cancelled> for VerificationError<E> {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

pub fn worker_limit() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_WORKERS)
}

/// Verify with the host's available parallelism, bounded to four disk readers.
pub fn verify_files<T, P, E>(
    files: &[T],
    cancel: &CancelToken,
    prepare: impl Fn(&T) -> Result<P, E> + Sync,
    verify: impl Fn(&P, &T, &CancelToken) -> Result<bool, E> + Sync,
    progress: impl Fn(u64, u32, &str) + Sync,
) -> Result<Vec<bool>, VerificationError<E>>
where
    T: VerificationFile + Sync,
    E: Send,
{
    verify_with_workers(files, cancel, prepare, verify, progress, worker_limit())
}

struct CheckedFiles {
    valid: Vec<bool>,
    bytes: u64,
    files: u32,
}

/// An explicit reader limit supports single-reader benchmarks and deterministic
/// overlap tests. Values are clamped to the production ceiling of four.
/// `prepare` runs before reporting a file, so unsafe paths never reach a reader.
/// Callbacks execute inline under one lock and must not reenter this verifier.
pub fn verify_with_workers<T, P, E>(
    files: &[T],
    cancel: &CancelToken,
    prepare: impl Fn(&T) -> Result<P, E> + Sync,
    verify: impl Fn(&P, &T, &CancelToken) -> Result<bool, E> + Sync,
    progress: impl Fn(u64, u32, &str) + Sync,
    workers: usize,
) -> Result<Vec<bool>, VerificationError<E>>
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
                        let prepared = prepare(file).map_err(VerificationError::File)?;
                        {
                            let checked =
                                state.lock().map_err(|_| VerificationError::WorkerFailed)?;
                            progress(checked.bytes, checked.files, file.name());
                        }
                        cancel.check()?;
                        let valid =
                            verify(&prepared, file, cancel).map_err(VerificationError::File)?;
                        cancel.check()?;
                        let mut checked =
                            state.lock().map_err(|_| VerificationError::WorkerFailed)?;
                        checked.valid[index] = valid;
                        if valid {
                            checked.bytes = checked
                                .bytes
                                .checked_add(file.size())
                                .ok_or(VerificationError::InvalidTotalSize)?;
                            checked.files = checked
                                .files
                                .checked_add(1)
                                .ok_or(VerificationError::InvalidFileCount)?;
                        }
                        progress(checked.bytes, checked.files, file.name());
                    }
                }))
                .unwrap_or(Err(VerificationError::WorkerFailed));
                if result.is_err() {
                    stopped.store(true, Ordering::Release);
                }
                result
            }));
        }
        let mut error = None;
        for handle in handles {
            let result = handle
                .join()
                .unwrap_or(Err(VerificationError::WorkerFailed));
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
    Ok(state
        .into_inner()
        .map_err(|_| VerificationError::WorkerFailed)?
        .valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    struct File(u64);
    impl VerificationFile for File {
        fn name(&self) -> &str {
            "fixture"
        }
        fn size(&self) -> u64 {
            self.0
        }
    }

    struct Reader<'a>(&'a AtomicUsize);
    impl Drop for Reader<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn error_and_panic_join_every_reader_and_stop_pending_files() {
        for panic in [false, true] {
            let active = AtomicUsize::new(0);
            let started = AtomicUsize::new(0);
            let overlap = Barrier::new(4);
            let result = verify_with_workers(
                &[File(1), File(1), File(1), File(1), File(1), File(1)],
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
                assert!(matches!(result, Err(VerificationError::WorkerFailed)));
            } else {
                assert!(matches!(
                    result,
                    Err(VerificationError::File("fixture read failure"))
                ));
            }
            assert_eq!(active.load(Ordering::Acquire), 0);
            assert_eq!(started.load(Ordering::Acquire), 4);
        }
    }

    #[test]
    fn reports_size_overflow_and_callback_panic_as_typed_errors() {
        let overflow = verify_with_workers(
            &[File(u64::MAX), File(1)],
            &CancelToken::new(),
            |_| Ok::<_, ()>(()),
            |_, _, _| Ok(true),
            |_, _, _| {},
            1,
        );
        assert!(matches!(overflow, Err(VerificationError::InvalidTotalSize)));
        let panic = verify_with_workers(
            &[File(1)],
            &CancelToken::new(),
            |_| Ok::<_, ()>(()),
            |_, _, _| Ok(true),
            |_, _, _| panic!("fixture callback failed"),
            1,
        );
        assert!(matches!(panic, Err(VerificationError::WorkerFailed)));
    }
}
