use super::*;
use crate::{Error, HostState, protocol::Credentials};
use std::{
    fs,
    path::Path,
    process::{Command as Process, Stdio},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

// Only the platform identity adapter is replaced. The subprocess, handoff,
// locks, IPC, game parentage, runtime teardown and recovery are production code.
fn identity(pid: u32) -> Option<Identity> {
    let out = Process::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "uid=,lstart=,comm="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() < 7 {
        return None;
    }
    let birth = fields[1..6]
        .join(" ")
        .bytes()
        .fold(0_u64, |n, b| n.wrapping_mul(31).wrapping_add(b as u64));
    Some(Identity {
        pid,
        uid: fields[0].parse().ok()?,
        birth_seconds: birth,
        birth_microseconds: 0,
        executable: fields[6..].join(" "),
    })
}
fn probe(_: &Path, pid: u32) -> Result<Option<Identity>, Error> {
    Ok(identity(pid))
}
fn fixture_command() -> Process {
    let mut command = Process::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "supervisor::tests::fixture", "--nocapture"])
        .env("FORGE_SUPERVISOR_FIXTURE", "1");
    command
}

#[test]
fn fixture() {
    if std::env::var_os("FORGE_SUPERVISOR_FIXTURE").is_none() {
        return;
    }
    server::serve_with(
        std::io::stdin().lock(),
        std::io::stderr().lock(),
        identity,
        |launch, config| {
            // A real disposable host exits when its owner's pipe closes, reproducing
            // the original lifetime dependency without Valve or a Steam account.
            if config.app_id == 99 {
                std::thread::sleep(Duration::from_millis(250));
                if launch.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
            }
            let runtime = crate::tests::runtime("read line", &config.cache);
            fs::write(
                config.cache.join("host-pid"),
                runtime.inner.lock().unwrap().host.id().to_string(),
            )
            .unwrap();
            Ok(runtime)
        },
    )
    .unwrap();
}

fn request(root: &Path, executable: &str, args: &[&str], app: u32) -> Request {
    let cache = root.join("runtime");
    fs::create_dir_all(&cache).unwrap();
    Request {
        root: root.join("sessions"),
        id: uuid::Uuid::new_v4().to_string(),
        config: crate::Config {
            app_id: app,
            helpers: root.into(),
            package: root.join("unused.zip"),
            cache,
            diagnostics: root.join("reports"),
            steam_api: root.join("unused.dylib"),
            home: root.into(),
        },
        target: Target {
            executable: executable.into(),
            arguments: args.iter().map(|s| s.to_string()).collect(),
            working_directory: root.into(),
            environment: Default::default(),
        },
    }
}
fn credentials() -> Credentials {
    Credentials {
        steam_id: 123,
        account_name: "fixture".into(),
        refresh_token: "never-persist-this-token".into(),
    }
}
fn start_fixture(request: Request) -> Client {
    client::start_command(
        Path::new("unused"),
        &credentials(),
        request,
        &AtomicBool::new(false),
        fixture_command(),
        probe,
    )
    .unwrap()
}
fn until(client: &Client, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = client.snapshot(None).unwrap();
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "last state: {snapshot:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn detach_recover_keeps_host_and_game_alive_then_cleans_up_without_ui() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["1.5"], 42));
    let running = until(&client, |s| s.phase == Phase::Running && s.game.is_some());
    let host: u32 = fs::read_to_string(root.path().join("runtime/host-pid"))
        .unwrap()
        .parse()
        .unwrap();
    drop(client);
    assert!(identity(host).is_some());
    assert_eq!(identity(running.game.as_ref().unwrap().pid), running.game);
    let recovered = Client::recover_with(&root.path().join("sessions"), Path::new("unused"), probe)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.snapshot(None).unwrap().id, running.id);
    let duplicate = client::start_command(
        Path::new("unused"),
        &credentials(),
        request(root.path(), "/bin/sleep", &["5"], 43),
        &AtomicBool::new(false),
        fixture_command(),
        probe,
    );
    assert!(matches!(duplicate, Err(Error::SessionBusy)));
    drop(recovered);
    std::thread::sleep(Duration::from_secs(2));
    assert!(identity(host).is_none());
    assert!(
        Client::recover_with(&root.path().join("sessions"), Path::new("unused"), probe)
            .unwrap()
            .is_none()
    );
    let final_state = storage::read(&root.path().join("sessions"))
        .unwrap()
        .unwrap();
    assert_eq!(final_state.exit_code, Some(0));
    assert!(!final_state.socket.exists());
    assert!(
        !fs::read_to_string(root.path().join("sessions/session.json"))
            .unwrap()
            .contains("never-persist")
    );
}

