//! Where Obelisk installs games: one folder, Obelisk's own inside its library
//! until the player chooses another, such as one on an external disk. Each
//! storefront keeps its games in its own folder inside it. Choosing another
//! queues the move of every installation outside it, one game at a time in
//! the download queue, and points the new installations still waiting there.
//! An installation whose disk is not connected shows as not installed until
//! the disk comes back.
use crate::supervisor::plain_absolute;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;

/// Obelisk's own games folder, inside its library.
const DEFAULT_DIRECTORY: &str = "Games";
/// The folder Obelisk makes inside the one the player chooses.
pub const CHOSEN_DIRECTORY: &str = "Obelisk Games";
/// The player's games folder, inside Obelisk's library; absent for its own.
const FILE: &str = "games-folder.json";

#[derive(Serialize, Deserialize)]
struct Saved {
    path: PathBuf,
}

/// Where new installations go, as the player sees it. `revision` grows with
/// every change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub revision: u64,
    pub path: PathBuf,
    /// Obelisk's own folder, inside its library.
    pub default: bool,
    /// Its disk is there: Obelisk's own folder always is.
    pub connected: bool,
}

/// `path` as the player sees it, at `revision`: Obelisk's own folder when it
/// is `default`.
fn view(path: PathBuf, default: &Path, revision: u64) -> View {
    let default = path == default;
    View {
        revision,
        connected: default || path.is_dir(),
        default,
        path,
    }
}

/// What choosing a folder would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The games folder it gives: the chosen folder's own, else one made
    /// inside it.
    pub path: PathBuf,
    /// The connected installations outside it, which move into it.
    pub games: u32,
    /// What those of another disk copy there; the others only change their
    /// place.
    pub copied_bytes: u64,
}

/// Why a folder cannot hold the games.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It is not an existing folder.
    Missing,
    /// It is inside Obelisk's library, or inside a game's installation.
    Inside,
}

/// An account's installations, as the games folder moves and finds them.
pub(crate) trait Resident: Send + Sync {
    /// The connected installations' folders, with their bytes.
    fn installations(&self) -> Vec<(PathBuf, u64)>;
    /// Queues the move into `folder` of each installation outside it, and
    /// points the new installations still waiting there.
    fn gather(self: Arc<Self>, folder: &Path);
    /// Hides the installations whose disk left, and shows those whose disk
    /// came back.
    fn refresh(&self);
}

/// Obelisk's games folder, which every account's installations share.
pub struct GamesFolder {
    library: PathBuf,
    published: watch::Sender<View>,
    residents: Mutex<Vec<Weak<dyn Resident>>>,
}

