use crate::*;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const MAX_JOURNAL: u64 = 64 * 1024 * 1024;
pub(super) const MAX_FILE: u64 = 256 * 1024 * 1024;

pub(super) fn root(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::Invalid);
    }
    fs::create_dir_all(path)?;
    directory(path)?;
    Ok(path.canonicalize()?)
}

pub(super) fn directory(path: &Path) -> Result<PathBuf> {
    match fs::create_dir(path) {
        Ok(()) => {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            sync_parent(path)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(Error::Invalid);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(path.to_path_buf())
}

/// True for an existing regular file, false when absent; anything else
/// (directory, symlink, device) is refused.
pub fn regular(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(Error::Invalid),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn lock(root: &Path) -> Result<File> {
    let path = root.join("owner.lock");
    regular(&path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.try_lock().map_err(|_| Error::Busy)?;
    Ok(file)
}

/// Bounded JSON journal read. Missing files are `None`; symlinks are refused.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !regular(path)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_JOURNAL + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err(Error::Invalid);
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub(super) fn sync_parent(path: &Path) -> Result<()> {
    File::open(path.parent().ok_or(Error::Invalid)?)?.sync_all()?;
    Ok(())
}

/// Bounded JSON journal write: temp file, fsync, rename, then parent fsync.
pub fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    regular(path)?;
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 > MAX_JOURNAL {
        return Err(Error::Invalid);
    }
    replace(path, &bytes)
}

pub(super) fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().ok_or(Error::Invalid)?)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    sync_parent(path)
}

pub(super) fn blob(directory: &Path, file: &FileVersion, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(Error::Invalid);
    }
    let path = directory.join("blobs").join(file.hash.as_str());
    if regular(&path)? {
        // Blobs are content-addressed and published by atomic rename, so an
        // existing entry only needs its size checked here; every consumer
        // re-hashes on read through `content`.
        if fs::symlink_metadata(&path)?.len() != file.size {
            return Err(Error::Integrity);
        }
    } else {
        replace(&path, bytes)?;
    }
    Ok(())
}

pub(super) fn content(directory: &Path, file: &FileVersion) -> Result<Vec<u8>> {
    let path = directory.join("blobs").join(file.hash.as_str());
    if file.size > MAX_FILE || !regular(&path)? {
        return Err(Error::Integrity);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(file.size + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != file.size || ContentHash::of(&bytes) != file.hash {
        return Err(Error::Integrity);
    }
    Ok(bytes)
}
