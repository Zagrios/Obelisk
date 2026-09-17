//! Hash-pinned Valve payload. Runtime preparation is local-only; downloads are
//! confined to the optional build-time packaging tool.
use crate::{Config, Error};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

const MANIFEST: &str = include_str!("../runtime-manifest.json");
pub(crate) const HELPERS: [&str; 2] =
    ["forge-steam-host", "libforge_steam_discovery_adapter.dylib"];

#[derive(Deserialize)]
pub struct Manifest {
    pub version: String,
    pub packages: Vec<SourcePackage>,
    pub files: Vec<PayloadFile>,
}
#[derive(Deserialize)]
pub struct SourcePackage {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Deserialize)]
pub struct PayloadFile {
    pub path: String,
    pub package: String,
    pub size: u64,
    pub sha256: String,
}
pub fn manifest() -> &'static Manifest {
    static VALUE: OnceLock<Manifest> = OnceLock::new();
    VALUE.get_or_init(|| serde_json::from_str(MANIFEST).expect("checked-in runtime manifest"))
}
pub fn entry(path: &str) -> &'static PayloadFile {
    manifest()
        .files
        .iter()
        .find(|file| file.path == path)
        .expect("checked-in runtime manifest")
}
pub(crate) fn check(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn regular(path: &Path) -> Result<fs::Metadata, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|_| Error::Unavailable)?;
    if !metadata.file_type().is_file() {
        return Err(Error::VersionMismatch);
    }
    Ok(metadata)
}
fn is_directory(path: &Path) -> Result<(), Error> {
    if fs::symlink_metadata(path)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(Error::VersionMismatch)
    }
}
fn directories(root: &Path, path: &Path) -> Result<(), Error> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| Error::VersionMismatch)?;
    is_directory(root)?;
    let mut parent = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(Error::VersionMismatch);
        };
        parent.push(name);
        is_directory(&parent)?;
    }
    Ok(())
}
fn verify_file(root: &Path, entry: &PayloadFile, cancelled: &AtomicBool) -> Result<(), Error> {
    check(cancelled)?;
    let path = root.join(&entry.path);
    directories(root, path.parent().ok_or(Error::VersionMismatch)?)?;
    let metadata = regular(&path)?;
    if metadata.len() != entry.size || crate::sha(&path)? != entry.sha256 {
        return Err(Error::VersionMismatch);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if entry.path == HELPERS[0] && metadata.permissions().mode() & 0o100 == 0 {
            return Err(Error::VersionMismatch);
        }
    }
    Ok(())
}
fn verify_tree(root: &Path, entries: &[PayloadFile], cancelled: &AtomicBool) -> Result<(), Error> {
    directories(root, root)?;
    let mut allowed = BTreeSet::new();
    for entry in entries {
        verify_file(root, entry, cancelled)?;
        let mut path = Path::new(&entry.path);
        while !path.as_os_str().is_empty() {
            allowed.insert(path.to_path_buf());
            path = path.parent().ok_or(Error::VersionMismatch)?;
        }
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            check(cancelled)?;
            let entry = entry?;
            if !allowed.contains(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| Error::VersionMismatch)?,
            ) {
                return Err(Error::VersionMismatch);
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if !kind.is_file() {
                return Err(Error::VersionMismatch);
            }
        }
    }
    Ok(())
}

