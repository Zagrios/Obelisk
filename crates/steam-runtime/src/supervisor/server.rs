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

struct State {
    snapshot: Snapshot,
    spawned: bool,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    root: PathBuf,
    launch: Arc<Launch>,
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
    mut output: impl Write,
    inspect: Inspect,
    prepare: impl FnOnce(&Launch, Config) -> Result<Runtime, Error>,
) -> Result<(), Error> {
    let mut input = input;
    let credentials = Credentials::read(&mut input)?;
    let request: Request = storage::packet(input)?;
    if uuid::Uuid::parse_str(&request.id).is_err() || request.config.app_id == 0 {
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
        reconnecting: false,
    };
    storage::write(&request.root, &snapshot)?;
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            snapshot,
            spawned: false,
        }),
        changed: Condvar::new(),
        root: request.root,
        launch: Arc::new(Launch::new(credentials)),
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
    let outcome = run_game(&request.target, &shared, inspect, || {
        prepare(&shared.launch, request.config)
    });
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
    prepare: impl FnOnce() -> Result<Runtime, Error>,
) -> Result<(), Error> {
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
        let mut state = shared.state.lock().unwrap();
        if shared.launch.cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let game = command.spawn().map_err(|_| Error::StartFailed)?;
        state.spawned = true;
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
    shared.update(|s| {
        s.phase = Phase::Finished;
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
    let mut state = shared.state.lock().unwrap();
    match command {
        Command::Observe { id, after } => {
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
        }
        Command::Cancel { id } => {
            if state.snapshot.id != id {
                return Err(Error::InvalidSession);
            }
            if !state.spawned {
                shared.launch.cancel();
            }
        }
    }
    let snapshot = state.snapshot.clone();
    drop(state);
    storage::send(&mut stream, &snapshot)
}
