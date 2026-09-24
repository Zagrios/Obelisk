//! A single-use native launch receives the session already loaded by Forge.
//! It never queries Keychain, and cancellation crosses UniFFI explicitly.
use crate::{GameSession, SteamCredentials, SteamError};
use std::sync::Arc;

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
}

#[derive(uniffi::Record)]
pub struct SteamNativeConfiguration {
    pub app_id: u32,
    pub helpers: String,
    pub package: String,
    pub cache: String,
    pub diagnostics: String,
    pub steam_api: String,
    pub home: String,
    pub cloud_directory: String,
    pub install_directory: String,
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
        })
    }
    pub fn cancel(&self) {
        self.launch.cancel();
    }
    pub async fn start_game(
        &self,
        configuration: SteamNativeConfiguration,
        target: SteamSupervisedTarget,
        session_directory: String,
    ) -> Result<Arc<GameSession>, SteamError> {
        let launch = self.launch.clone();
        tokio::task::spawn_blocking(move || {
            let cloud = game_runtime::steam::cloud::Config {
                root: configuration.cloud_directory.clone().into(),
                home: configuration.home.clone().into(),
                install: configuration.install_directory.clone().into(),
                app_id: configuration.app_id,
            };
            let client = launch.start_supervised(game_runtime::steam::Request {
                root: session_directory.into(),
                id: String::new(),
                config: configuration.into(),
                cloud: Some(cloud),
                target: game_runtime::supervisor::Target {
                    executable: target.executable.into(),
                    arguments: target.arguments,
                    working_directory: target.working_directory.into(),
                    environment: target.environment,
                },
            })?;
            Ok(GameSession::steam(client))
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

impl From<SteamNativeConfiguration> for game_runtime::Config {
    fn from(c: SteamNativeConfiguration) -> Self {
        Self {
            app_id: c.app_id,
            cloud_save_roots: vec![c.install_directory.into()],
            helpers: c.helpers.into(),
            package: c.package.into(),
            cache: c.cache.into(),
            diagnostics: c.diagnostics.into(),
            steam_api: c.steam_api.into(),
            home: c.home.into(),
        }
    }
}

#[derive(uniffi::Record)]
pub struct SteamSupervisedTarget {
    pub executable: String,
    pub arguments: Vec<String>,
    pub working_directory: String,
    pub environment: std::collections::HashMap<String, String>,
}

#[derive(uniffi::Record)]
pub struct SteamCloudConfiguration {
    pub root: String,
    pub home: String,
    pub install: String,
    pub app_id: u32,
}

impl From<SteamCloudConfiguration> for game_runtime::steam::cloud::Config {
    fn from(config: SteamCloudConfiguration) -> Self {
        Self {
            root: config.root.into(),
            home: config.home.into(),
            install: config.install.into(),
            app_id: config.app_id,
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
pub async fn steam_cloud_archive(
    config: SteamCloudConfiguration,
    account_id: u64,
) -> Result<(), SteamError> {
    tokio::task::spawn_blocking(move || {
        game_runtime::steam::cloud::archive(&config.into(), account_id).map_err(Into::into)
    })
    .await
    .map_err(|_| game_runtime::Error::StartFailed)?
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
