use super::*;
use crate::{
    Error, Launch, Runtime, architecture::Architecture, presence::GamePresence,
    protocol::Credentials,
};
use std::{
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    process::{Command as Process, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

// Keep lifecycle admission testable without giving a disposable fixture access
// to account credentials, network transports or the production Cloud vault.
pub(super) type Operation<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

pub(super) trait CloudLifecycle {
    fn prepare(&mut self) -> Operation<'_, Result<(), Error>>;
    fn started(&mut self) -> Result<(), Error>;
    fn finished(&mut self) -> Operation<'_, ()>;
    fn aborted(&mut self);
    fn native_storage(&self) -> Result<Option<crate::NativeStorage>, Error> {
        Ok(None)
    }
}

impl CloudLifecycle for cloud::Session {
    fn native_storage(&self) -> Result<Option<crate::NativeStorage>, Error> {
        cloud::Session::native_storage(self)
    }
    fn prepare(&mut self) -> Operation<'_, Result<(), Error>> {
        Box::pin(cloud::Session::prepare(self))
    }
    fn started(&mut self) -> Result<(), Error> {
        cloud::Session::started(self)
    }
    fn finished(&mut self) -> Operation<'_, ()> {
        Box::pin(cloud::Session::finished(self))
    }
    fn aborted(&mut self) {
        cloud::Session::aborted(self);
    }
}

struct State {
    snapshot: Snapshot,
    spawned: bool,
}
struct Shared {
    state: Mutex<State>,
    // Cloud callbacks update state. Keep the spawn/cancel gate separate so a
    // durable started() operation can publish status without locking itself out.
    admission: Mutex<()>,
    changed: Condvar,
    root: PathBuf,
    launch: Arc<Launch>,
    cloud: Arc<cloud::Control>,
    presence: GamePresence,
    rosetta: crate::rosetta::Gate,
}
impl Shared {
    /// Wake both the blocking observers and the executor's Rosetta wait.
    fn notify_changed(&self) {
        self.changed.notify_all();
        self.rosetta.wake();
    }

    fn update(&self, change: impl FnOnce(&mut Snapshot)) {
        let mut state = self.state.lock().unwrap();
        change(&mut state.snapshot);
        state.snapshot.revision += 1;
        // Disk failure after spawn cannot abandon or terminate the game. Live
        // observers still receive the authoritative in-memory state.
        let _ = storage::write(&self.root, &state.snapshot);
        self.notify_changed();
    }

    async fn wait_for_rosetta(&self) -> Result<bool, Error> {
        self.rosetta
            .wait(
                || {
                    let _admission = self.admission.lock().unwrap();
                    self.launch.check_cancelled()?;
                    self.update(|s| s.rosetta = Some(RosettaRequest { decision_id: 1 }));
                    Ok(())
                },
                || {
                    self.launch.check_cancelled()?;
                    Ok(self.state.lock().unwrap().snapshot.rosetta.is_some())
                },
            )
            .await
    }

    fn record_starting(&self, starting: StartingGame) -> Result<(), Error> {
        let mut state = self.state.lock().unwrap();
        state.snapshot.starting = Some(starting);
        state.snapshot.revision += 1;
        // Unlike diagnostics, this write must succeed before the Intel helper
        // receives permission to replace itself with the game.
        let result = storage::write(&self.root, &state.snapshot);
        self.notify_changed();
        result
    }
}

#[derive(Clone, Copy)]
pub(super) struct NativeChecks {
    pub select: fn(&Target, &Config) -> Result<Option<Architecture>, Error>,
    pub rosetta: fn(&std::path::Path) -> Result<bool, Error>,
}
impl NativeChecks {
    #[cfg(test)]
    fn fixture() -> Self {
        Self {
            select: |_, _| Ok(None),
            rosetta: |_| Ok(true),
        }
    }
}

pub fn serve(input: impl Read, output: impl Write, inspect: Inspect) -> Result<(), Error> {
    serve_with_native(
        input,
        output,
        inspect,
        |launch, config| launch.prepare(config),
        NativeChecks {
            select: |target, config| {
                Architecture::select(&target.executable, &config.steam_api).map(Some)
            },
            rosetta: crate::architecture::rosetta_available,
        },
        GamePresence::default(),
    )
}

