use super::*;
#[path = "rosetta_tests.rs"]
mod rosetta;
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
    let prepare = |launch: &crate::Launch, config: crate::Config| {
        if let Some(root) = std::env::var_os("FORGE_CLOUD_LIFECYCLE_FIXTURE") {
            let root = Path::new(&root);
            assert!(root.join("preflight-complete").exists());
            assert!(!root.join("game-started").exists());
            record(root, "native.prepare");
        }
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
    };
    if let Some(root) = std::env::var_os("FORGE_CLOUD_LIFECYCLE_FIXTURE") {
        server::serve_with_cloud(
            std::io::stdin().lock(),
            std::io::stderr().lock(),
            identity,
            prepare,
            Box::new(CloudRecorder { root: root.into() }),
        )
        .unwrap();
    } else {
        server::serve_with(
            std::io::stdin().lock(),
            std::io::stderr().lock(),
            identity,
            prepare,
        )
        .unwrap();
    }
}

struct CloudRecorder {
    root: PathBuf,
}

fn record(root: &Path, event: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("cloud-events"))
        .unwrap();
    writeln!(file, "{event}").unwrap();
    file.sync_all().unwrap();
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "missing fixture signal: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fixture_pid(root: &Path, name: &str) -> u32 {
    fs::read_to_string(root.join(name))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

impl server::CloudLifecycle for CloudRecorder {
    fn prepare(&mut self) -> Result<(), Error> {
        assert!(!self.root.join("runtime/host-pid").exists());
        assert!(!self.root.join("game-started").exists());
        record(&self.root, "cloud.prepare");
        fs::write(self.root.join("preflight-ready"), b"ready").unwrap();
        wait_for_file(&self.root.join("release-preflight"));
        fs::write(self.root.join("preflight-complete"), b"ready").unwrap();
        record(&self.root, "cloud.prepared");
        Ok(())
    }

    fn started(&mut self) -> Result<(), Error> {
        assert!(identity(fixture_pid(&self.root, "runtime/host-pid")).is_some());
        assert!(!self.root.join("game-started").exists());
        record(&self.root, "cloud.started");
        Ok(())
    }

    fn finished(&mut self) {
        // These are real reaped children, not simulated lifecycle booleans.
        assert!(self.root.join("game-exited").exists());
        assert!(identity(fixture_pid(&self.root, "game-pid")).is_none());
        assert!(identity(fixture_pid(&self.root, "runtime/host-pid")).is_none());
        record(&self.root, "cloud.finalizing");
        fs::write(self.root.join("finalization-ready"), b"ready").unwrap();
        wait_for_file(&self.root.join("release-finalization"));
        record(&self.root, "cloud.finished");
    }

    fn aborted(&mut self) {
        assert!(!self.root.join("game-started").exists());
        if self.root.join("runtime/host-pid").exists() {
            assert!(identity(fixture_pid(&self.root, "runtime/host-pid")).is_none());
        }
        record(&self.root, "cloud.aborted");
    }
}

fn start_cloud_fixture(root: &Path) -> Client {
    let request = request(
        root,
        "/bin/sh",
        &[
            "-c",
            "echo $$ > game-pid; echo started > game-started; /bin/sleep 1; echo exited > game-exited",
        ],
        42,
    );
    let mut command = fixture_command();
    command.env("FORGE_CLOUD_LIFECYCLE_FIXTURE", root);
    client::start_command(
        Path::new("unused"),
        &credentials(),
        request,
        &AtomicBool::new(false),
        command,
        probe,
    )
    .unwrap()
}

