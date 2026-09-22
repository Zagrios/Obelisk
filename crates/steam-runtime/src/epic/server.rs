use super::*;
use crate::supervisor::storage;
use std::{
    io::{Read, Write},
    os::{
        fd::OwnedFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::Path,
    process::{Child, Command as Process, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};

pub fn serve(input: impl Read, mut output: impl Write, inspect: Inspect) -> Result<()> {
    let request: Request = storage::packet(input)?;
    request.config.validate(&request.account_id)?;
    if uuid::Uuid::parse_str(&request.id).is_err() {
        return Err(Error::Session);
    }
    let _owner = storage::lock(&request.config.session_directory, "owner.lock")?;
    let supervisor = inspect(std::process::id())?.ok_or(Error::Session)?;
    let socket_directory = tempfile::Builder::new()
        .prefix("forge-epic-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")?;
    let socket = socket_directory.path().join("control.sock");
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let control = Arc::new(Control {
        state: Mutex::new(State {
            begun: false,
            snapshot: Snapshot {
                id: request.id.clone(),
                account_id: request.account_id.clone(),
                namespace: request.config.namespace.clone(),
                app_name: request.config.app_name.clone(),
                phase: if request.launch {
                    Phase::Preparing
                } else {
                    Phase::Finalizing
                },
                cloud_phase: CloudPhase::Checking,
                decision_id: 0,
                allowed_choices: vec![],
                error: None,
                failure_reason: None,
                exit_code: None,
                revision: 1,
                reconnecting: false,
                supervisor,
                game: None,
                starting: None,
                socket,
            },
            choice: None,
            observed: Instant::now(),
        }),
        admission: Mutex::new(false),
        changed: Condvar::new(),
        cancel: CancelToken::new(),
        root: request.config.session_directory.clone(),
    });
    write_snapshot(&control.root, &control.snapshot())?;
    // Persisted ownership precedes acknowledgement. The request pipe can now go away.
    storage::send(&mut output, &control.snapshot())?;
    drop(output);
    let stopped = Arc::new(AtomicBool::new(false));
    let listener_control = Arc::clone(&control);
    let listener_stopped = Arc::clone(&stopped);
    let listener_thread = std::thread::spawn(move || {
        while !listener_stopped.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let c = Arc::clone(&listener_control);
                    // Observe may wait, but a parallel cancel/choice must still arrive.
                    std::thread::spawn(move || {
                        let _ = handle(stream, &c);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(_) => break,
            }
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(request, Arc::clone(&control), inspect));
    if let Err(error) = &result {
        let _ = control.fail(error);
    }
    stopped.store(true, Ordering::Release);
    let _ = listener_thread.join();
    result
}

async fn run(request: Request, control: Arc<Control>, inspect: Inspect) -> Result<()> {
    await_begin(&control).await?;
    let api = Arc::new(
        epic_kit::client::Client::from_exchange_code(&request.exchange_code, &request.account_id)
            .await?,
    );
    let result = run_authenticated(&request, &control, inspect, Arc::clone(&api)).await;
    // This is the helper's independent session, never the UI's remembered token.
    let _ = api.disconnect().await;
    result
}
// Swift must receive the client and check cancellation before admitting work.
// Dropping a Swift Task does not necessarily drop UniFFI's Rust future.
pub(super) async fn await_begin(control: &Control) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        control.check()?;
        if control.state.lock().unwrap().begun {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Cancelled);
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(25)) => (),
            _ = control.cancel.cancelled() => return Err(Error::Cancelled),
        }
    }
}
async fn run_authenticated(
    request: &Request,
    control: &Arc<Control>,
    inspect: Inspect,
    api: Arc<epic_kit::client::Client>,
) -> Result<()> {
    let mut cloud = session::Session::new(
        request.config.clone(),
        request.account_id.clone(),
        Arc::clone(&api),
        Arc::clone(control),
    )
    .await?;
    if !request.launch {
        if let Some(cloud) = &mut cloud {
            cloud.resume().await?;
        }
        control.update(|s| s.phase = Phase::Finished)?;
        return Ok(());
    }
    if let Some(cloud) = &mut cloud {
        cloud.prepare().await?;
    }
    control.check()?;
    let config = &request.config;
    let plan = api
        .launch_plan(
            &config.namespace,
            &config.catalog_item_id,
            &config.app_name,
            &config.platform,
            &config.install,
            &config.language,
        )
        .await?;
    control.check()?;
    let (target, _ownership) = native_target(&plan, config)?;
    let launched = {
        let mut admitted = control.admission.lock().unwrap();
        control.check()?;
        if let Some(cloud) = &mut cloud {
            cloud.started(&request.id)?;
        }
        let result = spawn_game(&config.helper, target, control, inspect);
        if result.is_ok() {
            *admitted = true;
        }
        result
    };
    let mut game = match launched {
        Ok(child) => child,
        Err(error) => {
            if let Some(cloud) = &mut cloud {
                cloud.aborted(&request.id)?;
            }
            return Err(error);
        }
    };
    // Child::wait is the authoritative termination boundary, independently of UI.
    // Keep polling only to permit the async runtime's I/O and account cancellation.
    let exit = loop {
        if let Some(status) = game.try_wait()? {
            break status.code();
        }
        if let Ok(Some(identity)) = inspect(game.id()) {
            let snapshot = control.snapshot();
            if snapshot.game.as_ref() != Some(&identity) {
                let _ = control.update(|s| s.game = Some(identity));
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    control.update(|s| {
        s.phase = Phase::Finalizing;
        s.exit_code = exit;
        s.game = None;
        s.starting = None;
    })?;
    if let Some(cloud) = &mut cloud {
        cloud.finished(&request.id).await?;
    }
    control.update(|s| {
        s.phase = Phase::Finished;
        s.allowed_choices.clear();
    })?;
    Ok(())
}

pub(super) fn handle(mut stream: UnixStream, control: &Control) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let command: Command = storage::packet(stream.try_clone()?)?;
    match command {
        Command::Begin { id, account } => {
            let mut state = control.state.lock().unwrap();
            if id != state.snapshot.id || account != state.snapshot.account_id {
                return Err(Error::Session);
            }
            control.check()?;
            state.begun = true;
            control.changed.notify_all();
        }
        Command::Observe { id, after } => {
            let mut state = control.state.lock().unwrap();
            if id != state.snapshot.id {
                return Err(Error::Session);
            }
            state.observed = Instant::now();
            if after == Some(state.snapshot.revision) && !state.snapshot.finished() {
                state = control
                    .changed
                    .wait_timeout(state, Duration::from_secs(10))
                    .unwrap()
                    .0;
            }
            storage::send(&mut stream, &state.snapshot)?;
            return Ok(());
        }
        Command::Cancel { id } => {
            let admitted = control.admission.lock().unwrap();
            if control.snapshot().id != id {
                return Err(Error::Session);
            }
            if !*admitted {
                control.cancel.cancel();
                control.changed.notify_all();
            }
        }
        Command::Revoke { id, account } => {
            // Serialize revocation with opening the child's exec gate, just as
            // Cancel does. An acknowledged revoke cannot precede a later launch.
            let _admitted = control.admission.lock().unwrap();
            let state = control.state.lock().unwrap();
            if state.snapshot.id != id || state.snapshot.account_id != account {
                return Err(Error::Session);
            }
            control.cancel.cancel();
            control.changed.notify_all();
        }
        Command::Choose {
            id,
            account,
            decision,
            choice,
        } => {
            let mut state = control.state.lock().unwrap();
            if state.snapshot.id != id
                || state.snapshot.account_id != account
                || state.snapshot.decision_id != decision
                || !state.snapshot.allowed_choices.contains(&choice)
                || state.choice.is_some()
            {
                return Err(Error::Session);
            }
            state.choice = Some(choice);
            control.changed.notify_all();
        }
    }
    storage::send(&mut stream, &control.snapshot())?;
    Ok(())
}

fn native_target(
    plan: &epic_kit::client::LaunchPlan,
    config: &Configuration,
) -> Result<(Target, Option<tempfile::TempDir>)> {
    let root = config.install.canonicalize()?;
    let mut executable = Path::new(&plan.target.executable).canonicalize()?;
    if plan.target.is_application_bundle {
        let output = Process::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleExecutable", "raw", "-o", "-"])
            .arg(executable.join("Contents/Info.plist"))
            .env_clear()
            .output()?;
        let name = std::str::from_utf8(&output.stdout)
            .map_err(|_| Error::Unsupported)?
            .trim();
        if !output.status.success()
            || name.is_empty()
            || name.len() > 255
            || name.contains('/')
            || name == "."
            || name == ".."
        {
            return Err(Error::Unsupported);
        }
        executable = executable
            .join("Contents/MacOS")
            .join(name)
            .canonicalize()?;
    }
    if !executable.starts_with(&root) || !executable.is_file() {
        return Err(Error::Unsupported);
    }
    // The installer may omit execute permission for a Mach-O. Match the shared
    // native launcher: repair only the selected executable after verification.
    let mut header = [0u8; 4];
    std::fs::File::open(&executable)?.read_exact(&mut header)?;
    if !matches!(
        header,
        [0xcf, 0xfa, 0xed, 0xfe]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xfe, 0xed, 0xfa, 0xce]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xca, 0xfe, 0xba, 0xbf]
    ) {
        return Err(Error::Unsupported);
    }
    let metadata = std::fs::metadata(&executable)?;
    if metadata.permissions().mode() & 0o100 == 0 {
        std::fs::set_permissions(
            &executable,
            std::fs::Permissions::from_mode(metadata.permissions().mode() | 0o100),
        )?;
    }
    let mut arguments = plan.target.arguments.clone();
    let ownership = if let Some(bytes) = &plan.ownership_token {
        let directory = tempfile::Builder::new()
            .prefix("forge-epic-ownership-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let path = directory.path().join("ownership.bin");
        crate::files::atomic_write(&path, crate::files::Durability::FileSync, |f| {
            f.write_all(bytes)?;
            Ok(())
        })?;
        arguments.push(format!("-epicovt={}", path.display()));
        Some(directory)
    } else {
        None
    };
    let environment = [
        ("HOME".into(), config.home.to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        (
            "TMPDIR".into(),
            std::env::temp_dir().to_string_lossy().into_owned(),
        ),
        ("LANG".into(), "en_US.UTF-8".into()),
    ]
    .into_iter()
    .collect();
    Ok((
        Target {
            executable,
            arguments,
            working_directory: plan.target.working_directory.clone().into(),
            environment,
        },
        ownership,
    ))
}

pub(super) fn spawn_game(
    helper: &Path,
    target: Target,
    control: &Control,
    inspect: Inspect,
) -> Result<Child> {
    control.check()?;
    let (mut pipe, child_pipe) = UnixStream::pair()?;
    pipe.set_read_timeout(Some(Duration::from_secs(5)))?;
    pipe.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut child = Process::new(helper)
        .arg("epic-exec")
        .env_clear()
        .stdin(Stdio::from(OwnedFd::from(child_pipe)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let admission = (|| {
        let identity = inspect(child.id())?.ok_or(Error::Session)?;
        let starting = StartingGame::Process {
            identity,
            executable: target.executable.clone(),
        };
        control.update(|s| {
            s.starting = Some(starting);
            s.phase = Phase::Running;
        })?;
        // The helper cannot exec until its kernel identity and destination are durable.
        storage::send(&mut pipe, &target)?;
        Ok(())
    })();
    if let Err(error) = admission {
        drop(pipe);
        let _ = child.wait();
        return Err(error);
    }
    Ok(child)
}

/// Native helper entry point. The parent first persists this process's birth
/// identity, then supplies the target through stdin. EOF before a target exits.
pub fn exec_game(input: impl Read) -> Result<()> {
    let target: Target = storage::packet(input)?;
    if !target.executable.is_absolute() || !target.working_directory.is_absolute() {
        return Err(Error::Session);
    }
    let error = Process::new(target.executable)
        .args(target.arguments)
        .current_dir(target.working_directory)
        .env_clear()
        .envs(target.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .exec();
    Err(Error::Io(error))
}
