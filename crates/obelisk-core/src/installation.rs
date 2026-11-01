//! One installation Transfer, in the order every storefront follows around its
//! content engine. Obelisk confines the destination, lets the storefront prepare
//! before any file exists, holds the installation lock from the first file to
//! the receipt, withdraws the old Installation receipt before files change and
//! publishes the new one only after every file succeeded. Storefronts supply
//! only preparation and file installation.
use std::{
    fmt::Display,
    fs::{self, File, FileTimes},
    future::Future,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    install::{
        InstallError, LOCK_FILE, PARTIAL_DIRECTORY, RECEIPT_FILE, checked_file_path,
        read_file_bounded, try_lock_file,
    },
    transfer::{
        CancelToken, Cancelled, DownloadProgress, TransferBusy, TransferContext, TransferState,
        TransferStatus, Transfers,
    },
};

/// Receipts are small JSON documents; anything larger is not one of ours.
const MAX_RECEIPT_BYTES: usize = 1024 * 1024;
/// How much of a file a move copies between two looks at its cancellation.
const COPY_CHUNK: usize = 4 * 1024 * 1024;
/// Ends the name of the hidden folder a move to another disk copies into.
const STAGING_SUFFIX: &str = ".obelisk-moving";

/// A Transfer's progress, as every storefront presents it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferProgress {
    /// The storefront's own identity for the game.
    pub game: String,
    pub state: TransferState,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    /// Compressed bytes received, including manifests.
    pub network_bytes: u64,
    /// Compressed chunk bytes received, excluding manifests and disk reads.
    pub content_bytes: u64,
    pub current_file: String,
    /// A message key while the Transfer runs, then its outcome.
    pub message: String,
}

impl TransferProgress {
    pub fn new(game: impl Into<String>) -> Self {
        Self {
            game: game.into(),
            state: TransferState::Preparing,
            completed_bytes: 0,
            total_bytes: 0,
            network_bytes: 0,
            content_bytes: 0,
            current_file: String::new(),
            message: "backend.transfer.preparing".into(),
        }
    }

    /// Shows one engine snapshot as the Transfer's progress. While files are
    /// inspected, inspection bytes stand in for completed bytes. Clears the
    /// message: a storefront that labels the Transfer, or counts work done
    /// before this engine run, adjusts the fields afterwards.
    pub fn show(&mut self, snapshot: DownloadProgress) {
        self.state = snapshot.phase.into();
        self.completed_bytes = snapshot
            .verification_bytes
            .unwrap_or(snapshot.completed_bytes);
        self.total_bytes = snapshot.total_bytes;
        self.network_bytes = snapshot.network_bytes;
        self.content_bytes = snapshot.content_bytes;
        self.current_file = snapshot.current_file;
        self.message.clear();
    }
}

impl TransferStatus for TransferProgress {
    fn finish(&mut self, state: TransferState, error: Option<String>) {
        self.state = state;
        self.message = error.unwrap_or_else(|| {
            match state {
                TransferState::Complete if self.content_bytes == 0 => "backend.transfer.upToDate",
                TransferState::Complete => "backend.transfer.complete",
                _ => "backend.transfer.stopped",
            }
            .into()
        });
    }
}

