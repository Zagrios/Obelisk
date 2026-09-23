//! Auto-Cloud lifecycle owned by the detached game supervisor. The UI sends
//! decisions, never file transfers. Credentials exist only in process memory.
mod paths;
mod session;
mod store;
pub(crate) fn safe_storage_path(path: &std::path::Path) -> Result<(), crate::Error> {
    store::safe(path).map_err(Into::into)
}
pub(crate) use session::Session;
pub use session::maintain;
pub use store::{archive, archive_with_rules, indicators, removed};

use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub root: PathBuf,
    pub home: PathBuf,
    pub install: PathBuf,
    pub app_id: u32,
}

/// What Cloud maintenance syncs: an installed game, or an uninstalled game
/// whose saved context still holds pending work.
#[derive(Clone, Debug)]
pub enum MaintenanceTarget {
    Installed(Config),
    Uninstalled { root: PathBuf, app_id: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub phase: Phase,
    pub id: u64,
    pub error: Option<String>,
    /// Older persisted snapshots expose no choices until the supervisor sends
    /// a current decision. Presentation never derives policy from error text.
    #[serde(default)]
    pub allowed_choices: Vec<Choice>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Choice {
    Retry,
    Cancel,
    LaunchAnyway,
    Local,
    Cloud,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("{}", self.key())]
pub enum Failure {
    Network,
    Authentication,
    Preferences,
    Unsupported,
    Conflict,
    Storage,
    Ownership,
    Recovery,
    Uncertain,
    Cancelled,
    Changed,
    Busy,
    Quota,
    /// The other storefront's copy of this game uses the same save paths.
    SharedSaves,
    /// An uninstalled game's Cloud rules changed since its exit save.
    RulesChanged,
}
impl Failure {
    pub fn key(self) -> &'static str {
        match self {
            Self::Network => "backend.steam.cloud.network",
            Self::Authentication => "backend.steam.cloud.authentication",
            Self::Preferences => "backend.steam.cloud.preferences",
            Self::Unsupported => "backend.steam.cloud.unsupported",
            Self::Conflict => "backend.steam.cloud.conflict",
            Self::Storage => "backend.steam.cloud.storage",
            Self::Ownership => "backend.steam.cloud.ownership",
            Self::Recovery => "backend.steam.cloud.recovery",
            Self::Uncertain => "backend.steam.cloud.uncertain",
            Self::Cancelled => "backend.steam.runtime.cancelled",
            Self::Changed => "backend.steam.cloud.changed",
            Self::Busy => "backend.steam.runtime.sessionBusy",
            Self::Quota => "backend.steam.cloud.quota",
            Self::SharedSaves => "backend.cloud.sharedSavesInUse",
            Self::RulesChanged => "backend.steam.cloud.rulesChanged",
        }
    }
    fn retryable(self) -> bool {
        matches!(self, Self::Network | Self::Changed | Self::Uncertain)
    }
    fn skippable(self) -> bool {
        matches!(
            self,
            Self::Network
                | Self::Preferences
                | Self::Unsupported
                | Self::Uncertain
                | Self::Quota
                | Self::Changed
        )
    }
}
impl From<steam_cloud::Error> for Failure {
    fn from(e: steam_cloud::Error) -> Self {
        use steam_cloud::Error;
        match e {
            Error::Busy => Self::Busy,
            Error::OtherStoreBusy => Self::SharedSaves,
            Error::Authentication => Self::Ownership,
            Error::Preferences => Self::Preferences,
            Error::Recovery => Self::Recovery,
            Error::Stale => Self::Changed,
            Error::Invalid | Error::Integrity | Error::Io(_) | Error::Journal(_) => Self::Storage,
        }
    }
}
impl From<steam_kit::cm::CmError> for Failure {
    fn from(e: steam_kit::cm::CmError) -> Self {
        use steam_kit::cm::CmError;
        match e {
            e if e.authentication_required() => Self::Authentication,
            // EResult::LimitExceeded and EResult::InsufficientPrivilege.
            CmError::Rejected {
                result: 25 | 84, ..
            } => Self::Quota,
            CmError::Protocol(_) | CmError::MissingApp(_) => Self::Unsupported,
            CmError::Rejected { .. }
            | CmError::Disconnected
            | CmError::Timeout
            | CmError::NoContentServer
            | CmError::Directory(_) => Self::Network,
        }
    }
}
impl From<steam_kit::cloud::CloudError> for Failure {
    fn from(e: steam_kit::cloud::CloudError) -> Self {
        use steam_kit::cloud::CloudError;
        match e {
            CloudError::Cm(e) => e.into(),
            CloudError::Network | CloudError::Timeout => Self::Network,
            CloudError::Changed => Self::Changed,
            CloudError::Revoked => Self::Authentication,
            CloudError::Uncertain | CloudError::NotCommitted => Self::Uncertain,
            CloudError::Storage | CloudError::Integrity => Self::Storage,
            CloudError::Invalid(_)
            | CloudError::Delta
            | CloudError::UnsupportedFormat { .. }
            | CloudError::Host(_) => Self::Unsupported,
        }
    }
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
impl From<serde_json::Error> for Failure {
    fn from(_: serde_json::Error) -> Self {
        Self::Storage
    }
}
impl From<Failure> for crate::Error {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Cancelled => Self::Cancelled,
            Failure::Busy => Self::SessionBusy,
            failure => Self::Cloud(failure),
        }
    }
}

