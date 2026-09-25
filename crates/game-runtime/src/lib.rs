//! Safe launcher-side supervision. Native ABI code lives in the separate helper.
pub mod cloud;
pub mod cloud_status;
pub mod epic;
mod files;
mod native;
pub mod package;
#[cfg(feature = "packaging")]
pub mod packaging;
mod process;
pub mod protocol;
pub mod steam;
pub mod supervisor;
pub mod wine;
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
    #[error("backend.steam.runtime.incompatibleArchitecture")]
    IncompatibleArchitecture,
    #[error("backend.runtime.rosettaUnavailable")]
    RosettaUnavailable,
    #[error("backend.runtime.invalidApplication")]
    InvalidApplication,
    #[error("backend.runtime.notExecutable")]
    NotExecutable,
    #[error("backend.steam.runtime.apiMissing")]
    ApiMissing,
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
    pub steam_api: PathBuf,
    pub home: PathBuf,
    pub app_id: u32,
    #[serde(default)]
    pub cloud_save_roots: Vec<PathBuf>,
    /// Set for a Windows game, which runs in Forge's Wine runtime.
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
    pub fn start_supervised(&self, mut request: steam::Request) -> Result<steam::Client, Error> {
        self.check_cancelled()?;
        let credentials = self
            .credentials
            .lock()
            .map_err(|_| Error::InvalidSession)?
            .take()
            .ok_or(Error::InvalidSession)?;
        request.id = uuid::Uuid::new_v4().to_string();
        let helper = request.config.helpers.join(package::HOST);
        steam::start(&helper, &credentials, request, &self.cancelled)
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
            "com.zagrios.forge.discovery.{}",
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
                stopped: false,
            }),
            wakeup,
            adapter: prepared.directory.join(package::ADAPTER),
            service,
            report: prepared.report.clone(),
            _prepared: prepared,
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
struct Inner {
    host: process::Child,
    proof: Proof,
    stopped: bool,
}
pub struct Runtime {
    inner: Mutex<Inner>,
    wakeup: Arc<process::Wake>,
    adapter: PathBuf,
    service: String,
    report: PathBuf,
    _prepared: files::Prepared,
}
impl Runtime {
    pub fn environment(&self) -> std::collections::HashMap<String, String> {
        self._prepared.consumer_environment(&self.service)
    }
    pub fn adapter(&self) -> String {
        self.adapter.to_string_lossy().into_owned()
    }
    pub fn service(&self) -> String {
        self.service.clone()
    }
    pub fn diagnostic_path(&self) -> String {
        self.report.to_string_lossy().into_owned()
    }
    pub(crate) fn set_presence(&self, online: bool) -> Result<(), Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::StartFailed)?;
        if inner.stopped {
            return Ok(());
        }
        protocol::HostCommand::SetPresence { online }
            .write(inner.host.input.as_mut().ok_or(Error::StartFailed)?)
            .map_err(|_| Error::StartFailed)
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
        files::report(&self.report, &inner.proof)
    }
    /// Wait on the private event pipe, independently of the UI. `stopped` wakes
    /// this wait; Swift task cancellation must not abandon a live game.
    pub fn next_state(&self, previous: HostState) -> HostState {
        loop {
            let generation = self.wakeup.generation();
            let Ok(mut inner) = self.inner.lock() else {
                return HostState::Lost;
            };
            if inner.stopped {
                return HostState::Stopped;
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
                let _ = files::report(&self.report, &inner.proof);
            }
            if inner.proof.host_state != previous {
                return inner.proof.host_state;
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
            let _ = files::report(&self.report, &inner.proof);
            self.wakeup.notify();
        }
    }
    fn drain(inner: &mut Inner) {
        while let Some(event) = inner.host.try_event() {
            Self::accept(inner, event);
        }
    }
    fn accept(inner: &mut Inner, event: Event) {
        match event {
            Event::Discovery { paths } => inner.proof.discovery_paths = paths,
            Event::Disconnected {} if inner.proof.host_state != HostState::Lost => {
                inner.proof.host_state = HostState::Disconnected;
            }
            Event::Reconnected {} if inner.proof.host_state != HostState::Lost => {
                inner.proof.host_state = HostState::Online;
            }
            _ => {}
        }
    }
    fn write_report(&self) -> Result<(), Error> {
        files::report(
            &self.report,
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
                stopped: false,
            }),
            wakeup,
            adapter: root.join("adapter"),
            service: "fixture".into(),
            report: root.join("report.json"),
            _prepared: prepared(root),
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
    fn changed(runtime: &Arc<Runtime>, previous: HostState) -> HostState {
        let (send, receive) = std::sync::mpsc::channel();
        let observer = runtime.clone();
        let worker = std::thread::spawn(move || {
            let _ = send.send(observer.next_state(previous));
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
        assert_eq!(runtime.next_state(HostState::Lost), HostState::Stopped);
        let proof: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap())
                .unwrap();
        assert_eq!(proof["state"], "game_exited");
        assert_eq!(proof["host_state"], "lost");
        assert_eq!(proof["game_exit"], 0);
    }
    #[test]
    fn game_exit_wakes_observer_and_immediate_exit_cannot_restore_started_state() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Arc::new(runtime("read line", root.path()));
        runtime.started(1234).unwrap();
        let observer = runtime.clone();
        let worker = std::thread::spawn(move || observer.next_state(HostState::Online));
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
        assert_eq!(runtime.next_state(HostState::Online), HostState::Lost);
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
