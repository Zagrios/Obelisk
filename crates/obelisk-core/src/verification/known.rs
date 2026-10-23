//! What an installation's last finished Transfer found whole, so the next
//! one hashes only what changed since.
//!
//! Each file is known by what its disk says of it as the Transfer ended, once
//! every file was durable: size, modification time to the nanosecond, device
//! and inode, with the digest its manifest gave. A file whose disk says the
//! same still holds what was hashed then: installers only ever replace a file
//! by renaming a new one over it, which gives it another inode. A Transfer
//! that stops halfway records nothing, so the files it replaced, torn or
//! not, are hashed again. The player's own verification forgets the record
//! first and hashes everything.
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::install::{
    self, FileDigest, InstallError, PARTIAL_DIRECTORY, checked_file_path, safe_relative_path,
};

use super::VerificationFile;

/// The record, inside the installation's partial directory, which manifests
/// may never name.
const RECORD: &str = "verified.json";
/// A record of 100,000 files takes about 15 MB.
const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
/// Bumped whenever a recorded field changes meaning: an older record is then
/// ignored.
const VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
struct Record {
    version: u32,
    files: HashMap<String, Seen>,
}

/// What a file's disk said of it, and the digest it matched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    size: u64,
    modified: (i64, i64),
    device: u64,
    inode: u64,
    digest: String,
}

/// The files the installation's last finished Transfer found whole.
#[derive(Default)]
pub(super) struct Known {
    files: HashMap<String, Seen>,
}

impl Known {
    /// The record under `root`; nothing when it is missing, unreadable, of
    /// another version or malformed.
    pub(super) fn load(root: &Path) -> Self {
        let files = install::read_cached_file(root, &record_path(), MAX_RECORD_BYTES)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
            .filter(|record| record.version == VERSION)
            .map(|record| record.files)
            .unwrap_or_default();
        Self { files }
    }

    /// Whether `file`, at `path`, is as the record knows it whole.
    pub(super) fn holds<T: VerificationFile>(&self, path: &Path, file: &T) -> bool {
        self.files
            .get(file.name())
            .is_some_and(|known| seen(path, file).is_some_and(|now| now == *known))
    }
}

/// Records `files` as found whole at `root`, with what their disk says of
/// them now, beside what the record already knows of other files. Call it
/// only once every file is in place and durable: a file recorded is never
/// hashed again until its disk says something else. A file that is not a
/// regular file of its manifest size is left out.
pub(super) fn record_verified<T: VerificationFile>(
    root: &Path,
    files: &[T],
) -> Result<(), InstallError> {
    let mut known = Known::load(root).files;
    // Files no longer there are dropped, so the record never outgrows the
    // installation across updates; `files` are looked at below.
    let files: Vec<&T> = files.iter().filter(|file| !file.is_directory()).collect();
    let named: HashSet<&str> = files.iter().map(|file| file.name()).collect();
    known.retain(|name, _| {
        named.contains(name.as_str())
            || safe_relative_path(name)
                .and_then(|relative| checked_file_path(root, &relative))
                .is_ok_and(|path| path.is_file())
    });
    for file in files {
        match seen(&super::confined_path(root, file)?, file) {
            Some(now) => known.insert(file.name().to_owned(), now),
            None => known.remove(file.name()),
        };
    }
    let record = Record {
        version: VERSION,
        files: known,
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|error| InstallError::Io(std::io::Error::other(error)))?;
    install::write_cached_file(root, &record_path(), &bytes)
}

/// Forgets what the record knows, so the next verification of the
/// installation at `root` hashes every file. A missing record is forgotten
/// already.
pub fn forget_verified(root: &Path) -> std::io::Result<()> {
    match fs::remove_file(root.join(record_path())) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn record_path() -> PathBuf {
    Path::new(PARTIAL_DIRECTORY).join(RECORD)
}

/// What the disk says of `file` at `path`, never following a link; `None`
/// unless it is a regular file of the manifest's size.
fn seen<T: VerificationFile>(path: &Path, file: &T) -> Option<Seen> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.is_file() && metadata.size() == file.size()).then(|| Seen {
        size: metadata.size(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        device: metadata.dev(),
        inode: metadata.ino(),
        digest: identity(file.digest()),
    })
}

/// The digest a file must match, as one string.
fn identity(digest: FileDigest<'_>) -> String {
    match digest {
        FileDigest::Sha1(sha) => format!("sha1:{}", install::hex(sha)),
        FileDigest::Md5Parts(parts) => {
            let mut hash = Sha1::new();
            for part in parts {
                hash.update(part.size.to_le_bytes());
                hash.update(part.md5);
            }
            format!("md5-parts:{}", install::hex(&hash.finalize()))
        }
    }
}
