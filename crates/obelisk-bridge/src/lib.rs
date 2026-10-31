//! The complete Swift/Rust seam. Swift owns UI state and OS presentation.
//! Domain code and storefront kits have no dependency on UniFFI.

mod steam;
pub use steam::*;
mod steam_chat;
pub use steam_chat::*;
mod steam_friends;
pub use steam_friends::*;
mod steam_native;
pub use steam_native::*;
mod epic;
pub use epic::*;
mod credentials;
mod epic_cloud;
pub use credentials::CredentialSave;
mod epic_friends;
pub use epic_friends::*;
mod gog;
pub use gog::*;
mod cloud;
pub use cloud::*;
mod game_session;
pub use game_session::*;
mod locations;
pub use locations::*;
mod diagnostics;
pub use diagnostics::*;
mod game_notes;
pub use game_notes::*;
mod installations;
pub use installations::*;
mod games_folder;
pub use games_folder::{GamesFolder, GamesFolderInspection, GamesFolderState};
mod launch_settings;
pub use launch_settings::*;
mod downloads;
pub use downloads::*;
mod library_cache;
pub use library_cache::{CachedEpicGame, CachedGogGame, CachedSteamGame, GameLaunch, LibraryCache};

uniffi::setup_scaffolding!();

/// Lifecycle of one storefront transfer, shared by every store's progress record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferState {
    Preparing,
    Verifying,
    Downloading,
    /// The installation moves into the games folder.
    Moving,
    Complete,
    Cancelled,
    Failed,
}

impl From<obelisk_core::transfer::TransferState> for TransferState {
    fn from(state: obelisk_core::transfer::TransferState) -> Self {
        use obelisk_core::transfer::TransferState as State;
        match state {
            State::Preparing => Self::Preparing,
            State::Verifying => Self::Verifying,
            State::Downloading => Self::Downloading,
            State::Moving => Self::Moving,
            State::Complete => Self::Complete,
            State::Cancelled => Self::Cancelled,
            State::Failed => Self::Failed,
        }
    }
}

/// One installation Transfer's progress, the same record for every storefront.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GameTransfer {
    /// The storefront's own identity for the game: Steam's app ID, Epic's app name.
    pub game: String,
    pub state: TransferState,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub network_bytes: u64,
    pub content_bytes: u64,
    pub current_file: String,
    pub message: String,
}

impl From<TransferState> for obelisk_core::transfer::TransferState {
    fn from(state: TransferState) -> Self {
        match state {
            TransferState::Preparing => Self::Preparing,
            TransferState::Verifying => Self::Verifying,
            TransferState::Downloading => Self::Downloading,
            TransferState::Moving => Self::Moving,
            TransferState::Complete => Self::Complete,
            TransferState::Cancelled => Self::Cancelled,
            TransferState::Failed => Self::Failed,
        }
    }
}

impl From<GameTransfer> for obelisk_core::installation::TransferProgress {
    fn from(transfer: GameTransfer) -> Self {
        Self {
            game: transfer.game,
            state: transfer.state.into(),
            completed_bytes: transfer.completed_bytes,
            total_bytes: transfer.total_bytes,
            network_bytes: transfer.network_bytes,
            content_bytes: transfer.content_bytes,
            current_file: transfer.current_file,
            message: transfer.message,
        }
    }
}

impl From<obelisk_core::installation::TransferProgress> for GameTransfer {
    fn from(progress: obelisk_core::installation::TransferProgress) -> Self {
        Self {
            game: progress.game,
            state: progress.state.into(),
            completed_bytes: progress.completed_bytes,
            total_bytes: progress.total_bytes,
            network_bytes: progress.network_bytes,
            content_bytes: progress.content_bytes,
            current_file: progress.current_file,
            message: progress.message,
        }
    }
}
