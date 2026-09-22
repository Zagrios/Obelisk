use super::*;
use crate::Error;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

fn lock_file(root: &Path, name: &str) -> Result<File, Error> {
    crate::files::private_directory(root)?;
    let path = root.join(name);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(Error::Storage);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    Ok(file)
}

pub(crate) fn lock(root: &Path, name: &str) -> Result<File, Error> {
    let file = lock_file(root, name)?;
    file.try_lock().map_err(|_| Error::SessionBusy)?;
    Ok(file)
}

/// Wait for terminal teardown without polling. If the deadline expires, the
/// waiter retains start.lock until it finishes, preventing accumulated waiters
/// or a second launch racing an owner whose first record is not visible yet.
pub(crate) fn owner_released(root: &Path, starting: File) -> Result<File, Error> {
    let owner = lock_file(root, "owner.lock")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = owner.lock().map_err(|_| Error::Storage);
        drop(owner);
        let _ = send.send(result.map(|()| starting));
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(1))
        .map_err(|_| Error::SessionBusy)?
}

pub(super) fn read(root: &Path) -> Result<Option<Snapshot>, Error> {
    read_path(&root.join("session.json"))
}

pub(super) fn read_id(root: &Path, id: &str) -> Result<Option<Snapshot>, Error> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(Error::InvalidSession);
    }
    read_path(&root.join(format!("{id}.json")))
}

fn read_path(path: &Path) -> Result<Option<Snapshot>, Error> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(m)
            if m.file_type().is_file()
                && m.len() <= 65536
                && m.permissions().mode() & 0o077 == 0 => {}
        _ => return Err(Error::Storage),
    }
    let snapshot: Snapshot = packet(File::open(path)?)?;
    if uuid::Uuid::parse_str(&snapshot.id).is_err() || snapshot.app_id == 0 {
        return Err(Error::Storage);
    }
    Ok(Some(snapshot))
}

pub(super) fn write(root: &Path, snapshot: &Snapshot) -> Result<(), Error> {
    write_path(&root.join(format!("{}.json", snapshot.id)), snapshot)?;
    write_path(&root.join("session.json"), snapshot)
}

fn write_path(path: &Path, snapshot: &Snapshot) -> Result<(), Error> {
    crate::files::atomic_write(path, crate::files::Durability::FileSync, |file| {
        send(file, snapshot)
    })
}

pub(crate) fn packet<T: serde::de::DeserializeOwned>(input: impl Read) -> Result<T, Error> {
    use std::io::BufRead;
    let mut bytes = Vec::new();
    std::io::BufReader::new(input.take(65537)).read_until(b'\n', &mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::InvalidSession);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::InvalidSession)
}

pub(crate) fn send(output: &mut impl Write, value: &impl Serialize) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::InvalidSession)?;
    if bytes.len() > 65535 {
        return Err(Error::InvalidSession);
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
