//! A per-game supervisor owns the host and game, independently of the UI.
//! Credentials cross only the initial stdin pipe. Control uses session/account IDs.
mod client;
mod server;
mod storage;
#[cfg(test)]
mod tests;

use crate::{Config, HostState, cloud};
pub use client::{Client, IdleGuard, start};
use serde::{Deserialize, Serialize};
pub use server::serve;
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

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub root: PathBuf,
    pub id: String,
    pub config: Config,
    pub target: Target,
    #[serde(default)]
    pub cloud: Option<cloud::Config>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Running,
    Finalizing,
    Finished,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub app_id: u32,
    pub account_id: u64,
    pub supervisor: Identity,
    pub game: Option<Identity>,
    #[serde(default)]
    pub starting: Option<StartingGame>,
    pub socket: PathBuf,
    pub phase: Phase,
    pub host: HostState,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub revision: u64,
    #[serde(default)]
    pub cloud: cloud::Status,
    /// Client-side confidence in the control connection, never persisted.
    #[serde(skip)]
    pub reconnecting: bool,
    #[serde(default)]
    pub rosetta: Option<RosettaRequest>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RosettaRequest {
    pub decision_id: u64,
}
impl Snapshot {
    pub fn finished(&self) -> bool {
        matches!(self.phase, Phase::Finished | Phase::Failed)
    }
}

#[derive(Serialize, Deserialize)]
enum Command {
    Observe {
        id: String,
        after: Option<u64>,
    },
    Cancel {
        id: String,
    },
    CloudChoice {
        id: String,
        account_id: u64,
        decision_id: u64,
        choice: cloud::Choice,
    },
    Revoke {
        id: String,
        account_id: u64,
    },
    ResumeAfterRosetta {
        id: String,
        account_id: u64,
        decision_id: u64,
    },
}

// Native process inspection remains in steam-native; safe Rust never calls the
// Valve ABI or libproc. Tests inject identities for their disposable children.
pub type Inspect = fn(u32) -> Option<Identity>;
