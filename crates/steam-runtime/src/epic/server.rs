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
    process::{Command as Process, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::process::Child;

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
                rosetta: None,
                socket,
            },
            choice: None,
            observed: Instant::now(),
        }),
        admission: Mutex::new(false),
        changed: Condvar::new(),
        cancel: CancelToken::new(),
        root: request.config.session_directory.clone(),
        rosetta: crate::rosetta::Gate::new(
            crate::rosetta::helper(request.config.helper.parent().ok_or(Error::Session)?),
            crate::architecture::rosetta_available,
        ),
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
    let result = crate::executor()?.block_on(run(request, Arc::clone(&control), inspect));
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
    let PreparedGame {
        target,
        activity,
        _ownership,
    } = prepare_game(config, control, || async {
        api.launch_plan(
            &config.namespace,
            &config.catalog_item_id,
            &config.app_name,
            &config.platform,
            &config.install,
            &config.language,
        )
        .await
        .map_err(Into::into)
    })
    .await?;
    let executable = target.executable.clone();
    control.check()?;
    if let Some(cloud) = &mut cloud {
        cloud.started(&request.id)?;
    }
    let exit = async {
        let mut game = spawn_game(&config.helper, target, control, inspect).await?;
        wait_for_game(&mut game, control, inspect, &executable, |active| {
            if active && let Some(activity) = &activity {
                // Social availability must not change the game's lifetime.
                let _ = api.start_game_presence(activity.clone());
            } else {
                api.stop_friends();
            }
        })
        .await
    }
    .await;
    let exit = match exit {
        Ok(exit) => exit,
        Err(error) => {
            if let Some(cloud) = &mut cloud {
                cloud.aborted(&request.id)?;
            }
            return Err(error);
        }
    };
    control.update(|s| {
        s.phase = Phase::Finalizing;
        s.exit_code = Some(exit);
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

pub(super) struct PreparedGame {
    pub(super) target: Target,
    activity: Option<epic_kit::friends::Activity>,
    _ownership: Option<tempfile::TempDir>,
}

pub(super) async fn prepare_game<F, Fut>(
    config: &Configuration,
    control: &Control,
    mut prepare: F,
) -> Result<PreparedGame>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<epic_kit::client::LaunchPlan>>,
{
    let mut refreshed = false;
    loop {
        control.check()?;
        let plan = prepare().await?;
        control.check()?;
        let (target, architecture, ownership) = native_target(&plan, config)?;
        if architecture == crate::architecture::Architecture::Intel {
            if refreshed {
                // A second installation interruption cannot loop over fresh
                // exchange codes indefinitely. Fail if availability changed.
                if !control.rosetta.probe().await? {
                    return Err(Error::RosettaUnavailable);
                }
                control.check()?;
            } else if wait_for_rosetta(control).await? {
                // Rosetta installation can outlive the game's exchange code.
                // Drop the old ownership token and obtain a fresh launch plan.
                refreshed = true;
                continue;
            }
        }
        return Ok(PreparedGame {
            target,
            activity: plan.activity,
            _ownership: ownership,
        });
    }
}

pub(super) async fn wait_for_rosetta(control: &Control) -> Result<bool> {
    control
        .rosetta
        .wait(
            || {
                let _admitted = control.admission.lock().unwrap();
                control.check()?;
                control.update(|s| {
                    s.decision_id += 1;
                    s.rosetta = Some(RosettaRequest {
                        decision_id: s.decision_id,
                    });
                })
            },
            || {
                control.check()?;
                Ok(control.snapshot().rosetta.is_some())
            },
        )
        .await
}

pub(super) async fn wait_for_game(
    game: &mut Child,
    control: &Control,
    inspect: Inspect,
    executable: &Path,
    publish: impl FnMut(bool),
) -> Result<i32> {
    let mut presence = Advertised {
        publish,
        active: false,
    };
    let inspect = |pid| inspect(pid).ok().flatten();
    let execution = crate::game_session::confirm_execution(game, executable, inspect).await;
    if execution == crate::game_session::Execution::Started {
        if let Some(identity) = crate::game_session::wait_identity(game, executable, inspect).await
        {
            let _ = control.update(|s| {
                s.game = Some(identity);
                s.starting = None;
            });
        }
        // Share Revoke's admission boundary: an acknowledged sign-out cannot
        // be followed by a new presence connection.
        let _admitted = control.admission.lock().unwrap();
        presence.set(!control.cancel.is_cancelled());
    }
    let exit = tokio::select! {
        biased;
        _ = control.cancel.cancelled() => {
            presence.set(false);
            crate::game_session::wait_for_exit(game).await
        }
        exit = crate::game_session::wait_for_exit(game) => exit,
    };
    if execution != crate::game_session::Execution::Started {
        control.update(|s| {
            s.game = None;
            s.starting = None;
            s.exit_code = Some(exit);
        })?;
        return Err(Error::Failed);
    }
    Ok(exit)
}

/// The activity advertised for a running game. Dropping it withdraws the activity.
struct Advertised<P: FnMut(bool)> {
    publish: P,
    active: bool,
}
impl<P: FnMut(bool)> Advertised<P> {
    /// Publish only the edges, so the caller can drive this from a level signal.
    fn set(&mut self, active: bool) {
        if self.active != active {
            self.active = active;
            (self.publish)(active);
        }
    }
}
impl<P: FnMut(bool)> Drop for Advertised<P> {
    fn drop(&mut self) {
        self.set(false);
    }
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
            control.notify_changed();
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
                control.notify_changed();
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
            control.notify_changed();
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
            control.notify_changed();
        }
        Command::ResumeAfterRosetta {
            id,
            account,
            decision_id,
        } => {
            {
                let state = control.state.lock().unwrap();
                if state.snapshot.id != id
                    || state.snapshot.account_id != account
                    || state.snapshot.rosetta != Some(RosettaRequest { decision_id })
                    || state.snapshot.phase != Phase::Preparing
                    || control.cancel.is_cancelled()
                {
                    return Err(Error::Session);
                }
            }
            let available = control.rosetta.available()?;
            let admitted = control.admission.lock().unwrap();
            let state = control.state.lock().unwrap();
            if *admitted
                || control.cancel.is_cancelled()
                || state.snapshot.rosetta != Some(RosettaRequest { decision_id })
            {
                return Err(Error::Session);
            }
            drop(state);
            if available {
                control.update(|s| s.rosetta = None)?;
            }
        }
    }
    storage::send(&mut stream, &control.snapshot())?;
    Ok(())
}