#[test]
fn preparation_cancel_and_failed_spawn_release_owner_without_running_a_game() {
    for (executable, app) in [("/bin/sleep", 99), ("/forge-nonexistent-fixture", 42)] {
        let root = tempfile::tempdir().unwrap();
        let client = start_fixture(request(root.path(), executable, &["1"], app));
        if app == 99 {
            client.cancel_preparation();
        }
        let done = until(&client, Snapshot::finished);
        assert_eq!(done.phase, Phase::Failed);
        assert!(done.game.is_none());
    }
}

#[test]
fn immediate_nonzero_exit_is_retained_for_detached_observer() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/usr/bin/false", &[], 42));
    let done = until(&client, Snapshot::finished);
    assert_eq!(done.exit_code, Some(1));
    let second = start_fixture(request(root.path(), "/usr/bin/true", &[], 42));
    until(&second, Snapshot::finished);
    // An old observer must still see its own exit after a new game starts.
    assert_eq!(client.snapshot(None).unwrap().exit_code, Some(1));
}

#[test]
fn cancellation_after_spawn_does_not_stop_game_and_host_loss_does_not_release_it() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["1.5"], 42));
    let running = until(&client, |s| s.game.is_some());
    client.cancel_preparation();
    let host = fs::read_to_string(root.path().join("runtime/host-pid")).unwrap();
    assert!(
        Process::new("/bin/kill")
            .args(["-TERM", &host])
            .status()
            .unwrap()
            .success()
    );
    let lost = until(&client, |s| s.host == HostState::Lost);
    assert_eq!(lost.phase, Phase::Running);
    assert_eq!(identity(running.game.as_ref().unwrap().pid), running.game);
    assert_eq!(until(&client, Snapshot::finished).exit_code, Some(0));
}

// A separate parent simulates Forge. Killing only this fixture must leave its
// supervisor, host and game alive and recoverable by a new client process.
#[test]
fn ui_parent_fixture() {
    let Some(root) = std::env::var_os("FORGE_UI_PARENT_FIXTURE") else {
        return;
    };
    let root = Path::new(&root);
    let client = start_fixture(request(root, "/bin/sleep", &["2"], 42));
    until(&client, |s| s.game.is_some());
    fs::write(root.join("ui-ready"), "ready").unwrap();
    std::thread::sleep(Duration::from_secs(10));
}

#[test]
fn ui_process_crash_can_reattach_without_restarting_game_or_host() {
    let root = tempfile::tempdir().unwrap();
    let mut parent = Process::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "supervisor::tests::ui_parent_fixture",
            "--nocapture",
        ])
        .env("FORGE_UI_PARENT_FIXTURE", root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("ui-ready").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let before = storage::read(&root.path().join("sessions"))
        .unwrap()
        .unwrap();
    parent.kill().unwrap();
    parent.wait().unwrap();
    let client = Client::recover_with(&root.path().join("sessions"), Path::new("unused"), probe)
        .unwrap()
        .unwrap();
    let after = client.snapshot(None).unwrap();
    assert_eq!(before.supervisor, after.supervisor);
    assert_eq!(before.game, after.game);
    assert_eq!(after.phase, Phase::Running);
    assert_eq!(until(&client, Snapshot::finished).exit_code, Some(0));
}

