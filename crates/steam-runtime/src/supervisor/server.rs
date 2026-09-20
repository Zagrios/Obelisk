use super::*;
use crate::{Error, Launch, Runtime, protocol::Credentials};
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
pub(super) trait CloudLifecycle {
    fn prepare(&mut self) -> Result<(), Error>;
    fn started(&mut self) -> Result<(), Error>;
    fn finished(&mut self);
    fn aborted(&mut self);
}

impl CloudLifecycle for cloud::Session {
    fn prepare(&mut self) -> Result<(), Error> {
        cloud::Session::prepare(self)
    }
    fn started(&mut self) -> Result<(), Error> {
        cloud::Session::started(self)
    }
    fn finished(&mut self) {
        cloud::Session::finished(self);
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
}
impl Shared {
    fn update(&self, change: impl FnOnce(&mut Snapshot)) {
        let mut state = self.state.lock().unwrap();
        change(&mut state.snapshot);
        state.snapshot.revision += 1;
        // Disk failure after spawn cannot abandon or terminate the game. Live
        // observers still receive the authoritative in-memory state.
        let _ = storage::write(&self.root, &state.snapshot);
        self.changed.notify_all();
    }
}

pub fn serve(input: impl Read, output: impl Write, inspect: Inspect) -> Result<(), Error> {
    serve_with(input, output, inspect, |launch, config| {
        launch.prepare(config)
    })
}

pub(super) fn serve_with(
    input: impl Read,
    output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error>,
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
    )
}

#[cfg(test)]
pub(super) fn serve_with_cloud(
    input: impl Read,
    output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error>,
    cloud: Box<dyn CloudLifecycle>,
) -> Result<(), Error> {
    serve_with_factory(input, output, inspect, prepare, |_, _, _, _| {
        Ok(Some(cloud))
    })
}

fn serve_with_factory(
    input: impl Read,
    mut output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error>,
    make_cloud: impl FnOnce(
        Option<cloud::Config>,
        Option<Credentials>,
        String,
        Arc<cloud::Control>,
    ) -> Result<Option<Box<dyn CloudLifecycle>>, Error>,
) -> Result<(), Error> {
    let mut input = input;
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
    // Both owners retain secrets only in zeroizing Credentials values. Neither
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
    let outcome = (|| {
        let mut cloud = make_cloud(
            request.cloud,
            cloud_credentials,
            session_id,
            shared.cloud.clone(),
        )?;
        let lifecycle = cloud
            .as_mut()
            .map(|cloud| cloud.as_mut() as &mut dyn CloudLifecycle);
        let result = run_game(&request.target, &shared, inspect, lifecycle, || {
            prepare(&shared.launch, request.config)
        });
        let abort = result.is_err() && !shared.state.lock().unwrap().spawned;
        if abort && let Some(cloud) = cloud.as_mut() {
            cloud.aborted();
        }
        result
    })();
    if let Err(error) = outcome {
        shared.update(|s| {
            s.phase = Phase::Failed;
            s.error = Some(error.to_string());
            s.host = HostState::Stopped;
        });
    }
    finished.store(true, Ordering::Release);
    // Wake blocking accept even when no UI is connected.
    if UnixStream::connect(&socket).is_ok() || listening.is_finished() {
        let _ = listening.join();
    }
    // If the socket was removed externally, do not wait forever on accept.
    // This dedicated supervisor process exits next and closes its listener.
    // socket_dir and the owner lock are released only after game/host teardown.
    Ok(())
}

fn run_game(
    target: &Target,
    shared: &Arc<Shared>,
    inspect: Inspect,
    mut cloud: Option<&mut dyn CloudLifecycle>,
    prepare: impl FnOnce() -> Result<Runtime, Error>,
) -> Result<(), Error> {
    if let Some(cloud) = cloud.as_deref_mut() {
        cloud.prepare()?;
    }
    let runtime = Arc::new(prepare()?);
    let app_id = shared.state.lock().unwrap().snapshot.app_id.to_string();
    let mut command = Process::new(&target.executable);
    command
        .args(&target.arguments)
        .current_dir(&target.working_directory)
        .env_clear()
        .envs(&target.environment)
        .envs(runtime.environment())
        .env("SteamAppId", &app_id)
        .env("SteamGameId", &app_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut game = {
        // Cancel and spawn have one linearization point. Cancellation after this
        // succeeds never kills a game, including sign-out and UI disconnection.
        let _admission = shared.admission.lock().unwrap();
        if shared.launch.cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if let Some(cloud) = cloud.as_deref_mut() {
            cloud.started()?;
        }
        let game = command.spawn().map_err(|_| Error::StartFailed)?;
        shared.state.lock().unwrap().spawned = true;
        game
    };
    let identity = inspect(game.id());
    let _ = runtime.started(game.id());
    shared.update(|s| {
        s.game = identity;
        s.phase = Phase::Running;
    });
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
    // Parentage supplies the actual exit status; no PID lookup decides lifetime.
    let exit = loop {
        match game.wait() {
            Ok(status) => break status.code().unwrap_or(-1),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    runtime.stopped(Some(exit));
    let _ = observation.join();
    if let Some(cloud) = cloud {
        shared.update(|s| {
            s.phase = Phase::Finalizing;
            s.host = HostState::Stopped;
            s.exit_code = Some(exit);
        });
        cloud.finished();
    }
    shared.update(|s| {
        s.phase = Phase::Finished;
        s.host = HostState::Stopped;
        s.exit_code = Some(exit);
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
            let preparing = {
                let state = shared.state.lock().unwrap();
                if state.snapshot.id != id {
                    return Err(Error::InvalidSession);
                }
                !state.spawned
            };
            if preparing {
                shared.cloud.cancel();
                shared.launch.cancel();
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
            let state = shared.state.lock().unwrap();
            if state.snapshot.id != id || state.snapshot.account_id != account_id {
                return Err(Error::InvalidSession);
            }
            drop(state);
            shared.cloud.revoke(account_id)?;
        }
    }
    let snapshot = shared.state.lock().unwrap().snapshot.clone();
    storage::send(&mut stream, &snapshot)
}
