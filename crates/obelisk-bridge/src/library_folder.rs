//! Where Obelisk keeps its library, as Swift presents and moves it. Swift
//! keeps where the library is, outside it, and restarts Obelisk in it.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use game_runtime::library_folder::{self, Candidate, MoveFailure, Refusal};
use obelisk_core::transfer::CancelToken;

use crate::GameSessionError;

/// What moving the library would do.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LibraryFolderInspection {
    /// The library goes to `path`, copying `copied_bytes` to another disk.
    Ready { path: String, copied_bytes: u64 },
    /// The folder is gone.
    Missing,
    /// The folder is the library, inside it or holds it.
    Inside,
    /// A folder that is not empty is already where the library would go.
    Occupied,
    /// Games are installed in Obelisk's own games folder, inside the library.
    HoldsGames,
}

impl From<Result<Candidate, Refusal>> for LibraryFolderInspection {
    fn from(inspection: Result<Candidate, Refusal>) -> Self {
        match inspection {
            Ok(candidate) => Self::Ready {
                path: candidate.path.to_string_lossy().into_owned(),
                copied_bytes: candidate.copied_bytes,
            },
            Err(Refusal::Missing) => Self::Missing,
            Err(Refusal::Inside) => Self::Inside,
            Err(Refusal::Occupied) => Self::Occupied,
            Err(Refusal::HoldsGames) => Self::HoldsGames,
        }
    }
}

/// The bytes a move copied of its total.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LibraryMoveProgress {
    pub copied_bytes: u64,
    pub total_bytes: u64,
}

/// Obelisk's library, which the player may move.
#[derive(uniffi::Object)]
pub struct LibraryFolder {
    library: PathBuf,
    cancel: CancelToken,
    copied: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
}

#[uniffi::export(async_runtime = "tokio")]
impl LibraryFolder {
    /// The library in `library`.
    #[uniffi::constructor]
    pub fn new(library: String) -> Arc<Self> {
        Arc::new(Self {
            library: PathBuf::from(library),
            cancel: CancelToken::new(),
            copied: Arc::default(),
            total: Arc::default(),
        })
    }

    /// What moving the library into the folder the player chose would do.
    /// It reads the disk.
    pub fn inspect(&self, chosen: String) -> LibraryFolderInspection {
        let destination = library_folder::chosen(Path::new(&chosen));
        library_folder::inspect(&self.library, &destination).into()
    }

    /// What moving the library to exactly `destination`, such as Obelisk's
    /// own folder on this Mac, would do. It reads the disk.
    pub fn inspect_destination(&self, destination: String) -> LibraryFolderInspection {
        library_folder::inspect(&self.library, Path::new(&destination)).into()
    }

    /// Moves the library to `destination`, a path an inspection gave, and
    /// returns where it went. No game may run, and no Transfer go,
    /// meanwhile; Obelisk then restarts there.
    pub async fn move_to(&self, destination: String) -> Result<String, GameSessionError> {
        let (library, cancel) = (self.library.clone(), self.cancel.clone());
        let (copied, total) = (self.copied.clone(), self.total.clone());
        let moved = tokio::task::spawn_blocking(move || {
            library_folder::move_library(&library, Path::new(&destination), &cancel, |done, all| {
                copied.store(done, Ordering::Relaxed);
                total.store(all, Ordering::Relaxed);
            })
        })
        .await
        .map_err(|_| GameSessionError::new("model.libraryFolder.failed_unknown"))?;
        match moved {
            Ok(path) => Ok(path.to_string_lossy().into_owned()),
            Err(MoveFailure::Cancelled) => Err(GameSessionError::Cancelled),
            Err(MoveFailure::Refused(_)) => {
                Err(GameSessionError::new("model.libraryFolder.refused"))
            }
            Err(error) => Err(GameSessionError::new(error)),
        }
    }

    /// What the running move copied so far.
    pub fn progress(&self) -> LibraryMoveProgress {
        LibraryMoveProgress {
            copied_bytes: self.copied.load(Ordering::Relaxed),
            total_bytes: self.total.load(Ordering::Relaxed),
        }
    }

    /// Stops the running copy: the library stays where it was.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Removes the library Obelisk left at `left` once it started in `library`,
/// its new place. It blocks on the disk; false when something stayed.
#[uniffi::export]
pub fn remove_left_library(left: String, library: String) -> bool {
    library_folder::remove_left(Path::new(&left), Path::new(&library)).is_ok()
}