#[test]
fn supervisor_crash_keeps_live_game_reserved_and_rejects_reused_pid() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["2"], 42));
    let running = until(&client, |s| s.game.is_some());
    assert!(
        Process::new("/bin/kill")
            .args(["-KILL", &running.supervisor.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while identity(running.supervisor.pid).is_some() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let recovered = Client::recover_with(&root.path().join("sessions"), Path::new("unused"), probe)
        .unwrap()
        .unwrap();
    let orphan = recovered.snapshot(None).unwrap();
    assert_eq!(orphan.phase, Phase::Running);
    assert_eq!(orphan.host, HostState::Lost);
    assert_eq!(orphan.game, running.game);
    // A matching PID but different kernel birth time must never be adopted.
    let mut stale = running;
    stale.game.as_mut().unwrap().birth_microseconds += 1;
    storage::write(&root.path().join("sessions"), &stale).unwrap();
    assert!(
        Client::recover_with(&root.path().join("sessions"), Path::new("unused"), probe)
            .unwrap()
            .is_none()
    );
    // Restore the actual identity and wait for the disposable orphan's own exit.
    storage::write(&root.path().join("sessions"), &orphan).unwrap();
    until(&recovered, Snapshot::finished);
}

#[test]
fn corrupt_metadata_and_unknown_running_identity_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let sessions = root.path().join("sessions");
    crate::files::private_directory(&sessions).unwrap();
    fs::write(sessions.join("session.json"), "invalid").unwrap();
    assert!(Client::recover_with(&sessions, Path::new("unused"), probe).is_err());
    fs::remove_file(sessions.join("session.json")).unwrap();
    let client = start_fixture(request(root.path(), "/usr/bin/true", &[], 42));
    let mut snapshot = until(&client, Snapshot::finished);
    // An interrupted post-spawn metadata write is ambiguous, never permission
    // to uninstall a possibly live game.
    snapshot.phase = Phase::Running;
    snapshot.supervisor.birth_microseconds += 1;
    snapshot.game = None;
    std::thread::sleep(Duration::from_millis(100));
    storage::write(&sessions, &snapshot).unwrap();
    let uncertain = Client::recover_with(&sessions, Path::new("unused"), probe)
        .unwrap()
        .unwrap();
    let state = uncertain.snapshot(None).unwrap();
    assert_eq!(state.phase, Phase::Running);
    assert!(state.reconnecting);
    assert!(matches!(
        client::start_command(
            Path::new("unused"),
            &credentials(),
            request(root.path(), "/usr/bin/true", &[], 42),
            &AtomicBool::new(false),
            fixture_command(),
            probe
        ),
        Err(Error::SessionBusy)
    ));
}

#[test]
fn waiting_observer_receives_exit_without_holding_supervisor_alive() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["0.3"], 42));
    let running = until(&client, |s| s.game.is_some());
    let final_state = client.snapshot(Some(running.revision)).unwrap();
    assert_eq!(final_state.phase, Phase::Finished);
    assert_eq!(final_state.exit_code, Some(0));
}

#[test]
fn control_failure_is_paced_without_declaring_host_dead_or_releasing_game() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["2"], 42));
    let running = until(&client, |s| s.game.is_some());
    let mut unavailable = running.clone();
    unavailable.socket = unavailable.socket.with_file_name("missing");
    storage::write(&root.path().join("sessions"), &unavailable).unwrap();
    let started = Instant::now();
    let state = client.snapshot(Some(running.revision)).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(500));
    assert!(state.reconnecting);
    assert_eq!(state.phase, Phase::Running);
    assert_eq!(state.host, HostState::Online);
    assert_eq!(state.game, running.game);
    storage::write(&root.path().join("sessions"), &running).unwrap();
    let restored = client.snapshot(None).unwrap();
    assert!(!restored.reconnecting);
    assert_eq!(restored.game, running.game);
    assert_eq!(until(&client, Snapshot::finished).exit_code, Some(0));
}

