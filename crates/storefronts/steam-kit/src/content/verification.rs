//! Steam manifest entries described for the shared verification scheduler.
//!
//! Path confinement, the SHA-1 read, the reader bound and the join-before-repair
//! guarantee all live in `obelisk_core::verification`. Rust's orphan rule is why
//! the trait implementation stays in this crate.
use obelisk_core::{
    install::FileDigest,
    verification::{VerificationError, VerificationFile},
};

use super::{ContentError, format::ManifestFile, invalid};

pub(super) use obelisk_core::verification::verify_files;

impl VerificationFile for ManifestFile {
    fn name(&self) -> &str {
        &self.name
    }
    fn size(&self) -> u64 {
        self.size
    }
    fn digest(&self) -> FileDigest<'_> {
        FileDigest::Sha1(&self.sha)
    }
    /// Links are placed, never read: the scheduler passes over them like folders.
    fn is_directory(&self) -> bool {
        ManifestFile::is_directory(self) || self.is_link()
    }
}

impl From<VerificationError> for ContentError {
    fn from(error: VerificationError) -> Self {
        match error {
            VerificationError::Cancelled => Self::Cancelled,
            VerificationError::Install(error) => error.into(),
            // A reader panic and a counter overflow are the scheduler's own
            // failures; the installation stopped either way.
            VerificationError::SchedulerFailed => {
                invalid("backend.steam.verification.workerFailed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obelisk_core::{
        install::{checked_file_path, safe_relative_path},
        transfer::{CancelToken, DownloadProgress, Reporter},
        verification::{verify_with_workers, worker_limit},
    };
    use sha1::{Digest, Sha1};
    use std::fs;

    #[test]
    #[ignore = "Reads an existing installation selected by OBELISK_VERIFY_BENCH_DIRECTORY; no writes or network"]
    fn benchmark_installed_files() {
        use std::{fs::File, io::Read, time::Instant};

        let root = std::env::var_os("OBELISK_VERIFY_BENCH_DIRECTORY")
            .map(std::path::PathBuf::from)
            .expect("set OBELISK_VERIFY_BENCH_DIRECTORY to an existing game installation")
            .canonicalize()
            .unwrap();
        assert!(root.is_dir());
        let mut directories = vec![root.clone()];
        let mut paths = Vec::new();
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name().to_string_lossy().starts_with(".obelisk") {
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
        let reporter = Reporter::new(DownloadProgress::default(), |_| {});
        for workers in [1, worker_limit()] {
            let start = Instant::now();
            let result =
                verify_with_workers(&root, &files, &CancelToken::new(), &reporter, workers)
                    .unwrap();
            let duration = start.elapsed();
            let all_valid = result.valid.iter().all(|valid| *valid);
            println!(
                "verification_benchmark profile={profile} workers={workers} files={} bytes={bytes} seconds={:.6} all_valid={all_valid}",
                files.len(),
                duration.as_secs_f64()
            );
            assert!(all_valid, "a file changed during the read-only benchmark");
        }
    }
}
