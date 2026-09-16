//! Filesystem primitives shared by storefront installers: paths confined to an
//! installation root, the per-installation advisory lock, bounded cache files,
//! SHA-1 file checks and permission bits. Storefronts keep their manifest
//! formats and error vocabularies; these helpers never follow a symlink.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use sha1::{Digest, Sha1};
use thiserror::Error;

use crate::{
    atomic_file::write_atomic_with,
    transfer::{CancelToken, Cancelled},
};

/// Partial files, chunk caches, manifest caches and the engine lock live here,
/// inside the installation directory, so manifests may never name it.
pub const PARTIAL_DIRECTORY: &str = ".forge-content";
/// Receipt written only after every file of an installation succeeded.
pub const RECEIPT_FILE: &str = ".forge-install.json";
/// Advisory lock held from preparation until the receipt is published.
pub const LOCK_FILE: &str = ".forge-install.lock";

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("cancelled")]
    Cancelled,
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<Cancelled> for InstallError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

/// Lowercase hex without an allocation per byte.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 15)] as char);
    }
    output
}

/// Normalizes a manifest file name into a relative path made only of plain
/// components. Absolute paths, drive letters, `.`/`..`, empty segments, NUL and
/// Forge's own reserved names are rejected before touching the filesystem.
pub fn safe_relative_path(name: &str) -> Result<PathBuf, InstallError> {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains(['\0', ':'])
        || normalized
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || normalized.split('/').next().is_some_and(|part| {
            part.eq_ignore_ascii_case(PARTIAL_DIRECTORY)
                || part.eq_ignore_ascii_case(RECEIPT_FILE)
                || part.eq_ignore_ascii_case(LOCK_FILE)
        })
    {
        return Err(InstallError::UnsafePath(name.into()));
    }
    let path = PathBuf::from(normalized);
    if !path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(InstallError::UnsafePath(name.into()));
    }
    Ok(path)
}

/// Creates every missing directory of `relative` under `root`, refusing to
/// traverse or replace anything that is not a real directory.
pub fn ensure_directory(root: &Path, relative: &Path) -> Result<PathBuf, InstallError> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(InstallError::UnsafePath(relative.display().to_string()));
        };
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(InstallError::UnsafePath(path.display().to_string())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&path)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

