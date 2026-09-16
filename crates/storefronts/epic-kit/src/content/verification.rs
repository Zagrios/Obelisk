//! Epic path checks and SHA-1 reads on the same scheduler as Steam.
use std::path::{Path, PathBuf};

use forge_core::verification::{self, VerificationError, VerificationFile};

use super::{
    CancelToken, EpicError, ManifestFile, checked_file_path, safe_relative_path, verify_file,
};

impl VerificationFile for ManifestFile {
    fn name(&self) -> &str {
        &self.filename
    }
    fn size(&self) -> u64 {
        ManifestFile::size(self)
    }
}

pub(super) fn verify_files(
    root: &Path,
    files: &[ManifestFile],
    cancel: &CancelToken,
    progress: impl Fn(u64, u32, &str) + Sync,
) -> Result<Vec<bool>, EpicError> {
    verification::verify_files(
        files,
        cancel,
        |file| prepare(root, file),
        |path, file, cancel| verify_file(path, file.size(), &file.hash, cancel),
        progress,
    )
    .map_err(map_error)
}

fn prepare(root: &Path, file: &ManifestFile) -> Result<PathBuf, EpicError> {
    Ok(checked_file_path(
        root,
        &safe_relative_path(&file.filename)?,
    )?)
}

fn map_error(error: VerificationError<EpicError>) -> EpicError {
    match error {
        VerificationError::Cancelled => EpicError::Cancelled,
        VerificationError::File(error) => error,
        VerificationError::WorkerFailed
        | VerificationError::InvalidTotalSize
        | VerificationError::InvalidFileCount => EpicError::Protocol,
    }
}

#[cfg(test)]
mod tests;