#[test]
fn cloud_preflight_and_detached_finalization_preserve_the_session_reservation() {
    let root = tempfile::tempdir().unwrap();
    let client = start_cloud_fixture(root.path());
    wait_for_file(&root.path().join("preflight-ready"));
    assert!(!root.path().join("runtime/host-pid").exists());
    assert!(!root.path().join("game-started").exists());
    assert_eq!(client.snapshot(None).unwrap().phase, Phase::Preparing);
    fs::write(root.path().join("release-preflight"), b"go").unwrap();
    let running = until(&client, |snapshot| {
        snapshot.phase == Phase::Running && snapshot.game.is_some()
    });
    drop(client);
    // The disposable ps adapter may report "(sh)" during exec and "/bin/sh"
    // afterwards. This check proves detachment preserves liveness, not identity.
    assert!(identity(running.game.as_ref().unwrap().pid).is_some());
    assert!(identity(fixture_pid(root.path(), "runtime/host-pid")).is_some());
    wait_for_file(&root.path().join("finalization-ready"));
    let finalizing = storage::read(&root.path().join("sessions"))
        .unwrap()
        .unwrap();
    assert_eq!(finalizing.phase, Phase::Finalizing);
    assert!(!finalizing.finished());
    assert_eq!(finalizing.exit_code, Some(0));
    assert_eq!(finalizing.host, HostState::Stopped);
    assert!(identity(running.game.as_ref().unwrap().pid).is_none());
    assert!(identity(fixture_pid(root.path(), "runtime/host-pid")).is_none());
    assert!(matches!(
        storage::lock(&root.path().join("sessions"), "owner.lock"),
        Err(Error::SessionBusy)
    ));
    let recovered = Client::recover_with(&root.path().join("sessions"), Path::new("unused"), probe)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.snapshot(None).unwrap().phase, Phase::Finalizing);
    let duplicate = client::start_command(
        Path::new("unused"),
        &credentials(),
        request(root.path(), "/usr/bin/true", &[], 43),
        &AtomicBool::new(false),
        fixture_command(),
        probe,
    );
    assert!(matches!(duplicate, Err(Error::SessionBusy)));
    drop(recovered);
    fs::write(root.path().join("release-finalization"), b"go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = storage::read(&root.path().join("sessions"))
            .unwrap()
            .unwrap();
        if snapshot.finished() && identity(snapshot.supervisor.pid).is_none() {
            assert_eq!(snapshot.phase, Phase::Finished);
            assert_eq!(snapshot.exit_code, Some(0));
            assert!(!snapshot.socket.exists());
            break;
        }
        assert!(Instant::now() < deadline, "last state: {snapshot:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(storage::lock(&root.path().join("sessions"), "owner.lock").is_ok());
    assert_eq!(
        fs::read_to_string(root.path().join("cloud-events")).unwrap(),
        "cloud.prepare\ncloud.prepared\nnative.prepare\ncloud.started\ncloud.finalizing\ncloud.finished\n"
    );
}

#[test]
fn cancellation_during_cloud_preflight_aborts_before_game_admission() {
    let root = tempfile::tempdir().unwrap();
    let client = start_cloud_fixture(root.path());
    wait_for_file(&root.path().join("preflight-ready"));
    client.cancel_preparation();
    fs::write(root.path().join("release-preflight"), b"go").unwrap();
    let done = until(&client, Snapshot::finished);
    assert_eq!(done.phase, Phase::Failed);
    assert!(done.game.is_none());
    assert!(!root.path().join("game-started").exists());
    assert!(!root.path().join("finalization-ready").exists());
    assert_eq!(
        fs::read_to_string(root.path().join("cloud-events")).unwrap(),
        "cloud.prepare\ncloud.prepared\nnative.prepare\ncloud.aborted\n"
    );
}

fn request(root: &Path, executable: &str, args: &[&str], app: u32) -> Request {
    let cache = root.join("runtime");
    fs::create_dir_all(&cache).unwrap();
    Request {
        cloud: None,
        root: root.join("sessions"),
        id: uuid::Uuid::new_v4().to_string(),
        config: crate::Config {
            cloud_save_roots: vec![],
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

mod idle_admission {
    use super::*;

    fn assert_locks_released(root: &Path) {
        let root = root.to_path_buf();
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let acquire = || -> std::io::Result<()> {
                let starting = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(root.join("start.lock"))?;
                // Concurrent subprocess spawning can inherit flock descriptors
                // briefly until exec, even after this process drops its copy.
                // Wait for the kernel release, bounded by the receiver below.
                starting.lock()?;
                let owner = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(root.join("owner.lock"))?;
                owner.lock()?;
                // Explicitly release the test's own probe locks so another
                // concurrent spawn cannot carry them into the next assertion.
                owner.unlock()?;
                starting.unlock()
            };
            let _ = send.send(acquire());
        });
        receive
            .recv_timeout(Duration::from_secs(2))
            .expect("maintenance locks were not released within two seconds")
            .expect("reacquiring released maintenance locks failed");
    }

    // A synthetic kernel table keeps these recovery checks independent of PID
    // reuse, ps formatting and any real game, helper or account session.
    fn process(pid: u32) -> Identity {
        Identity {
            pid,
            uid: 501,
            birth_seconds: 123,
            birth_microseconds: u64::from(pid),
            executable: format!("/disposable/process-{pid}"),
        }
    }

    fn inspect_table(helper: &Path, pid: u32) -> Result<Option<Identity>, Error> {
        let table: Vec<Identity> =
            serde_json::from_slice(&fs::read(helper)?).map_err(|_| Error::InvalidSession)?;
        Ok(table.into_iter().find(|identity| identity.pid == pid))
    }

    fn fixture(root: &Path, phase: Phase, game: Option<Identity>, live: &[Identity]) -> PathBuf {
        let sessions = root.join("sessions");
        crate::files::private_directory(&sessions).unwrap();
        storage::write(
            &sessions,
            &Snapshot {
                id: uuid::Uuid::new_v4().to_string(),
                app_id: 42,
                account_id: 123,
                supervisor: process(1001),
                game,
                starting: None,
                socket: root.join("unused-socket"),
                phase,
                host: HostState::Lost,
                exit_code: None,
                error: None,
                revision: 1,
                cloud: cloud::Status::default(),
                reconnecting: false,
                rosetta: None,
            },
        )
        .unwrap();
        let helper = root.join("process-table.json");
        fs::write(&helper, serde_json::to_vec(live).unwrap()).unwrap();
        helper
    }

    #[test]
    fn matching_supervisor_or_orphan_game_reserves_nonterminal_session() {
        for (phase, game, live) in [
            (Phase::Finalizing, None, vec![process(1001)]),
            (Phase::Running, Some(process(1002)), vec![process(1002)]),
        ] {
            let root = tempfile::tempdir().unwrap();
            let helper = fixture(root.path(), phase, game, &live);
            let sessions = root.path().join("sessions");
            assert!(matches!(
                Client::idle_guard_with(&sessions, &helper, inspect_table),
                Err(Error::SessionBusy)
            ));
            // A rejected maintenance attempt releases both acquired locks.
            assert_locks_released(&sessions);
        }
    }

    fn starting_fixture(root: &Path, starting: StartingGame, live: &[Identity]) -> PathBuf {
        let helper = fixture(root, Phase::Preparing, None, live);
        let sessions = root.join("sessions");
        let mut snapshot = storage::read(&sessions).unwrap().unwrap();
        snapshot.starting = Some(starting);
        storage::write(&sessions, &snapshot).unwrap();
        helper
    }

    fn starting_process() -> StartingGame {
        StartingGame::Process {
            identity: process(1002),
            executable: "/disposable/intended-game".into(),
        }
    }

    #[test]
    fn dead_supervisor_keeps_starting_wrapper_and_its_exec_replacement_reserved() {
        for after_exec in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut live = process(1002);
            if after_exec {
                live.executable = "/disposable/intended-game".into();
            }
            let helper = starting_fixture(root.path(), starting_process(), &[live]);
            let sessions = root.path().join("sessions");
            assert!(matches!(
                Client::idle_guard_with(&sessions, &helper, inspect_table),
                Err(Error::SessionBusy)
            ));
            let client = Client::recover_with(&sessions, &helper, inspect_table)
                .unwrap()
                .expect("an accepted child remains recoverable without its supervisor");
            let snapshot = client.snapshot(None).unwrap();
            assert!(!snapshot.finished());
            assert_eq!(snapshot.account_id, 123);
            assert_eq!(snapshot.starting, Some(starting_process()));
            assert!(matches!(
                client::start_command(
                    &helper,
                    &credentials(),
                    request(root.path(), "/forge-no-game-fixture", &[], 42),
                    &AtomicBool::new(false),
                    Process::new("/forge-no-supervisor-fixture"),
                    inspect_table,
                ),
                Err(Error::SessionBusy)
            ));
            // Only a fresh negative process inspection releases the orphan.
            fs::write(&helper, b"[]").unwrap();
            let finished = client.snapshot(Some(snapshot.revision)).unwrap();
            assert!(finished.finished());
            assert!(finished.starting.is_none());
            drop(Client::idle_guard_with(&sessions, &helper, inspect_table).unwrap());
            assert_locks_released(&sessions);
        }
    }

    #[test]
    fn starting_child_pid_reuse_does_not_reserve_an_unrelated_process() {
        for after_exec in [false, true] {
            for change in 0..3 {
                let root = tempfile::tempdir().unwrap();
                let mut live = process(1002);
                if after_exec {
                    live.executable = "/disposable/intended-game".into();
                }
                match change {
                    0 => live.birth_seconds += 1,
                    1 => live.birth_microseconds += 1,
                    _ => live.uid += 1,
                }
                let helper = starting_fixture(root.path(), starting_process(), &[live]);
                let sessions = root.path().join("sessions");
                assert!(
                    Client::recover_with(&sessions, &helper, inspect_table)
                        .unwrap()
                        .is_none()
                );
                drop(Client::idle_guard_with(&sessions, &helper, inspect_table).unwrap());
            }
        }
    }

    #[test]
    fn failed_starting_child_inspection_preserves_recovery_and_maintenance_guard() {
        fn failed_child(_: &Path, pid: u32) -> Result<Option<Identity>, Error> {
            if pid == 1002 {
                Err(Error::Unavailable)
            } else {
                Ok(None)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let helper = starting_fixture(root.path(), starting_process(), &[]);
        let sessions = root.path().join("sessions");
        assert!(matches!(
            Client::idle_guard_with(&sessions, &helper, failed_child),
            Err(Error::Unavailable)
        ));
        let client = Client::recover_with(&sessions, &helper, failed_child)
            .unwrap()
            .unwrap();
        let snapshot = client.snapshot(None).unwrap();
        assert!(!snapshot.finished());
        assert!(snapshot.reconnecting);
        assert_eq!(snapshot.starting, Some(starting_process()));
    }

    #[test]
    fn older_snapshots_without_starting_child_still_decode() {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), Phase::Preparing, None, &[]);
        let snapshot = storage::read(&root.path().join("sessions"))
            .unwrap()
            .unwrap();
        let mut json = serde_json::to_value(&snapshot).unwrap();
        json.as_object_mut().unwrap().remove("starting");
        let decoded: Snapshot = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn reused_pid_with_different_identity_does_not_reserve_the_old_game() {
        for change in 0..4 {
            let root = tempfile::tempdir().unwrap();
            let mut live = vec![process(1001), process(1002)];
            for identity in &mut live {
                match change {
                    0 => identity.birth_seconds += 1,
                    1 => identity.birth_microseconds += 1,
                    2 => identity.uid += 1,
                    _ => identity.executable.push_str("-replaced"),
                }
            }
            let helper = fixture(root.path(), Phase::Running, Some(process(1002)), &live);
            let guard =
                Client::idle_guard_with(&root.path().join("sessions"), &helper, inspect_table)
                    .unwrap();
            drop(guard);
        }
    }

    #[test]
    fn unknown_running_game_and_failed_inspection_refuse_maintenance() {
        let root = tempfile::tempdir().unwrap();
        let helper = fixture(root.path(), Phase::Running, None, &[]);
        let sessions = root.path().join("sessions");
        assert!(matches!(
            Client::idle_guard_with(&sessions, &helper, inspect_table),
            Err(Error::SessionBusy)
        ));
        assert_locks_released(&sessions);
        fs::write(&helper, b"unavailable identity response").unwrap();
        assert!(matches!(
            Client::idle_guard_with(&sessions, &helper, inspect_table),
            Err(Error::InvalidSession)
        ));
        assert_locks_released(&sessions);
    }

    #[test]
    fn maintenance_guard_excludes_another_maintenance_and_launch_until_drop() {
        let root = tempfile::tempdir().unwrap();
        let helper = fixture(root.path(), Phase::Running, Some(process(1002)), &[]);
        let sessions = root.path().join("sessions");
        let guard = Client::idle_guard_with(&sessions, &helper, inspect_table).unwrap();
        assert!(matches!(
            Client::idle_guard_with(&sessions, &helper, inspect_table),
            Err(Error::SessionBusy)
        ));
        for name in ["start.lock", "owner.lock"] {
            assert!(matches!(
                storage::lock(&sessions, name),
                Err(Error::SessionBusy)
            ));
        }
        assert!(matches!(
            client::start_command(
                &helper,
                &credentials(),
                request(root.path(), "/forge-no-game-fixture", &[], 42),
                &AtomicBool::new(false),
                Process::new("/forge-no-supervisor-fixture"),
                inspect_table,
            ),
            Err(Error::SessionBusy)
        ));
        drop(guard);
        assert_locks_released(&sessions);
    }
}
