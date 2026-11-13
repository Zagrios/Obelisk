//! Puts the files of one Transfer into an installation. A storefront plans and
//! assembles each file in a partial file; the [`Publisher`] turns it into an
//! installed file by one rule, the same for every storefront, and ends the
//! Transfer's files.
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    install::{self, InstallError, checked_file_path, ensure_directory, safe_relative_path},
    transfer::{CancelToken, Cancelled},
    verification::{self, VerificationFile},
};

/// Installs the files of one installation, from its engine lock to its
/// receipt: the disk worker that holds the lock makes one and drops it,
/// unfinished, when the Transfer fails or is cancelled. Dropping writes
/// nothing, so a stopped Transfer does no more disk work.
pub struct Publisher {
    root: PathBuf,
    cancel: CancelToken,
    /// Parent folders made and checked already.
    parents: HashSet<PathBuf>,
    /// The folder of the last file published, whose entries the drive has not
    /// been handed yet.
    unsynced: Option<PathBuf>,
}

/// A file assembled whole in a partial file, to be installed.
pub struct Assembled<'a> {
    /// The partial file, open for writing. [`Publisher::publish`] writes it
    /// out and closes it.
    pub file: File,
    /// Where it waits.
    pub at: &'a Path,
    /// Where it belongs: its manifest name, confined by the publisher
    /// (reserved names, `..`, drive letters), so no raw path can be handed in.
    pub name: &'a str,
    /// Whether the manifest flags it a program. Some manifests flag only a
    /// build's launch executable and leave the helpers it starts unflagged
    /// (Prison Architect's bootstrap runs an unflagged installer, OpenFront's
    /// Electron helpers come unflagged), so a file that starts as one, a
    /// Mach-O binary or a script, is installed as one anyway.
    pub program: bool,
    pub integrity: Integrity<'a>,
}

/// How the finished file was checked. A storefront says which: there is no
/// default.
pub enum Integrity<'a> {
    /// The whole file hashed to `written` as it was assembled; its manifest
    /// says `expected`.
    Whole {
        written: &'a [u8],
        expected: &'a [u8],
    },
    /// Every part was checked as it was assembled and the manifest records no
    /// hash of the whole.
    PartsChecked,
}

#[derive(Debug, Error)]
pub enum PublishError {
    /// The assembled file is not what its manifest says. Nothing was
    /// installed.
    #[error("the assembled file does not match its manifest")]
    Mismatch,
    /// `Cancelled`, `UnsafePath` or `Io`.
    #[error(transparent)]
    Install(#[from] InstallError),
}

impl From<io::Error> for PublishError {
    fn from(error: io::Error) -> Self {
        Self::Install(error.into())
    }
}

impl From<Cancelled> for PublishError {
    fn from(error: Cancelled) -> Self {
        Self::Install(error.into())
    }
}

impl Publisher {
    /// For the installation at `root`: its canonical folder, whose engine
    /// lock the caller holds.
    pub fn new(root: &Path, cancel: &CancelToken) -> Self {
        Self {
            root: root.to_path_buf(),
            cancel: cancel.clone(),
            parents: HashSet::new(),
            unsynced: None,
        }
    }

    /// Installs `assembled`: makes its folders, compares its hash, writes it
    /// out, sets its mode, checks for cancellation, confines its target again
    /// and renames it into place. An installed file is only ever replaced by
    /// renaming a new one over it: the verified record relies on that. On any
    /// `Err` before the rename, the partial file is where it was, an existing
    /// file is untouched and nothing outside the root was reached. An `Err`
    /// from the previous folder's sync comes after the rename: this file is in
    /// place.
    ///
    /// Folders reach the drive once per run of files sharing one, when the
    /// next file lands elsewhere, and the last one in [`Self::complete`]. A
    /// rename a power loss takes back never leaves a wrong file: the next
    /// Transfer verifies what the disk holds and publishes the file again,
    /// from its partial file, or from parts fetched again when the storefront
    /// dropped the ones it cached.
    pub fn publish(&mut self, assembled: Assembled<'_>) -> Result<(), PublishError> {
        let Assembled {
            file,
            at,
            name,
            program,
            integrity,
        } = assembled;
        let relative = safe_relative_path(name)?;
        let parent = relative.parent().unwrap_or(Path::new(""));
        if !self.parents.contains(parent) {
            ensure_directory(&self.root, parent)?;
            self.parents.insert(parent.to_path_buf());
        }
        if let Integrity::Whole { written, expected } = integrity
            && written != expected
        {
            return Err(PublishError::Mismatch);
        }
        install::write_out(&file)?;
        drop(file);
        set_mode(at, program)?;
        self.cancel.check()?;
        // Right before the rename, never following a link put under the root
        // since the file was planned.
        let target = checked_file_path(&self.root, &relative)?;
        fs::rename(at, &target)?;
        let folder = target.parent().unwrap_or(&self.root);
        if self.unsynced.as_deref() != Some(folder)
            && let Some(previous) = self.unsynced.replace(folder.to_path_buf())
        {
            install::write_out(&File::open(previous)?)?;
        }
        Ok(())
    }

    /// A file already in place and verified whole: only its mode is settled,
    /// by the same rule, through the same confinement.
    pub fn keep(&self, name: &str, program: bool) -> Result<(), InstallError> {
        let target = checked_file_path(&self.root, &safe_relative_path(name)?)?;
        set_mode(&target, program)?;
        Ok(())
    }

    /// Every file is in place. Hands the folder still unsynced to the drive,
    /// makes the installation durable with one flush of its drive, then
    /// records `files` whole for the next Transfer to skip. The receipt is
    /// written after this returns.
    pub fn complete<T: VerificationFile>(self, files: &[T]) -> io::Result<()> {
        if let Some(folder) = &self.unsynced {
            install::write_out(&File::open(folder)?)?;
        }
        verification::complete(&self.root, files)
    }
}

/// Whether the file starts as a program does: a Mach-O binary or a script.
fn starts_as_program(path: &Path) -> io::Result<bool> {
    const PROGRAMS: [&[u8]; 5] = [
        &[0xcf, 0xfa, 0xed, 0xfe], // 64-bit Mach-O
        &[0xce, 0xfa, 0xed, 0xfe], // 32-bit Mach-O
        &[0xca, 0xfe, 0xba, 0xbe], // universal binary
        &[0xca, 0xfe, 0xba, 0xbf], // 64-bit universal binary
        b"#!",
    ];
    let mut header = Vec::with_capacity(4);
    File::open(path)?.take(4).read_to_end(&mut header)?;
    Ok(PROGRAMS.iter().any(|magic| header.starts_with(magic)))
}

/// Installed files are user-readable; only programs get the execute bit: those
/// the manifest flags as `program`, and those whose bytes start as one. No
/// other mode information crosses a storefront manifest.
fn set_mode(path: &Path, program: bool) -> io::Result<()> {
    let program = program || starts_as_program(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if program { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, program);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
