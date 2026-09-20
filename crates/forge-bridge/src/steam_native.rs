//! A single-use native launch receives the session already loaded by Forge.
//! It never queries Keychain, and cancellation crosses UniFFI explicitly.
use crate::{SteamCredentials, SteamError};
use std::sync::Arc;

impl From<steam_runtime::Error> for SteamError {
    fn from(error: steam_runtime::Error) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

#[derive(uniffi::Object)]
pub struct SteamNativeLaunch {
    launch: Arc<steam_runtime::Launch>,
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
            launch: Arc::new(steam_runtime::Launch::new(
                steam_runtime::protocol::Credentials {
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
    #[cfg(debug_assertions)]
    pub async fn probe_storage(
        &self,
        configuration: SteamNativeConfiguration,
        transfers: bool,
    ) -> Result<String, SteamError> {
        let launch = self.launch.clone();
        tokio::task::spawn_blocking(move || {
            launch
                .probe_storage(configuration.into(), transfers)
                .map_err(Into::into)
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
    }
    pub async fn start_game(
        &self,
        configuration: SteamNativeConfiguration,
        target: SteamSupervisedTarget,
        session_directory: String,
    ) -> Result<Arc<SteamGameSession>, SteamError> {
        let launch = self.launch.clone();
        tokio::task::spawn_blocking(move || {
            let cloud = steam_runtime::cloud::Config {
                root: configuration.cloud_directory.clone().into(),
                home: configuration.home.clone().into(),
                install: configuration.install_directory.clone().into(),
                app_id: configuration.app_id,
            };
            let client = launch.start_supervised(steam_runtime::supervisor::Request {
                root: session_directory.into(),
                id: String::new(),
                config: configuration.into(),
                cloud: Some(cloud),
                target: steam_runtime::supervisor::Target {
                    executable: target.executable.into(),
                    arguments: target.arguments,
                    working_directory: target.working_directory.into(),
                    environment: target.environment,
                },
            })?;
            Ok(Arc::new(SteamGameSession {
                client: Arc::new(client),
            }))
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
    }
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum SteamHostState {
    Online,
    Disconnected,
    Lost,
    Stopped,
}

impl From<steam_runtime::HostState> for SteamHostState {
    fn from(value: steam_runtime::HostState) -> Self {
        match value {
            steam_runtime::HostState::Online => Self::Online,
            steam_runtime::HostState::Disconnected => Self::Disconnected,
            steam_runtime::HostState::Lost => Self::Lost,
            steam_runtime::HostState::Stopped => Self::Stopped,
        }
    }
}

impl From<SteamNativeConfiguration> for steam_runtime::Config {
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

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum SteamGamePhase {
    Preparing,
    Running,
    Finalizing,
    Finished,
    Failed,
}
impl From<steam_runtime::supervisor::Phase> for SteamGamePhase {
    fn from(phase: steam_runtime::supervisor::Phase) -> Self {
        use steam_runtime::supervisor::Phase;
        match phase {
            Phase::Preparing => Self::Preparing,
            Phase::Running => Self::Running,
            Phase::Finalizing => Self::Finalizing,
            Phase::Finished => Self::Finished,
            Phase::Failed => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum SteamSupervisorConnection {
    Connected,
    Reconnecting,
}

#[derive(uniffi::Record)]
pub struct SteamGameSnapshot {
    pub id: String,
    pub app_id: u32,
    pub account_id: u64,
    pub phase: SteamGamePhase,
    pub cloud_phase: SteamCloudPhase,
    pub cloud_decision_id: u64,
    pub cloud_error: Option<String>,
    pub cloud_decision_token: Option<String>,
    pub cloud_allowed_choices: Vec<SteamCloudChoice>,
    pub connection: SteamSupervisorConnection,
    pub host: SteamHostState,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub revision: u64,
}

impl From<steam_runtime::supervisor::Snapshot> for SteamGameSnapshot {
    fn from(s: steam_runtime::supervisor::Snapshot) -> Self {
        Self {
            phase: s.phase.into(),
            connection: if s.reconnecting {
                SteamSupervisorConnection::Reconnecting
            } else {
                SteamSupervisorConnection::Connected
            },
            id: s.id,
            app_id: s.app_id,
            account_id: s.account_id,
            cloud_phase: s.cloud.phase.into(),
            cloud_decision_id: s.cloud.id,
            cloud_error: s.cloud.error,
            cloud_decision_token: s.cloud.decision_token,
            cloud_allowed_choices: s
                .cloud
                .allowed_choices
                .into_iter()
                .map(Into::into)
                .collect(),
            host: s.host.into(),
            exit_code: s.exit_code,
            error: s.error,
            revision: s.revision,
        }
    }
}

#[derive(uniffi::Object)]
pub struct SteamGameSession {
    client: Arc<steam_runtime::supervisor::Client>,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamGameSession {
    pub async fn snapshot(&self, after: Option<u64>) -> Result<SteamGameSnapshot, SteamError> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || {
            client.snapshot(after).map(Into::into).map_err(Into::into)
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
    }
    pub fn cancel_preparation(&self) {
        self.client.cancel_preparation();
    }

    pub async fn choose_cloud(
        &self,
        account_id: u64,
        decision_id: u64,
        choice: SteamCloudChoice,
    ) -> Result<(), SteamError> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || {
            client
                .cloud_choice(account_id, decision_id, choice.into())
                .map_err(Into::into)
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
    }

    pub async fn revoke_cloud(&self, account_id: u64) -> Result<(), SteamError> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || client.revoke_cloud(account_id).map_err(Into::into))
            .await
            .map_err(|_| steam_runtime::Error::StartFailed)?
    }
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum SteamCloudPhase {
    Idle,
    Checking,
    Conflict,
    Failed,
    Disabled,
    Ready,
    Syncing,
    Pending,
    Synced,
}

impl From<steam_runtime::cloud::Phase> for SteamCloudPhase {
    fn from(phase: steam_runtime::cloud::Phase) -> Self {
        use steam_runtime::cloud::Phase;
        match phase {
            Phase::Idle => Self::Idle,
            Phase::Checking => Self::Checking,
            Phase::Conflict => Self::Conflict,
            Phase::Failed => Self::Failed,
            Phase::Disabled => Self::Disabled,
            Phase::Ready => Self::Ready,
            Phase::Syncing => Self::Syncing,
            Phase::Pending => Self::Pending,
            Phase::Synced => Self::Synced,
        }
    }
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum SteamCloudChoice {
    Retry,
    Cancel,
    LaunchAnyway,
    Local,
    Cloud,
}

impl From<SteamCloudChoice> for steam_runtime::cloud::Choice {
    fn from(choice: SteamCloudChoice) -> Self {
        match choice {
            SteamCloudChoice::Retry => Self::Retry,
            SteamCloudChoice::Cancel => Self::Cancel,
            SteamCloudChoice::LaunchAnyway => Self::LaunchAnyway,
            SteamCloudChoice::Local => Self::Local,
            SteamCloudChoice::Cloud => Self::Cloud,
        }
    }
}

impl From<steam_runtime::cloud::Choice> for SteamCloudChoice {
    fn from(choice: steam_runtime::cloud::Choice) -> Self {
        use steam_runtime::cloud::Choice;
        match choice {
            Choice::Retry => Self::Retry,
            Choice::Cancel => Self::Cancel,
            Choice::LaunchAnyway => Self::LaunchAnyway,
            Choice::Local => Self::Local,
            Choice::Cloud => Self::Cloud,
        }
    }
}

#[derive(uniffi::Record)]
pub struct SteamCloudConfiguration {
    pub root: String,
    pub home: String,
    pub install: String,
    pub app_id: u32,
}

impl From<SteamCloudConfiguration> for steam_runtime::cloud::Config {
    fn from(config: SteamCloudConfiguration) -> Self {
        Self {
            root: config.root.into(),
            home: config.home.into(),
            install: config.install.into(),
            app_id: config.app_id,
        }
    }
}

#[derive(uniffi::Record)]
pub struct SteamCloudPending {
    pub app_id: u32,
    pub phase: SteamCloudPhase,
    pub error: Option<String>,
    pub decision_token: String,
}

/// One authenticated account's bounded maintenance. Cancellation revokes every
/// future network step; dropping a UI observer never revokes a game supervisor.
#[derive(uniffi::Object)]
pub struct SteamCloudMaintenance {
    credentials: steam_runtime::protocol::Credentials,
    control: Arc<steam_runtime::cloud::Control>,
    operation: std::sync::Mutex<()>,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamCloudMaintenance {
    #[uniffi::constructor]
    pub fn new(credentials: SteamCredentials) -> Arc<Self> {
        Arc::new(Self {
            control: Arc::new(steam_runtime::cloud::Control::new(
                credentials.steam_id,
                |_| {},
            )),
            credentials: steam_runtime::protocol::Credentials {
                steam_id: credentials.steam_id,
                account_name: credentials.account_name,
                refresh_token: credentials.refresh_token,
            },
            operation: std::sync::Mutex::new(()),
        })
    }
    pub fn cancel(&self) {
        self.control.cancel();
    }
    pub async fn resume(
        self: Arc<Self>,
        root: String,
        session_directory: String,
        helper: String,
        excluded_app_ids: Vec<u32>,
    ) -> Result<Vec<SteamCloudPending>, SteamError> {
        tokio::task::spawn_blocking(move || {
            let _operation = self
                .operation
                .try_lock()
                .map_err(|_| steam_runtime::Error::SessionBusy)?;
            let idle = steam_runtime::supervisor::Client::idle_guard(
                std::path::Path::new(&session_directory),
                std::path::Path::new(&helper),
            )?;
            Ok(steam_runtime::cloud::resume(
                std::path::Path::new(&root),
                &self.credentials,
                self.control.clone(),
                &idle,
                &excluded_app_ids,
            )?
            .into_iter()
            .map(|(app_id, status, decision_token)| SteamCloudPending {
                app_id,
                phase: status.phase.into(),
                error: status.error,
                decision_token,
            })
            .collect())
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
    }
    pub async fn resolve(
        self: Arc<Self>,
        root: String,
        app_id: u32,
        choice: SteamCloudChoice,
        decision_token: String,
    ) -> Result<SteamCloudPending, SteamError> {
        tokio::task::spawn_blocking(move || {
            let _operation = self
                .operation
                .try_lock()
                .map_err(|_| steam_runtime::Error::SessionBusy)?;
            let (status, decision_token) = steam_runtime::cloud::resolve(
                std::path::Path::new(&root),
                app_id,
                &self.credentials,
                self.control.clone(),
                choice.into(),
                &decision_token,
            )?;
            Ok(SteamCloudPending {
                app_id,
                phase: status.phase.into(),
                error: status.error,
                decision_token,
            })
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
    }
}

impl From<steam_runtime::cloud::Failure> for SteamError {
    fn from(failure: steam_runtime::cloud::Failure) -> Self {
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
        steam_runtime::cloud::archive(&config.into(), account_id).map_err(Into::into)
    })
    .await
    .map_err(|_| steam_runtime::Error::StartFailed)?
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn steam_cloud_removed(
    config: SteamCloudConfiguration,
    account_id: u64,
) -> Result<(), SteamError> {
    tokio::task::spawn_blocking(move || {
        steam_runtime::cloud::removed(&config.into(), account_id).map_err(Into::into)
    })
    .await
    .map_err(|_| steam_runtime::Error::StartFailed)?
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn recover_steam_game_session(
    session_directory: String,
    helper: String,
) -> Result<Option<Arc<SteamGameSession>>, SteamError> {
    tokio::task::spawn_blocking(move || {
        Ok(steam_runtime::supervisor::Client::recover(
            std::path::Path::new(&session_directory),
            std::path::Path::new(&helper),
        )?
        .map(|client| {
            Arc::new(SteamGameSession {
                client: Arc::new(client),
            })
        }))
    })
    .await
    .map_err(|_| steam_runtime::Error::StartFailed)?
}
