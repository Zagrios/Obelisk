use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn fixture() {
    let Some(root) = std::env::var_os("FORGE_ROSETTA_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    server::serve_with_cloud_checks(
        std::io::stdin().lock(),
        std::io::stderr().lock(),
        identity,
        |launch, config| {
            launch.check_cancelled()?;
            assert!(config.home.join("preflight-complete").exists());
            assert!(config.home.join("rosetta-ready").exists());
            record(&config.home, "native.prepare");
            let helper = config.cache.join(crate::package::SDK_INTEL);
            // This native shell fixture only exercises supervision and exec
            // ownership. It cannot prove Intel translation or the real SDK.
            let script = if std::env::var_os("FORGE_ROSETTA_EXEC_FAIL").is_some() {
                "#!/bin/sh\nprintf 'exec-failed\\n'\nexit 126\n"
            } else {
                "#!/bin/sh\nshift\nexec \"$@\"\n"
            };
            let admission = "#!/bin/sh\nIFS= read -r admitted || :\nif [ \"$admitted\" != 1 ]; then printf 'exec-failed\\n'; exit 126; fi\nexec </dev/null\n";
            let hold = if std::env::var_os("FORGE_ROSETTA_EXEC_HOLD").is_some() {
                "printf ready > exec-ready\nwhile [ ! -f release-exec ]; do /bin/sleep 0.01; done\n"
            } else {
                ""
            };
            fs::write(&helper, format!("{admission}{hold}{}", script.strip_prefix("#!/bin/sh\n").unwrap())).unwrap();
            fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
            let runtime = crate::tests::runtime("read line", &config.cache);
            fs::write(
                config.cache.join("host-pid"),
                runtime.inner.lock().unwrap().host.id().to_string(),
            )
            .unwrap();
            Ok(runtime)
        },
        Box::new(CloudRecorder { root: root.clone() }),
        server::NativeChecks {
            select: |_, _| Ok(Some(crate::architecture::Architecture::Intel)),
            rosetta: |_| {
                let root = PathBuf::from(std::env::var_os("FORGE_ROSETTA_FIXTURE").unwrap());
                assert!(root.join("preflight-complete").exists());
                fs::write(root.join("rosetta-probed"), b"yes").unwrap();
                Ok(root.join("rosetta-ready").exists())
            },
        },
        recorded_presence(Some(root)),
    )
    .unwrap();
}

fn start(root: &Path) -> Client {
    start_mode(root, false, false)
}

fn start_mode(root: &Path, failed_exec: bool, hold_exec: bool) -> Client {
    let request = request(
        root,
        "/bin/sh",
        &[
            "-c",
            "echo $$ > game-pid; echo started > game-started; /bin/sleep 1; echo exited > game-exited",
        ],
        42,
    );
    let mut command = Process::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "supervisor::tests::rosetta::fixture",
            "--nocapture",
        ])
        .env("FORGE_ROSETTA_FIXTURE", root);
    if failed_exec {
        command.env("FORGE_ROSETTA_EXEC_FAIL", "1");
    }
    if hold_exec {
        command.env("FORGE_ROSETTA_EXEC_HOLD", "1");
    }
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

fn waiting(root: &Path, client: &Client) -> Snapshot {
    wait_for_file(&root.join("preflight-ready"));
    assert!(!root.join("rosetta-probed").exists());
    assert!(client.snapshot(None).unwrap().rosetta.is_none());
    fs::write(root.join("release-preflight"), b"go").unwrap();
    until(client, |snapshot| snapshot.rosetta.is_some())
}

#[test]
fn waits_after_cloud_retains_admission_and_rechecks_before_resuming() {
    let root = tempfile::tempdir().unwrap();
    let client = start(root.path());
    let pending = waiting(root.path(), &client);
    let decision = pending.rosetta.unwrap().decision_id;
    assert_eq!(pending.phase, Phase::Preparing);
    assert!(!root.path().join("runtime/host-pid").exists());
    assert!(!root.path().join("game-started").exists());
    assert!(presence_events(root.path()).is_empty());
    assert!(matches!(
        client.resume_after_rosetta(456, decision),
        Err(Error::InvalidSession)
    ));
    assert!(client.resume_after_rosetta(123, decision + 1).is_err());
    assert!(matches!(
        client.resume_after_rosetta(123, decision),
        Err(Error::RosettaUnavailable)
    ));
    assert!(matches!(
        Client::idle_guard_with(&root.path().join("sessions"), Path::new("unused"), probe),
        Err(Error::SessionBusy)
    ));
    crate::tests::wait_for_lock_release(&root.path().join("sessions/start.lock"));
    drop(client);
    let recovered = Client::recover_with(&root.path().join("sessions"), Path::new("unused"), probe)
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.snapshot(None).unwrap().rosetta,
        Some(RosettaRequest {
            decision_id: decision
        })
    );
    fs::write(root.path().join("rosetta-ready"), b"ready").unwrap();
    recovered.resume_after_rosetta(123, decision).unwrap();
    wait_for_file(&root.path().join("game-started"));
    fs::write(root.path().join("release-finalization"), b"go").unwrap();
    let done = until(&recovered, Snapshot::finished);
    assert_eq!(done.phase, Phase::Finished);
    assert_eq!(done.exit_code, Some(0));
    assert!(done.rosetta.is_none());
    assert_eq!(
        fs::read_to_string(root.path().join("cloud-events")).unwrap(),
        "cloud.prepare\ncloud.prepared\nnative.prepare\ncloud.started\ncloud.finalizing\ncloud.finished\n"
    );
}

