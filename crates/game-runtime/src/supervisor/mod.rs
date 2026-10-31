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
pub(crate) mod quit;
pub(crate) mod rosetta;
pub(crate) mod session;
pub(crate) mod storage;

use client::SessionAlive;
pub use client::{Client, IdleGuard, OwnerLock};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
pub use session::Inspect;
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

/// Whether `path` is absolute and never climbs with `..`.
pub(crate) fn plain_absolute(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|c| c == Component::ParentDir)
}

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
    /// Where the game's standard error goes, replaced on each launch. Wine
    /// writes its diagnostics there. Without it, standard error is discarded.
    #[serde(default)]
    pub log: Option<PathBuf>,
    /// The installation the game runs from. Force quitting it ends what still
    /// runs from there; without one, only the game and what it started end.
    #[serde(default)]
    pub installation: Option<PathBuf>,
    /// The Windows program Wine starts, which quitting asks to close.
    #[serde(default)]
    pub program: Option<PathBuf>,
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
    /// What the player may ask of the storefront's running game, such as
    /// accepting an invitation; `NoControl` when nothing.
    type Control: std::fmt::Debug + Serialize + DeserializeOwned + Send;
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

/// What a session's game did once it ran: for how long, how smoothly, how it
/// ended and what ran it. Obelisk reports it when the player shares game
/// performance.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Played {
    pub seconds: u64,
    pub frames: crate::metal_hud::Frames,
    /// Absent when a signal ended the game.
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    /// The player quit or stopped the game from Obelisk.
    pub ended_by_obelisk: bool,
    /// What ran a Windows game; absent for a Mac one.
    pub engine: Option<crate::wine::Engine>,
    /// The graphics backend a Windows game ran with, as its launch settings
    /// picked it; absent for a Mac one.
    pub graphics: Option<crate::wine::Graphics>,
    /// The Voile release a Windows game ran in.
    pub voile: Option<String>,
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
    pub cloud: crate::cloud::Status,
    pub exit_code: Option<i32>,
    /// Set once the game ran and exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub played: Option<Played>,
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
    /// Ends the game itself, as quitting Obelisk does. Before spawn it
    /// cancels preparation.
    Stop {
        id: String,
    },
    /// The player's Quit: the game is asked to close its windows, and nothing
    /// is killed. Before spawn it cancels preparation.
    Quit {
        id: String,
    },
    /// The player's Force Quit, for a game that ignored Quit: it ends now,
    /// with whatever still runs for its installation. Before spawn it cancels
    /// preparation.
    ForceQuit {
        id: String,
    },
    /// The player's `control` of the running game, through its storefront.
    Control {
        id: String,
        account_id: P::Account,
        control: P::Control,
    },
    CloudChoice {
        id: String,
        account_id: P::Account,
        decision_id: u64,
        choice: crate::cloud::Choice,
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

/// The controls of a storefront that has none to offer.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum NoControl {}

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