#[test]
fn failed_identity_query_is_uncertainty_not_process_exit() {
    fn unavailable(_: &Path, _: u32) -> Result<Option<Identity>, Error> {
        Err(Error::Unavailable)
    }
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["2"], 42));
    let running = until(&client, |s| s.game.is_some());
    let recovered = Client::recover_with(
        &root.path().join("sessions"),
        Path::new("unused"),
        unavailable,
    )
    .unwrap()
    .unwrap();
    let state = recovered.snapshot(None).unwrap();
    assert!(state.reconnecting);
    assert_eq!(state.host, running.host);
    assert_eq!(state.game, running.game);
    assert!(!state.finished());
    until(&client, Snapshot::finished);
}

#[test]
fn owner_wait_times_out_without_permitting_another_starter() {
    let root = tempfile::tempdir().unwrap();
    let owner = storage::lock(root.path(), "owner.lock").unwrap();
    let starting = storage::lock(root.path(), "start.lock").unwrap();
    assert!(matches!(
        storage::owner_released(root.path(), starting),
        Err(Error::SessionBusy)
    ));
    assert!(matches!(
        storage::lock(root.path(), "start.lock"),
        Err(Error::SessionBusy)
    ));
    drop(owner);
    let deadline = Instant::now() + Duration::from_secs(2);
    while storage::lock(root.path(), "start.lock").is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn closing_handoff_before_acknowledgement_does_not_abandon_accepted_launch() {
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path(), "/bin/sleep", &["1"], 99);
    let (mut pipe, remote) = UnixStream::pair().unwrap();
    let mut command = fixture_command();
    let mut child = command
        .stdin(Stdio::from(OwnedFd::from(remote.try_clone().unwrap())))
        .stderr(Stdio::from(OwnedFd::from(remote)))
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    drop(command);
    credentials().write(&mut pipe).unwrap();
    storage::send(&mut pipe, &request).unwrap();
    // No reply is ever read; this emulates Forge disappearing during handoff.
    pipe.shutdown(std::net::Shutdown::Both).unwrap();
    drop(pipe);
    let deadline = Instant::now() + Duration::from_secs(3);
    let client = loop {
        if let Ok(Some(client)) = Client::recover_with(&request.root, Path::new("unused"), probe) {
            break client;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    let running = until(&client, |s| s.phase == Phase::Running);
    assert_eq!(running.host, HostState::Online);
    assert_eq!(until(&client, Snapshot::finished).exit_code, Some(0));
    assert!(child.wait().unwrap().success());
}

#[test]
fn metadata_failure_does_not_regress_a_previously_observed_live_session() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["1"], 42));
    let running = until(&client, |s| s.game.is_some());
    fs::write(
        root.path()
            .join("sessions")
            .join(format!("{}.json", running.id)),
        b"truncated",
    )
    .unwrap();
    let state = client.snapshot(None).unwrap();
    assert_eq!(state.phase, Phase::Running);
    assert_eq!(state.game, running.game);
    assert!(!state.reconnecting);
    until(&client, Snapshot::finished);
}

#[test]
fn removed_socket_does_not_prevent_supervisor_teardown() {
    let root = tempfile::tempdir().unwrap();
    let client = start_fixture(request(root.path(), "/bin/sleep", &["0.4"], 42));
    let running = until(&client, |s| s.game.is_some());
    fs::remove_file(&running.socket).unwrap();
    until(&client, Snapshot::finished);
    let deadline = Instant::now() + Duration::from_secs(2);
    while identity(running.supervisor.pid).is_some() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(storage::lock(&root.path().join("sessions"), "owner.lock").is_ok());
}
