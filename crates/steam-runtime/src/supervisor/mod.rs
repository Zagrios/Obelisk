//! The Game session supervisor every storefront shares. A per-game supervisor
//! owns the game, independently of the UI. Credentials cross only the initial
//! stdin pipe; control uses session and account IDs. Each storefront plugs in
//! as a `Provider`.
pub(crate) mod architecture;
mod child;
pub(crate) mod client;
pub mod exec;
pub(crate) mod journal;
pub(crate) mod ownership;
pub(crate) mod rosetta;
pub(crate) mod session;
pub(crate) mod storage;

use client::SessionAlive;
pub use client::{Client, IdleGuard, OwnerLock};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
pub use session::Inspect;
use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub pid: u32,
    pub uid: u32,
    pub birth_seconds: u64,
    pub birth_microseconds: u64,
    pub executable: String,
}

/// Durable ownership between spawning an architecture helper and observing its
/// replacement by the game. Missing inspection never proves that child exited.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StartingGame {
    Process {
        identity: Identity,
        executable: PathBuf,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Target {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub environment: HashMap<String, String>,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RosettaRequest {
    pub decision_id: u64,
}

/// What a storefront adds to the storefront-neutral supervisor.
pub trait Provider: Clone + std::fmt::Debug + PartialEq + Eq + Send + Sync + 'static {
    type Account: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync;
    /// What the session records about the storefront's game.
    type Store: Clone
        + std::fmt::Debug
        + PartialEq
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync;
    /// The storefront's Cloud progress and pending question.
    type Cloud: Clone
        + std::fmt::Debug
        + Default
        + PartialEq
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync;
    type Choice: Copy
        + std::fmt::Debug
        + PartialEq
        + Eq
        + Serialize
        + DeserializeOwned
        + Send
        + Sync;
    /// The errors the storefront's callers expect. Core failures map into
    /// it; its display is a localization key.
    type Error: From<Error> + std::fmt::Debug + std::fmt::Display + Send;
    /// The helper subcommand that becomes this storefront's supervisor.
    const SUPERVISE: &'static str;
    /// Failure keys older helpers used for a cancellation.
    const CANCELLED: &'static [&'static str];

    fn valid(record: &Record<Self>) -> bool;
    /// Whether a failure is a cancellation, which ends a session silently.
    fn cancelled(error: &Self::Error) -> bool;
    /// Whether two records describe the same game; live facts may differ.
    fn same_game(a: &Self::Store, b: &Self::Store) -> bool;
    /// How a record whose supervisor is gone presents. `owned` means its game
    /// still runs; otherwise the record is already terminal.
    fn orphaned(record: &mut Record<Self>, owned: bool);
}

/// A Game session as its supervisor journals it. Ownership (the supervisor,
/// game, starting helper and socket) decides the Reservation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(bound = "")]
pub struct Record<P: Provider> {
    pub version: u32,
    pub id: String,
    pub account_id: P::Account,
    pub store: P::Store,
    pub phase: Phase,
    pub cloud: P::Cloud,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<FailureReason>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rosetta: Option<RosettaRequest>,
    /// Client-side confidence in the control connection, never persisted.
    #[serde(skip)]
    pub reconnecting: bool,
    pub supervisor: Identity,
    pub game: Option<Identity>,
    #[serde(default)]
    pub starting: Option<StartingGame>,
    pub socket: PathBuf,
}
impl<P: Provider> Record<P> {
    /// Whether the game or starting process this record owns still lives.
    pub(crate) fn liveness(
        &self,
        inspect: impl Fn(u32) -> Result<Option<Identity>, Error>,
    ) -> Result<ownership::Liveness, Error> {
        ownership::owns_process(
            self.phase == Phase::Running,
            self.game.as_ref(),
            self.starting.as_ref(),
            inspect,
        )
    }
    /// Terminal only once its phase is final and it owns no process.
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
        // Records without a typed reason classify only the cancellation key.
        Some(match self.error.as_deref() {
            Some(error) if P::CANCELLED.contains(&error) => FailureReason::Cancelled,
            _ => FailureReason::Failed,
        })
    }
}
impl<P: Provider> journal::Entry for Record<P> {
    fn id(&self) -> &str {
        &self.id
    }
    fn valid(&self) -> bool {
        P::valid(self)
    }
}

/// Control requests, identical for every storefront. Answers name the
/// account and the decision they answer.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub(crate) enum Command<P: Provider> {
    Begin {
        id: String,
        account_id: P::Account,
    },
    Observe {
        id: String,
        after: Option<u64>,
    },
    Cancel {
        id: String,
    },
    CloudChoice {
        id: String,
        account_id: P::Account,
        decision_id: u64,
        choice: P::Choice,
    },
    Revoke {
        id: String,
        account_id: P::Account,
    },
    ResumeAfterRosetta {
        id: String,
        account_id: P::Account,
        decision_id: u64,
    },
}

/// Storefront-neutral supervisor failures. Each storefront maps them to its
/// own localization keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("session storage failed")]
    Storage,
    #[error("invalid session")]
    InvalidSession,
    /// Written by a newer build: its process may be alive, so it stays reserved.
    #[error("session written by a newer build")]
    NewerSession,
    #[error("session busy")]
    Busy,
    #[error("supervisor unavailable")]
    Unavailable,
    #[error("cancelled")]
    Cancelled,
    #[error("supervisor start failed")]
    StartFailed,
    #[error("Rosetta unavailable")]
    RosettaUnavailable,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Storage => Self::Storage,
            Error::InvalidSession => Self::InvalidSession,
            Error::NewerSession | Error::Busy => Self::SessionBusy,
            Error::Unavailable => Self::Unavailable,
            Error::Cancelled => Self::Cancelled,
            Error::StartFailed => Self::StartFailed,
            Error::RosettaUnavailable => Self::RosettaUnavailable,
        }
    }
}