pub(super) fn native_target(
    plan: &epic_kit::client::LaunchPlan,
    config: &Configuration,
) -> Result<(
    Target,
    crate::architecture::Architecture,
    Option<tempfile::TempDir>,
)> {
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
    let architecture = crate::architecture::Architecture::preferred(&executable)?;
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
        architecture,
        ownership,
    ))
}

pub(super) async fn spawn_game(
    helper: &Path,
    target: Target,
    control: &Control,
    inspect: Inspect,
) -> Result<Child> {
    let (mut child, admission) = {
        let mut admitted = control.admission.lock().unwrap();
        control.check()?;
        let (mut pipe, child_pipe) = UnixStream::pair()?;
        pipe.set_read_timeout(Some(Duration::from_secs(5)))?;
        pipe.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut command = Process::new(helper);
        command
            .arg("epic-exec")
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(child_pipe)))
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = tokio::process::Command::from(command).spawn()?;
        let admission = (|| {
            let identity = inspect(child.id().ok_or(Error::Session)?)?.ok_or(Error::Session)?;
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
        if admission.is_ok() {
            *admitted = true;
        }
        // EOF closes the exec gate on failure before awaiting child termination.
        (child, admission)
    };
    if let Err(error) = admission {
        crate::game_session::wait_for_exit(&mut child).await;
        let _ = control.update(|s| s.starting = None);
        return Err(error);
    }
    Ok(child)
}

/// Native helper entry point. The parent first persists this process's birth
/// identity, then supplies the target through stdin. EOF before a target exits.
pub fn exec_game(input: impl Read) -> Result<()> {
    crate::protocol::exec_or_report(|| {
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
    })
}
