//! The Steam adapter of the Game session supervisor.
use crate::{
    Config, Error, HostState,
    protocol::Credentials,
    supervisor::{self, Record, Target},
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub mod cloud;
pub(crate) mod launch;
mod windows;
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

/// The launch of the Steam game `target` names inside `install`, with its
/// Cloud, ready for its supervisor. A Windows game runs in Wine, in the
/// installation's own prefix, which is readied first.
pub fn prepare(
    locations: &crate::Locations,
    app_id: u32,
    platform: &str,
    install: &Path,
    target: &sailover_core::launch::LaunchTarget,
) -> Result<Request, Error> {
    let home = &locations.home;
    let wine = match platform {
        "windows" => Some(crate::wine::Config::locate(
            &locations.voile,
            crate::wine::prefix(&locations.wine_prefixes, install)?,
            locations
                .steam_diagnostics
                .join(format!("wine-{app_id}.log")),
        )?),
        _ => None,
    };
    let (target, steam_api) = match &wine {
        Some(wine) => {
            crate::wine::prepare(wine, home, install, target, &windows::Prefix { install })?;
            windows::target(install, target, home, wine)?
        }
        None => native_target(install, target, home)?,
    };
    let cloud = cloud::Config {
        root: locations.cloud_archive.clone(),
        home: home.clone(),
        install: install.to_owned(),
        app_id,
        prefix: wine.as_ref().map(|wine| wine.prefix.clone()),
    };
    Ok(Request {
        root: locations.steam_sessions.clone(),
        id: String::new(),
        config: Config {
            app_id,
            cloud_save_roots: vec![install.to_owned()],
            helpers: locations.helpers.clone(),
            package: locations.steam_runtime_package.clone(),
            cache: locations.steam_runtime_cache.clone(),
            diagnostics: locations.steam_diagnostics.clone(),
            steam_api,
            home: home.clone(),
            wine,
        },
        cloud: Some(cloud),
        target,
    })
}

/// The Mac game `target` names inside `install`, and its Steam API library.
fn native_target(
    install: &Path,
    target: &sailover_core::launch::LaunchTarget,
    home: &Path,
) -> Result<(Target, PathBuf), Error> {
    let steam_api = launch::steam_api(target)?;
    let (executable, _) = crate::native::executable(install, target)?;
    Ok((
        Target {
            executable,
            arguments: target.arguments.clone(),
            working_directory: target.working_directory.clone().into(),
            environment: crate::native::environment(home),
            log: None,
        },
        steam_api,
    ))
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