/// Resolves `relative` under `root` for a file that may or may not exist yet.
/// Every existing intermediate must be a real directory and an existing leaf
/// must be a regular file with a single link, so a write can never escape root.
pub fn checked_file_path(root: &Path, relative: &Path) -> Result<PathBuf, InstallError> {
    let mut path = root.to_path_buf();
    let count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(name) = component else {
            return Err(InstallError::UnsafePath(relative.display().to_string()));
        };
        path.push(name);
        let last = index + 1 == count;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !last && !metadata.is_dir()
                    || last && !metadata.is_file()
                    || last && has_extra_links(&metadata)
                {
                    return Err(InstallError::UnsafePath(path.display().to_string()));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

/// Accepts a missing path or a regular, unlinked file; rejects everything else.
pub fn reject_link_or_special_file(path: &Path) -> Result<(), InstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || has_extra_links(&metadata) =>
        {
            Err(InstallError::UnsafePath(path.display().to_string()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn has_extra_links(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.nlink() > 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// `Ok(true)` for an existing real directory, `Ok(false)` when nothing exists
/// there, and an error for a symlink or a file in its place.
pub fn existing_plain_directory(path: &Path) -> Result<bool, InstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(InstallError::UnsafePath(path.display().to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Opens (creating if needed) `path` and takes an exclusive advisory lock.
/// `Ok(None)` means another process or session already holds it.
pub fn try_lock_file(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    Ok(file.try_lock().is_ok().then_some(file))
}

/// Reads a whole file of at most `limit` bytes. `None` when the file is missing
/// or larger than the limit, which callers treat alike: as absent.
pub fn read_file_bounded(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > limit as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() <= limit).then_some(bytes))
}

/// Reads a bounded cache file confined under an existing installation root.
/// A missing root or file is a miss; a redirected path is an error.
pub fn read_cached_file(
    root: &Path,
    relative: &Path,
    limit: usize,
) -> Result<Option<Vec<u8>>, InstallError> {
    if !existing_plain_directory(root)? {
        return Ok(None);
    }
    let path = checked_file_path(root, relative)?;
    Ok(read_file_bounded(&path, limit)?)
}

/// Atomically stores a cache file confined under an existing installation
/// root, creating parent directories and re-checking the path right before the
/// replacement so a link substituted meanwhile is rejected too.
pub fn write_cached_file(root: &Path, relative: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    if !existing_plain_directory(root)? {
        return Err(io::Error::from(io::ErrorKind::NotFound).into());
    }
    ensure_directory(root, relative.parent().unwrap_or(Path::new("")))?;
    let path = checked_file_path(root, relative)?;
    write_atomic_with(&path, bytes, || checked_file_path(root, relative).map(drop))
}

/// Streams a file through SHA-1 in 64 KiB reads. A missing file or a size
/// mismatch is simply `false`; cancellation is checked between reads.
pub fn verify_sha1_file(
    path: &Path,
    size: u64,
    expected: &[u8],
    cancel: &CancelToken,
) -> Result<bool, InstallError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() != size {
        return Ok(false);
    }
    let mut hasher = Sha1::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        cancel.check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize()[..] == *expected)
}

/// Installed files are user-readable; only manifest-declared executables get
/// the execute bit. No other mode information crosses a storefront manifest.
pub fn apply_permissions(path: &Path, executable: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, executable);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_reject_escapes_and_reserved_names() {
        for name in [
            "",
            "/absolute",
            "C:\\outside",
            "../outside",
            "safe/../outside",
            "safe/./file",
            "safe//file",
            "nul\0byte",
            ".forge-content/lock",
            ".FORGE-INSTALL.json",
            ".forge-install.lock",
        ] {
            assert!(
                matches!(safe_relative_path(name), Err(InstallError::UnsafePath(_))),
                "{name:?}"
            );
        }
        assert_eq!(
            safe_relative_path("Game.app\\Contents/MacOS\\Game").unwrap(),
            Path::new("Game.app/Contents/MacOS/Game")
        );
    }

    #[test]
    fn bounded_reads_treat_missing_and_oversized_files_as_absent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache");
        assert!(read_file_bounded(&path, 4).unwrap().is_none());
        fs::write(&path, b"12345").unwrap();
        assert!(read_file_bounded(&path, 4).unwrap().is_none());
        assert_eq!(read_file_bounded(&path, 5).unwrap().unwrap(), b"12345");
    }

    #[test]
    fn cached_files_round_trip_under_an_existing_root_only() {
        let directory = tempfile::tempdir().unwrap();
        let relative = Path::new(PARTIAL_DIRECTORY).join("manifests/1.bin");
        let missing_root = directory.path().join("missing");
        assert!(
            read_cached_file(&missing_root, &relative, 16)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            write_cached_file(&missing_root, &relative, b"x"),
            Err(InstallError::Io(_))
        ));
        write_cached_file(directory.path(), &relative, b"payload").unwrap();
        assert_eq!(
            read_cached_file(directory.path(), &relative, 16)
                .unwrap()
                .unwrap(),
            b"payload"
        );
    }

    #[cfg(unix)]
    #[test]
    fn confined_paths_refuse_links_and_special_files() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(matches!(
            checked_file_path(root.path(), Path::new("link/game")),
            Err(InstallError::UnsafePath(_))
        ));
        assert!(matches!(
            ensure_directory(root.path(), Path::new("link")),
            Err(InstallError::UnsafePath(_))
        ));
        fs::write(outside.path().join("original"), b"unchanged").unwrap();
        fs::hard_link(
            outside.path().join("original"),
            root.path().join("hardlink"),
        )
        .unwrap();
        assert!(matches!(
            checked_file_path(root.path(), Path::new("hardlink")),
            Err(InstallError::UnsafePath(_))
        ));
        assert!(matches!(
            reject_link_or_special_file(&root.path().join("hardlink")),
            Err(InstallError::UnsafePath(_))
        ));
        fs::write(root.path().join("plain"), b"file").unwrap();
        assert!(matches!(
            checked_file_path(root.path(), Path::new("plain/child")),
            Err(InstallError::UnsafePath(_))
        ));
        assert!(checked_file_path(root.path(), Path::new("new/file")).is_ok());
        assert!(reject_link_or_special_file(&root.path().join("plain")).is_ok());
    }

    #[test]
    fn sha1_verification_handles_missing_and_mismatched_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file");
        let cancel = CancelToken::new();
        let digest: [u8; 20] = Sha1::digest(b"content").into();
        assert!(!verify_sha1_file(&path, 7, &digest, &cancel).unwrap());
        fs::write(&path, b"content").unwrap();
        assert!(verify_sha1_file(&path, 7, &digest, &cancel).unwrap());
        assert!(!verify_sha1_file(&path, 8, &digest, &cancel).unwrap());
        assert!(!verify_sha1_file(&path, 7, &[0; 20], &cancel).unwrap());
        cancel.cancel();
        assert!(matches!(
            verify_sha1_file(&path, 7, &digest, &cancel),
            Err(InstallError::Cancelled)
        ));
    }
}
