//! Epic game ownership and EGS Cloud orchestration. The UI only observes and
//! answers revision-bound choices; a detached process owns the entire session.
mod client;
mod server;
mod session;
#[cfg(test)]
mod tests;

use crate::supervisor::{Identity, RosettaRequest, StartingGame, Target};
pub use client::{Client, start};
use forge_core::transfer::CancelToken;
use serde::{Deserialize, Serialize};
pub use server::{exec_game, serve};
pub use session::{archive, pending, removed};
use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("backend.epic.cloud.session")]
    Session,
    #[error("backend.epic.cloud.busy")]
    Busy,
    #[error("backend.epic.cloud.cancelled")]
    Cancelled,
    #[error("backend.epic.cloud.recovery")]
    Recovery,
    #[error("backend.epic.cloud.storage")]
    Storage,
    #[error("backend.epic.cloud.failed")]
    Failed,
    #[error("backend.epic.cloud.unsupported")]
    Unsupported,
    #[error("backend.steam.runtime.rosettaUnavailable")]
    RosettaUnavailable,
    #[error("{0}")]
    Epic(#[from] epic_kit::EpicError),
    #[error("backend.epic.cloud.storage")]
    Vault(#[from] steam_cloud::Error),
    #[error("backend.epic.cloud.storage")]
    Io(#[from] std::io::Error),
}
impl From<crate::Error> for Error {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::RosettaUnavailable => Self::RosettaUnavailable,
            crate::Error::IncompatibleArchitecture => Self::Unsupported,
            _ => Self::Session,
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub type Inspect = fn(u32) -> Result<Option<Identity>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub root: PathBuf,
    pub home: PathBuf,
    pub install: PathBuf,
    pub session_directory: PathBuf,
    pub helper: PathBuf,
    pub namespace: String,
    pub catalog_item_id: String,
    pub app_name: String,
    pub platform: String,
    pub language: String,
}
impl Configuration {
    fn scope(&self, account: &str) -> steam_cloud::EpicScope {
        steam_cloud::EpicScope {
            account_id: account.into(),
            namespace: self.namespace.clone(),
            app_name: self.app_name.clone(),
        }
    }
    fn validate(&self, account: &str) -> Result<()> {
        steam_cloud::StoreScope::from(self.scope(account)).validate()?;
        if [
            &self.root,
            &self.home,
            &self.install,
            &self.session_directory,
            &self.helper,
        ]
        .into_iter()
        .any(|p| {
            !p.is_absolute()
                || p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        }) || self.catalog_item_id.is_empty()
            || self.catalog_item_id.len() > 256
            || self.platform != "Mac"
            || self.language.len() > 128
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Running,
    Finalizing,
    Finished,
    Failed,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FailureReason {
    Cancelled,
    Failed,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum CloudPhase {
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
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Choice {
    Local,
    Cloud,
    Retry,
    Cancel,
    LaunchAnyway,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub id: String,
    pub account_id: String,
    pub namespace: String,
    pub app_name: String,
    pub phase: Phase,
    pub cloud_phase: CloudPhase,
    pub decision_id: u64,
    pub allowed_choices: Vec<Choice>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<FailureReason>,
    pub exit_code: Option<i32>,
    pub revision: u64,
    #[serde(skip)]
    pub reconnecting: bool,
    pub supervisor: Identity,
    pub game: Option<Identity>,
    pub starting: Option<StartingGame>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rosetta: Option<RosettaRequest>,
    pub socket: PathBuf,
}
impl Snapshot {
    pub fn finished(&self) -> bool {
        matches!(self.phase, Phase::Finished | Phase::Failed)
            && self.game.is_none()
            && self.starting.is_none()
    }

    pub fn failure_reason(&self) -> Option<FailureReason> {
        if !self.finished() || self.phase != Phase::Failed {
            return None;
        }
        if let Some(reason) = self.failure_reason {
            return Some(reason);
        }
        // Compatibility with journals and helpers predating typed failures.
        // New snapshots classify the Rust variant before formatting it.
        Some(match self.error.as_deref() {
            Some("backend.epic.cloud.cancelled" | "backend.epic.cancelled") => {
                FailureReason::Cancelled
            }
            _ => FailureReason::Failed,
        })
    }
}

// A one-use exchange code travels solely through the private handoff pipe.
// This request is never persisted, formatted with Debug, or put in argv.
#[derive(Serialize, Deserialize)]
struct Request {
    config: Configuration,
    id: String,
    account_id: String,
    exchange_code: String,
    launch: bool,
}
#[derive(Serialize, Deserialize)]
enum Command {
    Begin {
        id: String,
        account: String,
    },
    Observe {
        id: String,
        after: Option<u64>,
    },
    Cancel {
        id: String,
    },
    Choose {
        id: String,
        account: String,
        decision: u64,
        choice: Choice,
    },
    Revoke {
        id: String,
        account: String,
    },
    ResumeAfterRosetta {
        id: String,
        account: String,
        decision_id: u64,
    },
}
struct State {
    snapshot: Snapshot,
    begun: bool,
    choice: Option<Choice>,
    observed: Instant,
}
struct Control {
    state: Mutex<State>,
    changed: Condvar,
    admission: Mutex<bool>,
    cancel: CancelToken,
    root: PathBuf,
    rosetta: crate::rosetta::Gate,
}
impl Control {
    fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().snapshot.clone()
    }
    /// Wake both the blocking observers and the executor's Rosetta wait.
    fn notify_changed(&self) {
        self.changed.notify_all();
        self.rosetta.wake();
    }
    fn update(&self, change: impl FnOnce(&mut Snapshot)) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        change(&mut state.snapshot);
        state.snapshot.revision += 1;
        let result = write_snapshot(&self.root, &state.snapshot);
        self.notify_changed();
        result
    }
    fn fail(&self, error: &Error) -> Result<()> {
        self.update(|s| {
            s.phase = if s.game.is_some() || s.starting.is_some() {
                Phase::Running
            } else {
                Phase::Failed
            };
            s.failure_reason = (s.phase == Phase::Failed).then_some(match error {
                Error::Cancelled | Error::Epic(epic_kit::EpicError::Cancelled) => {
                    FailureReason::Cancelled
                }
                _ => FailureReason::Failed,
            });
            if !matches!(s.cloud_phase, CloudPhase::Conflict | CloudPhase::Pending) {
                s.cloud_phase = CloudPhase::Failed;
            }
            s.error = Some(error.to_string());
            s.allowed_choices.clear();
            s.rosetta = None;
        })
    }
    fn phase(&self, phase: CloudPhase) -> Result<()> {
        self.update(|s| {
            s.cloud_phase = phase;
            s.allowed_choices.clear();
            s.error = None;
        })
    }
    fn check(&self) -> Result<()> {
        self.cancel.check().map_err(|_| Error::Cancelled)
    }
    fn ask(
        &self,
        phase: CloudPhase,
        error: Option<String>,
        choices: Vec<Choice>,
    ) -> Result<Choice> {
        {
            let mut state = self.state.lock().unwrap();
            state.choice = None;
            state.snapshot.decision_id += 1;
            state.snapshot.cloud_phase = phase;
            state.snapshot.error = error;
            state.snapshot.allowed_choices = choices;
            state.snapshot.revision += 1;
            write_snapshot(&self.root, &state.snapshot)?;
            self.changed.notify_all();
        }
        let mut state = self.state.lock().unwrap();
        loop {
            self.check()?;
            if let Some(choice) = state.choice.take() {
                state.snapshot.allowed_choices.clear();
                return Ok(choice);
            }
            // A vanished observer cannot strand an authenticated helper forever.
            // The durable conflict remains pending for the next account session.
            if state.observed.elapsed() > Duration::from_secs(120) {
                return Err(Error::Cancelled);
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_secs(1))
                .unwrap()
                .0;
        }
    }
}

fn read_snapshot(root: &std::path::Path, id: Option<&str>) -> Result<Option<Snapshot>> {
    if id.is_some_and(|id| uuid::Uuid::parse_str(id).is_err()) {
        return Err(Error::Session);
    }
    let path = root.join(id.map_or_else(|| "session.json".into(), |id| format!("{id}.json")));
    let value = steam_cloud::journal::read::<Snapshot>(&path)?;
    if let Some(s) = &value
        && (uuid::Uuid::parse_str(&s.id).is_err()
            || id.is_some_and(|id| id != s.id)
            || s.account_id.len() != 32
            || s.app_name.is_empty()
            || !s.socket.is_absolute())
    {
        return Err(Error::Session);
    }
    Ok(value)
}
fn write_snapshot(root: &std::path::Path, snapshot: &Snapshot) -> Result<()> {
    steam_cloud::journal::write(&root.join(format!("{}.json", snapshot.id)), snapshot)?;
    steam_cloud::journal::write(&root.join("session.json"), snapshot)?;
    Ok(())
}
fn owns_child(
    snapshot: &Snapshot,
    inspect: impl Fn(u32) -> Result<Option<Identity>>,
) -> Result<bool> {
    let same_process = |before: &Identity, now: &Identity| {
        now.pid == before.pid
            && now.uid == before.uid
            && now.birth_seconds == before.birth_seconds
            && now.birth_microseconds == before.birth_microseconds
    };
    // exec() can replace a game's executable without ending its process. Kernel
    // birth identity, not the last observed executable, controls ownership.
    if let Some(game) = &snapshot.game
        && inspect(game.pid)?.is_some_and(|now| same_process(game, &now))
    {
        return Ok(true);
    }
    if let Some(StartingGame::Process { identity, .. }) = &snapshot.starting
        && let Some(now) = inspect(identity.pid)?
    {
        return Ok(same_process(identity, &now));
    }
    Ok(false)
}
