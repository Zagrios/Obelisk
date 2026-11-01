//! Where Obelisk keeps its library: its folder on this Mac until the player
//! moves it, such as to an external disk. The library moves whole, saves,
//! sessions, runtimes and Wine prefixes included, to a real folder: never a
//! link, which Steam and the Cloud archive refuse. Obelisk then restarts in
//! it, and removes the library it left at its next start. The app keeps
//! where the library is outside the library itself.
use crate::{games_folder, supervisor::plain_absolute};
use obelisk_core::{
    installation::{self, RelocationFailure},
    transfer::CancelToken,
};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// The folder Obelisk makes inside the one the player chooses.
pub const CHOSEN_DIRECTORY: &str = "Obelisk Library";

/// What moving the library would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Where the library goes: a real folder, reached through no link.
    pub path: PathBuf,
    /// What the move copies to another disk; nothing when the library only
    /// changes its place on its own disk.
    pub copied_bytes: u64,
}

/// Why the library cannot go to a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The folder that would hold it is not an existing folder.
    Missing,
    /// It is the library, or inside it, or holds it.
    Inside,
    /// A folder that is not empty is already there.
    Occupied,
    /// Games are installed in Obelisk's own games folder, inside the library,
    /// and their installations name it: they move to another games folder
    /// first.
    HoldsGames,
}

/// Why the move failed. The library stays where it was.
#[derive(Debug, thiserror::Error)]
pub enum MoveFailure {
    #[error("the library cannot go there: {0:?}")]
    Refused(Refusal),
    #[error("the move was cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("the Cloud archive could not follow the library: {0}")]
    Cloud(#[from] cloud_vault::Error),
}

impl From<Refusal> for MoveFailure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// The library folder choosing `chosen` gives: the one Obelisk makes inside
/// it, unless it is one already.
pub fn chosen(chosen: &Path) -> PathBuf {
    if chosen
        .file_name()
        .is_some_and(|name| name == CHOSEN_DIRECTORY)
    {
        chosen.to_owned()
    } else {
        chosen.join(CHOSEN_DIRECTORY)
    }
}

/// What moving `library` to `destination` would do: `destination` is the
/// folder the library becomes, absent or empty, inside an existing folder.
pub fn inspect(library: &Path, destination: &Path) -> Result<Candidate, Refusal> {
    let (Some(parent), Some(name)) = (destination.parent(), destination.file_name()) else {
        return Err(Refusal::Missing);
    };
    if !plain_absolute(destination) {
        return Err(Refusal::Missing);
    }
    // The library goes to the folder the links lead to, so that it is a
    // real folder all the way.
    let parent = parent.canonicalize().map_err(|_| Refusal::Missing)?;
    if !fs::metadata(&parent).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(Refusal::Missing);
    }
    let path = parent.join(name);
    let library = library
        .canonicalize()
        .unwrap_or_else(|_| library.to_owned());
    if path.starts_with(&library) || library.starts_with(&path) {
        return Err(Refusal::Inside);
    }
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(metadata) if metadata.is_dir() && empty(&path) => {}
        _ => return Err(Refusal::Occupied),
    }
    if holds_games(&library) {
        return Err(Refusal::HoldsGames);
    }
    let device = |path: &Path| fs::metadata(path).map(|metadata| metadata.dev()).ok();
    let copied_bytes = if device(&parent) == device(&library) {
        0
    } else {
        installation::size(&library).unwrap_or(0)
    };
    Ok(Candidate { path, copied_bytes })
}

/// Moves `library` to `destination`, as `inspect` allows. On its own disk
/// the library only changes its place; to another disk it is copied, links
/// as links, and the original stays until `remove_left` removes it, so that
/// a failure or a quit leaves the library whole where it was. `progress`
/// hears the bytes copied of the total. The Cloud archive's claims on saves
/// inside the library, such as a Windows game's in its Wine prefix, then
/// name their new place. No game may run, and no Transfer go, meanwhile.
pub fn move_library(
    library: &Path,
    destination: &Path,
    cancel: &CancelToken,
    progress: impl FnMut(u64, u64),
) -> Result<PathBuf, MoveFailure> {
    move_with(library, destination, cancel, progress, |from, to| {
        fs::rename(from, to)
    })
}

/// `move_library`, whose library changes its place through `rename`, or is
/// copied when `rename` cannot.
fn move_with(
    library: &Path,
    destination: &Path,
    cancel: &CancelToken,
    mut progress: impl FnMut(u64, u64),
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<PathBuf, MoveFailure> {
    let candidate = inspect(library, destination)?;
    let library = library.canonicalize()?;
    let path = candidate.path;
    if path.exists() {
        let _ = fs::remove_file(path.join(".DS_Store"));
        fs::remove_dir(&path)?;
    }
    let renamed = match rename(&library, &path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => false,
        // A mount point stays where it is: its files are copied.
        Err(error) if error.kind() == io::ErrorKind::ResourceBusy => false,
        Err(error) => return Err(error.into()),
    };
    if !renamed {
        installation::copy_folder(&library, &path, cancel, &mut progress).map_err(|failure| {
            match failure {
                RelocationFailure::Cancelled => MoveFailure::Cancelled,
                RelocationFailure::Io(error) => MoveFailure::Io(error),
                other => MoveFailure::Io(io::Error::other(other.to_string())),
            }
        })?;
    }
    let archive = path.join(crate::locations::CLOUD_ARCHIVE);
    if let Err(error) = cloud_vault::relocate(&archive, &library, &path) {
        // The library goes back as it was.
        if renamed {
            let _ = fs::rename(&path, &library);
        } else {
            let _ = fs::remove_dir_all(&path);
        }
        return Err(error.into());
    }
    Ok(path)
}

/// Removes the library Obelisk left at `left` for `library`, its new place,
/// once Obelisk started there. Nothing goes that holds `library` or that it
/// holds.
pub fn remove_left(left: &Path, library: &Path) -> io::Result<()> {
    let library = library.canonicalize()?;
    if !plain_absolute(left)
        || left.components().count() < 3
        || library.starts_with(left)
        || left.starts_with(&library)
    {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    match fs::symlink_metadata(left) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(left),
        Ok(_) => Err(io::Error::from(io::ErrorKind::InvalidInput)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether `folder` holds nothing but Finder's own files.
fn empty(folder: &Path) -> bool {
    fs::read_dir(folder).is_ok_and(|mut entries| {
        entries.all(|entry| entry.is_ok_and(|entry| entry.file_name() == ".DS_Store"))
    })
}

/// Whether Obelisk's own games folder inside `library` holds an
/// installation, or what is left of one, in a storefront's folder.
fn holds_games(library: &Path) -> bool {
    let Ok(storefronts) = fs::read_dir(library.join(games_folder::DEFAULT_DIRECTORY)) else {
        return false;
    };
    storefronts.flatten().any(|storefront| {
        storefront.file_type().is_ok_and(|kind| kind.is_dir()) && !empty(&storefront.path())
    })
}

#[cfg(test)]
mod tests;
