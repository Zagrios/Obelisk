use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 1;
const CATALOG_NAME: &str = "library.json";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CoreError {
    #[error("Invalid game: {reason}")]
    InvalidGame { reason: String },
    #[error("Cannot access the library: {reason}")]
    Storage { reason: String },
    #[error("The library cannot be read: {reason}")]
    CorruptLibrary { reason: String },
    #[error("Game not found: {id}")]
    NotFound { id: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GameKind {
    NativeMac,
    WindowsExecutable,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub id: String,
    pub title: String,
    pub path: String,
    pub added_at: u64,
    pub last_played_at: Option<u64>,
    pub kind: GameKind,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    games: Vec<Game>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            games: Vec::new(),
        }
    }
}

/// A small, synchronous catalog. Call from a background executor in a UI.
///
/// Each operation reloads validated data under an OS file lock. This avoids
/// stale overwrites across multiple instances and processes. A mutex also
/// serializes calls on the same instance. Replacing JSON is atomic; failed
/// mutations leave the previous catalog intact. Game files are never changed.
#[derive(Debug)]
pub struct Library {
    storage_dir: PathBuf,
    operation: Mutex<()>,
}

impl Library {
    pub fn open(storage_dir: impl AsRef<Path>) -> Result<Self, CoreError> {
        fs::create_dir_all(storage_dir.as_ref()).map_err(storage_error)?;
        let storage_dir = fs::canonicalize(storage_dir.as_ref()).map_err(storage_error)?;
        let library = Self {
            storage_dir,
            operation: Mutex::new(()),
        };
        // Check existing data immediately; malformed data must not appear empty.
        library.with_catalog(|_| Ok(()))?;
        Ok(library)
    }

    pub fn games(&self) -> Result<Vec<Game>, CoreError> {
        self.with_catalog(|catalog| {
            let mut games = catalog.games;
            games.sort_by(|left, right| {
                right
                    .last_played_at
                    .unwrap_or(right.added_at)
                    .cmp(&left.last_played_at.unwrap_or(left.added_at))
                    .then_with(|| left.title.cmp(&right.title))
                    .then_with(|| left.id.cmp(&right.id))
            });
            Ok(games)
        })
    }

    pub fn add_local_game(&self, path: impl AsRef<Path>) -> Result<Game, CoreError> {
        let canonical =
            fs::canonicalize(path.as_ref()).map_err(|error| CoreError::InvalidGame {
                reason: format!("The selected path is unavailable: {error}"),
            })?;
        let kind = game_kind(&canonical)?;
        let path = canonical
            .to_str()
            .ok_or_else(|| invalid_game("The selected path is not valid UTF-8."))?
            .to_owned();
        let title = canonical
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| invalid_game("The selected game has no valid name."))?
            .to_owned();
        self.with_catalog(|mut catalog| {
            if let Some(existing) = catalog.games.iter().find(|game| game.path == path) {
                return Ok(existing.clone());
            }
            let game = Game {
                id: Uuid::new_v4().to_string(),
                title,
                path,
                added_at: now()?,
                last_played_at: None,
                kind,
            };
            catalog.games.push(game.clone());
            self.save(&catalog)?;
            Ok(game)
        })
    }

    /// Removes only the catalog entry. Never deletes the original game.
    pub fn remove_game(&self, id: &str) -> Result<(), CoreError> {
        self.with_catalog(|mut catalog| {
            let index = catalog
                .games
                .iter()
                .position(|game| game.id == id)
                .ok_or_else(|| CoreError::NotFound { id: id.to_owned() })?;
            catalog.games.remove(index);
            self.save(&catalog)
        })
    }

    /// Records an acknowledged launch. Does not launch the game itself.
    pub fn record_launch(&self, id: &str) -> Result<(), CoreError> {
        self.with_catalog(|mut catalog| {
            let game = catalog
                .games
                .iter_mut()
                .find(|game| game.id == id)
                .ok_or_else(|| CoreError::NotFound { id: id.to_owned() })?;
            game.last_played_at = Some(now()?);
            self.save(&catalog)
        })
    }

    fn with_catalog<T>(
        &self,
        action: impl FnOnce(Catalog) -> Result<T, CoreError>,
    ) -> Result<T, CoreError> {
        let _operation = self.operation.lock().map_err(|_| CoreError::Storage {
            reason: "The library operation lock was poisoned.".to_owned(),
        })?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.storage_dir.join("library.lock"))
            .map_err(storage_error)?;
        lock.lock().map_err(storage_error)?;
        // Dropping this file releases the OS lock even when reading/action fails.
        let catalog = self.load()?;
        action(catalog)
    }

    fn load(&self) -> Result<Catalog, CoreError> {
        let bytes = match fs::read(self.storage_dir.join(CATALOG_NAME)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Catalog::default());
            }
            Err(error) => return Err(storage_error(error)),
        };
        let catalog: Catalog = serde_json::from_slice(&bytes)
            .map_err(|error| corrupt_library(format!("Invalid JSON or catalog fields: {error}")))?;
        validate_catalog(&catalog)?;
        Ok(catalog)
    }

    fn save(&self, catalog: &Catalog) -> Result<(), CoreError> {
        let mut bytes = serde_json::to_vec_pretty(catalog).map_err(storage_error)?;
        bytes.push(b'\n');
        crate::write_atomic(&self.storage_dir.join(CATALOG_NAME), &bytes).map_err(storage_error)
    }
}

fn game_kind(path: &Path) -> Result<GameKind, CoreError> {
    let extension = path.extension().and_then(|value| value.to_str());
    match extension {
        Some(value) if value.eq_ignore_ascii_case("app") && path.is_dir() => {
            Ok(GameKind::NativeMac)
        }
        Some(value) if value.eq_ignore_ascii_case("exe") && path.is_file() => {
            Ok(GameKind::WindowsExecutable)
        }
        _ => Err(invalid_game(
            "Select a macOS .app directory or a Windows .exe file.",
        )),
    }
}

fn validate_catalog(catalog: &Catalog) -> Result<(), CoreError> {
    if catalog.schema_version != SCHEMA_VERSION {
        return Err(corrupt_library(format!(
            "Unsupported schema version {} (expected {SCHEMA_VERSION}).",
            catalog.schema_version
        )));
    }
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for game in &catalog.games {
        if Uuid::parse_str(&game.id).is_err() || !ids.insert(&game.id) {
            return Err(corrupt_library("Invalid or duplicate game ID."));
        }
        let path = Path::new(&game.path);
        let extension = path.extension().and_then(|value| value.to_str());
        let expected_extension = match game.kind {
            GameKind::NativeMac => "app",
            GameKind::WindowsExecutable => "exe",
        };
        if !path.is_absolute()
            || !paths.insert(&game.path)
            || game.title.trim().is_empty()
            || !extension.is_some_and(|value| value.eq_ignore_ascii_case(expected_extension))
        {
            return Err(corrupt_library(
                "Invalid or duplicate game path, title, or kind.",
            ));
        }
        // Missing games remain in the catalog so users can remove them explicitly.
    }
    Ok(())
}

fn now() -> Result<u64, CoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(storage_error)
}

fn invalid_game(reason: impl Into<String>) -> CoreError {
    CoreError::InvalidGame {
        reason: reason.into(),
    }
}

fn corrupt_library(reason: impl Into<String>) -> CoreError {
    CoreError::CorruptLibrary {
        reason: reason.into(),
    }
}

fn storage_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::Storage {
        reason: error.to_string(),
    }
}
