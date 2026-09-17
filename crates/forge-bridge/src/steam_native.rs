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
    pub async fn start_game(
        &self,
        configuration: SteamNativeConfiguration,
        target: SteamSupervisedTarget,
        session_directory: String,
    ) -> Result<Arc<SteamGameSession>, SteamError> {
        let launch = self.launch.clone();
        tokio::task::spawn_blocking(move || {
            let client = launch.start_supervised(steam_runtime::supervisor::Request {
                root: session_directory.into(),
                id: String::new(),
                config: configuration.into(),
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
    Finished,
    Failed,
}
impl From<steam_runtime::supervisor::Phase> for SteamGamePhase {
    fn from(phase: steam_runtime::supervisor::Phase) -> Self {
        use steam_runtime::supervisor::Phase;
        match phase {
            Phase::Preparing => Self::Preparing,
            Phase::Running => Self::Running,
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
    pub phase: SteamGamePhase,
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