struct DecisionState {
    status: Status,
    answer: Option<Choice>,
}
pub struct Control {
    account_id: u64,
    revoked: AtomicBool,
    cancelled: AtomicBool,
    state: Mutex<DecisionState>,
    changed: tokio::sync::Notify,
    stop: tokio::sync::Notify,
    publish: Box<dyn Fn(Status) + Send + Sync>,
}
impl Control {
    pub fn new(account_id: u64, publish: impl Fn(Status) + Send + Sync + 'static) -> Self {
        Self {
            account_id,
            revoked: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            state: Mutex::new(DecisionState {
                status: Status::default(),
                answer: None,
            }),
            changed: tokio::sync::Notify::new(),
            stop: tokio::sync::Notify::new(),
            publish: Box::new(publish),
        }
    }
    pub fn respond(&self, id: u64, choice: Choice) -> Result<(), crate::Error> {
        let mut state = self.state.lock().unwrap();
        if state.status.id != id
            || state.answer.is_some()
            || !state.status.allowed_choices.contains(&choice)
        {
            return Err(crate::Error::InvalidSession);
        }
        state.answer = Some(choice);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn revoke(&self, account_id: u64) -> Result<(), crate::Error> {
        if self.account_id != account_id {
            return Err(crate::Error::InvalidSession);
        }
        self.revoked.store(true, Ordering::Release);
        self.stop.notify_waiters();
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.stop.notify_waiters();
        self.changed.notify_waiters();
    }
    fn check(&self) -> Result<(), Failure> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(Failure::Cancelled)
        } else if self.revoked.load(Ordering::Acquire) {
            Err(Failure::Authentication)
        } else {
            Ok(())
        }
    }
    fn status(&self, phase: Phase, error: Option<Failure>) {
        let status = {
            let mut state = self.state.lock().unwrap();
            state.status = Status {
                phase,
                id: state.status.id + 1,
                error: error.map(|e| e.key().into()),
                allowed_choices: match phase {
                    Phase::Conflict => vec![Choice::Local, Choice::Cloud, Choice::Cancel],
                    Phase::Failed => {
                        let mut choices = vec![Choice::Retry, Choice::Cancel];
                        if error.is_some_and(Failure::skippable) {
                            choices.push(Choice::LaunchAnyway);
                        }
                        choices
                    }
                    _ => Vec::new(),
                },
            };
            state.answer = None;
            state.status.clone()
        };
        (self.publish)(status);
    }
    async fn ask(&self, failure: Failure) -> Result<Choice, Failure> {
        self.status(
            if matches!(failure, Failure::Conflict) {
                Phase::Conflict
            } else {
                Phase::Failed
            },
            Some(failure),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.check()?;
            if let Some(choice) = self.state.lock().unwrap().answer.take() {
                return if choice == Choice::Cancel {
                    Err(Failure::Cancelled)
                } else {
                    Ok(choice)
                };
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return Err(Failure::Cancelled);
            }
        }
    }
    async fn guarded<T>(
        &self,
        duration: Duration,
        future: impl std::future::Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        let stop = self.stop.notified();
        tokio::pin!(stop);
        stop.as_mut().enable();
        self.check()?;
        tokio::select! {
            biased;
            _ = &mut stop => { self.check()?; Err(Failure::Cancelled) },
            result = tokio::time::timeout(duration, future) => result.map_err(|_| Failure::Network)?,
        }
    }
    async fn wait(&self, duration: Duration) -> Result<(), Failure> {
        let changed = self.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        self.check()?;
        let _ = tokio::time::timeout(duration, changed).await;
        self.check()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn scope(config: &Config, account: u64) -> steam_cloud::Scope {
    steam_cloud::Scope {
        steam_id: account,
        app_id: config.app_id,
        cloud_app_id: config.app_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_published_choices_are_accepted_without_reissuing_the_decision() {
        for failure in [
            Failure::Network,
            Failure::Authentication,
            Failure::Preferences,
            Failure::Unsupported,
            Failure::Conflict,
            Failure::Storage,
            Failure::Ownership,
            Failure::Recovery,
            Failure::Uncertain,
            Failure::Cancelled,
            Failure::Changed,
            Failure::Busy,
            Failure::Quota,
            Failure::SharedSaves,
            Failure::RulesChanged,
        ] {
            let control = Control::new(42, |_| {});
            let phase = if matches!(failure, Failure::Conflict) {
                Phase::Conflict
            } else {
                Phase::Failed
            };
            control.status(phase, Some(failure));
            let status = control.state.lock().unwrap().status.clone();
            assert_eq!(
                status.allowed_choices.contains(&Choice::LaunchAnyway),
                failure.skippable()
            );
            for choice in [
                Choice::Retry,
                Choice::Local,
                Choice::Cloud,
                Choice::LaunchAnyway,
                Choice::Cancel,
            ] {
                if !status.allowed_choices.contains(&choice) {
                    assert!(control.respond(status.id, choice).is_err());
                    assert_eq!(control.state.lock().unwrap().status, status);
                }
            }
            assert!(control.respond(status.id, Choice::Cancel).is_ok());
        }
        let control = Control::new(42, |_| {});
        control.status(Phase::Checking, None);
        assert!(control.respond(1, Choice::Retry).is_err());
    }

    #[test]
    fn prompt_choices_are_bound_to_revision_and_account() {
        let control = Control::new(42, |_| {});
        control.status(Phase::Conflict, Some(Failure::Conflict));
        assert!(control.respond(0, Choice::Local).is_err());
        assert!(control.respond(1, Choice::LaunchAnyway).is_err());
        assert!(control.respond(1, Choice::Local).is_ok());
        assert!(control.respond(1, Choice::Cloud).is_err());
        assert!(control.revoke(43).is_err());
        assert!(control.check().is_ok());
        control.revoke(42).unwrap();
        assert!(matches!(control.check(), Err(Failure::Authentication)));
    }
    #[tokio::test]
    async fn a_pending_choice_keeps_the_supervisors_executor_running() {
        let (published, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let control = Control::new(42, move |status| {
            published.send(status).unwrap();
        });
        let (choice, ()) = tokio::join!(control.ask(Failure::Conflict), async {
            let status = requests.recv().await.unwrap();
            tokio::task::yield_now().await;
            control.respond(status.id, Choice::Local).unwrap();
        });
        assert_eq!(choice.unwrap(), Choice::Local);
    }

    #[tokio::test]
    async fn signout_wakes_pending_decision_and_detachment_does_not_revoke() {
        let control = Arc::new(Control::new(42, |_| {}));
        let worker = control.clone();
        let task = tokio::spawn(async move { worker.ask(Failure::Conflict).await });
        control.revoke(42).unwrap();
        assert!(matches!(task.await.unwrap(), Err(Failure::Authentication)));
        let control = Arc::new(Control::new(42, |_| {}));
        let observer = control.clone();
        drop(observer);
        assert!(control.check().is_ok());
    }
}