#[cfg(test)]
pub(super) fn serve_with(
    input: impl Read,
    output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error> + Send + 'static,
    presence: GamePresence,
) -> Result<(), Error> {
    serve_with_native(
        input,
        output,
        inspect,
        prepare,
        NativeChecks::fixture(),
        presence,
    )
}

fn serve_with_native(
    input: impl Read,
    output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error> + Send + 'static,
    checks: NativeChecks,
    presence: GamePresence,
) -> Result<(), Error> {
    serve_with_factory(
        input,
        output,
        inspect,
        prepare,
        |config, credentials, id, control| match (config, credentials) {
            (Some(config), Some(credentials)) => {
                Ok(Some(
                    Box::new(cloud::Session::new(config, credentials, id, control)?)
                        as Box<dyn CloudLifecycle>,
                ))
            }
            (None, None) => Ok(None),
            _ => Err(Error::InvalidSession),
        },
        checks,
        presence,
    )
}

#[cfg(test)]
pub(super) fn serve_with_cloud(
    input: impl Read,
    output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error> + Send + 'static,
    cloud: Box<dyn CloudLifecycle>,
    presence: GamePresence,
) -> Result<(), Error> {
    serve_with_cloud_checks(
        input,
        output,
        inspect,
        prepare,
        cloud,
        NativeChecks::fixture(),
        presence,
    )
}

#[cfg(test)]
pub(super) fn serve_with_cloud_checks(
    input: impl Read,
    output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error> + Send + 'static,
    cloud: Box<dyn CloudLifecycle>,
    checks: NativeChecks,
    presence: GamePresence,
) -> Result<(), Error> {
    serve_with_factory(
        input,
        output,
        inspect,
        prepare,
        |_, _, _, _| Ok(Some(cloud)),
        checks,
        presence,
    )
}

