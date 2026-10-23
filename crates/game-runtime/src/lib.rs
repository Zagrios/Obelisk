//! Safe launcher-side supervision. Native ABI code lives in the separate helper.
pub mod cloud;
pub mod cloud_status;
pub mod diagnostics;
pub mod epic;
mod files;
pub mod games_folder;
pub mod gog;
mod input;
pub mod installations;
pub mod launch_settings;
mod locations;
pub mod metal_hud;
mod native;
pub mod notes;
pub mod package;
mod process;
pub mod profile;
pub mod protocol;
pub mod queue;
mod remote;
pub mod runtimes;
pub mod sessions;
pub mod steam;
pub mod supervisor;
pub mod valve;
pub mod voile;
pub mod wine;
pub use locations::Locations;
use protocol::{Credentials, Event};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// Every supervisor entry point drives its async work from one blocking thread.
pub(crate) fn executor() -> std::io::Result<Executor> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|runtime| Executor(Some(runtime)))
}

/// Dropping waits at most a second for blocking work, so a pending DNS lookup
/// cannot hold a caller whose async work has already completed.
pub(crate) struct Executor(Option<tokio::runtime::Runtime>);
impl Executor {
    pub(crate) fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.0
            .as_ref()
            .expect("executor runs until dropped")
            .block_on(future)
    }
}
impl Drop for Executor {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The pinned engine hash lives only in the runtime manifest; this reads it back.
pub fn client_sha256() -> &'static str {
    &package::entry("steamclient.dylib").sha256
}
pub fn native_cache_checkpoint(directory: PathBuf, app_id: u32, platforms: u32) -> bool {
    native_cache_files(directory, app_id, platforms).is_ok()
}
/// A published projection always carries its index, so a missing `remotecache.vdf`
/// is an incomplete projection rather than an empty one. `SdkCloudPaths::records`
/// cannot tell the two apart, so the distinction is enforced here for every caller.
pub fn native_cache_files(
    directory: PathBuf,
    app_id: u32,
    platforms: u32,
) -> Result<Vec<steam_kit::cloud::LocalCloudFile>, Error> {
    if !directory.join("remotecache.vdf").is_file() {
        return Err(Error::Storage);
    }
    steam_kit::cloud::SdkCloudPaths::new(directory, app_id, platforms)
        .and_then(|paths| paths.local_files(&[]))
        .map_err(|_| Error::Storage)
}
pub fn sha(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut file = std::io::BufReader::with_capacity(64 * 1024, std::fs::File::open(path)?);
    std::io::copy(&mut file, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Cloud(steam::cloud::Failure),
    #[error("backend.steam.runtime.sessionBusy")]
    SessionBusy,
    #[error("backend.steam.runtime.cancelled")]
    Cancelled,
    #[error("backend.steam.runtime.invalidSession")]
    InvalidSession,
    #[error("backend.steam.runtime.unavailable")]
    Unavailable,
    #[error("backend.steam.runtime.versionMismatch")]
    VersionMismatch,
    #[error("backend.steam.runtime.startFailed")]
    StartFailed,
    #[error("backend.steam.runtime.authenticationFailed")]
    Authentication,
    #[error("backend.steam.runtime.ownershipFailed")]
    Ownership,
    #[error("backend.steam.runtime.timeout")]
    Timeout,
    #[error("backend.steam.runtime.storageFailed")]
    Storage,
    /// A Mac build without an arm64 slice, installed before Obelisk
    /// installed such a game's Windows build instead.
    #[error("backend.runtime.macBuildNotNative")]
    IncompatibleArchitecture,
    #[error("backend.runtime.rosettaUnavailable")]
    RosettaUnavailable,
    #[error("backend.runtime.invalidApplication")]
    InvalidApplication,
    #[error("backend.runtime.notExecutable")]
    NotExecutable,
    #[error("backend.steam.runtime.apiMissing")]
    ApiMissing,
    #[error("backend.runtime.wineUnavailable")]
    WineUnavailable,
    /// Valve's Windows Steam starts the game, and is signed in to no account.
    #[error("backend.steam.windowsSteam.signInRequired")]
    WindowsSteamSignedOut,
    /// Valve's Windows Steam did not take the account's login.
    #[error("backend.steam.windowsSteam.signInFailed")]
    WindowsSteamSignIn,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub helpers: PathBuf,
    pub package: PathBuf,
    pub cache: PathBuf,
    pub diagnostics: PathBuf,
    /// A Mac game's Steam API library, whose slices choose the game's. A
    /// Windows game names none: Wine loads whatever Steam API the game
    /// ships, if any.
    pub steam_api: Option<PathBuf>,
    pub home: PathBuf,
    pub app_id: u32,
    #[serde(default)]
    pub cloud_save_roots: Vec<PathBuf>,
    /// Set for a Windows game, which runs in Obelisk's Wine runtime.
    #[serde(default)]
    pub wine: Option<wine::Config>,
}

pub struct Launch {
    credentials: Mutex<Option<Credentials>>,
    cancelled: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct NativeStorage {
    pub home: PathBuf,
    pub account_id: u64,
    pub app_id: u32,
    /// The game's platform, as a Cloud platform mask. A Windows game's SDK
    /// files live in the macOS Steam engine's storage too.
    pub platforms: u32,
}
impl NativeStorage {
    pub fn steam_directory(&self) -> PathBuf {
        self.home.join("Library/Application Support/Steam")
    }
    /// The account- and app-specific SDK cloud cache the coordinator projects into.
    pub fn cache_directory(&self) -> PathBuf {
        self.steam_directory().join(format!(
            "userdata/{}/{}",
            self.account_id as u32, self.app_id
        ))
    }
}
impl Launch {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials: Mutex::new(Some(credentials)),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    /// Starts the native host. `storage` is the Steam Cloud cache the game's
    /// SDK reads, when Cloud runs for this launch.
    pub fn prepare(
        &self,
        config: Config,
        storage: Option<NativeStorage>,
    ) -> Result<Runtime, Error> {
        let mut events = Vec::new();
        let result = self.prepare_inner(&config, storage, &mut events);
        if let Err(error) = &result {
            let _ = files::failure_report(&config, error, &events);
        }
        result
    }
    fn prepare_inner(
        &self,
        config: &Config,
        storage: Option<NativeStorage>,
        events: &mut Vec<Event>,
    ) -> Result<Runtime, Error> {
        self.check_cancelled()?;
        let credentials = self
            .credentials
            .lock()
            .map_err(|_| Error::InvalidSession)?
            .take()
            .ok_or(Error::InvalidSession)?;
        if !credentials.valid() || config.app_id == 0 {
            return Err(Error::InvalidSession);
        }
        let prepared = files::prepare(config, &self.cancelled)?;
        self.prepare_runtime(
            config,
            storage,
            events,
            credentials,
            prepared,
            process::Child::host,
        )
    }
    fn prepare_runtime(
        &self,
        config: &Config,
        storage: Option<NativeStorage>,
        events: &mut Vec<Event>,
        credentials: Credentials,
        mut prepared: files::Prepared,
        start_host: impl FnOnce(&files::Prepared, &Config, &str) -> Result<process::Child, Error>,
    ) -> Result<Runtime, Error> {
        let id = credentials.steam_id;
        if let Some(storage) = &storage {
            if storage.account_id != id || storage.app_id != config.app_id {
                return Err(Error::InvalidSession);
            }
            prepared.enable_storage(storage)?;
        }
        self.check_cancelled()?;
        let service = format!(
            "com.zagrios.obelisk.discovery.{}",
            uuid::Uuid::new_v4().simple()
        );
        let mut host = start_host(&prepared, config, &service)?;
        credentials
            .write(host.input.as_mut().ok_or(Error::StartFailed)?)
            .map_err(|_| Error::StartFailed)?;
        drop(credentials);
        let ticket_bytes = self.wait_for_host(&mut host, config.app_id, events)?;
        self.check_cancelled()?;
        if !host.alive()? {
            return Err(Error::StartFailed);
        }
        // The pinned host has verified its account, subscription and ticket.
        // Let the game attach using its own SDK and interface versions. The
        // host does not initialize the game's SDK.
        let proof = Proof {
            app_id: config.app_id,
            host_pid: host.id(),
            authenticated: true,
            ticket_bytes,
            host_account_verified: true,
            host_license_verified: true,
            sdk_checked: false,
            game_pid: None,
            game_exit: None,
            discovery_paths: 0,
            host_state: HostState::Online,
            state: "ready".into(),
        };
        let wakeup = host.wakeup.clone();
        let runtime = Runtime {
            inner: Mutex::new(Inner {
                host,
                proof,
                requests: GameRequests::default(),
                sequence: 0,
                stopped: false,
            }),
            wakeup,
            service,
            prepared,
        };
        runtime.write_report()?;
        Ok(runtime)
    }
    fn wait_for_host(
        &self,
        host: &mut process::Child,
        expected_app: u32,
        events: &mut Vec<Event>,
    ) -> Result<u32, Error> {
        let start = Instant::now();
        let mut authenticated = false;
        loop {
            self.check_cancelled()?;
            if start.elapsed() > Duration::from_secs(50) {
                return Err(Error::Timeout);
            }
            let event = host.next(Duration::from_millis(50))?;
            if let Some(event) = &event {
                events.push(event.clone());
            }
            match event {
                Some(Event::Authenticated {}) if !authenticated => authenticated = true,
                Some(Event::Ready {
                    app_id,
                    host_pid,
                    ticket_bytes,
                }) if authenticated
                    && app_id == expected_app
                    && host_pid == host.id()
                    && ticket_bytes > 0 =>
                {
                    return Ok(ticket_bytes);
                }
                Some(Event::Failed { reason }) => return Err(failure(reason)),
                Some(_) => return Err(Error::StartFailed),
                None => {}
            }
        }
    }
    fn check_cancelled(&self) -> Result<(), Error> {
        package::check(&self.cancelled)
    }
}
fn failure(reason: protocol::Failure) -> Error {
    match reason {
        protocol::Failure::Authentication => Error::Authentication,
        protocol::Failure::Ownership => Error::Ownership,
        protocol::Failure::Cancelled => Error::Cancelled,
        _ => Error::StartFailed,
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Proof {
    pub app_id: u32,
    pub host_pid: u32,
    pub authenticated: bool,
    pub ticket_bytes: u32,
    pub host_account_verified: bool,
    pub host_license_verified: bool,
    pub sdk_checked: bool,
    pub game_pid: Option<u32>,
    pub game_exit: Option<i32>,
    pub discovery_paths: u32,
    pub host_state: HostState,
    pub state: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    Online,
    Disconnected,
    Lost,
    Stopped,
}
/// The latest invite dialog the game asked for. `sequence` grows with every
/// request of the session, so a repeated request still reads as new.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InviteRequest {
    pub sequence: u64,
    pub target: protocol::InviteTarget,
}

/// The latest overlay the game asked Steam for. `sequence` grows with every
/// request of the session, so a repeated request still reads as new.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OverlayRequest {
    pub sequence: u64,
    pub dialog: protocol::OverlayDialog,
}
impl OverlayRequest {
    /// The same request, when it asks for an invite dialog.
    pub fn invite(&self) -> Option<InviteRequest> {
        Some(InviteRequest {
            sequence: self.sequence,
            target: self.dialog.invite()?,
        })
    }
}

/// An invitation a friend sent into the running game.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReceivedInvite {
    pub friend: u64,
    pub target: protocol::InviteTarget,
    /// When the host last told of it, in seconds since the Unix epoch.
    pub received_at: u64,
}

/// A purchase the running game asks the player to confirm.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MicroTxn {
    pub order_id: u64,
    pub app_id: u32,
}

