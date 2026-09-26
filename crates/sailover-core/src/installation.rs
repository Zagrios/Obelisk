//! One installation Transfer, in the order every storefront follows around its
//! content engine. SailOver confines the destination, lets the storefront prepare
//! before any file exists, holds the installation lock from the first file to
//! the receipt, withdraws the old Installation receipt before files change and
//! publishes the new one only after every file succeeded. Storefronts supply
//! only preparation and file installation.
use std::{
    fmt::Display,
    fs::{self, File},
    future::Future,
    io,
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
        CancelToken, Cancelled, TransferBusy, TransferContext, TransferState, TransferStatus,
        Transfers,
    },
};

/// Receipts are small JSON documents; anything larger is not one of ours.
const MAX_RECEIPT_BYTES: usize = 1024 * 1024;

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

/// What sailover-core refused or failed around an installation. Each storefront
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

/// Every file of an installation succeeded. SailOver publishes `receipt` next.
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
    let bytes = serde_json::to_vec_pretty(&installed.receipt)
        .map_err(io::Error::other)
        .map_err(InstallFailure::Io)?;
    // Re-checked right before the replacement, so a link substituted while
    // files were written is refused too.
    crate::write_atomic_with(&receipt, &bytes, || receipt_path(&root).map(drop))?;
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
/// across SailOver processes. The lock is released when the file is dropped.
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
    let receipt = read_file_bounded(&receipt_path(&root)?, MAX_RECEIPT_BYTES)?
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Ok(Installation {
        root,
        receipt,
        _lock: lock,
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
    let refuse = || RemovalFailure::Unsafe(directory.to_owned());
    if !directory.is_absolute()
        || expected.is_empty()
        || expected.contains('/')
        || directory
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(refuse());
    }
    let candidate: PathBuf = directory
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    let named = |path: &Path| path.file_name().is_some_and(|name| name == expected);
    if !fs::symlink_metadata(&candidate)?.is_dir() || !named(&candidate) {
        return Err(refuse());
    }
    let resolved = candidate.canonicalize()?;
    if !named(&resolved)
        || resolved.components().count() <= 2
        || protected.iter().any(|path| {
            path.canonicalize()
                .unwrap_or_else(|_| path.to_path_buf())
                .starts_with(&resolved)
        })
        // macOS spells its temporary folders both ways; any other link on
        // the way redirects the removal.
        || system_path(&candidate) != system_path(&resolved)
    {
        return Err(refuse());
    }
    let _lock = lock(&resolved).map_err(|failure| match failure {
        InstallFailure::Locked => RemovalFailure::Busy,
        InstallFailure::Io(error) => RemovalFailure::Io(error),
        _ => refuse(),
    })?;
    Ok(fs::remove_dir_all(&resolved)?)
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