fn serve_with_factory(
    input: impl Read,
    mut output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error> + Send + 'static,
    make_cloud: impl FnOnce(
        Option<cloud::Config>,
        Option<Credentials>,
        String,
        Arc<cloud::Control>,
    ) -> Result<Option<Box<dyn CloudLifecycle>>, Error>,
    checks: NativeChecks,
    presence: GamePresence,
) -> Result<(), Error> {
    let mut input = input;
    let executor = crate::executor()?;
    let credentials = Credentials::read(&mut input)?;
    let request: Request = storage::packet(input)?;
    if uuid::Uuid::parse_str(&request.id).is_err()
        || request.config.app_id == 0
        || request
            .cloud
            .as_ref()
            .is_some_and(|cloud| cloud.app_id != request.config.app_id)
    {
        return Err(Error::InvalidSession);
    }
    let _lock = storage::lock(&request.root, "owner.lock")?;
    if let Some(old) = storage::read(&request.root)?
        && !old.finished()
        && (old
            .game
            .as_ref()
            .is_some_and(|g| inspect(g.pid).as_ref() == Some(g))
            || old.phase == Phase::Running && old.game.is_none())
    {
        return Err(Error::SessionBusy);
    }
    // A short socket path avoids macOS's 104-byte sockaddr_un limit, including
    // when the account home is long. tempfile creates an unpredictable 0700 dir.
    let socket_dir = tempfile::Builder::new()
        .prefix("forge-session-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    let socket = socket_dir.path().join("control");
    let listener = UnixListener::bind(&socket)?;
    // Owners retain secrets only in zeroizing Credentials values. Neither
    // the supervisor journal nor the serialized request contains credentials.
    let cloud_credentials = request.cloud.as_ref().map(|_| credentials.clone());
    let account_id = credentials.steam_id;
    let session_id = request.id.clone();
    let snapshot = Snapshot {
        id: request.id,
        app_id: request.config.app_id,
        account_id: credentials.steam_id,
        supervisor: inspect(std::process::id()).ok_or(Error::StartFailed)?,
        game: None,
        socket: socket.clone(),
        phase: Phase::Preparing,
        host: HostState::Online,
        exit_code: None,
        error: None,
        revision: 1,
        cloud: cloud::Status::default(),
        reconnecting: false,
        rosetta: None,
        starting: None,
    };
    storage::write(&request.root, &snapshot)?;
    let shared = Arc::new_cyclic(|weak: &std::sync::Weak<Shared>| {
        let observer = weak.clone();
        Shared {
            state: Mutex::new(State {
                snapshot,
                spawned: false,
            }),
            admission: Mutex::new(()),
            changed: Condvar::new(),
            root: request.root,
            launch: Arc::new(Launch::new(credentials)),
            cloud: Arc::new(cloud::Control::new(account_id, move |status| {
                if let Some(shared) = observer.upgrade() {
                    shared.update(|snapshot| snapshot.cloud = status);
                }
            })),
            presence,
            rosetta: crate::rosetta::Gate::new(
                crate::rosetta::helper(
                    request
                        .config
                        .helpers
                        .parent()
                        .unwrap_or(&request.config.helpers),
                ),
                checks.rosetta,
            ),
        }
    });
    let finished = Arc::new(AtomicBool::new(false));
    let listening = {
        let shared = shared.clone();
        let finished = finished.clone();
        std::thread::spawn(move || listen(listener, shared, finished))
    };
    // Publish and listen before acknowledging. Losing the parent here never
    // revokes an accepted launch or ownership of its game.
    let _ = storage::send(&mut output, &shared.state.lock().unwrap().snapshot);
    drop(output);
    let outcome = executor.block_on(async {
        let mut cloud = make_cloud(
            request.cloud,
            cloud_credentials,
            session_id,
            shared.cloud.clone(),
        )?;
        let lifecycle = cloud
            .as_mut()
            .map(|cloud| cloud.as_mut() as &mut dyn CloudLifecycle);
        let preparing = shared.clone();
        let native_target = request.target.clone();
        let result = run_game(
            &request.target,
            &shared,
            inspect,
            lifecycle,
            || async move {
                let architecture = (checks.select)(&native_target, &request.config)?;
                if architecture == Some(Architecture::Intel) && cfg!(target_arch = "aarch64") {
                    preparing.wait_for_rosetta().await?;
                }
                tokio::task::spawn_blocking(move || {
                    *preparing
                        .launch
                        .architecture
                        .lock()
                        .map_err(|_| Error::InvalidSession)? = architecture;
                    prepare(&preparing.launch, request.config)
                })
                .await
                .map_err(|_| Error::StartFailed)?
            },
        )
        .await;
        let abort = result.is_err() && !shared.state.lock().unwrap().spawned;
        if abort && let Some(cloud) = cloud.as_mut() {
            cloud.aborted();
        }
        result
    });
    shared.presence.stop();
    if let Err(error) = outcome {
        shared.update(|s| {
            s.phase = Phase::Failed;
            s.error = Some(error.to_string());
            s.host = HostState::Stopped;
            s.rosetta = None;
            s.starting = None;
        });
    }
    finished.store(true, Ordering::Release);
    // Wake blocking accept even when no UI is connected.
    if UnixStream::connect(&socket).is_ok() || listening.is_finished() {
        let _ = listening.join();
    }
    // If the socket was removed externally, do not wait forever on accept.
    // This dedicated supervisor process exits next and closes its listener.
    // socket_dir and the owner lock are released only after game/host teardown,
    // and after the executor's bounded shutdown.
    drop(executor);
    Ok(())
}

async fn run_game<F>(
    target: &Target,
    shared: &Arc<Shared>,
    inspect: Inspect,
    mut cloud: Option<&mut dyn CloudLifecycle>,
    prepare: impl FnOnce() -> F,
) -> Result<(), Error>
where
    F: std::future::Future<Output = Result<Runtime, Error>>,
{
    use crate::game_session::{Execution, confirm_execution, wait_for_exit, wait_identity};
    use std::os::fd::OwnedFd;

    if let Some(cloud) = cloud.as_deref_mut() {
        cloud.prepare().await?;
        *shared
            .launch
            .storage
            .lock()
            .map_err(|_| Error::InvalidSession)? = cloud.native_storage()?;
    }
    let runtime = Arc::new(prepare().await?);
    let app_id = shared.state.lock().unwrap().snapshot.app_id.to_string();
    let architecture = *shared
        .launch
        .architecture
        .lock()
        .map_err(|_| Error::InvalidSession)?;
    let intel = architecture == Some(Architecture::Intel);
    let mut command = if intel {
        let mut command = Process::new(runtime._prepared.directory.join(crate::package::SDK_INTEL));
        command.arg("exec-game").arg(&target.executable);
        command
    } else {
        Process::new(&target.executable)
    };
    // A private synchronous gate can be granted under the admission mutex;
    // the child's asynchronous output and wait never hold that mutex.
    let mut admission = if intel {
        let executable = target
            .executable
            .canonicalize()
            .map_err(|_| Error::StartFailed)?;
        let (parent, child) = UnixStream::pair()?;
        parent.set_write_timeout(Some(Duration::from_secs(5)))?;
        command
            .stdin(Stdio::from(OwnedFd::from(child)))
            .stdout(Stdio::piped());
        Some((executable, parent))
    } else {
        command.stdin(Stdio::null()).stdout(Stdio::null());
        None
    };
    command
        .args(&target.arguments)
        .current_dir(&target.working_directory)
        .env_clear()
        .envs(&target.environment)
        .envs(runtime.environment())
        .env("SteamAppId", &app_id)
        .env("SteamGameId", &app_id)
        .stderr(Stdio::null());
    let (mut game, admitted) = {
        // Cancel and spawn have one linearization point. Cancellation after this
        // succeeds never kills a game, including sign-out and UI disconnection.
        let _admission = shared.admission.lock().unwrap();
        if shared.launch.cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if let Some(cloud) = cloud.as_deref_mut() {
            cloud.started()?;
        }
        let game = tokio::process::Command::from(command)
            .kill_on_drop(false)
            .spawn()
            .map_err(|_| Error::StartFailed)?;
        shared.state.lock().unwrap().spawned = true;
        let admitted = match &mut admission {
            Some((executable, pipe)) => (|| {
                let identity =
                    inspect(game.id().ok_or(Error::StartFailed)?).ok_or(Error::StartFailed)?;
                shared.record_starting(StartingGame::Process {
                    identity,
                    executable: executable.clone(),
                })?;
                pipe.write_all(b"1").map_err(|_| Error::StartFailed)
            })(),
            None => Ok(()),
        };
        (game, admitted)
    };
    drop(admission);
    if let Err(error) = admitted {
        // Without the admission byte, EOF makes the helper exit without exec.
        // Retain ownership until it is reaped, outside the cancellation gate.
        let exit = wait_for_exit(&mut game).await;
        runtime.stopped(Some(exit));
        shared.state.lock().unwrap().spawned = false;
        shared.update(|s| s.starting = None);
        return Err(error);
    }
    let game_pid = game.id().ok_or(Error::StartFailed)?;
    let execution = if intel {
        confirm_execution(&mut game, &target.executable, inspect).await
    } else {
        Execution::Started
    };
    if matches!(execution, Execution::Failed) {
        let exit = wait_for_exit(&mut game).await;
        runtime.stopped(Some(exit));
        shared.state.lock().unwrap().spawned = false;
        return Err(Error::StartFailed);
    }
    let identity = if intel {
        wait_identity(&mut game, &target.executable, inspect).await
    } else {
        inspect(game_pid)
    };
    let started = matches!(execution, Execution::Started);
    if started {
        // Revoke uses this same gate. Its permanent stop fences a delayed exec.
        let _admission = shared.admission.lock().unwrap();
        shared.presence.start(runtime.clone());
        let _ = runtime.started(game_pid);
        shared.update(|s| {
            if identity.is_some() {
                s.starting = None;
            }
            s.game = identity;
            s.phase = Phase::Running;
        });
    }
    let observation = {
        let runtime = runtime.clone();
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut previous = HostState::Online;
            loop {
                let next = runtime.next_state(previous);
                if next == HostState::Stopped {
                    break;
                }
                shared.update(|s| s.host = next);
                if next == HostState::Lost {
                    break;
                }
                previous = next;
            }
        })
    };
    // Both stores await the same parent-owned exit while network tasks progress.
    let exit = wait_for_exit(&mut game).await;
    shared.presence.stop();
    runtime.stopped(Some(exit));
    let _ = observation.join();
    if let Some(cloud) = cloud {
        shared.update(|s| {
            s.phase = Phase::Finalizing;
            s.host = HostState::Stopped;
            s.exit_code = Some(exit);
        });
        cloud.finished().await;
    }
    shared.update(|s| {
        s.phase = if started {
            Phase::Finished
        } else {
            Phase::Failed
        };
        s.host = HostState::Stopped;
        s.exit_code = Some(exit);
        s.starting = None;
        if !started {
            s.error = Some(Error::StartFailed.to_string());
        }
    });
    Ok(())
}