/// The most invitations a session keeps; the oldest goes first.
pub const MAX_RECEIVED_INVITES: usize = 16;

/// What the running game asks of the player's Steam session, as its host
/// tells it. A record from before any of it has none.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GameRequests {
    /// The latest overlay the game asked Steam for, invite dialogs included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<OverlayRequest>,
    /// The Steam lobby the game is in now, as the account's engine shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lobby: Option<u64>,
    /// Invitations friends sent into the game, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invites: Vec<ReceivedInvite>,
    /// The purchase the game waits on the player to confirm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microtxn: Option<MicroTxn>,
}
impl GameRequests {
    /// The overlay's invite request, while it asks for an invite dialog.
    pub fn invite(&self) -> Option<InviteRequest> {
        self.overlay.as_ref().and_then(OverlayRequest::invite)
    }
}

/// What the native host tells of the game's Steam session. The requests
/// only hold while the host is online: a host that lost Steam, or the game,
/// is in no lobby anyone can join, and can answer nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostUpdate {
    pub state: HostState,
    pub requests: GameRequests,
}
impl HostUpdate {
    pub fn new(state: HostState) -> Self {
        Self {
            state,
            requests: GameRequests::default(),
        }
    }
}

struct Inner {
    host: process::Child,
    proof: Proof,
    /// Its invitations are kept across a disconnection: Steam still holds
    /// them.
    requests: GameRequests,
    /// Overlay requests of the session so far, the last one's sequence.
    sequence: u64,
    stopped: bool,
}
impl Inner {
    fn update(&self) -> HostUpdate {
        let state = self.proof.host_state;
        if state != HostState::Online {
            return HostUpdate::new(state);
        }
        HostUpdate {
            state,
            requests: self.requests.clone(),
        }
    }
    /// Keeps `target` from `friend` first, once.
    fn received(&mut self, friend: u64, target: protocol::InviteTarget) {
        if !protocol::individual(friend) || !target.valid() {
            return;
        }
        let invites = &mut self.requests.invites;
        invites.retain(|invite| invite.friend != friend || invite.target != target);
        invites.insert(
            0,
            ReceivedInvite {
                friend,
                target,
                received_at: unix_now(),
            },
        );
        invites.truncate(MAX_RECEIVED_INVITES);
    }
}
pub struct Runtime {
    inner: Mutex<Inner>,
    wakeup: Arc<process::Wake>,
    service: String,
    prepared: files::Prepared,
}
impl Runtime {
    pub fn environment(&self) -> std::collections::HashMap<String, String> {
        self.prepared.consumer_environment(&self.service)
    }
    pub(crate) fn set_presence(&self, online: bool) -> Result<(), Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::StartFailed)?;
        if inner.stopped {
            return Ok(());
        }
        Self::send(&mut inner, &protocol::HostCommand::SetPresence { online })
            .map_err(|_| Error::StartFailed)
    }
    fn send(inner: &mut Inner, command: &protocol::HostCommand) -> Result<(), Error> {
        command
            .write(inner.host.input.as_mut().ok_or(Error::Unavailable)?)
            .map_err(|_| Error::Unavailable)
    }
    /// Sends the player's `command` to the host while it serves the game
    /// online; `change` first checks it against what the host told and
    /// records its effect. An observer sees that effect at once.
    fn control(
        &self,
        command: &protocol::HostCommand,
        change: impl FnOnce(&mut Inner) -> Result<(), Error>,
    ) -> Result<(), Error> {
        if !command.valid() {
            return Err(Error::InvalidSession);
        }
        let mut inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        if inner.stopped || inner.proof.host_state != HostState::Online {
            return Err(Error::Unavailable);
        }
        change(&mut inner)?;
        let sent = Self::send(&mut inner, command);
        drop(inner);
        self.wakeup.notify();
        sent
    }
    /// Tells the game Obelisk's overlay opened or closed over it.
    pub fn overlay_state(&self, active: bool, user_initiated: bool) -> Result<(), Error> {
        self.control(
            &protocol::HostCommand::OverlayState {
                active,
                user_initiated,
            },
            |_| Ok(()),
        )
    }
    /// Hands the running game the invitation `friend` sent into it, which
    /// the player accepted, from the host or from Steam's chat alike.
    pub fn accept_invite(&self, friend: u64, target: protocol::InviteTarget) -> Result<(), Error> {
        let command = match &target {
            protocol::InviteTarget::Lobby { lobby_id } => {
                protocol::HostCommand::AcceptLobbyInvite {
                    lobby_id: *lobby_id,
                    friend,
                }
            }
            protocol::InviteTarget::Connect { connect } => {
                protocol::HostCommand::AcceptRichInvite {
                    friend,
                    connect: connect.clone(),
                }
            }
        };
        self.control(&command, |inner| {
            inner
                .requests
                .invites
                .retain(|invite| invite.friend != friend || invite.target != target);
            Ok(())
        })
    }
    /// The player's answer to the purchase the game waits on: only that one.
    pub fn authorize_microtxn(&self, order_id: u64, approve: bool) -> Result<(), Error> {
        self.control(
            &protocol::HostCommand::AuthorizeMicroTxn { order_id, approve },
            |inner| match &inner.requests.microtxn {
                Some(pending) if pending.order_id == order_id => {
                    inner.requests.microtxn = None;
                    Ok(())
                }
                _ => Err(Error::InvalidSession),
            },
        )
    }
    pub fn started(&self, pid: u32) -> Result<(), Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::StartFailed)?;
        if inner.stopped {
            return Ok(());
        }
        // The game already exists. Record it even if its host has just died;
        // losing Steam must never turn into permission to terminate the game.
        Self::drain(&mut inner);
        inner.proof.game_pid = Some(pid);
        inner.proof.state = "game_started".into();
        files::report(&self.prepared.report, &inner.proof)
    }
    /// Wait on the private event pipe, independently of the UI, until the
    /// host's state, the game's invite request or its lobby differs from
    /// `previous`.
    /// `stopped` wakes this wait; Swift task cancellation must not abandon a
    /// live game.
    pub fn next_update(&self, previous: &HostUpdate) -> HostUpdate {
        loop {
            let generation = self.wakeup.generation();
            let Ok(mut inner) = self.inner.lock() else {
                return HostUpdate::new(HostState::Lost);
            };
            if inner.stopped {
                return HostUpdate::new(HostState::Stopped);
            }
            let before = (inner.proof.host_state, inner.proof.discovery_paths);
            // Sweep without blocking: `next` reports host death as an error, and
            // only returns `Ok(None)` once it has confirmed the host is alive.
            loop {
                match inner.host.next(Duration::ZERO) {
                    Ok(Some(event)) => Self::accept(&mut inner, event),
                    Ok(None) => break,
                    Err(_) => {
                        inner.proof.host_state = HostState::Lost;
                        break;
                    }
                }
            }
            if before != (inner.proof.host_state, inner.proof.discovery_paths) {
                let _ = files::report(&self.prepared.report, &inner.proof);
            }
            let update = inner.update();
            if update != *previous {
                return update;
            }
            // Never hold the runtime mutex while waiting for pipe input.
            drop(inner);
            self.wakeup.wait(generation);
        }
    }
    pub fn stopped(&self, exit: Option<i32>) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner.stopped {
                return;
            }
            inner.stopped = true;
            Self::drain(&mut inner);
            inner.host.stop();
            inner.proof.game_exit = exit;
            inner.proof.state = if inner.proof.game_pid.is_some() {
                "game_exited"
            } else {
                "stopped"
            }
            .into();
            let _ = files::report(&self.prepared.report, &inner.proof);
            self.wakeup.notify();
        }
    }
    fn drain(inner: &mut Inner) {
        while let Some(event) = inner.host.try_event() {
            Self::accept(inner, event);
        }
    }
    fn accept(inner: &mut Inner, event: Event) {
        let app = inner.proof.app_id;
        match event {
            Event::Discovery { paths } => inner.proof.discovery_paths = paths,
            // Off Steam, the game's lobby, the dialog that named it and the
            // purchase it asked about are gone; a reconnected engine reports
            // its lobby again.
            Event::Disconnected {} if inner.proof.host_state != HostState::Lost => {
                inner.proof.host_state = HostState::Disconnected;
                inner.requests.overlay = None;
                inner.requests.lobby = None;
                inner.requests.microtxn = None;
            }
            Event::Reconnected {} if inner.proof.host_state != HostState::Lost => {
                inner.proof.host_state = HostState::Online;
            }
            // Only the game this host serves, and only what Steam accepts.
            Event::OverlayRequested { app_id, request } if app_id == app && request.valid() => {
                inner.sequence += 1;
                inner.requests.overlay = Some(OverlayRequest {
                    sequence: inner.sequence,
                    dialog: request,
                });
            }
            Event::GameLobby { app_id, lobby_id } if app_id == app => {
                inner.requests.lobby = lobby_id.filter(|lobby| protocol::lobby(*lobby));
            }
            Event::LobbyInviteReceived {
                inviter,
                lobby_id,
                app_id,
            } if app_id == app => {
                inner.received(inviter, protocol::InviteTarget::Lobby { lobby_id });
            }
            Event::RichInviteReceived {
                friend,
                app_id,
                connect,
            } if app_id == app => {
                inner.received(friend, protocol::InviteTarget::Connect { connect });
            }
            Event::MicroTxnRequested { order_id, app_id } if app_id == app && order_id != 0 => {
                inner.requests.microtxn = Some(MicroTxn { order_id, app_id });
            }
            Event::MicroTxnDismissed { order_id }
                if inner
                    .requests
                    .microtxn
                    .as_ref()
                    .is_some_and(|pending| pending.order_id == order_id) =>
            {
                inner.requests.microtxn = None;
            }
            _ => {}
        }
    }
    fn write_report(&self) -> Result<(), Error> {
        files::report(
            &self.prepared.report,
            &self.inner.lock().map_err(|_| Error::StartFailed)?.proof,
        )
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stopped(None);
    }
}