/// What obelisk-core refused or failed around an installation. Each storefront
/// turns it into its own message vocabulary.
#[derive(Debug, Error)]
pub enum InstallFailure {
    #[error("the installation folder's parent must be an existing absolute folder")]
    ParentMissing,
    #[error("the installation folder name must be one plain path component: {0}")]
    UnsafeDirectory(String),
    #[error("the installation folder must not be a link: {0}")]
    DestinationIsLink(PathBuf),
    #[error("a file occupies the installation folder: {0}")]
    DestinationIsFile(PathBuf),
    #[error("the installation lock must be a regular unlinked file: {0}")]
    UnsafeLock(PathBuf),
    #[error("another Transfer holds the installation")]
    Locked,
    #[error("the Installation receipt must be a regular unlinked file: {0}")]
    UnsafeReceipt(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// A storefront's errors around an installation Transfer.
pub trait TransferError:
    From<InstallFailure> + From<Cancelled> + From<TransferBusy> + Display
{
    /// The content engine stopped because the Transfer was cancelled.
    fn is_cancellation(&self) -> bool;
}

/// Every file of an installation succeeded. Obelisk publishes `receipt` next.
pub struct InstalledFiles<R> {
    pub receipt: R,
    pub total_bytes: u64,
    pub network_bytes: u64,
    pub content_bytes: u64,
}

/// Where one installation Transfer writes: `directory` inside `parent`.
pub struct Destination<'a> {
    /// The folder the player chose. It must exist.
    pub parent: &'a Path,
    /// The storefront's dedicated folder name for this installation.
    pub directory: &'a str,
}

/// Runs one installation Transfer through `transfers`, which admits one at a
/// time. `prepare` resolves everything the storefront needs before any file of
/// the installation exists, and decides itself how to honor cancellation.
/// `install_files` writes every file under the installation lock. The old
/// Installation receipt is withdrawn before `install_files` starts, so a failed
/// or cancelled Transfer leaves no receipt behind.
pub async fn install<R, P, E, Prepare, Prepared, Files, Installed>(
    transfers: &Transfers<TransferProgress>,
    game: impl Into<String>,
    destination: Destination<'_>,
    prepare: Prepare,
    install_files: Files,
) -> Result<R, E>
where
    R: Serialize,
    E: TransferError,
    Prepare: FnOnce(PathBuf, CancelToken) -> Prepared,
    Prepared: Future<Output = Result<P, E>>,
    Files: FnOnce(P, PathBuf, TransferContext<TransferProgress>) -> Installed,
    Installed: Future<Output = Result<InstalledFiles<R>, E>>,
{
    transfers
        .run(TransferProgress::new(game), |transfer| async move {
            let result = run(destination, &transfer, prepare, install_files).await;
            if result.as_ref().is_err_and(E::is_cancellation) {
                // Publish the engine's cancellation as the Transfer's outcome.
                transfer.cancel_token().cancel();
            }
            result
        })
        .await
}

async fn run<R, P, E, Prepare, Prepared, Files, Installed>(
    destination: Destination<'_>,
    transfer: &TransferContext<TransferProgress>,
    prepare: Prepare,
    install_files: Files,
) -> Result<R, E>
where
    R: Serialize,
    E: TransferError,
    Prepare: FnOnce(PathBuf, CancelToken) -> Prepared,
    Prepared: Future<Output = Result<P, E>>,
    Files: FnOnce(P, PathBuf, TransferContext<TransferProgress>) -> Installed,
    Installed: Future<Output = Result<InstalledFiles<R>, E>>,
{
    let cancel = transfer.cancel_token().clone();
    let root = confine(destination)?;
    let prepared = prepare(root.clone(), cancel.clone()).await?;
    cancel.check()?;
    // Held across every file and the new receipt. Other sessions and
    // processes, including uninstall, must not interleave.
    let _lock = lock(&root)?;
    let receipt = receipt_path(&root)?;
    match fs::remove_file(&receipt) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(InstallFailure::Io(error).into()),
    }
    let installed = install_files(prepared, root.clone(), transfer.clone()).await?;
    cancel.check()?;
    write_receipt(&root, &installed.receipt)?;
    transfer.update(|progress| {
        progress.completed_bytes = installed.total_bytes;
        progress.total_bytes = installed.total_bytes;
        progress.network_bytes = installed.network_bytes;
        progress.content_bytes = installed.content_bytes;
        progress.current_file.clear();
        progress.message.clear();
    });
    Ok(installed.receipt)
}

