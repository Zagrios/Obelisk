//! A single-use native launch receives the session already loaded by Forge.
//! It never queries Keychain, and cancellation crosses UniFFI explicitly.
use crate::{ForgeLocations, GameSession, SteamCredentials, SteamError, SteamLaunchPlan};
use std::{
    path::PathBuf,
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
        locations: ForgeLocations,
    ) -> Result<(), SteamError> {
        let request = tokio::task::spawn_blocking(move || {
            let install = PathBuf::from(&plan.install_directory);
            let home = PathBuf::from(&locations.home);
            let target = plan.target.into();
            // A Windows installation runs in Wine, in the prefix named after it.
            let wine = match (plan.platform == "windows", install.file_name()) {
                (false, _) => None,
                (true, Some(_)) => Some(game_runtime::wine::Config {
                    runtime: PathBuf::from(&locations.wine_runtime),
                    prefix: crate::locations::prefix(&locations, &plan.install_directory).into(),
                }),
                (true, None) => return Err(game_runtime::Error::NotExecutable),
            };
            let (target, steam_api) = match &wine {
                Some(wine) => {
                    let (mut target, steam_api) =
                        game_runtime::wine::steam_target(&install, &target, &home, wine)?;
                    game_runtime::wine::prepare(wine, &home)?;
                    target.log = Some(
                        PathBuf::from(&locations.steam_diagnostics)
                            .join(format!("wine-{}.log", plan.app_id)),
                    );
                    (target, steam_api)
                }
                None => game_runtime::steam::target(&install, &target, &home)?,
            };
            let cloud = Some(game_runtime::steam::cloud::Config {
                root: locations.cloud_archive.into(),
                home: home.clone(),
                install: install.clone(),
                app_id: plan.app_id,
                prefix: wine.as_ref().map(|wine| wine.prefix.clone()),
            });
            Ok::<_, game_runtime::Error>(game_runtime::steam::Request {
                root: locations.steam_sessions.into(),
                id: String::new(),
                config: game_runtime::Config {
                    app_id: plan.app_id,
                    cloud_save_roots: vec![install.clone()],
                    helpers: locations.helpers.into(),
                    package: locations.steam_runtime_package.into(),
                    cache: locations.steam_runtime_cache.into(),
                    diagnostics: locations.steam_diagnostics.into(),
                    steam_api,
                    home,
                    wine,
                },
                cloud,
                target,
            })
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
