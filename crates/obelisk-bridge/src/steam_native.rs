//! What Steam's Game sessions and Cloud share across UniFFI.
use crate::SteamError;
use game_runtime::steam::host::HostState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamHostState {
    Online,
    Disconnected,
    Lost,
    Stopped,
}

impl From<HostState> for SteamHostState {
    fn from(value: HostState) -> Self {
        match value {
            HostState::Online => Self::Online,
            HostState::Disconnected => Self::Disconnected,
            HostState::Lost => Self::Lost,
            HostState::Stopped => Self::Stopped,
        }
    }
}

impl From<game_runtime::steam::cloud::Failure> for SteamError {
    fn from(failure: game_runtime::steam::cloud::Failure) -> Self {
        Self::Failure {
            reason: failure.key().into(),
        }
    }
}
