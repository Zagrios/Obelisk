//! The Steam host of a Steam game: the sandboxed process that holds the
//! account's Steam session for one game, from the pinned Steam components,
//! and what it tells of the game's requests while the game runs.
mod files;
mod licenses;
mod process;

use super::{Control, Error};
use crate::{
    package,
    protocol::{self, Credentials, Event},
    supervisor, unix_now, wine,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub fn native_cache_checkpoint(directory: PathBuf, app_id: u32, platforms: u32) -> bool {
    native_cache_files(directory, app_id, platforms).is_ok()
}
/// A published projection always carries its index, so a missing `remotecache.vdf`
/// is an incomplete projection rather than an empty one. `SdkCloudPaths::records`
/// cannot tell the two apart, so the distinction is enforced here for every caller.
pub(crate) fn native_cache_files(
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
    /// Obelisk's library, which the native host may not read, wherever the
    /// player keeps it.
    #[serde(default)]
    pub library: PathBuf,
    pub app_id: u32,
    #[serde(default)]
    pub cloud_save_roots: Vec<PathBuf>,
    /// Set for a Windows game, which runs in Obelisk's Wine runtime.
    #[serde(default)]
    pub wine: Option<wine::Config>,
}

/// Where the pinned Steam client keeps its files under the `HOME` it runs
/// with, which is the host's private home. It is not the player's own Steam,
/// which the host can neither read nor need.
const STEAM_DIRECTORY: &str = "Library/Application Support/Steam";

/// One start of the Steam host, which another thread may cancel. Its
/// credentials go to the host once.
pub(crate) struct Startup {
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
        self.home.join(STEAM_DIRECTORY)
    }
    /// The account- and app-specific SDK cloud cache the coordinator projects into.
    pub fn cache_directory(&self) -> PathBuf {
        self.steam_directory().join(format!(
            "userdata/{}/{}",
            self.account_id as u32, self.app_id
        ))
    }
}
impl Startup {
    pub(crate) fn new(credentials: Credentials) -> Self {
        Self {
            credentials: Mutex::new(Some(credentials)),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    /// Starts the native host. `storage` is the Steam Cloud cache the game's
    /// SDK reads, when Cloud runs for this launch.
    pub(crate) fn prepare(
        &self,
        config: Config,
        storage: Option<NativeStorage>,
    ) -> Result<Host, Error> {
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
    ) -> Result<Host, Error> {
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
        self.prepare_host(
            config,
            storage,
            events,
            credentials,
            prepared,
            process::Child::host,
        )
    }
    fn prepare_host(
        &self,
        config: &Config,
        storage: Option<NativeStorage>,
        events: &mut Vec<Event>,
        credentials: Credentials,
        mut prepared: files::Prepared,
        start_host: impl FnOnce(&files::Prepared, &Config, &str) -> Result<process::Child, Error>,
    ) -> Result<Host, Error> {
        let id = credentials.steam_id;
        if let Some(storage) = &storage {
            if storage.account_id != id || storage.app_id != config.app_id {
                return Err(Error::InvalidSession);
            }
            prepared.enable_storage(storage)?;
        }
        // A cache that cannot be kept only costs the launch its head start.
        let _ = prepared.enable_licenses(&config.cache, id);
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
        let host = Host {
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
        host.write_report()?;
        Ok(host)
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
            if start.elapsed() > protocol::SUPERVISOR_STARTUP_TIMEOUT {
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
    /// A host running `script` from `root` instead, unless this start was
    /// cancelled.
    #[cfg(test)]
    pub(crate) fn scripted(&self, script: &str, root: &Path) -> Result<Host, Error> {
        self.check_cancelled()?;
        Ok(tests::host(script, root))
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
struct Proof {
    app_id: u32,
    host_pid: u32,
    authenticated: bool,
    ticket_bytes: u32,
    host_account_verified: bool,
    host_license_verified: bool,
    sdk_checked: bool,
    game_pid: Option<u32>,
    game_exit: Option<i32>,
    discovery_paths: u32,
    host_state: HostState,
    state: String,
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
const MAX_RECEIVED_INVITES: usize = 16;

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
/// A started Steam host; dropping it stops it.
pub(crate) struct Host {
    inner: Mutex<Inner>,
    wakeup: Arc<process::Wake>,
    service: String,
    prepared: files::Prepared,
}
impl Host {
    pub(crate) fn environment(&self) -> std::collections::HashMap<String, String> {
        self.prepared.consumer_environment(&self.service)
    }
    /// The Steam components it runs from, which the game's own Steam API
    /// loads too.
    pub(crate) fn directory(&self) -> &Path {
        &self.prepared.directory
    }
    pub(crate) fn set_presence(
        &self,
        status: Option<obelisk_core::OnlineStatus>,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::StartFailed)?;
        if inner.stopped {
            return Ok(());
        }
        Self::send(&mut inner, &protocol::HostCommand::SetPresence { status })
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
    fn command(
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
    /// The player's control of the game it serves, while it serves it online.
    pub(crate) fn control(&self, control: Control) -> Result<(), supervisor::Error> {
        match control {
            Control::Overlay {
                active,
                user_initiated,
            } => self.overlay_state(active, user_initiated),
            Control::AcceptInvite { friend, target } => self.accept_invite(friend, target),
            Control::AuthorizeMicroTxn { order_id, approve } => {
                self.authorize_microtxn(order_id, approve)
            }
        }
        .map_err(|error| match error {
            Error::InvalidSession => supervisor::Error::InvalidSession,
            _ => supervisor::Error::Unavailable,
        })
    }
    /// Tells the game Obelisk's overlay opened or closed over it.
    fn overlay_state(&self, active: bool, user_initiated: bool) -> Result<(), Error> {
        self.command(
            &protocol::HostCommand::OverlayState {
                active,
                user_initiated,
            },
            |_| Ok(()),
        )
    }
    /// Hands the running game the invitation `friend` sent into it, which
    /// the player accepted, from the host or from Steam's chat alike.
    fn accept_invite(&self, friend: u64, target: protocol::InviteTarget) -> Result<(), Error> {
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
        self.command(&command, |inner| {
            inner
                .requests
                .invites
                .retain(|invite| invite.friend != friend || invite.target != target);
            Ok(())
        })
    }
    /// The player's answer to the purchase the game waits on: only that one.
    fn authorize_microtxn(&self, order_id: u64, approve: bool) -> Result<(), Error> {
        self.command(
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
    pub(crate) fn started(&self, pid: u32) -> Result<(), Error> {
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
    pub(crate) fn next_update(&self, previous: &HostUpdate) -> HostUpdate {
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
            // Never hold the host's mutex while waiting for pipe input.
            drop(inner);
            self.wakeup.wait(generation);
        }
    }
    pub(crate) fn stopped(&self, exit: Option<i32>) {
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
    #[cfg(test)]
    pub(crate) fn pid(&self) -> u32 {
        self.inner.lock().unwrap().host.id()
    }
    fn write_report(&self) -> Result<(), Error> {
        files::report(
            &self.prepared.report,
            &self.inner.lock().map_err(|_| Error::StartFailed)?.proof,
        )
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stopped(None);
    }
}

#[cfg(test)]
mod start_tests;

#[cfg(test)]
mod tests;