/// Resolves the installation folder under its existing parent. Nothing is
/// created yet; a link or a file in the folder's place is refused.
fn confine(destination: Destination<'_>) -> Result<PathBuf, InstallFailure> {
    let Destination { parent, directory } = destination;
    if !parent.is_absolute() || !parent.is_dir() {
        return Err(InstallFailure::ParentMissing);
    }
    let mut components = Path::new(directory).components();
    let plain = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !directory.contains(['/', '\\', '\0'])
        && !directory.eq_ignore_ascii_case(PARTIAL_DIRECTORY);
    if !plain {
        return Err(InstallFailure::UnsafeDirectory(directory.into()));
    }
    let root = parent.canonicalize()?.join(directory);
    check_folder(&root)?;
    Ok(root)
}

fn check_folder(root: &Path) -> Result<(), InstallFailure> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(InstallFailure::DestinationIsLink(root.to_owned()))
        }
        Ok(metadata) if !metadata.is_dir() => {
            Err(InstallFailure::DestinationIsFile(root.to_owned()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Takes an installation's exclusive lock, creating its folder when missing.
/// Every Transfer, launch and uninstall of the installation shares this lock,
/// across Obelisk processes. The lock is released when the file is dropped.
pub fn lock(root: &Path) -> Result<File, InstallFailure> {
    check_folder(root)?;
    fs::create_dir_all(root)?;
    check_folder(root)?;
    let path = checked_file_path(root, Path::new(LOCK_FILE)).map_err(|error| match error {
        InstallError::Io(error) => InstallFailure::Io(error),
        _ => InstallFailure::UnsafeLock(root.join(LOCK_FILE)),
    })?;
    try_lock_file(&path)?.ok_or(InstallFailure::Locked)
}

fn receipt_path(root: &Path) -> Result<PathBuf, InstallFailure> {
    checked_file_path(root, Path::new(RECEIPT_FILE)).map_err(|error| match error {
        InstallError::Io(error) => InstallFailure::Io(error),
        _ => InstallFailure::UnsafeReceipt(root.join(RECEIPT_FILE)),
    })
}

/// A completed installation, opened for launch under its installation lock.
/// No Transfer or uninstall of it can start until this is dropped.
pub struct Installation<R> {
    /// The canonical installation folder.
    pub root: PathBuf,
    /// Absent when no Transfer completed, or when the receipt is unreadable.
    pub receipt: Option<R>,
    _lock: File,
}

/// Opens an existing installation folder and reads its Installation receipt.
pub fn open<R: DeserializeOwned>(directory: &Path) -> Result<Installation<R>, InstallFailure> {
    if !directory.is_dir() {
        return Err(io::Error::from(io::ErrorKind::NotFound).into());
    }
    let lock = lock(directory)?;
    let root = directory.canonicalize()?;
    let receipt = receipt_in(&root)?;
    Ok(Installation {
        root,
        receipt,
        _lock: lock,
    })
}

/// Reads an installation folder's Installation receipt without its lock, as
/// the game it holds may be running. `None` while a Transfer has withdrawn
/// it, or when it is unreadable.
pub fn read_receipt<R: DeserializeOwned>(directory: &Path) -> Result<Option<R>, InstallFailure> {
    if !directory.is_dir() {
        return Err(io::Error::from(io::ErrorKind::NotFound).into());
    }
    receipt_in(&directory.canonicalize()?)
}

fn receipt_in<R: DeserializeOwned>(root: &Path) -> Result<Option<R>, InstallFailure> {
    Ok(read_file_bounded(&receipt_path(root)?, MAX_RECEIPT_BYTES)?
        .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
}

/// Makes `receipt` the Installation receipt in `root`. Its path is checked
/// again right before the replacement, so a link substituted while files
/// were written is refused too.
fn write_receipt(root: &Path, receipt: &impl Serialize) -> Result<(), InstallFailure> {
    let bytes = serde_json::to_vec_pretty(receipt).map_err(io::Error::other)?;
    crate::write_atomic_with(&receipt_path(root)?, &bytes, || {
        receipt_path(root).map(drop)
    })
}

/// Why an installation was not removed.
#[derive(Debug, Error)]
pub enum RemovalFailure {
    #[error("not a dedicated installation folder: {0}")]
    Unsafe(PathBuf),
    #[error("a Transfer, launch or uninstall holds the installation")]
    Busy,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Removes a storefront's dedicated installation folder under its
/// installation lock. The folder must be named `expected`, must not be or
/// contain any of `protected`, and no link may lead to it. Links inside it
/// are removed, never followed.
pub fn remove(directory: &Path, expected: &str, protected: &[&Path]) -> Result<(), RemovalFailure> {
    let (resolved, _lock) = held(directory, expected, protected)?;
    Ok(fs::remove_dir_all(&resolved)?)
}

/// Removes, as [`remove`] does, the folder of a Transfer that never
/// finished. `false` when nothing of it is there to remove: no folder, as a
/// Transfer stopped before its first file leaves, or one an Installation
/// receipt names, an installation since. The receipt is read under the lock
/// every Transfer holds, so none publishes one meanwhile.
pub fn remove_unfinished(
    directory: &Path,
    expected: &str,
    protected: &[&Path],
) -> Result<bool, RemovalFailure> {
    if !directory.exists() {
        return Ok(false);
    }
    let (resolved, _lock) = held(directory, expected, protected)?;
    match receipt_in::<serde::de::IgnoredAny>(&resolved) {
        Ok(None) => {}
        Ok(Some(_)) => return Ok(false),
        Err(_) => return Err(RemovalFailure::Unsafe(directory.to_owned())),
    }
    fs::remove_dir_all(&resolved)?;
    Ok(true)
}

/// The dedicated installation folder `directory`, resolved, under its
/// installation lock.
fn held(
    directory: &Path,
    expected: &str,
    protected: &[&Path],
) -> Result<(PathBuf, File), RemovalFailure> {
    let refuse = || RemovalFailure::Unsafe(directory.to_owned());
    let resolved = dedicated(directory, expected, protected)?.ok_or_else(refuse)?;
    let lock = lock(&resolved).map_err(|failure| match failure {
        InstallFailure::Locked => RemovalFailure::Busy,
        InstallFailure::Io(error) => RemovalFailure::Io(error),
        _ => refuse(),
    })?;
    Ok((resolved, lock))
}

/// The canonical path of a storefront's dedicated installation folder: named
/// `expected`, neither being nor holding any of `protected`, and reached
/// through no link. `None` for any other folder.
fn dedicated(directory: &Path, expected: &str, protected: &[&Path]) -> io::Result<Option<PathBuf>> {
    if !directory.is_absolute()
        || expected.is_empty()
        || expected.contains('/')
        || directory
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Ok(None);
    }
    let candidate: PathBuf = directory
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    let named = |path: &Path| path.file_name().is_some_and(|name| name == expected);
    if !fs::symlink_metadata(&candidate)?.is_dir() || !named(&candidate) {
        return Ok(None);
    }
    let resolved = candidate.canonicalize()?;
    let safe = named(&resolved)
        && resolved.components().count() > 2
        && !protected.iter().any(|path| {
            path.canonicalize()
                .unwrap_or_else(|_| path.to_path_buf())
                .starts_with(&resolved)
        })
        // macOS spells its temporary folders both ways; any other link on
        // the way redirects the removal.
        && system_path(&candidate) == system_path(&resolved);
    Ok(safe.then_some(resolved))
}

/// Why an installation was not moved.
#[derive(Debug, Error)]
pub enum RelocationFailure {
    #[error("not a dedicated installation folder: {0}")]
    Unsafe(PathBuf),
    #[error("a folder is already where the installation goes: {0}")]
    Occupied(PathBuf),
    #[error("a Transfer, launch or uninstall holds the installation")]
    Busy,
    #[error("the move was cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Where an installation went.
#[derive(Debug)]
pub struct Relocation {
    /// The canonical installation folder at its old place.
    pub origin: PathBuf,
    /// The canonical installation folder at its new place.
    pub directory: PathBuf,
    /// Whether the folder at its old place is gone. A copy to another disk
    /// that could not remove every original file still succeeded.
    pub original_removed: bool,
}

/// Moves a storefront's dedicated installation folder, named `expected` as
/// `remove` requires, into `parent`, an existing folder, under its
/// installation lock. On the same disk the folder only changes its place. To
/// another disk its files are copied into a hidden folder beside the
/// destination, which takes the installation's name once complete, then the
/// original goes; `progress` hears the bytes copied of the total as the copy
/// goes, and a cancellation removes the copy and keeps the original. Links
/// are copied as links. The Installation receipt the folder keeps then names
/// its new place.
pub fn relocate(
    directory: &Path,
    parent: &Path,
    expected: &str,
    protected: &[&Path],
    cancel: &CancelToken,
    mut progress: impl FnMut(u64, u64),
) -> Result<Relocation, RelocationFailure> {
    let refuse = || RelocationFailure::Unsafe(directory.to_owned());
    let resolved = dedicated(directory, expected, protected)?.ok_or_else(refuse)?;
    let parent = parent.canonicalize()?;
    if parent.starts_with(&resolved) || resolved.parent() == Some(parent.as_path()) {
        return Err(refuse());
    }
    let destination = parent.join(expected);
    match fs::symlink_metadata(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => return Err(RelocationFailure::Occupied(destination)),
        Err(error) => return Err(error.into()),
    }
    let _lock = lock(&resolved).map_err(|failure| match failure {
        InstallFailure::Locked => RelocationFailure::Busy,
        InstallFailure::Io(error) => RelocationFailure::Io(error),
        _ => refuse(),
    })?;
    let original_removed = match fs::rename(&resolved, &destination) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_across(&resolved, &destination, cancel, &mut progress)?
        }
        Err(error) => return Err(error.into()),
    };
    rebase_receipt(&destination)?;
    Ok(Relocation {
        origin: resolved,
        directory: destination,
        original_removed,
    })
}

/// Copies the installation folder `from`, whose lock the caller holds, to
/// `to` on another disk through its staging folder, then removes `from`:
/// `false` when some original file stayed.
fn copy_across(
    from: &Path,
    to: &Path,
    cancel: &CancelToken,
    progress: &mut impl FnMut(u64, u64),
) -> Result<bool, RelocationFailure> {
    copy_folder(from, to, cancel, progress)?;
    Ok(fs::remove_dir_all(from).is_ok())
}

/// Copies the folder `from` to `to`, which must not exist, on any disk
/// through a hidden folder beside `to` that takes its name once complete,
/// and keeps `from`. `progress` hears the bytes copied of the total; a
/// cancellation or a failure removes the copy. Links are copied as links,
/// and an installation lock at the top of `from` stays behind.
pub fn copy_folder(
    from: &Path,
    to: &Path,
    cancel: &CancelToken,
    progress: &mut impl FnMut(u64, u64),
) -> Result<(), RelocationFailure> {
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(RelocationFailure::Unsafe(to.to_owned()));
    };
    // A copy an earlier move left behind is only ever Obelisk's own.
    let staging = staging(parent, &name.to_string_lossy());
    match fs::symlink_metadata(&staging) {
        Ok(_) => fs::remove_dir_all(&staging)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut copy = Copy {
        cancel,
        buffer: vec![0; COPY_CHUNK],
        copied: 0,
        total: size(from)?,
        progress,
    };
    (copy.progress)(0, copy.total);
    let outcome = copy
        .folder(from, &staging, true)
        .and_then(|()| Ok(fs::rename(&staging, to)?));
    if let Err(failure) = outcome {
        let _ = fs::remove_dir_all(&staging);
        return Err(failure);
    }
    Ok(())
}

/// The hidden folder a move to another disk copies `expected` into, inside
/// the destination's `parent`.
fn staging(parent: &Path, expected: &str) -> PathBuf {
    parent.join(format!(".{expected}{STAGING_SUFFIX}"))
}

/// Whether `name` is one of the hidden folders a move to another disk
/// copies into.
pub fn is_staging(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(STAGING_SUFFIX)
}

/// The bytes of every regular file in `folder`, links not followed.
pub fn size(folder: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            total += size(&entry.path())?;
        } else if kind.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// One copy of an installation folder to another disk.
struct Copy<'a, P: FnMut(u64, u64)> {
    cancel: &'a CancelToken,
    buffer: Vec<u8>,
    copied: u64,
    total: u64,
    /// Hears the bytes copied of the total after each run written.
    progress: &'a mut P,
}

impl<P: FnMut(u64, u64)> Copy<'_, P> {
    /// Copies `from` into the new folder `to`: folders, regular files with
    /// their permissions and times, and links as links. The installation
    /// lock stays behind.
    fn folder(&mut self, from: &Path, to: &Path, root: bool) -> Result<(), RelocationFailure> {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            if root && name == LOCK_FILE {
                continue;
            }
            let (source, target) = (entry.path(), to.join(&name));
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
            } else if kind.is_dir() {
                self.folder(&source, &target, false)?;
            } else if kind.is_file() {
                self.file(&source, &target)?;
            }
            // Nothing else, such as a socket, belongs to an installation.
        }
        // Last, so a folder that only reads still takes its files. A disk
        // that keeps no permissions, such as exFAT, keeps what it can.
        let _ = fs::set_permissions(to, fs::metadata(from)?.permissions());
        Ok(())
    }

    fn file(&mut self, from: &Path, to: &Path) -> Result<(), RelocationFailure> {
        let mut source = File::open(from)?;
        let metadata = source.metadata()?;
        let mut target = File::create_new(to)?;
        loop {
            if self.cancel.is_cancelled() {
                return Err(RelocationFailure::Cancelled);
            }
            let read = match source.read(&mut self.buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            target.write_all(&self.buffer[..read])?;
            self.copied += read as u64;
            (self.progress)(self.copied, self.total);
        }
        let _ = target.set_permissions(metadata.permissions());
        if let (Ok(accessed), Ok(modified)) = (metadata.accessed(), metadata.modified()) {
            let _ = target.set_times(
                FileTimes::new()
                    .set_accessed(accessed)
                    .set_modified(modified),
            );
        }
        Ok(())
    }
}

/// Makes the Installation receipt in `root` name `root` as its folder, as
/// after the folder moved.
fn rebase_receipt(root: &Path) -> Result<(), RelocationFailure> {
    let failure = |failure: InstallFailure| match failure {
        InstallFailure::Io(error) => RelocationFailure::Io(error),
        _ => RelocationFailure::Unsafe(root.join(RECEIPT_FILE)),
    };
    let Some(mut receipt) = receipt_in::<serde_json::Value>(root).map_err(failure)? else {
        return Ok(());
    };
    let Some(directory) = receipt.get_mut("directory") else {
        return Ok(());
    };
    let named = serde_json::Value::String(root.to_string_lossy().into_owned());
    if *directory == named {
        return Ok(());
    }
    *directory = named;
    write_receipt(root, &receipt).map_err(failure)
}

fn system_path(path: &Path) -> PathBuf {
    for alias in ["/var", "/tmp"] {
        if let Ok(rest) = path.strip_prefix(alias) {
            return Path::new("/private")
                .join(alias.trim_start_matches('/'))
                .join(rest);
        }
    }
    path.to_owned()
}

#[cfg(test)]
mod tests;
