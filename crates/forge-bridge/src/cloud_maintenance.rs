//! Cloud indicators and Cloud maintenance across UniFFI. Maintenance is one
//! player-requested sync of a game, run inside Forge. UniFFI does not forward
//! Swift task cancellation, so an attempt has an explicit revisioned status,
//! answer and cancel.
use crate::{EpicCloudConfiguration, EpicError, EpicSession, SteamCredentials, SteamError};
use game_runtime::{cloud, cloud_status, epic, protocol::Credentials, steam};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudSavedState {
    RecoveryNeeded,
    UploadUncertain,
    Conflict,
    Pending,
    Unavailable,
    LocalChanges,
    LastFailed,
    Synced,
    NeverSynced,
}

impl From<cloud_status::SavedState> for CloudSavedState {
    fn from(state: cloud_status::SavedState) -> Self {
        use cloud_status::SavedState;
        match state {
            SavedState::RecoveryNeeded => Self::RecoveryNeeded,
            SavedState::UploadUncertain => Self::UploadUncertain,
            SavedState::Conflict => Self::Conflict,
            SavedState::Pending => Self::Pending,
            SavedState::Unavailable => Self::Unavailable,
            SavedState::LocalChanges => Self::LocalChanges,
            SavedState::LastFailed => Self::LastFailed,
            SavedState::Synced => Self::Synced,
            SavedState::NeverSynced => Self::NeverSynced,
        }
    }
}

/// A game's Cloud indicator from persisted state. Swift adds the live states:
/// a sync in progress, a running game, and a game guard with no live session.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudIndicator {
    pub state: CloudSavedState,
    pub game_session: bool,
    /// Last Cloud sync, in Unix seconds.
    pub synced_at: Option<u64>,
    pub pending_upload: bool,
    pub installation_removed: bool,
}

