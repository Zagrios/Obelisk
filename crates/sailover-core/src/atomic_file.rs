//! Crash-safe file replacement shared by every catalogue, receipt and cache writer.
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use tempfile::NamedTempFile;

/// Writes `bytes` to a temporary file beside `path`, fsyncs it, then atomically
/// replaces `path`. The directory sync afterwards is best effort: a failure
/// there cannot truthfully mean the replacement was rolled back.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_with(path, bytes, || Ok(()))
}

/// [`write_atomic`] with a hook that runs after the temporary file is durable
/// and immediately before the rename, so a link substituted at `path` while
/// the bytes were being written can still be rejected.
pub fn write_atomic_with<E: From<io::Error>>(
    path: &Path,
    bytes: &[u8],
    before_replace: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    // The temporary file lives beside the destination, on the same volume.
    let mut temporary = NamedTempFile::new_in(directory)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    before_replace()?;
    temporary.persist(path).map_err(|error| error.error)?;
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}