#[cfg(test)]
mod launch_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn launch() -> Launch {
        Launch::new(Credentials {
            steam_id: 1,
            account_name: "fixture".into(),
            refresh_token: "fixture-token".into(),
        })
    }
    pub(crate) fn child(script: &str) -> process::Child {
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        process::Child::spawn(command).unwrap()
    }
    /// Parallel fixture processes may retain a dropped flock until exec. Bound
    /// the wait and unlock the probe before the caller reopens the lock file.
    pub(crate) fn wait_for_lock_release(path: &std::path::Path) {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        if lock.try_lock().is_ok() {
            lock.unlock().unwrap();
            return;
        }
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = lock.lock().and_then(|()| lock.unlock());
            let _ = send.send(result);
        });
        receive
            .recv_timeout(Duration::from_secs(2))
            .expect("dropped fixture lock remained held")
            .expect("probing the released fixture lock failed");
    }
    pub(crate) fn prepared(root: &std::path::Path) -> files::Prepared {
        files::Prepared {
            storage: None,
            directory: root.into(),
            run_directory: root.join("scratch"),
            profile: root.join("profile"),
            report: root.join("report.json"),
            endpoints: [
                ("Steam3Master".into(), "127.0.0.1:1".into()),
                ("SteamClientService".into(), "127.0.0.1:2".into()),
            ],
        }
    }
    pub(crate) fn runtime(script: &str, root: &std::path::Path) -> Runtime {
        let host = child(script);
        let wakeup = host.wakeup.clone();
        Runtime {
            inner: Mutex::new(Inner {
                proof: Proof {
                    app_id: 42,
                    host_pid: host.id(),
                    authenticated: true,
                    ticket_bytes: 178,
                    host_account_verified: true,
                    host_license_verified: true,
                    sdk_checked: false,
                    game_pid: None,
                    game_exit: None,
                    discovery_paths: 0,
                    host_state: HostState::Online,
                    state: "ready".into(),
                },
                host,
                requests: GameRequests::default(),
                sequence: 0,
                stopped: false,
            }),
            wakeup,
            service: "fixture".into(),
            prepared: prepared(root),
        }
    }
    fn signal(runtime: &Runtime) {
        use std::io::Write;
        runtime
            .inner
            .lock()
            .unwrap()
            .host
            .input
            .as_mut()
            .unwrap()
            .write_all(b"go\n")
            .unwrap();
    }
    fn state(state: HostState) -> HostUpdate {
        HostUpdate::new(state)
    }
    fn changed(runtime: &Arc<Runtime>, previous: HostState) -> HostState {
        updated(runtime, &state(previous)).state
    }
    fn updated(runtime: &Arc<Runtime>, previous: &HostUpdate) -> HostUpdate {
        let (send, receive) = std::sync::mpsc::channel();
        let observer = runtime.clone();
        let previous = previous.clone();
        let worker = std::thread::spawn(move || {
            let _ = send.send(observer.next_update(&previous));
        });
        let result = receive.recv_timeout(Duration::from_secs(3));
        if result.is_err() {
            runtime.stopped(None);
        }
        worker.join().unwrap();
        result.expect("host state did not change")
    }
    #[test]
    fn session_distinguishes_connection_recovery_and_host_loss_until_game_exit() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Arc::new(runtime(
            r#"read line; printf '%s\n' '{"event":"disconnected"}'; read line; printf '%s\n' '{"event":"reconnected"}'; read line; exit 0"#,
            root.path(),
        ));
        runtime.started(1234).unwrap();
        signal(&runtime);
        assert_eq!(
            changed(&runtime, HostState::Online),
            HostState::Disconnected
        );
        signal(&runtime);
        assert_eq!(
            changed(&runtime, HostState::Disconnected),
            HostState::Online
        );
        signal(&runtime);
        assert_eq!(changed(&runtime, HostState::Online), HostState::Lost);
        {
            let inner = runtime.inner.lock().unwrap();
            assert!(!inner.stopped);
            assert_eq!(inner.proof.game_pid, Some(1234));
            assert_eq!(inner.proof.state, "game_started");
        }
        runtime.stopped(Some(0));
        assert_eq!(
            runtime.next_update(&state(HostState::Lost)).state,
            HostState::Stopped
        );
        let proof: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap())
                .unwrap();
        assert_eq!(proof["state"], "game_exited");
        assert_eq!(proof["host_state"], "lost");
        assert_eq!(proof["game_exit"], 0);
    }
    #[test]
    fn overlay_requests_of_the_game_wake_the_observer_with_a_new_sequence() {
        let root = tempfile::tempdir().unwrap();
        let lobby = r#"'{"event":"overlay_requested","app_id":42,"request":{"lobby_invite":{"lobby_id":109775244421126053}}}'"#;
        let script = [
            // Another game's request and an individual account as a lobby
            // are ignored; the same lobby asked twice is two requests.
            r#"read line; printf '%s\n' '{"event":"overlay_requested","app_id":7,"request":{"lobby_invite":{"lobby_id":109775244421126053}}}' '{"event":"overlay_requested","app_id":42,"request":{"lobby_invite":{"lobby_id":76561197960265729}}}' LOBBY"#,
            "read line; printf '%s\\n' LOBBY",
            r#"read line; printf '%s\n' '{"event":"overlay_requested","app_id":42,"request":{"connect_invite":{"connect":"+connect 10.0.0.1"}}}'"#,
            r#"read line; printf '%s\n' '{"event":"overlay_requested","app_id":42,"request":{"web_page":{"url":"file:///etc/passwd","modal":false}}}' '{"event":"overlay_requested","app_id":42,"request":{"store":{"app_id":42,"flag":1}}}'"#,
            r#"read line; printf '%s\n' '{"event":"disconnected"}'"#,
            r#"read line; printf '%s\n' '{"event":"reconnected"}'"#,
            "read line; printf '%s\\n' LOBBY",
            "read line",
        ]
        .join("; ")
        .replace("LOBBY", lobby);
        let runtime = Arc::new(runtime(&script, root.path()));
        runtime.started(1234).unwrap();
        let request = |sequence, dialog: &protocol::OverlayDialog| OverlayRequest {
            sequence,
            dialog: dialog.clone(),
        };
        let invite = |sequence, target: &protocol::InviteTarget| InviteRequest {
            sequence,
            target: target.clone(),
        };
        let dialog = protocol::OverlayDialog::LobbyInvite {
            lobby_id: 109_775_244_421_126_053,
        };
        let target = protocol::InviteTarget::Lobby {
            lobby_id: 109_775_244_421_126_053,
        };
        signal(&runtime);
        let first = updated(&runtime, &state(HostState::Online));
        assert_eq!(first.state, HostState::Online);
        assert_eq!(first.requests.overlay, Some(request(1, &dialog)));
        assert_eq!(first.requests.invite(), Some(invite(1, &target)));
        signal(&runtime);
        let second = updated(&runtime, &first);
        assert_eq!(second.requests.invite(), Some(invite(2, &target)));
        signal(&runtime);
        let third = updated(&runtime, &second);
        let connect = protocol::InviteTarget::Connect {
            connect: "+connect 10.0.0.1".into(),
        };
        assert_eq!(third.requests.invite(), Some(invite(3, &connect)));
        signal(&runtime);
        let store = updated(&runtime, &third);
        assert_eq!(
            store.requests.overlay,
            Some(request(
                4,
                &protocol::OverlayDialog::Store {
                    app_id: 42,
                    flag: 1
                }
            )),
            "a file URL is no web page"
        );
        assert_eq!(
            store.requests.invite(),
            None,
            "the store is no invite dialog"
        );
        signal(&runtime);
        let disconnected = updated(&runtime, &store);
        assert_eq!(
            disconnected,
            state(HostState::Disconnected),
            "off Steam, the dialog's target is gone"
        );
        signal(&runtime);
        let reconnected = updated(&runtime, &disconnected);
        assert_eq!(reconnected, state(HostState::Online), "and it stays gone");
        signal(&runtime);
        let again = updated(&runtime, &reconnected);
        assert_eq!(
            again.requests.invite(),
            Some(invite(5, &target)),
            "the session's sequence keeps growing"
        );
        runtime.stopped(Some(0));
        assert_eq!(
            runtime.next_update(&again),
            state(HostState::Stopped),
            "nothing outlives the game"
        );
    }
    const FRIEND: u64 = 76_561_197_960_265_729;
    const OTHER: u64 = 76_561_197_960_265_730;
    const LOBBY: u64 = 109_775_244_421_126_053;
    /// A host that tells `events`, one batch per signal, then records every
    /// command it gets into `input`.
    fn recording(root: &std::path::Path, events: &[&str]) -> Arc<Runtime> {
        let mut script: Vec<String> = events
            .iter()
            .map(|batch| format!("read line; printf '%s\\n' {batch}"))
            .collect();
        script.push(format!("cat > '{}'", root.join("input").display()));
        let runtime = Arc::new(runtime(&script.join("; "), root));
        runtime.started(1234).unwrap();
        runtime
    }
    fn commands(root: &std::path::Path) -> Vec<protocol::HostCommand> {
        let wire = std::fs::read(root.join("input")).unwrap();
        let mut input = wire.as_slice();
        let mut commands = Vec::new();
        while let Some(command) = protocol::HostCommand::read(&mut input).unwrap() {
            commands.push(command);
        }
        commands
    }
    /// The first update after `previous` that is `done`: a batch of events
    /// may wake the observer more than once.
    fn until(
        runtime: &Arc<Runtime>,
        previous: &HostUpdate,
        done: impl Fn(&HostUpdate) -> bool,
    ) -> HostUpdate {
        let mut update = updated(runtime, previous);
        while !done(&update) {
            update = updated(runtime, &update);
        }
        update
    }
    fn received(friend: u64, target: protocol::InviteTarget) -> (u64, protocol::InviteTarget) {
        (friend, target)
    }
    fn invites(update: &HostUpdate) -> Vec<(u64, protocol::InviteTarget)> {
        update
            .requests
            .invites
            .iter()
            .map(|invite| received(invite.friend, invite.target.clone()))
            .collect()
    }
    #[test]
    fn received_invites_of_the_game_come_newest_first_once_each() {
        let root = tempfile::tempdir().unwrap();
        let lobby = format!(
            r#"'{{"event":"lobby_invite_received","inviter":{FRIEND},"lobby_id":{LOBBY},"app_id":42}}'"#
        );
        let rich = format!(
            r#"'{{"event":"rich_invite_received","friend":{OTHER},"app_id":42,"connect":"+connect 10.0.0.1"}}'"#
        );
        let ignored = [
            format!(r#"'{{"event":"lobby_invite_received","inviter":{FRIEND},"lobby_id":{LOBBY},"app_id":7}}'"#),
            format!(r#"'{{"event":"lobby_invite_received","inviter":{LOBBY},"lobby_id":{LOBBY},"app_id":42}}'"#),
            format!(r#"'{{"event":"lobby_invite_received","inviter":{FRIEND},"lobby_id":{FRIEND},"app_id":42}}'"#),
            format!(r#"'{{"event":"rich_invite_received","friend":{OTHER},"app_id":42,"connect":""}}'"#),
        ]
        .join(" ");
        let many: String = (1..=20)
            .map(|account| {
                format!(
                    r#"'{{"event":"rich_invite_received","friend":{},"app_id":42,"connect":"+connect 10.0.0.{account}"}}'"#,
                    FRIEND + account
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let batches = [
            format!("{lobby} {ignored} {rich}"),
            lobby.clone(),
            r#"'{"event":"disconnected"}'"#.to_owned(),
            r#"'{"event":"reconnected"}'"#.to_owned(),
            many,
        ];
        let runtime = recording(
            root.path(),
            &batches.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let lobby = || received(FRIEND, protocol::InviteTarget::Lobby { lobby_id: LOBBY });
        let rich = || {
            received(
                OTHER,
                protocol::InviteTarget::Connect {
                    connect: "+connect 10.0.0.1".into(),
                },
            )
        };
        signal(&runtime);
        let first = until(&runtime, &state(HostState::Online), |update| {
            update.requests.invites.len() == 2
        });
        assert_eq!(invites(&first), [rich(), lobby()]);
        signal(&runtime);
        let second = updated(&runtime, &first);
        assert_eq!(invites(&second), [lobby(), rich()], "again, first and once");
        signal(&runtime);
        let away = updated(&runtime, &second);
        assert_eq!(
            away,
            state(HostState::Disconnected),
            "off Steam, none shows"
        );
        signal(&runtime);
        let back = updated(&runtime, &away);
        assert_eq!(invites(&back), [lobby(), rich()], "Steam still holds them");
        signal(&runtime);
        let many = until(&runtime, &back, |update| {
            update.requests.invites.first().map(|invite| invite.friend) == Some(FRIEND + 20)
        });
        assert_eq!(many.requests.invites.len(), MAX_RECEIVED_INVITES);
        assert_eq!(many.requests.invites[0].friend, FRIEND + 20);
        assert_eq!(
            many.requests.invites[15].friend,
            FRIEND + 5,
            "the oldest go"
        );
        // Accepting one hands it to the game and takes it off the list.
        runtime
            .accept_invite(
                FRIEND + 20,
                protocol::InviteTarget::Connect {
                    connect: "+connect 10.0.0.20".into(),
                },
            )
            .unwrap();
        let accepted = updated(&runtime, &many);
        assert_eq!(accepted.requests.invites.len(), MAX_RECEIVED_INVITES - 1);
        assert_eq!(accepted.requests.invites[0].friend, FRIEND + 19);
        // An invitation from Steam's chat, never told by the host.
        runtime
            .accept_invite(FRIEND, protocol::InviteTarget::Lobby { lobby_id: LOBBY })
            .unwrap();
        assert!(matches!(
            runtime.accept_invite(LOBBY, protocol::InviteTarget::Lobby { lobby_id: LOBBY }),
            Err(Error::InvalidSession)
        ));
        runtime.stopped(Some(0));
        assert_eq!(runtime.next_update(&accepted), state(HostState::Stopped));
        assert_eq!(
            commands(root.path()),
            [
                protocol::HostCommand::AcceptRichInvite {
                    friend: FRIEND + 20,
                    connect: "+connect 10.0.0.20".into(),
                },
                protocol::HostCommand::AcceptLobbyInvite {
                    lobby_id: LOBBY,
                    friend: FRIEND,
                },
            ]
        );
        assert!(matches!(
            runtime.overlay_state(true, true),
            Err(Error::Unavailable)
        ));
    }
    #[test]
    fn only_the_purchase_the_game_waits_on_is_answered_once() {
        let root = tempfile::tempdir().unwrap();
        let runtime = recording(
            root.path(),
            &[
                r#"'{"event":"micro_txn_requested","order_id":7,"app_id":7}' '{"event":"micro_txn_requested","order_id":11,"app_id":42}'"#,
                r#"'{"event":"micro_txn_dismissed","order_id":12}' '{"event":"micro_txn_requested","order_id":12,"app_id":42}'"#,
                r#"'{"event":"micro_txn_dismissed","order_id":12}'"#,
                r#"'{"event":"micro_txn_requested","order_id":13,"app_id":42}'"#,
            ],
        );
        let pending = |order_id| {
            Some(MicroTxn {
                order_id,
                app_id: 42,
            })
        };
        signal(&runtime);
        let first = updated(&runtime, &state(HostState::Online));
        assert_eq!(first.requests.microtxn, pending(11), "only the game's own");
        signal(&runtime);
        let second = updated(&runtime, &first);
        assert_eq!(
            second.requests.microtxn,
            pending(12),
            "the latest replaces it"
        );
        signal(&runtime);
        let dismissed = updated(&runtime, &second);
        assert_eq!(dismissed.requests.microtxn, None);
        assert!(matches!(
            runtime.authorize_microtxn(12, true),
            Err(Error::InvalidSession)
        ));
        signal(&runtime);
        let third = updated(&runtime, &dismissed);
        assert_eq!(third.requests.microtxn, pending(13));
        assert!(matches!(
            runtime.authorize_microtxn(11, true),
            Err(Error::InvalidSession)
        ));
        runtime.authorize_microtxn(13, false).unwrap();
        let answered = updated(&runtime, &third);
        assert_eq!(answered.requests.microtxn, None, "answered once");
        assert!(matches!(
            runtime.authorize_microtxn(13, true),
            Err(Error::InvalidSession)
        ));
        runtime.overlay_state(true, false).unwrap();
        runtime.overlay_state(false, false).unwrap();
        runtime.stopped(Some(0));
        assert_eq!(
            commands(root.path()),
            [
                protocol::HostCommand::AuthorizeMicroTxn {
                    order_id: 13,
                    approve: false,
                },
                protocol::HostCommand::OverlayState {
                    active: true,
                    user_initiated: false,
                },
                protocol::HostCommand::OverlayState {
                    active: false,
                    user_initiated: false,
                },
            ]
        );
    }
    #[test]
    fn the_engines_lobby_follows_the_game_while_the_host_is_online() {
        let root = tempfile::tempdir().unwrap();
        let script = [
            r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":7,"lobby_id":109775244421126053}' '{"event":"game_lobby","app_id":42,"lobby_id":109775244421126053}'"#,
            r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":42,"lobby_id":109775244423207541}'"#,
            r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":42,"lobby_id":76561197960265729}'"#,
            r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":42,"lobby_id":109775244421126053}'"#,
            r#"read line; printf '%s\n' '{"event":"disconnected"}'"#,
            "read line",
        ]
        .join("; ");
        let runtime = Arc::new(runtime(&script, root.path()));
        runtime.started(1234).unwrap();
        let lobby = |lobby| HostUpdate {
            requests: GameRequests {
                lobby,
                ..GameRequests::default()
            },
            ..state(HostState::Online)
        };
        signal(&runtime);
        let first = updated(&runtime, &state(HostState::Online));
        assert_eq!(first, lobby(Some(109_775_244_421_126_053)));
        signal(&runtime);
        let second = updated(&runtime, &first);
        assert_eq!(second, lobby(Some(109_775_244_423_207_541)), "a new run");
        signal(&runtime);
        let third = updated(&runtime, &second);
        assert_eq!(third, lobby(None), "an account is no lobby");
        signal(&runtime);
        let fourth = updated(&runtime, &third);
        assert_eq!(fourth, lobby(Some(109_775_244_421_126_053)));
        signal(&runtime);
        assert_eq!(
            updated(&runtime, &fourth),
            state(HostState::Disconnected),
            "off Steam, the game is in no lobby"
        );
        runtime.stopped(Some(0));
    }
    #[test]
    fn game_exit_wakes_observer_and_immediate_exit_cannot_restore_started_state() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Arc::new(runtime("read line", root.path()));
        runtime.started(1234).unwrap();
        let observer = runtime.clone();
        let worker =
            std::thread::spawn(move || observer.next_update(&state(HostState::Online)).state);
        runtime.stopped(Some(0));
        assert_eq!(worker.join().unwrap(), HostState::Stopped);
        runtime.started(5678).unwrap();
        assert_eq!(runtime.inner.lock().unwrap().proof.game_pid, Some(1234));
        assert_eq!(runtime.inner.lock().unwrap().proof.state, "game_exited");
        runtime.stopped(Some(9));
        assert_eq!(runtime.inner.lock().unwrap().proof.game_exit, Some(0));
    }
    #[test]
    fn host_loss_after_spawn_still_records_the_live_game() {
        let root = tempfile::tempdir().unwrap();
        let runtime = runtime("read line", root.path());
        runtime.inner.lock().unwrap().host.stop();
        runtime.started(1234).unwrap();
        assert_eq!(
            runtime.next_update(&state(HostState::Online)).state,
            HostState::Lost
        );
        assert!(!runtime.inner.lock().unwrap().stopped);
        runtime.stopped(Some(0));
    }
    #[test]
    fn readiness_requires_authentication_matching_app_pid_and_nonempty_ticket() {
        for (prefix, app, pid, ticket) in [
            ("", 1030300, "$$", 178),
            (
                "printf '%s\\n' '{\"event\":\"authenticated\"}';",
                999,
                "$$",
                178,
            ),
            (
                "printf '%s\\n' '{\"event\":\"authenticated\"}';",
                1030300,
                "0",
                178,
            ),
            (
                "printf '%s\\n' '{\"event\":\"authenticated\"}';",
                1030300,
                "$$",
                0,
            ),
        ] {
            let mut host = child(&format!(
                "{prefix} printf '{{\"event\":\"ready\",\"app_id\":{app},\"host_pid\":%s,\"ticket_bytes\":{ticket}}}\\n' \"{pid}\"; read line"
            ));
            assert!(matches!(
                launch().wait_for_host(&mut host, 1030300, &mut Vec::new()),
                Err(Error::StartFailed)
            ));
            host.stop();
            assert!(!host.alive().unwrap());
        }
        let mut host = child(
            "printf '%s\\n' '{\"event\":\"authenticated\"}'; printf '{\"event\":\"ready\",\"app_id\":1030300,\"host_pid\":%s,\"ticket_bytes\":178}\\n' \"$$\"; read line",
        );
        assert_eq!(
            launch()
                .wait_for_host(&mut host, 1030300, &mut Vec::new())
                .unwrap(),
            178
        );
        host.stop();
        assert!(!host.alive().unwrap());
    }
    #[test]
    fn cancellation_interrupts_waiting_for_a_live_helper() {
        let launch = Arc::new(launch());
        let mut host = child("read line");
        let cancel = launch.clone();
        let cancellation = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let started = Instant::now();
        assert!(matches!(
            launch.wait_for_host(&mut host, 1030300, &mut Vec::new()),
            Err(Error::Cancelled)
        ));
        host.stop();
        cancellation.join().unwrap();
        assert!(!host.alive().unwrap());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn dead_or_malformed_helper_never_becomes_ready() {
        for script in ["exit 1", "printf '%s\\n' 'native token=fixture'"] {
            let mut host = child(script);
            assert!(matches!(
                launch().wait_for_host(&mut host, 1030300, &mut Vec::new()),
                Err(Error::StartFailed)
            ));
        }
    }
}
