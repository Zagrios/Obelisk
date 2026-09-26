//! One Game session across UniFFI, whatever the storefront. The supervisor
//! decides every fact here, including when the session is finished; Swift
//! observes it and sends answers bound to its account and decisions.
use crate::SteamHostState;
use game_runtime::{cloud, epic, steam, supervisor};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GamePhase {
    Preparing,
    Running,
    Finalizing,
    Finished,
    Failed,
}
impl From<supervisor::Phase> for GamePhase {
    fn from(phase: supervisor::Phase) -> Self {
        match phase {
            supervisor::Phase::Preparing => Self::Preparing,
            supervisor::Phase::Running => Self::Running,
            supervisor::Phase::Finalizing => Self::Finalizing,
            supervisor::Phase::Finished => Self::Finished,
            supervisor::Phase::Failed => Self::Failed,
        }
    }
}

/// Why a finished session failed. Swift ends a cancellation silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameFailureReason {
    Cancelled,
    Failed,
}
impl From<supervisor::FailureReason> for GameFailureReason {
    fn from(reason: supervisor::FailureReason) -> Self {
        match reason {
            supervisor::FailureReason::Cancelled => Self::Cancelled,
            supervisor::FailureReason::Failed => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudPhase {
    Idle,
    Checking,
    Ready,
    Syncing,
    Synced,
    Conflict,
    Pending,
    Disabled,
    Unsupported,
    Failed,
}
impl From<cloud::Phase> for CloudPhase {
    fn from(phase: cloud::Phase) -> Self {
        match phase {
            cloud::Phase::Idle => Self::Idle,
            cloud::Phase::Checking => Self::Checking,
            cloud::Phase::Ready => Self::Ready,
            cloud::Phase::Syncing => Self::Syncing,
            cloud::Phase::Synced => Self::Synced,
            cloud::Phase::Conflict => Self::Conflict,
            cloud::Phase::Pending => Self::Pending,
            cloud::Phase::Disabled => Self::Disabled,
            cloud::Phase::Unsupported => Self::Unsupported,
            cloud::Phase::Failed => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudChoice {
    Retry,
    Cancel,
    LaunchAnyway,
    Local,
    Cloud,
}
impl From<cloud::Choice> for CloudChoice {
    fn from(choice: cloud::Choice) -> Self {
        match choice {
            cloud::Choice::Retry => Self::Retry,
            cloud::Choice::Cancel => Self::Cancel,
            cloud::Choice::LaunchAnyway => Self::LaunchAnyway,
            cloud::Choice::Local => Self::Local,
            cloud::Choice::Cloud => Self::Cloud,
        }
    }
}
impl From<CloudChoice> for cloud::Choice {
    fn from(choice: CloudChoice) -> Self {
        match choice {
            CloudChoice::Retry => Self::Retry,
            CloudChoice::Cancel => Self::Cancel,
            CloudChoice::LaunchAnyway => Self::LaunchAnyway,
            CloudChoice::Local => Self::Local,
            CloudChoice::Cloud => Self::Cloud,
        }
    }
}

/// A Cloud sync's progress and the Cloud question it waits on, if any.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudStatus {
    pub phase: CloudPhase,
    /// Binds an answer to the question this status shows.
    pub decision_id: u64,
    /// Localization key of the failure the status reports.
    pub error: Option<String>,
    /// Empty unless a question waits.
    pub allowed_choices: Vec<CloudChoice>,
}
impl From<cloud::Status> for CloudStatus {
    fn from(status: cloud::Status) -> Self {
        Self {
            phase: status.phase.into(),
            decision_id: status.decision_id,
            error: status.error,
            allowed_choices: status.allowed_choices.into_iter().map(Into::into).collect(),
        }
    }
}

/// What the storefront's supervisor records about the game.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameStore {
    Steam {
        app_id: u32,
        account_id: u64,
        /// The native host that owns the game's Steam session.
        host: SteamHostState,
    },
    Epic {
        account_id: String,
        namespace: String,
        app_name: String,
    },
}

/// The account an answer or a revocation names.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameAccount {
    Steam { steam_id: u64 },
    Epic { account_id: String },
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct RosettaRequest {
    pub decision_id: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GameSnapshot {
    pub id: String,
    pub phase: GamePhase,
    /// The phase is final and the session owns no game or starting process:
    /// only then is the Reservation released.
    pub finished: bool,
    pub failure_reason: Option<GameFailureReason>,
    pub exit_code: Option<i32>,
    /// Localization key of the session's own failure.
    pub error: Option<String>,
    pub revision: u64,
    /// The control connection was lost; the game may still run.
    pub reconnecting: bool,
    pub rosetta: Option<RosettaRequest>,
    pub cloud: CloudStatus,
    pub store: GameStore,
}
impl<P: supervisor::Provider> From<supervisor::Record<P>> for GameSnapshot
where
    GameStore: From<(P::Account, P::Store)>,
{
    fn from(record: supervisor::Record<P>) -> Self {
        Self {
            finished: record.finished(),
            failure_reason: record.failure_reason().map(Into::into),
            id: record.id,
            phase: record.phase.into(),
            exit_code: record.exit_code,
            error: record.error,
            revision: record.revision,
            reconnecting: record.reconnecting,
            rosetta: record.rosetta.map(|request| RosettaRequest {
                decision_id: request.decision_id,
            }),
            cloud: record.cloud.into(),
            store: (record.account_id, record.store).into(),
        }
    }
}
impl From<(u64, steam::Store)> for GameStore {
    fn from((account_id, store): (u64, steam::Store)) -> Self {
        Self::Steam {
            app_id: store.app_id,
            account_id,
            host: store.host.into(),
        }
    }
}
impl From<(String, epic::Store)> for GameStore {
    fn from((account_id, store): (String, epic::Store)) -> Self {
        Self::Epic {
            account_id,
            namespace: store.namespace,
            app_name: store.app_name,
        }
    }
}

/// A refused or failed session call, as a localization key.
#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum GameSessionError {
    #[error("{reason}")]
    Failure { reason: String },
}
impl GameSessionError {
    fn new(error: impl std::fmt::Display) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

enum Supervised {
    Steam(steam::Client),
    Epic(epic::Client),
}

/// One Game session's supervisor, for either storefront.
#[derive(uniffi::Object)]
pub struct GameSession {
    supervised: Arc<Supervised>,
}
impl GameSession {
    pub(crate) fn steam(client: steam::Client) -> Arc<Self> {
        Arc::new(Self {
            supervised: Arc::new(Supervised::Steam(client)),
        })
    }
    pub(crate) fn epic(client: epic::Client) -> Arc<Self> {
        Arc::new(Self {
            supervised: Arc::new(Supervised::Epic(client)),
        })
    }

    /// Every supervisor call parks on IPC, so it runs off the async runtime.
    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Supervised) -> Result<T, GameSessionError> + Send + 'static,
    ) -> Result<T, GameSessionError> {
        let supervised = Arc::clone(&self.supervised);
        tokio::task::spawn_blocking(move || work(&supervised))
            .await
            .map_err(|_| GameSessionError::new("backend.steam.runtime.startFailed"))?
    }
}

fn refused() -> GameSessionError {
    GameSessionError::new("backend.steam.runtime.invalidSession")
}

#[uniffi::export(async_runtime = "tokio")]
impl GameSession {
    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn snapshot(&self, after: Option<u64>) -> Result<GameSnapshot, GameSessionError> {
        self.blocking(move |supervised| match supervised {
            Supervised::Steam(client) => client
                .snapshot(after)
                .map(Into::into)
                .map_err(GameSessionError::new),
            Supervised::Epic(client) => client
                .snapshot(after)
                .map(Into::into)
                .map_err(GameSessionError::new),
        })
        .await
    }

    /// Cancels preparation. After spawn the game keeps running.
    pub fn cancel_preparation(&self) {
        match &*self.supervised {
            Supervised::Steam(client) => client.cancel(),
            Supervised::Epic(client) => client.cancel(),
        }
    }

    /// Opens the start gate of a supervisor that waits for its UI. Only Epic
    /// launches wait; a Steam session has nothing to begin.
    pub async fn begin(&self) -> Result<(), GameSessionError> {
        self.blocking(|supervised| match supervised {
            Supervised::Steam(_) => Ok(()),
            Supervised::Epic(client) => client.begin().map_err(GameSessionError::new),
        })
        .await
    }

    pub async fn choose(
        &self,
        account: GameAccount,
        decision_id: u64,
        choice: CloudChoice,
    ) -> Result<(), GameSessionError> {
        self.blocking(move |supervised| match (supervised, account) {
            (Supervised::Steam(client), GameAccount::Steam { steam_id }) => client
                .choose(&steam_id, decision_id, choice.into())
                .map_err(GameSessionError::new),
            (Supervised::Epic(client), GameAccount::Epic { account_id }) => client
                .choose(&account_id, decision_id, choice.into())
                .map_err(GameSessionError::new),
            _ => Err(refused()),
        })
        .await
    }

    pub async fn resume_after_rosetta(
        &self,
        account: GameAccount,
        decision_id: u64,
    ) -> Result<(), GameSessionError> {
        self.blocking(move |supervised| match (supervised, account) {
            (Supervised::Steam(client), GameAccount::Steam { steam_id }) => client
                .resume_after_rosetta(&steam_id, decision_id)
                .map_err(GameSessionError::new),
            (Supervised::Epic(client), GameAccount::Epic { account_id }) => client
                .resume_after_rosetta(&account_id, decision_id)
                .map_err(GameSessionError::new),
            _ => Err(refused()),
        })
        .await
    }

    /// Account revocation: network work stops, the game keeps running.
    pub async fn revoke(&self, account: GameAccount) -> Result<(), GameSessionError> {
        self.blocking(move |supervised| match (supervised, account) {
            (Supervised::Steam(client), GameAccount::Steam { steam_id }) => {
                client.revoke(&steam_id).map_err(GameSessionError::new)
            }
            (Supervised::Epic(client), GameAccount::Epic { account_id }) => {
                client.revoke(&account_id).map_err(GameSessionError::new)
            }
            _ => Err(refused()),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn steam_record(error: &str) -> Value {
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
                "executable": "/fixture/sailover-steam-host"
            },
            "game": null,
            "socket": "/fixture/control"
        })
    }

    fn steam(value: Value) -> GameSnapshot {
        serde_json::from_value::<steam::Snapshot>(value)
            .unwrap()
            .into()
    }

    #[test]
    fn a_cancellation_reaches_swift_typed_independently_of_its_message() {
        let mut value = steam_record("A renamed cancellation message");
        value["failure_reason"] = json!("Cancelled");
        assert_eq!(
            steam(value).failure_reason,
            Some(GameFailureReason::Cancelled)
        );
        // Records without a typed reason classify only the cancellation key.
        assert_eq!(
            steam(steam_record("backend.steam.runtime.cancelled")).failure_reason,
            Some(GameFailureReason::Cancelled)
        );
        assert_eq!(
            steam(steam_record("backend.steam.runtime.startFailed")).failure_reason,
            Some(GameFailureReason::Failed)
        );
        let mut running = steam_record("backend.steam.runtime.cancelled");
        running["phase"] = json!("Running");
        assert!(steam(running).failure_reason.is_none());
    }

    #[test]
    fn a_session_is_finished_only_once_it_owns_no_process() {
        let finished = steam(steam_record("backend.steam.runtime.startFailed"));
        assert!(finished.finished);
        let mut owning = steam_record("backend.steam.runtime.startFailed");
        owning["game"] = json!({
            "pid": 43, "uid": 501, "birth_seconds": 1, "birth_microseconds": 2,
            "executable": "/fixture/game"
        });
        let owning = steam(owning);
        assert_eq!(owning.phase, GamePhase::Failed);
        assert!(!owning.finished, "the Reservation outlives a final phase");
    }

    #[test]
    fn both_storefronts_report_the_same_cloud_status() {
        let mut value = steam_record("backend.steam.runtime.startFailed");
        value["cloud"] = json!({
            "phase": "Conflict", "id": 7, "error": "backend.steam.cloud.conflict",
            "allowed_choices": ["Local", "Cloud", "Cancel"],
        });
        let snapshot = steam(value);
        assert_eq!(
            snapshot.cloud,
            CloudStatus {
                phase: CloudPhase::Conflict,
                decision_id: 7,
                error: Some("backend.steam.cloud.conflict".into()),
                allowed_choices: vec![CloudChoice::Local, CloudChoice::Cloud, CloudChoice::Cancel],
            }
        );
        assert_eq!(
            snapshot.store,
            GameStore::Steam {
                app_id: 620,
                account_id: 42,
                host: SteamHostState::Stopped,
            }
        );
    }
}
