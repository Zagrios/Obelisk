//! Epic manifest entries described for the same verification scheduler as Steam.
//!
//! Path confinement, the SHA-1 read, the reader bound and the join-before-repair
//! guarantee all live in `obelisk_core::verification`. Rust's orphan rule is why
//! the trait implementation stays in this crate.
use obelisk_core::{
    install::FileDigest,
    verification::{VerificationError, VerificationFile},
};

use crate::{EpicError, content_manifest::ManifestFile};

pub(super) use obelisk_core::verification::verify_files;

impl VerificationFile for ManifestFile {
    fn name(&self) -> &str {
        &self.filename
    }
    fn size(&self) -> u64 {
        ManifestFile::size(self)
    }
    fn digest(&self) -> FileDigest<'_> {
        FileDigest::Sha1(&self.hash)
    }
}

impl From<VerificationError> for EpicError {
    fn from(error: VerificationError) -> Self {
        match error {
            VerificationError::Cancelled => Self::Cancelled,
            VerificationError::Install(error) => error.into(),
            // A reader panic and a counter overflow are the scheduler's own
            // failures; the installation stopped either way.
            VerificationError::SchedulerFailed => Self::Protocol,
        }
    }
}
