//! A Steam launch receives the session already loaded by SailOver. It never
//! queries Keychain, and cancellation crosses UniFFI through its admission.
use crate::{
    GameAdmission, GameSessionError, SailOverLocations, SteamCredentials, SteamError,
    SteamLaunchPlan, game_session::Admitted,
};
use std::path::PathBuf;

impl From<game_runtime::Error> for SteamError {
    fn from(error: game_runtime::Error) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

/// Launches the game `plan` resolves for its admission, installing Voile first
/// for a Windows game, then follows its session until it is finished.
#[uniffi::export(async_runtime = "tokio")]
pub async fn launch_steam_game(
    admission: std::sync::Arc<GameAdmission>,
    plan: SteamLaunchPlan,
    credentials: SteamCredentials,
    locations: SailOverLocations,
) -> Result<(), GameSessionError> {
    let Admitted::Steam(slot) = &admission.admission else {
        return Err(GameSessionError::new(
            "backend.steam.runtime.invalidSession",
        ));
    };
    let admission = slot.take()?;
    let handoff = game_runtime::steam::hand_off(
        admission.canceller(),
        (&locations).into(),
        game_runtime::protocol::Credentials {
            steam_id: credentials.steam_id,
            account_name: credentials.account_name,
            refresh_token: credentials.refresh_token,
        },
        plan.app_id,
        plan.platform,
        PathBuf::from(plan.install_directory),
        plan.target.into(),
    );
    Ok(admission.launch(handoff).await?)
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

#[uniffi::export(async_runtime = "tokio")]
pub async fn steam_cloud_removed(
    config: SteamCloudConfiguration,
    account_id: u64,
) -> Result<(), SteamError> {
    tokio::task::spawn_blocking(move || {
        game_runtime::steam::cloud::removed(&config.into(), account_id).map_err(Into::into)
    })
    .await
    .map_err(|_| game_runtime::Error::StartFailed)?
}
