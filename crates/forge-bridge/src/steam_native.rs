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
            let client = launch.start_supervised(steam_runtime::steam::Request {
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

/// Why a terminal session failed. Swift treats a cancellation as silent.
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum SteamGameFailureReason {
    Cancelled,
    Failed,
}
impl From<steam_runtime::supervisor::FailureReason> for SteamGameFailureReason {
    fn from(reason: steam_runtime::supervisor::FailureReason) -> Self {
        match reason {
            steam_runtime::supervisor::FailureReason::Cancelled => Self::Cancelled,
            steam_runtime::supervisor::FailureReason::Failed => Self::Failed,
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
    pub cloud_allowed_choices: Vec<SteamCloudChoice>,
    pub connection: SteamSupervisorConnection,
    pub host: SteamHostState,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub revision: u64,
    pub rosetta: Option<RosettaRequest>,
    /// Defaulted so that records built before it existed still compile.
    #[uniffi(default)]
    pub failure_reason: Option<SteamGameFailureReason>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct RosettaRequest {
    pub decision_id: u64,
}
impl From<steam_runtime::supervisor::RosettaRequest> for RosettaRequest {
    fn from(request: steam_runtime::supervisor::RosettaRequest) -> Self {
        Self {
            decision_id: request.decision_id,
        }
    }
}

impl From<steam_runtime::steam::Snapshot> for SteamGameSnapshot {
    fn from(s: steam_runtime::steam::Snapshot) -> Self {
        let failure_reason = s.failure_reason().map(Into::into);
        Self {
            failure_reason,
            phase: s.phase.into(),
            connection: if s.reconnecting {
                SteamSupervisorConnection::Reconnecting
            } else {
                SteamSupervisorConnection::Connected
            },
            id: s.id,
            app_id: s.store.app_id,
            account_id: s.account_id,
            cloud_phase: s.cloud.phase.into(),
            cloud_decision_id: s.cloud.id,
            cloud_error: s.cloud.error,
            cloud_allowed_choices: s
                .cloud
                .allowed_choices
                .into_iter()
                .map(Into::into)
                .collect(),
            host: s.store.host.into(),
            exit_code: s.exit_code,
            error: s.error,
            revision: s.revision,
            rosetta: s.rosetta.map(Into::into),
        }
    }
}

#[derive(uniffi::Object)]
pub struct SteamGameSession {
    client: Arc<steam_runtime::steam::Client>,
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
        self.client.cancel();
    }

    pub async fn resume_after_rosetta(
        &self,
        account_id: u64,
        decision_id: u64,
    ) -> Result<(), SteamError> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || {
            client
                .resume_after_rosetta(&account_id, decision_id)
                .map_err(Into::into)
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
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
                .choose(&account_id, decision_id, choice.into())
                .map_err(Into::into)
        })
        .await
        .map_err(|_| steam_runtime::Error::StartFailed)?
    }

    pub async fn revoke_cloud(&self, account_id: u64) -> Result<(), SteamError> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || client.revoke(&account_id).map_err(Into::into))
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
        Ok(steam_runtime::steam::Client::recover(
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn failed(error: &str) -> Value {
        json!({
            "version": 2,
            "id": "fixture-session",
            "account_id": 42,
            "store": {"app_id": 620, "host": "stopped"},
            "phase": "Failed",
            "cloud": {"phase": "Idle", "id": 0, "error": null, "allowed_choices": []},
            "exit_code": null,
            "error": error,
            "revision": 2,
            "supervisor": {
                "pid": 42, "uid": 501, "birth_seconds": 1, "birth_microseconds": 2,
                "executable": "/fixture/forge-steam-host"
            },
            "game": null,
            "socket": "/fixture/control"
        })
    }

    fn bridged(value: Value) -> SteamGameSnapshot {
        serde_json::from_value::<steam_runtime::steam::Snapshot>(value)
            .unwrap()
            .into()
    }

    #[test]
    fn a_steam_cancellation_reaches_swift_typed_independently_of_its_message() {
        let mut value = failed("A renamed cancellation message");
        value["failure_reason"] = json!("Cancelled");
        assert!(matches!(
            bridged(value).failure_reason,
            Some(SteamGameFailureReason::Cancelled)
        ));
        // Records without a typed reason classify only the cancellation key.
        assert!(matches!(
            bridged(failed("backend.steam.runtime.cancelled")).failure_reason,
            Some(SteamGameFailureReason::Cancelled)
        ));
        assert!(matches!(
            bridged(failed("backend.steam.runtime.startFailed")).failure_reason,
            Some(SteamGameFailureReason::Failed)
        ));
        let mut running = failed("backend.steam.runtime.cancelled");
        running["phase"] = json!("Running");
        assert!(bridged(running).failure_reason.is_none());
    }
}