impl GamesFolder {
    /// The games folder of Obelisk's `library`, as the player last chose it.
    pub fn new(library: &Path) -> Arc<Self> {
        // Installations name their canonical folder.
        let library = library
            .canonicalize()
            .unwrap_or_else(|_| library.to_owned());
        let default = library.join(DEFAULT_DIRECTORY);
        let path = fs::read(library.join(FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
            .map(|saved| saved.path)
            .filter(|path| plain_absolute(path) && *path != default)
            .unwrap_or_else(|| default.clone());
        // A disk that sleeps wakes slowly: Obelisk's start does not wait.
        let games = path.clone();
        std::thread::spawn(move || clear_staging(&games));
        Arc::new(Self {
            library,
            published: watch::channel(view(path, &default, 1)).0,
            residents: Mutex::new(Vec::new()),
        })
    }

    pub fn view(&self) -> View {
        self.published.borrow().clone()
    }

    /// Obelisk's library, which holds its own games folder.
    pub fn library(&self) -> &Path {
        &self.library
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> View {
        let mut receiver = self.published.subscribe();
        let _ = receiver.wait_for(|view| view.revision > after).await;
        receiver.borrow().clone()
    }

    /// Where new installations go.
    pub fn path(&self) -> PathBuf {
        self.published.borrow().path.clone()
    }

    /// Whether new installations can go there now: Obelisk's own folder is
    /// made when needed, the player's must be there.
    pub fn connected(&self) -> bool {
        let view = self.view();
        view.default || view.path.is_dir()
    }

    fn default_path(&self) -> PathBuf {
        self.library.join(DEFAULT_DIRECTORY)
    }

    /// Publishes `path` as the games folder, and whether its disk is
    /// connected.
    fn publish(&self, path: PathBuf) {
        let mut next = view(path, &self.default_path(), 0);
        self.published.send_if_modified(|current| {
            next.revision = current.revision;
            if next == *current {
                return false;
            }
            next.revision += 1;
            *current = next;
            true
        });
    }

    /// Whether `games` is a games folder Obelisk made: its own, or the one
    /// inside a folder the player chose.
    fn ours(&self, games: &Path) -> bool {
        games == self.default_path()
            || games
                .file_name()
                .is_some_and(|name| name == CHOSEN_DIRECTORY)
    }

    /// The games folder `chosen` gives, and what choosing it moves.
    pub fn inspect(&self, chosen: &Path) -> Result<Candidate, Refusal> {
        let (path, installations) = self.destination(chosen)?;
        let device = fs::metadata(chosen).map_err(|_| Refusal::Missing)?.dev();
        let mut candidate = Candidate {
            path: path.clone(),
            games: 0,
            copied_bytes: 0,
        };
        let mut seen = Vec::new();
        for (installation, bytes) in installations {
            // Two accounts may share one installation: it moves once.
            if installation.starts_with(&path) || seen.contains(&installation) {
                continue;
            }
            candidate.games += 1;
            if fs::metadata(&installation).is_ok_and(|metadata| metadata.dev() != device) {
                candidate.copied_bytes += bytes;
            }
            seen.push(installation);
        }
        Ok(candidate)
    }

    /// What making Obelisk's own folder the games folder again would do. It
    /// makes that folder.
    pub fn inspect_default(&self) -> Result<Candidate, Refusal> {
        let path = self.default_path();
        // A folder it cannot make reads as missing.
        let _ = fs::create_dir_all(&path);
        self.inspect(&path)
    }

    /// The games folder `chosen` gives, and the connected installations,
    /// none of which holds it.
    fn destination(&self, chosen: &Path) -> Result<(PathBuf, Vec<(PathBuf, u64)>), Refusal> {
        let path = self.resolve(chosen)?;
        let installations: Vec<(PathBuf, u64)> = self
            .residents()
            .iter()
            .flat_map(|resident| resident.installations())
            .collect();
        if installations
            .iter()
            .any(|(installation, _)| path.starts_with(installation))
        {
            return Err(Refusal::Inside);
        }
        Ok((path, installations))
    }

    /// The games folder `chosen` gives: Obelisk's own folder, or the one
    /// Obelisk makes inside the chosen folder, unless that is one already.
    fn resolve(&self, chosen: &Path) -> Result<PathBuf, Refusal> {
        if !plain_absolute(chosen) {
            return Err(Refusal::Missing);
        }
        let chosen = chosen.canonicalize().map_err(|_| Refusal::Missing)?;
        if !chosen.is_dir() {
            return Err(Refusal::Missing);
        }
        if self.ours(&chosen) {
            return Ok(chosen);
        }
        if chosen.starts_with(&self.library) {
            return Err(Refusal::Inside);
        }
        Ok(chosen.join(CHOSEN_DIRECTORY))
    }

    /// Makes the games folder `chosen` gives the one new installations go
    /// to, and queues the move into it of every installation outside it.
    pub fn choose(&self, chosen: &Path) -> Result<(), ChoiceFailure> {
        let (path, _) = self.destination(chosen)?;
        fs::create_dir_all(&path)?;
        self.save(&path)?;
        self.publish(path);
        self.gather();
        Ok(())
    }

    /// Makes Obelisk's own folder the games folder again, and queues the
    /// move into it of every installation outside it.
    pub fn choose_default(&self) -> Result<(), ChoiceFailure> {
        let path = self.default_path();
        fs::create_dir_all(&path)?;
        self.choose(&path)
    }

    /// Queues the move of the installations still outside the games folder.
    pub fn gather(&self) {
        let path = self.path();
        for resident in self.residents() {
            resident.gather(&path);
        }
    }

    /// A disk came or left: the games folder tells whether its disk is
    /// connected, and each account shows the installations whose disk is.
    pub fn volumes_changed(&self) {
        self.publish(self.path());
        for resident in self.residents() {
            resident.refresh();
        }
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        let file = self.library.join(FILE);
        if path == self.default_path() {
            return match fs::remove_file(&file) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let saved = Saved {
            path: path.to_owned(),
        };
        obelisk_core::write_atomic(&file, &serde_json::to_vec(&saved)?)
    }

    pub(crate) fn register(&self, resident: Weak<dyn Resident>) {
        let mut residents = self.residents.lock().unwrap();
        residents.retain(|kept| kept.strong_count() > 0);
        residents.push(resident);
    }

    fn residents(&self) -> Vec<Arc<dyn Resident>> {
        self.residents
            .lock()
            .unwrap()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }

    /// The installation `name` of a storefront whose games go in `storefront`,
    /// once moved into the games folder.
    pub(crate) fn installation(&self, storefront: &str, name: &Path) -> Option<PathBuf> {
        let name = name.file_name()?;
        let found = self.path().join(storefront).join(name);
        found.is_dir().then_some(found)
    }

    /// Removes the storefront's folder an installation left, `storefront`,
    /// and the games folder holding it, once empty and no longer the games
    /// folder. Only Obelisk's own folders go.
    pub(crate) fn tidy(&self, storefront: &Path) {
        let Some(games) = storefront.parent() else {
            return;
        };
        if games == self.path() {
            return;
        }
        if self.ours(games) && fs::remove_dir(storefront).is_ok() {
            let _ = fs::remove_dir(games);
        }
    }
}

/// Removes the copies moves to the games folder `games` left when Obelisk
/// quit before they ended.
fn clear_staging(games: &Path) {
    let Ok(storefronts) = fs::read_dir(games) else {
        return;
    };
    for storefront in storefronts.flatten() {
        let Ok(entries) = fs::read_dir(storefront.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if obelisk_core::installation::is_staging(&entry.file_name().to_string_lossy())
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Why choosing a folder failed.
#[derive(Debug, thiserror::Error)]
pub enum ChoiceFailure {
    #[error("the folder cannot hold the games: {0:?}")]
    Refused(Refusal),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<Refusal> for ChoiceFailure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// Whether `path` lies on a disk that is not connected: an external disk's
/// folder under /Volumes whose disk is gone.
pub fn disconnected(path: &Path) -> bool {
    #[cfg(test)]
    if disks::holds(path) {
        return true;
    }
    let mut components = path.components();
    let (Some(Component::RootDir), Some(Component::Normal(volumes)), Some(Component::Normal(disk))) =
        (components.next(), components.next(), components.next())
    else {
        return false;
    };
    volumes == "Volumes" && !Path::new("/Volumes").join(disk).exists()
}

/// The external disks of the tests: a folder stands for each, as no test can
/// mount one under /Volumes. A path under an unplugged disk is on a disk that
/// is not connected, whether or not the folder is there.
#[cfg(test)]
pub(crate) mod disks {
    use std::{
        path::{Path, PathBuf},
        sync::Mutex,
    };

    static UNPLUGGED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(super) fn holds(path: &Path) -> bool {
        UNPLUGGED
            .lock()
            .unwrap()
            .iter()
            .any(|disk| path.starts_with(disk))
    }

    /// An external disk the test unplugs and plugs in again. Unplugged, its
    /// folder goes aside, files and all.
    pub(crate) struct Disk {
        _directory: tempfile::TempDir,
        mount: PathBuf,
        aside: PathBuf,
    }

    impl Disk {
        /// A new disk, plugged in.
        pub(crate) fn plugged() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let (mount, aside) = (root.join("Obelisk Disk"), root.join("Obelisk Disk Aside"));
            std::fs::create_dir(&mount).unwrap();
            Self {
                _directory: directory,
                mount,
                aside,
            }
        }

        /// The disk's folder: where its files are while it is plugged.
        pub(crate) fn mount(&self) -> &Path {
            &self.mount
        }

        pub(crate) fn plug(&self) {
            self.forget();
            std::fs::rename(&self.aside, &self.mount).unwrap();
        }

        pub(crate) fn unplug(&self) {
            std::fs::rename(&self.mount, &self.aside).unwrap();
            UNPLUGGED.lock().unwrap().push(self.mount.clone());
        }

        fn forget(&self) {
            UNPLUGGED.lock().unwrap().retain(|kept| *kept != self.mount);
        }
    }

    impl Drop for Disk {
        fn drop(&mut self) {
            self.forget();
        }
    }
}

#[cfg(test)]
mod tests;
