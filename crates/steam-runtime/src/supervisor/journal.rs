//! Durable Game session records, one private directory per storefront.
//!
//! `session.v2.json` only names the current session; `<id>.v2.json` holds its
//! record. A launch writes the name first and its record last, so a record
//! exists only once the whole initial write succeeded. Older builds' files use
//! other names and are never opened.
use super::Error;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

/// The record and command protocol this build writes and understands.
pub(crate) const VERSION: u32 = 2;
const CURRENT: &str = "session.v2.json";
const MAX: u64 = 64 * 1024;

pub(crate) trait Entry: Serialize + DeserializeOwned {
    fn id(&self) -> &str;
    /// Storefront-specific checks beyond the shared identity checks.
    fn valid(&self) -> bool;
}

#[derive(Serialize, Deserialize)]
struct Current {
    version: u32,
    id: String,
}

#[derive(Deserialize)]
struct Header {
    version: u32,
}

fn record_name(id: &str) -> Result<String, Error> {
    uuid::Uuid::parse_str(id).map_err(|_| Error::InvalidSession)?;
    Ok(format!("{id}.v2.json"))
}

/// The current session's record, or `None` when no launch was accepted.
pub(crate) fn current<T: Entry>(root: &Path) -> Result<Option<T>, Error> {
    match read::<Current>(&root.join(CURRENT))? {
        Some(current) => by_id(root, &current.id),
        None => Ok(None),
    }
}

pub(crate) fn by_id<T: Entry>(root: &Path, id: &str) -> Result<Option<T>, Error> {
    let Some(entry) = read::<T>(&root.join(record_name(id)?))? else {
        return Ok(None);
    };
    if entry.id() != id {
        return Err(Error::InvalidSession);
    }
    if !entry.valid() {
        return Err(Error::Storage);
    }
    Ok(Some(entry))
}

/// Names the session current first, then writes its record.
pub(crate) fn write<T: Entry>(root: &Path, entry: &T) -> Result<(), Error> {
    let name = record_name(entry.id())?;
    let path = root.join(CURRENT);
    if read::<Current>(&path)
        .ok()
        .flatten()
        .is_none_or(|current| current.id != entry.id())
    {
        replace(
            &path,
            &Current {
                version: VERSION,
                id: entry.id().into(),
            },
        )?;
    }
    replace(&root.join(name), entry)
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => return Err(Error::Storage),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Storage),
    };
    let file = File::open(path).map_err(|_| Error::Storage)?;
    // Check the opened file: an atomic writer may have replaced the path.
    let opened = file.metadata().map_err(|_| Error::Storage)?;
    let directory =
        fs::metadata(path.parent().ok_or(Error::Storage)?).map_err(|_| Error::Storage)?;
    if !opened.is_file()
        || (opened.dev(), opened.ino()) != (before.dev(), before.ino())
        || opened.uid() != directory.uid()
        || opened.permissions().mode() & 0o077 != 0
        || opened.len() > MAX
    {
        return Err(Error::Storage);
    }
    let mut bytes = Vec::new();
    file.take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Storage)?;
    if bytes.len() as u64 > MAX {
        return Err(Error::Storage);
    }
    let header: Header = serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?;
    if header.version > VERSION {
        return Err(Error::NewerSession);
    }
    if header.version != VERSION {
        return Err(Error::Storage);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::Storage)
}

fn replace(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Storage)?;
    if bytes.len() as u64 > MAX
        || fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file())
    {
        return Err(Error::Storage);
    }
    let directory = path.parent().ok_or(Error::Storage)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|_| Error::Storage)?;
    temporary.write_all(&bytes).map_err(|_| Error::Storage)?;
    temporary.as_file().sync_all().map_err(|_| Error::Storage)?;
    temporary.persist(path).map_err(|_| Error::Storage)?;
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| Error::Storage)
}

#[cfg(test)]
mod tests;
