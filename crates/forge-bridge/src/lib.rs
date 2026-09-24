//! The complete Swift/Rust seam. Swift owns UI state and OS presentation.
//! Domain code and storefront kits have no dependency on UniFFI.

use std::sync::Arc;

mod steam;
pub use steam::*;
mod steam_friends;
pub use steam_friends::*;
mod steam_native;
pub use steam_native::*;
mod epic;
pub use epic::*;
mod epic_cloud;
pub use epic_cloud::*;
mod epic_friends;
pub use epic_friends::*;
mod cloud_maintenance;
pub use cloud_maintenance::*;
mod game_session;
pub use game_session::*;

uniffi::setup_scaffolding!();

#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
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

impl From<forge_core::CoreError> for CoreError {
    fn from(error: forge_core::CoreError) -> Self {
        match error {
            forge_core::CoreError::InvalidGame { reason } => Self::InvalidGame { reason },
            forge_core::CoreError::Storage { reason } => Self::Storage { reason },
            forge_core::CoreError::CorruptLibrary { reason } => Self::CorruptLibrary { reason },
            forge_core::CoreError::NotFound { id } => Self::NotFound { id },
        }
    }
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum GameKind {
    NativeMac,
    WindowsExecutable,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct Game {
    pub id: String,
    pub title: String,
    pub path: String,
    pub added_at: u64,
    pub last_played_at: Option<u64>,
    pub kind: GameKind,
}

impl From<forge_core::Game> for Game {
    fn from(game: forge_core::Game) -> Self {
        Self {
            id: game.id,
            title: game.title,
            path: game.path,
            added_at: game.added_at,
            last_played_at: game.last_played_at,
            kind: match game.kind {
                forge_core::GameKind::NativeMac => GameKind::NativeMac,
                forge_core::GameKind::WindowsExecutable => GameKind::WindowsExecutable,
            },
        }
    }
}

/// Lifecycle of one storefront transfer, shared by every store's progress record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferState {
    Preparing,
    Verifying,
    Downloading,
    Complete,
    Cancelled,
    Failed,
}

impl From<forge_core::transfer::TransferState> for TransferState {
    fn from(state: forge_core::transfer::TransferState) -> Self {
        use forge_core::transfer::TransferState as State;
        match state {
            State::Preparing => Self::Preparing,
            State::Verifying => Self::Verifying,
            State::Downloading => Self::Downloading,
            State::Complete => Self::Complete,
            State::Cancelled => Self::Cancelled,
            State::Failed => Self::Failed,
        }
    }
}

/// Validated executable, arguments and working directory resolved by a storefront kit.
#[derive(Clone, Debug, uniffi::Record)]
pub struct LaunchTarget {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub is_application_bundle: bool,
}

impl From<forge_core::launch::LaunchTarget> for LaunchTarget {
    fn from(target: forge_core::launch::LaunchTarget) -> Self {
        Self {
            executable: target.executable,
            arguments: target.arguments,
            working_directory: target.working_directory,
            is_application_bundle: target.is_application_bundle,
        }
    }
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum StorefrontId {
    Steam,
    Epic,
    Gog,
    Other,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct StorefrontInfo {
    pub id: StorefrontId,
    pub display_name: String,
    pub available: bool,
}

impl From<forge_core::storefront::StorefrontDescriptor> for StorefrontInfo {
    fn from(storefront: forge_core::storefront::StorefrontDescriptor) -> Self {
        use forge_core::storefront::StorefrontId as Id;
        Self {
            id: match storefront.id {
                Id::Steam => StorefrontId::Steam,
                Id::Epic => StorefrontId::Epic,
                Id::Gog => StorefrontId::Gog,
                Id::Other => StorefrontId::Other,
            },
            display_name: storefront.display_name.to_owned(),
            available: storefront.capabilities.any_available(),
        }
    }
}

#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[uniffi::export]
pub fn storefronts() -> Vec<StorefrontInfo> {
    use forge_core::storefront::{StorefrontCapabilities, StorefrontDescriptor};
    vec![
        steam_kit::descriptor().into(),
        epic_kit::descriptor().into(),
        gog_kit::descriptor().into(),
        StorefrontDescriptor {
            id: forge_core::storefront::StorefrontId::Other,
            display_name: "Autres",
            capabilities: StorefrontCapabilities {
                local_import: true,
                ..Default::default()
            },
        }
        .into(),
    ]
}

#[derive(Debug, uniffi::Object)]
pub struct Library {
    inner: forge_core::Library,
}

#[uniffi::export]
impl Library {
    #[uniffi::constructor]
    pub fn new(storage_dir: String) -> Result<Arc<Self>, CoreError> {
        Ok(Arc::new(Self {
            inner: forge_core::Library::open(storage_dir)?,
        }))
    }

    pub fn games(&self) -> Result<Vec<Game>, CoreError> {
        Ok(self.inner.games()?.into_iter().map(Into::into).collect())
    }

    pub fn add_local_game(&self, path: String) -> Result<Game, CoreError> {
        Ok(self.inner.add_local_game(path)?.into())
    }

    pub fn remove_game(&self, id: String) -> Result<(), CoreError> {
        self.inner.remove_game(&id).map_err(Into::into)
    }

    pub fn record_launch(&self, id: String) -> Result<(), CoreError> {
        self.inner.record_launch(&id).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_catalog_reports_real_availability() {
        let sources = storefronts();
        assert_eq!(sources.len(), 4);
        assert!(sources[0].available);
        assert!(sources[1].available);
        assert!(!sources[2].available);
        assert!(sources[3].available);
        assert!(!core_version().is_empty());
    }

    #[test]
    fn bridge_maps_games_and_errors() {
        let root = tempfile::TempDir::new().unwrap();
        let library =
            Library::new(root.path().join("catalog").to_str().unwrap().to_owned()).unwrap();
        let path = root.path().join("Example.exe");
        std::fs::write(&path, "fixture").unwrap();
        let game = library
            .add_local_game(path.to_str().unwrap().to_owned())
            .unwrap();
        assert!(matches!(game.kind, GameKind::WindowsExecutable));
        assert_eq!(library.games().unwrap()[0].id, game.id);
        assert!(matches!(
            library.remove_game("missing".to_owned()),
            Err(CoreError::NotFound { .. })
        ));
    }
}
