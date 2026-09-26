//! What Steam's Game sessions and Cloud share across UniFFI.
use crate::SteamError;

impl From<game_runtime::Error> for SteamError {
    fn from(error: game_runtime::Error) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamHostState {
    Online,
    Disconnected,
    Lost,
    Stopped,
}

impl From<game_runtime::HostState> for SteamHostState {
    fn from(value: game_runtime::HostState) -> Self {
        match value {
            game_runtime::HostState::Online => Self::Online,
            game_runtime::HostState::Disconnected => Self::Disconnected,
            game_runtime::HostState::Lost => Self::Lost,
            game_runtime::HostState::Stopped => Self::Stopped,
        }
    }
}

#[derive(uniffi::Record)]
pub struct SteamCloudConfiguration {
    pub root: String,
    pub home: String,
    pub install: String,
    pub app_id: u32,
    /// The Wine prefix of a Windows installation, where its saves live.
    pub prefix: Option<String>,
}

impl From<SteamCloudConfiguration> for game_runtime::steam::cloud::Config {
    fn from(config: SteamCloudConfiguration) -> Self {
        Self {
            root: config.root.into(),
            home: config.home.into(),
            install: config.install.into(),
            app_id: config.app_id,
            prefix: config.prefix.map(Into::into),
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
