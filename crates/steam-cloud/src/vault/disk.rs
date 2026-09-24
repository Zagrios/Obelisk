use crate::*;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
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

fn lock_file(path: &Path) -> Result<File> {
    regular(path)?;
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?)
}

/// One storefront's exclusive claim on its part of the archive. Steam keeps
/// the original root lock; Epic locks beside its own account directories.
pub(super) fn store_lock(root: &Path, scope: &StoreScope) -> Result<File> {
    let directory = scope.store_directory(root);
    if directory != root {
        self::directory(&directory)?;
    }
    let file = lock_file(&directory.join("owner.lock"))?;
    try_lock(&file)?;
    Ok(file)
}

/// How long a child spawned by another thread can share this process's locks:
/// until its exec closes them. That window is not another owner.
const INHERITED_LOCK: Duration = Duration::from_millis(250);

/// A free lock can look taken while another thread spawns a child, so only a
/// lock still taken after `INHERITED_LOCK` is busy.
pub fn try_lock(file: &File) -> Result<()> {
    let deadline = Instant::now() + INHERITED_LOCK;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err(Error::Busy),
        }
    }
}

/// Proof that this process holds `owners.lock`. flock belongs to an open file
/// description, so a nested call must receive this guard instead of locking
/// the file again, which would contend with itself.
pub(crate) struct OwnersGuard {
    _file: File,
}

pub(super) const OWNERS_WAIT: Duration = Duration::from_secs(10);

/// Serializes `owners.json` across storefronts. A switch copies whole save
/// sets, but only locally, so a bounded wait replaces an immediate refusal.
pub(super) fn owners_lock(root: &Path, wait: Duration) -> Result<OwnersGuard> {
    let file = lock_file(&root.join("owners.lock"))?;
    let deadline = Instant::now() + wait;
    loop {
        if file.try_lock().is_ok() {
            return Ok(OwnersGuard { _file: file });
        }
        if Instant::now() >= deadline {
            return Err(Error::Busy);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Bounded private JSON journal read. Missing files are `None`; symlinks and
/// files with any group or other permission are refused.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    if !regular(path)? {
        return Ok(None);
    }
    let file = File::open(path)?;
    // Check the opened file, not a second lookup of a path that an atomic
    // journal writer may have replaced in the meantime.
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL + 1).read_to_end(&mut bytes)?;
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

#[cfg(test)]
mod spawn_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    /// Another thread keeps spawning short-lived children, as Forge does
    /// while it inspects processes.
    fn spawning() -> (Arc<AtomicBool>, std::thread::JoinHandle<()>) {
        let stop = Arc::new(AtomicBool::new(false));
        let spawner = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut children = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(child) = std::process::Command::new("/bin/sleep").arg("0.1").spawn() {
                        children.push(child);
                    }
                }
                for mut child in children {
                    let _ = child.wait();
                }
            })
        };
        (stop, spawner)
    }

    #[test]
    fn a_released_store_lock_is_free_again_while_other_threads_spawn_children() {
        let root = tempfile::tempdir().unwrap();
        let scope = StoreScope::from(crate::EpicScope {
            account_id: "0123456789abcdef0123456789abcdef".into(),
            namespace: "namespace".into(),
            app_name: "game".into(),
        });
        let (stop, spawner) = spawning();
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut busy = 0;
        while Instant::now() < deadline {
            busy += usize::from(store_lock(root.path(), &scope).is_err());
        }
        stop.store(true, Ordering::Relaxed);
        spawner.join().unwrap();
        assert_eq!(busy, 0, "a spawning child briefly shares every open lock");
    }
}
