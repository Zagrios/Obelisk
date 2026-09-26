//! A single-use native launch receives the session already loaded by SailOver.
//! It never queries Keychain, and cancellation crosses UniFFI explicitly.
use crate::{GameSession, SailOverLocations, SteamCredentials, SteamError, SteamLaunchPlan};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

impl From<game_runtime::Error> for SteamError {
    fn from(error: game_runtime::Error) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct SteamNativeLaunch {
    launch: Arc<game_runtime::Launch>,
    prepared: Mutex<Option<game_runtime::steam::Request>>,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamNativeLaunch {
    #[uniffi::constructor]
    pub fn new(credentials: SteamCredentials) -> Arc<Self> {
        Arc::new(Self {
            launch: Arc::new(game_runtime::Launch::new(
                game_runtime::protocol::Credentials {
                    steam_id: credentials.steam_id,
                    account_name: credentials.account_name,
                    refresh_token: credentials.refresh_token,
                },
            )),
            prepared: Mutex::new(None),
        })
    }
    pub fn cancel(&self) {
        self.launch.cancel();
    }
    /// Resolves the game inside its installation. A failure here is definite:
    /// no supervisor exists yet.
    pub async fn prepare(
        &self,
        plan: SteamLaunchPlan,
        locations: SailOverLocations,
    ) -> Result<(), SteamError> {
        let request = tokio::task::spawn_blocking(move || {
            game_runtime::steam::prepare(
                &(&locations).into(),
                plan.app_id,
                &plan.platform,
                Path::new(&plan.install_directory),
                &plan.target.into(),
            )
        })
        .await
        .map_err(|_| game_runtime::Error::StartFailed)??;
        *self
            .prepared
            .lock()
            .map_err(|_| game_runtime::Error::StartFailed)? = Some(request);
        Ok(())
    }
    /// Hands the prepared game to its supervisor.
    pub async fn start_game(&self) -> Result<Arc<GameSession>, SteamError> {
        let request = self
            .prepared
            .lock()
            .map_err(|_| game_runtime::Error::StartFailed)?
            .take()
            .ok_or(game_runtime::Error::InvalidSession)?;
        let launch = self.launch.clone();
        tokio::task::spawn_blocking(move || {
            Ok(GameSession::steam(launch.start_supervised(request)?))
        })
        .await
        .map_err(|_| game_runtime::Error::StartFailed)?
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

#[uniffi::export(async_runtime = "tokio")]
pub async fn recover_steam_game_session(
    session_directory: String,
    helper: String,
) -> Result<Option<Arc<GameSession>>, SteamError> {
    tokio::task::spawn_blocking(move || {
        Ok(game_runtime::steam::Client::recover(
            std::path::Path::new(&session_directory),
            std::path::Path::new(&helper),
        )?
        .map(GameSession::steam))
    })
    .await
    .map_err(|_| game_runtime::Error::StartFailed)?
}
