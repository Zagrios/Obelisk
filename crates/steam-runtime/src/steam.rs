//! The Steam adapter of the Game session supervisor.
use crate::{
    Config, Error, HostState, cloud,
    protocol::Credentials,
    supervisor::{self, Record, Target},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) mod launch;
pub use launch::serve;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Steam;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub app_id: u32,
    pub host: HostState,
}

pub type Snapshot = Record<Steam>;
pub type Client = supervisor::Client<Steam>;
pub type IdleGuard = supervisor::IdleGuard<Steam>;

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub root: PathBuf,
    pub id: String,
    pub config: Config,
    pub target: Target,
    #[serde(default)]
    pub cloud: Option<cloud::Config>,
}

impl supervisor::Provider for Steam {
    type Account = u64;
    type Store = Store;
    type Cloud = cloud::Status;
    type Choice = cloud::Choice;
    type Error = Error;
    const SUPERVISE: &'static str = "supervise";
    const CANCELLED: &'static [&'static str] = &["backend.steam.runtime.cancelled"];

    fn valid(record: &Record<Self>) -> bool {
        record.store.app_id != 0 && record.account_id != 0
    }

    fn cancelled(error: &Error) -> bool {
        matches!(
            error,
            Error::Cancelled | Error::Cloud(cloud::Failure::Cancelled)
        )
    }

    fn same_game(a: &Store, b: &Store) -> bool {
        a.app_id == b.app_id
    }

    /// The UI reports host loss and keeps protecting a game that still runs.
    fn orphaned(record: &mut Record<Self>, _owned: bool) {
        record.store.host = HostState::Lost;
    }
}

pub fn start(
    helper: &Path,
    credentials: &Credentials,
    request: Request,
    cancelled: &AtomicBool,
) -> Result<Client, Error> {
    start_command(
        helper,
        credentials,
        request,
        cancelled,
        Client::supervisor_command(helper),
        supervisor::client::inspect,
    )
}

pub(crate) fn start_command(
    helper: &Path,
    credentials: &Credentials,
    request: Request,
    cancelled: &AtomicBool,
    command: std::process::Command,
    probe: supervisor::client::Probe,
) -> Result<Client, Error> {
    let root = request.root.clone();
    let id = request.id.clone();
    let app_id = request.config.app_id;
    let account_id = credentials.steam_id;
    Ok(Client::start_with(
        &root,
        helper,
        id,
        command,
        probe,
        |pipe| {
            credentials
                .write(&mut *pipe)
                .map_err(|_| supervisor::Error::StartFailed)?;
            supervisor::storage::send(pipe, &request)
        },
        |ack| ack.account_id == account_id && ack.store.app_id == app_id,
        || cancelled.load(Ordering::Acquire),
    )?)
}