fn listen(listener: UnixListener, shared: Arc<Shared>, finished: Arc<AtomicBool>) {
    let workers = Arc::new(AtomicUsize::new(0));
    while !finished.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if finished.load(Ordering::Acquire) {
                    break;
                }
                if workers.load(Ordering::Acquire) >= 16 {
                    continue;
                }
                workers.fetch_add(1, Ordering::AcqRel);
                let workers = workers.clone();
                let shared = shared.clone();
                std::thread::spawn(move || {
                    let _ = respond(stream, &shared);
                    workers.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn respond(mut stream: UnixStream, shared: &Shared) -> Result<(), Error> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let command: Command = storage::packet(&stream)?;
    match command {
        Command::Observe { id, after } => {
            let mut state = shared.state.lock().unwrap();
            if state.snapshot.id != id {
                return Err(Error::InvalidSession);
            }
            if after == Some(state.snapshot.revision) && !state.snapshot.finished() {
                state = shared
                    .changed
                    .wait_timeout_while(state, Duration::from_secs(15), |s| {
                        after == Some(s.snapshot.revision) && !s.snapshot.finished()
                    })
                    .unwrap()
                    .0;
            }
            let snapshot = state.snapshot.clone();
            drop(state);
            return storage::send(&mut stream, &snapshot);
        }
        Command::Cancel { id } => {
            let _admission = shared.admission.lock().unwrap();
            let state = shared.state.lock().unwrap();
            if state.snapshot.id != id {
                return Err(Error::InvalidSession);
            }
            let preparing = !state.spawned;
            if preparing {
                shared.launch.cancel();
                shared.notify_changed();
            }
            drop(state);
            if preparing {
                shared.cloud.cancel();
            }
        }
        Command::CloudChoice {
            id,
            account_id,
            decision_id,
            choice,
        } => {
            let state = shared.state.lock().unwrap();
            if state.snapshot.id != id || state.snapshot.account_id != account_id {
                return Err(Error::InvalidSession);
            }
            drop(state);
            shared.cloud.respond(decision_id, choice)?;
        }
        Command::Revoke { id, account_id } => {
            let _admission = shared.admission.lock().unwrap();
            let state = shared.state.lock().unwrap();
            if state.snapshot.id != id || state.snapshot.account_id != account_id {
                return Err(Error::InvalidSession);
            }
            let preparing = !state.spawned;
            if preparing {
                shared.launch.cancel();
                shared.notify_changed();
            }
            drop(state);
            shared.presence.stop();
            shared.cloud.revoke(account_id)?;
        }
        Command::ResumeAfterRosetta {
            id,
            account_id,
            decision_id,
        } => {
            {
                let state = shared.state.lock().unwrap();
                if state.snapshot.id != id
                    || state.snapshot.account_id != account_id
                    || state.snapshot.rosetta != Some(RosettaRequest { decision_id })
                    || state.spawned
                    || shared.launch.cancelled.load(Ordering::Acquire)
                {
                    return Err(Error::InvalidSession);
                }
            }
            let available = shared.rosetta.available()?;
            let _admission = shared.admission.lock().unwrap();
            let state = shared.state.lock().unwrap();
            if shared.launch.cancelled.load(Ordering::Acquire)
                || state.snapshot.rosetta != Some(RosettaRequest { decision_id })
            {
                return Err(Error::InvalidSession);
            }
            drop(state);
            if available {
                shared.update(|s| s.rosetta = None);
            }
        }
    }
    let snapshot = shared.state.lock().unwrap().snapshot.clone();
    storage::send(&mut stream, &snapshot)
}