#[test]
fn cancellation_and_account_revocation_wake_the_requirement_without_starting() {
    for revoke in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let client = start(root.path());
        let decision = waiting(root.path(), &client).rosetta.unwrap().decision_id;
        if revoke {
            client.revoke_cloud(123).unwrap();
        } else {
            client.cancel_preparation();
        }
        let done = until(&client, Snapshot::finished);
        assert_eq!(done.phase, Phase::Failed);
        assert_eq!(
            done.error.as_deref(),
            Some("backend.steam.runtime.cancelled")
        );
        assert!(done.rosetta.is_none());
        fs::write(root.path().join("rosetta-ready"), b"late installation").unwrap();
        assert!(client.resume_after_rosetta(123, decision).is_err());
        assert!(!root.path().join("runtime/host-pid").exists());
        assert!(!root.path().join("game-started").exists());
        assert_eq!(
            fs::read_to_string(root.path().join("cloud-events")).unwrap(),
            "cloud.prepare\ncloud.prepared\ncloud.aborted\n"
        );
    }
}

#[test]
fn already_available_rosetta_starts_without_a_requirement() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("rosetta-ready"), b"ready").unwrap();
    fs::write(root.path().join("release-preflight"), b"go").unwrap();
    fs::write(root.path().join("release-finalization"), b"go").unwrap();
    let client = start(root.path());
    let done = until(&client, |snapshot| {
        assert!(snapshot.rosetta.is_none());
        snapshot.finished()
    });
    assert_eq!(done.phase, Phase::Finished);
    assert_eq!(done.exit_code, Some(0));
}

#[test]
fn failed_exec_reaps_the_helper_and_aborts_without_claiming_a_running_game() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("rosetta-ready"), b"ready").unwrap();
    fs::write(root.path().join("release-preflight"), b"go").unwrap();
    let client = start_mode(root.path(), true, false);
    let done = until(&client, |snapshot| {
        assert_ne!(snapshot.phase, Phase::Running);
        snapshot.finished()
    });
    assert_eq!(done.phase, Phase::Failed);
    assert_eq!(
        done.error.as_deref(),
        Some("backend.steam.runtime.startFailed")
    );
    assert!(!root.path().join("game-started").exists());
    assert!(presence_events(root.path()).is_empty());
    assert!(!root.path().join("finalization-ready").exists());
    assert!(identity(fixture_pid(root.path(), "runtime/host-pid")).is_none());
    assert_eq!(
        fs::read_to_string(root.path().join("cloud-events")).unwrap(),
        "cloud.prepare\ncloud.prepared\nnative.prepare\ncloud.started\ncloud.aborted\n"
    );
}

#[test]
fn presence_waits_for_exec_and_account_revocation_fences_late_confirmation() {
    for revoke in [false, true] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("rosetta-ready"), b"ready").unwrap();
        fs::write(root.path().join("release-preflight"), b"go").unwrap();
        fs::write(root.path().join("release-finalization"), b"go").unwrap();
        let client = start_mode(root.path(), false, true);
        wait_for_file(&root.path().join("exec-ready"));
        assert!(presence_events(root.path()).is_empty());
        assert!(!root.path().join("game-started").exists());
        if revoke {
            client.revoke_cloud(123).unwrap();
        }
        fs::write(root.path().join("release-exec"), b"go").unwrap();
        let done = until(&client, Snapshot::finished);
        assert_eq!(done.phase, Phase::Finished);
        assert_eq!(done.exit_code, Some(0));
        assert_eq!(
            presence_events(root.path()),
            if revoke { "" } else { "start\nstop\n" }
        );
        assert!(root.path().join("game-exited").exists());
    }
}