/// Inspect every archive entry before writing. Only exact manifest paths and
/// ordinary files are accepted; ZIP paths are never used as arbitrary targets.
fn archive(path: &Path, entries: &[PayloadFile]) -> Result<zip::ZipArchive<File>, Error> {
    if regular(path)?.len() > 128 * 1024 * 1024 {
        return Err(Error::VersionMismatch);
    }
    let mut archive =
        zip::ZipArchive::new(File::open(path)?).map_err(|_| Error::VersionMismatch)?;
    if archive.len() != entries.len() {
        return Err(Error::VersionMismatch);
    }
    let mut seen = BTreeSet::new();
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| Error::VersionMismatch)?;
        let expected = entries
            .iter()
            .find(|entry| entry.path == file.name())
            .ok_or(Error::VersionMismatch)?;
        if !seen.insert(file.name().to_owned())
            || file.size() != expected.size
            || !file.is_file()
            || file
                .unix_mode()
                .is_some_and(|mode| ![0, 0o100000].contains(&(mode & 0o170000)))
        {
            return Err(Error::VersionMismatch);
        }
    }
    Ok(archive)
}
pub(crate) fn copy_verified(
    mut input: impl Read,
    mut output: impl Write,
    entry: &PayloadFile,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check(cancelled)?;
        let length = input
            .read(&mut buffer)
            .map_err(|_| Error::VersionMismatch)?;
        if length == 0 {
            break;
        }
        size += length as u64;
        if size > entry.size {
            return Err(Error::VersionMismatch);
        }
        digest.update(&buffer[..length]);
        output.write_all(&buffer[..length])?;
    }
    if size != entry.size || format!("{:x}", digest.finalize()) != entry.sha256 {
        return Err(Error::VersionMismatch);
    }
    Ok(())
}
/// Used by the build tool too; no files are extracted by this validation.
pub fn verify_archive(path: &Path) -> Result<(), Error> {
    let entries = &manifest().files;
    let mut archive = archive(path, entries)?;
    for entry in entries {
        let file = archive
            .by_name(&entry.path)
            .map_err(|_| Error::VersionMismatch)?;
        copy_verified(file, std::io::sink(), entry, &AtomicBool::new(false))?;
    }
    Ok(())
}

pub(crate) fn prepare(config: &Config, cancelled: &AtomicBool) -> Result<PathBuf, Error> {
    prepare_with(config, &manifest().files, cancelled)
}
fn prepare_with(
    config: &Config,
    payload: &[PayloadFile],
    cancelled: &AtomicBool,
) -> Result<PathBuf, Error> {
    check(cancelled)?;
    let mut entries = payload.to_vec();
    for name in HELPERS {
        let path = config.helpers.join(name);
        let metadata = regular(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if name == HELPERS[0] && metadata.permissions().mode() & 0o100 == 0 {
                return Err(Error::Unavailable);
            }
        }
        entries.push(PayloadFile {
            path: name.into(),
            package: String::new(),
            size: metadata.len(),
            sha256: crate::sha(&path)?,
        });
    }
    let mut digest = Sha256::new();
    // Hashes are fixed-width; paths are delimited, so different inventories cannot alias.
    for entry in &entries {
        digest.update(entry.path.as_bytes());
        digest.update([0]);
        digest.update(entry.sha256.as_bytes());
    }
    let prefix = format!("runtime-v1-{:x}-", digest.finalize());
    super::files::private_directory(&config.cache)?;
    // Sandbox literal rules use canonical paths (not macOS /var aliases).
    let cache = config.cache.canonicalize()?;
    let mut candidates = fs::read_dir(&cache)?.collect::<Result<Vec<_>, _>>()?;
    candidates.sort_by_key(|entry| entry.file_name());
    for candidate in candidates {
        check(cancelled)?;
        if candidate.file_name().to_string_lossy().starts_with(&prefix)
            && candidate.file_type()?.is_dir()
        {
            match verify_tree(&candidate.path(), &entries, cancelled) {
                Ok(()) => return Ok(candidate.path()),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => {} // Repair into a new directory; never alter a running runtime.
            }
        }
    }
    let mut archive = archive(&config.package, payload)?;
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&cache)?;
    for entry in payload {
        check(cancelled)?;
        let target = staging.path().join(&entry.path);
        fs::create_dir_all(target.parent().ok_or(Error::Storage)?)?;
        let mut output = File::create(&target)?;
        let input = archive
            .by_name(&entry.path)
            .map_err(|_| Error::VersionMismatch)?;
        copy_verified(input, &mut output, entry, cancelled)?;
        output.sync_all()?;
    }
    for name in HELPERS {
        check(cancelled)?;
        let target = staging.path().join(name);
        fs::copy(config.helpers.join(name), &target)?;
        File::open(target)?.sync_all()?;
    }
    verify_tree(staging.path(), &entries, cancelled)?;
    check(cancelled)?;
    let directory = cache.join(format!("{prefix}{}", uuid::Uuid::new_v4()));
    // Unique immutable generations also make concurrent preparations safe.
    fs::rename(staging.path(), &directory)?;
    File::open(&cache)?.sync_all()?;
    Ok(directory)
}

#[cfg(test)]
mod tests;
