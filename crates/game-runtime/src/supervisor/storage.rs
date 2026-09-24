//! Private session-directory locks and bounded one-line packets.
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

fn lock_file(root: &Path, name: &str) -> Result<File, Error> {
    crate::files::private_directory(root).map_err(|_| Error::Storage)?;
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

/// Busy only once the lock stays taken past the window in which a child that
/// another thread spawns shares it.
pub(crate) fn lock(root: &Path, name: &str) -> Result<File, Error> {
    let file = lock_file(root, name)?;
    cloud_vault::lock::try_lock(&file).map_err(|_| Error::Busy)?;
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
        .map_err(|_| Error::Busy)?
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

/// Every control command names the protocol version it was written for.
#[derive(Serialize, Deserialize)]
struct Envelope<C> {
    version: u32,
    command: C,
}

pub(crate) fn send_command(output: &mut impl Write, command: &impl Serialize) -> Result<(), Error> {
    send(
        output,
        &Envelope {
            version: journal::VERSION,
            command,
        },
    )
}

pub(crate) fn command<C: serde::de::DeserializeOwned>(input: impl Read) -> Result<C, Error> {
    let envelope: Envelope<C> = packet(input)?;
    if envelope.version != journal::VERSION {
        return Err(Error::InvalidSession);
    }
    Ok(envelope.command)
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
