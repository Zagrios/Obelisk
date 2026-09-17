//! A per-game supervisor owns the host and game, independently of the UI.
//! Credentials cross only the initial stdin pipe. Reattachment is read-only.
mod client;
mod server;
mod storage;
#[cfg(test)]
mod tests;

use crate::{Config, HostState};
pub use client::{Client, start};
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    Running,
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
    pub socket: PathBuf,
    pub phase: Phase,
    pub host: HostState,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub revision: u64,
    /// Client-side confidence in the control connection, never persisted.
    #[serde(skip)]
    pub reconnecting: bool,
}
impl Snapshot {
    pub fn finished(&self) -> bool {
        matches!(self.phase, Phase::Finished | Phase::Failed)
    }
}

#[derive(Serialize, Deserialize)]
enum Command {
    Observe { id: String, after: Option<u64> },
    Cancel { id: String },
}

// Native process inspection remains in steam-native; safe Rust never calls the
// Valve ABI or libproc. Tests inject identities for their disposable children.
pub type Inspect = fn(u32) -> Option<Identity>;