impl From<cloud_status::Indicator> for CloudIndicator {
    fn from(indicator: cloud_status::Indicator) -> Self {
        Self {
            state: indicator.state.into(),
            game_session: indicator.game_session,
            synced_at: indicator.synced_at,
            pending_upload: indicator.pending_upload,
            installation_removed: indicator.installation_removed,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamCloudIndicator {
    pub app_id: u32,
    pub indicator: CloudIndicator,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicCloudGame {
    pub namespace: String,
    pub app_name: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicCloudIndicator {
    pub namespace: String,
    pub app_name: String,
    pub indicator: CloudIndicator,
    /// The saved configuration, which an uninstalled game's maintenance uses.
    pub configuration: Option<EpicCloudConfiguration>,
}

/// Reads journals without any lock. Also lists uninstalled games with pending
/// work, which Swift does not know about.
#[uniffi::export]
pub fn steam_cloud_indicators(
    root: String,
    account_id: u64,
    app_ids: Vec<u32>,
) -> Result<Vec<SteamCloudIndicator>, SteamError> {
    Ok(
        steam::cloud::indicators(Path::new(&root), account_id, &app_ids)?
            .into_iter()
            .map(|(app_id, indicator)| SteamCloudIndicator {
                app_id,
                indicator: indicator.into(),
            })
            .collect(),
    )
}

/// Reads journals without any lock. Also lists uninstalled games with pending
/// work, which Swift does not know about.
#[uniffi::export]
pub fn epic_cloud_indicators(
    root: String,
    account_id: String,
    games: Vec<EpicCloudGame>,
) -> Result<Vec<EpicCloudIndicator>, EpicError> {
    let games: Vec<_> = games
        .into_iter()
        .map(|game| (game.namespace, game.app_name))
        .collect();
    Ok(epic::indicators(Path::new(&root), &account_id, &games)?
        .into_iter()
        .map(|shown| EpicCloudIndicator {
            namespace: shown.namespace,
            app_name: shown.app_name,
            indicator: shown.indicator.into(),
            configuration: shown.configuration.map(Into::into),
        })
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudMaintenancePhase {
    Checking,
    Syncing,
    Conflict,
    Synced,
    Unavailable,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudMaintenanceChoice {
    Local,
    Cloud,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudMaintenanceStatus {
    pub revision: u64,
    pub phase: CloudMaintenancePhase,
    /// Binds an answer to the question this status shows.
    pub decision_id: u64,
    pub allowed_choices: Vec<CloudMaintenanceChoice>,
    pub error: Option<String>,
    pub finished: bool,
}

/// How an attempt ends. A failure ends it: the attempt never asks to retry.
enum Outcome {
    Done,
    Cancelled,
    Failed(String),
}

struct Progress {
    status: Mutex<CloudMaintenanceStatus>,
    changed: Condvar,
}

impl Progress {
    fn new() -> Self {
        Self {
            status: Mutex::new(CloudMaintenanceStatus {
                revision: 0,
                phase: CloudMaintenancePhase::Checking,
                decision_id: 0,
                allowed_choices: Vec::new(),
                error: None,
                finished: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn update(&self, change: impl FnOnce(&mut CloudMaintenanceStatus)) {
        let mut status = self.status.lock().unwrap();
        if status.finished {
            return;
        }
        change(&mut status);
        status.revision += 1;
        self.changed.notify_all();
    }

    fn show(
        &self,
        phase: CloudMaintenancePhase,
        decision_id: u64,
        allowed_choices: Vec<CloudMaintenanceChoice>,
        error: Option<String>,
    ) {
        self.update(|status| {
            status.phase = phase;
            status.decision_id = decision_id;
            status.allowed_choices = allowed_choices;
            status.error = error;
        });
    }

    fn finish(&self, outcome: Outcome) {
        self.update(|status| {
            (status.phase, status.error) = match outcome {
                // Cloud found unusable stays visible as the result.
                Outcome::Done if status.phase == CloudMaintenancePhase::Unavailable => {
                    (CloudMaintenancePhase::Unavailable, None)
                }
                Outcome::Done => (CloudMaintenancePhase::Synced, None),
                Outcome::Cancelled => (CloudMaintenancePhase::Cancelled, None),
                Outcome::Failed(key) => (CloudMaintenancePhase::Failed, Some(key)),
            };
            status.allowed_choices.clear();
            status.finished = true;
        });
    }

    /// Waits for a status newer than `after`. A finished status returns at once.
    fn wait(&self, after: Option<u64>) -> CloudMaintenanceStatus {
        let mut status = self.status.lock().unwrap();
        while !status.finished && after == Some(status.revision) {
            status = self.changed.wait(status).unwrap();
        }
        status.clone()
    }

    fn publish(&self, status: &cloud::Status) {
        use cloud::Phase;
        let phase = match status.phase {
            Phase::Idle | Phase::Checking | Phase::Ready => CloudMaintenancePhase::Checking,
            Phase::Syncing | Phase::Pending | Phase::Failed => CloudMaintenancePhase::Syncing,
            Phase::Conflict => CloudMaintenancePhase::Conflict,
            Phase::Synced => CloudMaintenancePhase::Synced,
            Phase::Disabled | Phase::Unsupported => CloudMaintenancePhase::Unavailable,
        };
        let choices = status
            .allowed_choices
            .iter()
            .filter_map(|choice| match choice {
                cloud::Choice::Local => Some(CloudMaintenanceChoice::Local),
                cloud::Choice::Cloud => Some(CloudMaintenanceChoice::Cloud),
                cloud::Choice::Cancel => Some(CloudMaintenanceChoice::Cancel),
                cloud::Choice::Retry | cloud::Choice::LaunchAnyway => None,
            })
            .collect();
        self.show(phase, status.decision_id, choices, status.error.clone());
    }
}

/// What an attempt forwards to its running sync.
trait Controls: Send + Sync {
    /// False when the answer no longer fits the question shown.
    fn answer(&self, decision_id: u64, choice: CloudMaintenanceChoice) -> bool;
    fn cancel(&self);
}

impl Controls for cloud::Control {
    fn answer(&self, decision_id: u64, choice: CloudMaintenanceChoice) -> bool {
        let choice = match choice {
            CloudMaintenanceChoice::Local => cloud::Choice::Local,
            CloudMaintenanceChoice::Cloud => cloud::Choice::Cloud,
            CloudMaintenanceChoice::Cancel => cloud::Choice::Cancel,
        };
        cloud::Control::answer(self, decision_id, choice).is_ok()
    }
    fn cancel(&self) {
        cloud::Control::cancel(self);
    }
}

/// One Cloud maintenance attempt. Dropping it does not stop the sync; only
/// `cancel`, sign-out or Forge quitting does.
#[derive(uniffi::Object)]
pub struct CloudMaintenance {
    progress: Arc<Progress>,
    controls: Arc<dyn Controls>,
}

#[uniffi::export(async_runtime = "tokio")]
impl CloudMaintenance {
    pub async fn status(self: Arc<Self>, after: Option<u64>) -> CloudMaintenanceStatus {
        let progress = Arc::clone(&self.progress);
        match tokio::task::spawn_blocking(move || progress.wait(after)).await {
            Ok(status) => status,
            Err(_) => self.progress.wait(None),
        }
    }

    pub fn answer(&self, decision_id: u64, choice: CloudMaintenanceChoice) -> bool {
        self.controls.answer(decision_id, choice)
    }

    pub fn cancel(&self) {
        self.controls.cancel();
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamCloudMaintenanceRequest {
    pub root: String,
    pub home: String,
    /// `None` for an uninstalled game: its saved context names the rest.
    pub install: Option<String>,
    pub app_id: u32,
    pub session_directory: String,
    pub helper: String,
}

/// Starts Cloud maintenance of one Steam game on its own thread.
#[uniffi::export]
pub fn start_steam_cloud_maintenance(
    credentials: SteamCredentials,
    request: SteamCloudMaintenanceRequest,
) -> Arc<CloudMaintenance> {
    let progress = Arc::new(Progress::new());
    let publish = Arc::clone(&progress);
    let control = Arc::new(cloud::Control::new(move |status| publish.publish(&status)));
    let credentials = Credentials {
        steam_id: credentials.steam_id,
        account_name: credentials.account_name,
        refresh_token: credentials.refresh_token,
    };
    let (worker, finished) = (Arc::clone(&control), Arc::clone(&progress));
    std::thread::spawn(move || {
        let outcome = steam_maintenance(&credentials, request, worker);
        finished.finish(outcome);
    });
    Arc::new(CloudMaintenance {
        progress,
        controls: control,
    })
}

fn steam_maintenance(
    credentials: &Credentials,
    request: SteamCloudMaintenanceRequest,
    control: Arc<cloud::Control>,
) -> Outcome {
    let idle = match game_runtime::steam::Client::idle_guard(
        Path::new(&request.session_directory),
        Path::new(&request.helper),
    ) {
        Ok(idle) => idle,
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    let root = PathBuf::from(request.root);
    let target = match request.install {
        Some(install) => steam::cloud::MaintenanceTarget::Installed(steam::cloud::Config {
            root,
            home: request.home.into(),
            install: install.into(),
            app_id: request.app_id,
        }),
        None => steam::cloud::MaintenanceTarget::Uninstalled {
            root,
            app_id: request.app_id,
        },
    };
    match steam::cloud::maintain(target, credentials, control, &idle) {
        Ok(()) => Outcome::Done,
        Err(steam::cloud::Failure::Cancelled) => Outcome::Cancelled,
        Err(failure) => Outcome::Failed(failure.key().into()),
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl EpicSession {
    /// Starts Cloud maintenance of one Epic game with this session's account.
    pub async fn maintain_cloud(
        &self,
        configuration: EpicCloudConfiguration,
    ) -> Arc<CloudMaintenance> {
        let progress = Arc::new(Progress::new());
        let publish = Arc::clone(&progress);
        let control = Arc::new(cloud::Control::new(move |status| publish.publish(&status)));
        let (client, worker, finished) = (
            Arc::clone(&self.client),
            Arc::clone(&control),
            Arc::clone(&progress),
        );
        tokio::spawn(async move {
            let outcome = epic_maintenance(configuration.into(), client, worker).await;
            finished.finish(outcome);
        });
        Arc::new(CloudMaintenance {
            progress,
            controls: control,
        })
    }
}

async fn epic_maintenance(
    config: epic::Configuration,
    client: Arc<epic_kit::client::Client>,
    control: Arc<cloud::Control>,
) -> Outcome {
    let (sessions, helper) = (config.session_directory.clone(), config.helper.clone());
    let idle = tokio::task::spawn_blocking(move || epic::Client::idle_guard(&sessions, &helper))
        .await
        .map_err(|_| epic::Error::Session)
        .and_then(|idle| idle);
    let result = match idle {
        Ok(idle) => epic::maintain(config, client, control, &idle).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => Outcome::Done,
        Err(epic::Error::Cancelled) => Outcome::Cancelled,
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorded {
        answers: Mutex<Vec<(u64, CloudMaintenanceChoice)>>,
        cancelled: Mutex<bool>,
    }

    impl Controls for Arc<Recorded> {
        fn answer(&self, decision_id: u64, choice: CloudMaintenanceChoice) -> bool {
            self.answers.lock().unwrap().push((decision_id, choice));
            decision_id == 4
        }
        fn cancel(&self) {
            *self.cancelled.lock().unwrap() = true;
        }
    }

    #[test]
    fn an_attempt_forwards_answers_and_cancellation_to_its_sync() {
        let recorded = Arc::new(Recorded::default());
        let attempt = CloudMaintenance {
            progress: Arc::new(Progress::new()),
            controls: Arc::new(Arc::clone(&recorded)),
        };

        assert!(attempt.answer(4, CloudMaintenanceChoice::Local));
        assert!(!attempt.answer(3, CloudMaintenanceChoice::Cloud));
        attempt.cancel();

        assert_eq!(
            *recorded.answers.lock().unwrap(),
            [
                (4, CloudMaintenanceChoice::Local),
                (3, CloudMaintenanceChoice::Cloud)
            ]
        );
        assert!(*recorded.cancelled.lock().unwrap());
    }

    #[test]
    fn status_waits_for_a_newer_revision_until_the_attempt_finishes() {
        let progress = Arc::new(Progress::new());
        let first = progress.wait(None);
        let waiting = {
            let progress = Arc::clone(&progress);
            std::thread::spawn(move || progress.wait(Some(first.revision)))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!waiting.is_finished());

        progress.publish(&cloud::Status {
            phase: cloud::Phase::Conflict,
            decision_id: 9,
            error: Some("backend.steam.cloud.conflict".into()),
            allowed_choices: vec![
                cloud::Choice::Local,
                cloud::Choice::Cloud,
                cloud::Choice::Cancel,
            ],
        });

        let conflict = waiting.join().unwrap();
        assert_eq!(conflict.phase, CloudMaintenancePhase::Conflict);
        assert_eq!(conflict.decision_id, 9);
        assert_eq!(
            conflict.allowed_choices,
            [
                CloudMaintenanceChoice::Local,
                CloudMaintenanceChoice::Cloud,
                CloudMaintenanceChoice::Cancel
            ]
        );
        progress.finish(Outcome::Failed("backend.steam.cloud.network".into()));
        let finished = progress.wait(Some(conflict.revision));
        assert!(finished.finished);
        assert_eq!(finished.phase, CloudMaintenancePhase::Failed);
        assert!(finished.allowed_choices.is_empty());
        assert_eq!(
            finished.error.as_deref(),
            Some("backend.steam.cloud.network")
        );
        progress.show(CloudMaintenancePhase::Syncing, 0, Vec::new(), None);
        assert_eq!(progress.wait(Some(finished.revision)), finished);
    }

    #[test]
    fn unusable_cloud_stays_the_result_of_a_finished_attempt() {
        let progress = Progress::new();
        progress.publish(&cloud::Status {
            phase: cloud::Phase::Unsupported,
            ..Default::default()
        });
        progress.finish(Outcome::Done);
        assert_eq!(
            progress.wait(None).phase,
            CloudMaintenancePhase::Unavailable
        );

        let progress = Progress::new();
        progress.finish(Outcome::Cancelled);
        assert_eq!(progress.wait(None).phase, CloudMaintenancePhase::Cancelled);
    }

    #[test]
    fn indicators_keep_every_saved_state_and_their_facts() {
        use cloud_status::SavedState;
        let states = [
            (SavedState::RecoveryNeeded, CloudSavedState::RecoveryNeeded),
            (
                SavedState::UploadUncertain,
                CloudSavedState::UploadUncertain,
            ),
            (SavedState::Conflict, CloudSavedState::Conflict),
            (SavedState::Pending, CloudSavedState::Pending),
            (SavedState::Unavailable, CloudSavedState::Unavailable),
            (SavedState::LocalChanges, CloudSavedState::LocalChanges),
            (SavedState::LastFailed, CloudSavedState::LastFailed),
            (SavedState::Synced, CloudSavedState::Synced),
            (SavedState::NeverSynced, CloudSavedState::NeverSynced),
        ];
        for (saved, exported) in states {
            let indicator = CloudIndicator::from(cloud_status::Indicator {
                state: saved,
                game_session: true,
                synced_at: Some(1_789_000_000),
                pending_upload: true,
                installation_removed: true,
            });
            assert_eq!(
                indicator,
                CloudIndicator {
                    state: exported,
                    game_session: true,
                    synced_at: Some(1_789_000_000),
                    pending_upload: true,
                    installation_removed: true,
                }
            );
        }
    }

    #[test]
    fn indicator_exports_list_requested_games_without_creating_journals() {
        let root = tempfile::tempdir().unwrap();
        let account = 0x0110_0001_0000_0001;

        let steam = steam_cloud_indicators(
            root.path().to_string_lossy().into_owned(),
            account,
            vec![42],
        )
        .unwrap();
        let epic = epic_cloud_indicators(
            root.path().to_string_lossy().into_owned(),
            "0123456789abcdef0123456789abcdef".into(),
            vec![EpicCloudGame {
                namespace: "namespace".into(),
                app_name: "game".into(),
            }],
        )
        .unwrap();

        assert_eq!(steam.len(), 1);
        assert_eq!(steam[0].app_id, 42);
        assert_eq!(steam[0].indicator.state, CloudSavedState::NeverSynced);
        assert_eq!(epic.len(), 1);
        assert_eq!(epic[0].indicator.state, CloudSavedState::NeverSynced);
        assert!(epic[0].configuration.is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
