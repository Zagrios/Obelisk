use super::*;
use crate::architecture::tests::{ARM64, INTEL};
use crate::supervisor::storage;
use std::{
    collections::HashMap,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::Path,
};

fn identity(pid: u32) -> Identity {
    Identity {
        pid,
        uid: 501,
        birth_seconds: 123,
        birth_microseconds: 456,
        executable: "/fixture/helper".into(),
    }
}
fn control(root: &Path) -> Arc<Control> {
    Arc::new(Control {
        root: root.into(),
        cancel: CancelToken::new(),
        admission: Mutex::new(false),
        changed: Condvar::new(),
        rosetta: crate::rosetta::Gate::new(root.join("rosetta"), |path| Ok(path.exists())),
        state: Mutex::new(State {
            begun: false,
            observed: Instant::now(),
            choice: None,
            snapshot: Snapshot {
                id: uuid::Uuid::new_v4().to_string(),
                account_id: "a".repeat(32),
                namespace: "namespace".into(),
                app_name: "app".into(),
                phase: Phase::Preparing,
                cloud_phase: CloudPhase::Checking,
                decision_id: 1,
                allowed_choices: vec![],
                error: None,
                failure_reason: None,
                exit_code: None,
                revision: 1,
                reconnecting: false,
                supervisor: identity(std::process::id()),
                game: None,
                starting: None,
                rosetta: None,
                socket: root.join("control.sock"),
            },
        }),
    })
}

#[test]
fn terminal_failures_preserve_typed_cancellation_through_the_journal() {
    for (error, expected) in [
        (Error::Cancelled, FailureReason::Cancelled),
        (
            Error::Epic(epic_kit::EpicError::Cancelled),
            FailureReason::Cancelled,
        ),
        (
            Error::Epic(epic_kit::EpicError::Unauthorized),
            FailureReason::Failed,
        ),
        (Error::Storage, FailureReason::Failed),
    ] {
        let root = tempfile::tempdir().unwrap();
        let control = control(root.path());
        control.fail(&error).unwrap();
        let saved = read_snapshot(root.path(), None).unwrap().unwrap();
        assert!(saved.finished());
        assert_eq!(saved.failure_reason, Some(expected));
        assert_eq!(saved.failure_reason(), Some(expected));
        assert_eq!(saved.error, Some(error.to_string()));
    }
}

#[test]
fn cancellation_failure_keeps_live_or_starting_process_ownership() {
    for starting in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let control = control(root.path());
        control
            .update(|s| {
                if starting {
                    s.starting = Some(StartingGame::Process {
                        identity: identity(42),
                        executable: "/fixture/game".into(),
                    });
                } else {
                    s.game = Some(identity(42));
                }
            })
            .unwrap();
        control
            .fail(&Error::Epic(epic_kit::EpicError::Cancelled))
            .unwrap();
        let saved = read_snapshot(root.path(), None).unwrap().unwrap();
        assert_eq!(saved.phase, Phase::Running);
        assert!(!saved.finished());
        assert_eq!(saved.failure_reason, None);
        assert_eq!(saved.failure_reason(), None);
        assert!(saved.game.is_some() || saved.starting.is_some());
    }
}

#[test]
fn legacy_failure_snapshots_are_classified_only_when_terminal() {
    let root = tempfile::tempdir().unwrap();
    let mut legacy = serde_json::to_value(control(root.path()).snapshot()).unwrap();
    assert!(legacy.get("failure_reason").is_none());
    legacy["phase"] = serde_json::json!("Failed");
    for (message, expected) in [
        ("backend.epic.cloud.cancelled", FailureReason::Cancelled),
        ("backend.epic.cancelled", FailureReason::Cancelled),
        ("backend.epic.unauthorized", FailureReason::Failed),
    ] {
        legacy["error"] = serde_json::json!(message);
        let saved: Snapshot = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(saved.failure_reason, None);
        assert_eq!(saved.failure_reason(), Some(expected));
    }
    legacy["error"] = serde_json::json!("backend.epic.cancelled");
    legacy["phase"] = serde_json::json!("Running");
    let saved: Snapshot = serde_json::from_value(legacy).unwrap();
    assert_eq!(saved.failure_reason(), None);
}

#[test]
fn typed_failure_reason_is_independent_of_the_display_message() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    for (reason, message) in [
        (FailureReason::Cancelled, "renamed cancellation"),
        (FailureReason::Failed, "backend.epic.cloud.cancelled"),
    ] {
        control
            .update(|s| {
                s.phase = Phase::Failed;
                s.failure_reason = Some(reason);
                s.error = Some(message.into());
            })
            .unwrap();
        let saved = read_snapshot(root.path(), None).unwrap().unwrap();
        assert_eq!(saved.failure_reason(), Some(reason));
    }
}

#[test]
fn decisions_are_bound_to_account_session_revision_and_allowed_choices() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    control
        .update(|s| {
            s.cloud_phase = CloudPhase::Conflict;
            s.allowed_choices = vec![Choice::Local, Choice::Cloud];
        })
        .unwrap();
    let snapshot = control.snapshot();
    for (id, account, decision, choice) in [
        (
            "wrong".into(),
            snapshot.account_id.clone(),
            1,
            Choice::Local,
        ),
        (snapshot.id.clone(), "b".repeat(32), 1, Choice::Local),
        (
            snapshot.id.clone(),
            snapshot.account_id.clone(),
            2,
            Choice::Local,
        ),
        (
            snapshot.id.clone(),
            snapshot.account_id.clone(),
            1,
            Choice::LaunchAnyway,
        ),
    ] {
        assert!(
            send_command(
                &control,
                &Command::Choose {
                    id,
                    account,
                    decision,
                    choice,
                },
            )
            .is_err()
        );
        assert!(control.state.lock().unwrap().choice.is_none());
    }
    send_command(
        &control,
        &Command::Choose {
            id: snapshot.id,
            account: snapshot.account_id,
            decision: 1,
            choice: Choice::Cloud,
        },
    )
    .unwrap();
    assert_eq!(control.state.lock().unwrap().choice, Some(Choice::Cloud));
}

fn send_command(control: &Control, command: &Command) -> Result<Snapshot> {
    let (mut send, receive) = UnixStream::pair().unwrap();
    storage::send(&mut send, command).unwrap();
    server::handle(receive, control)?;
    storage::packet(send).map_err(Into::into)
}

fn resume_rosetta(
    control: &Control,
    id: &str,
    account: &str,
    decision_id: u64,
) -> Result<Snapshot> {
    send_command(
        control,
        &Command::ResumeAfterRosetta {
            id: id.into(),
            account: account.into(),
            decision_id,
        },
    )
}

async fn rosetta_request(control: &Control) -> RosettaRequest {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(request) = control.snapshot().rosetta {
                return request;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn intel_rosetta_wait_requires_matching_session_and_a_successful_new_probe() {
    let root = tempfile::tempdir().unwrap();
    let mut control = control(root.path());
    Arc::get_mut(&mut control).unwrap().rosetta.check = |path| {
        let count = path.with_extension("count");
        let previous = std::fs::read_to_string(&count).unwrap_or_else(|_| "0".into());
        std::fs::write(count, (previous.parse::<u32>().unwrap() + 1).to_string())?;
        Ok(path.exists())
    };
    let pending = Arc::clone(&control);
    let gate = tokio::spawn(async move { server::wait_for_rosetta(&pending).await });
    let request = rosetta_request(&control).await;
    let snapshot = control.snapshot();
    assert!(snapshot.game.is_none() && snapshot.starting.is_none());
    let saved = read_snapshot(root.path(), None).unwrap().unwrap();
    assert_eq!(saved.rosetta, Some(request.clone()));
    for (id, account, decision) in [
        ("wrong", snapshot.account_id.as_str(), request.decision_id),
        (snapshot.id.as_str(), "wrong", request.decision_id),
        (
            snapshot.id.as_str(),
            snapshot.account_id.as_str(),
            request.decision_id + 1,
        ),
    ] {
        assert!(resume_rosetta(&control, id, account, decision).is_err());
    }
    let unchanged = resume_rosetta(
        &control,
        &snapshot.id,
        &snapshot.account_id,
        request.decision_id,
    )
    .unwrap();
    assert_eq!(unchanged.rosetta, Some(request.clone()));
    assert!(!gate.is_finished());
    std::fs::write(&control.rosetta.helper, b"available").unwrap();
    let resumed = resume_rosetta(
        &control,
        &snapshot.id,
        &snapshot.account_id,
        request.decision_id,
    )
    .unwrap();
    assert!(resumed.rosetta.is_none());
    gate.await.unwrap().unwrap();
    assert_eq!(
        std::fs::read_to_string(control.rosetta.helper.with_extension("count")).unwrap(),
        "4"
    );
    assert!(
        resume_rosetta(
            &control,
            &snapshot.id,
            &snapshot.account_id,
            request.decision_id
        )
        .is_err()
    );
}

#[tokio::test]
async fn rosetta_cancellation_and_account_revocation_prevent_later_admission() {
    for revoke in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let control = control(root.path());
        let pending = Arc::clone(&control);
        let gate = tokio::spawn(async move { server::wait_for_rosetta(&pending).await });
        let request = rosetta_request(&control).await;
        let snapshot = control.snapshot();
        let command = if revoke {
            Command::Revoke {
                id: snapshot.id.clone(),
                account: snapshot.account_id.clone(),
            }
        } else {
            Command::Cancel {
                id: snapshot.id.clone(),
            }
        };
        send_command(&control, &command).unwrap();
        assert!(matches!(gate.await.unwrap(), Err(Error::Cancelled)));
        std::fs::write(&control.rosetta.helper, b"available").unwrap();
        assert!(
            resume_rosetta(
                &control,
                &snapshot.id,
                &snapshot.account_id,
                request.decision_id
            )
            .is_err()
        );
        assert!(matches!(
            server::spawn_game(
                &helper(root.path()),
                disposable_target(root.path()),
                &control,
                inspect_fixture
            )
            .await,
            Err(Error::Cancelled)
        ));
        assert!(!root.path().join("marker").exists());
        control.fail(&Error::Cancelled).unwrap();
        assert!(control.snapshot().rosetta.is_none());
    }
}

#[tokio::test]
async fn available_rosetta_needs_no_prompt_and_legacy_snapshots_remain_readable() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    std::fs::write(&control.rosetta.helper, b"available").unwrap();
    server::wait_for_rosetta(&control).await.unwrap();
    let mut legacy = serde_json::to_value(control.snapshot()).unwrap();
    legacy.as_object_mut().unwrap().remove("rosetta");
    let snapshot: Snapshot = serde_json::from_value(legacy).unwrap();
    assert!(snapshot.rosetta.is_none());
}

fn launch_fixture(root: &Path) -> (Configuration, epic_kit::client::LaunchPlan) {
    let executable = root.join("game");
    let config = Configuration {
        root: root.join("vault"),
        home: root.into(),
        install: root.into(),
        session_directory: root.join("session"),
        helper: root.join("helper"),
        namespace: "fixture".into(),
        catalog_item_id: "fixture".into(),
        app_name: "fixture".into(),
        platform: "Mac".into(),
        language: "en".into(),
    };
    let plan = epic_kit::client::LaunchPlan {
        app_name: "fixture".into(),
        platform: "Mac".into(),
        activity: None,
        ownership_token: None,
        target: forge_core::launch::LaunchTarget {
            executable: executable.to_string_lossy().into_owned(),
            arguments: vec![],
            working_directory: root.to_string_lossy().into_owned(),
            is_application_bundle: false,
        },
    };
    (config, plan)
}

fn write_macho(executable: &Path, cpu: u32) {
    std::fs::write(executable, crate::architecture::tests::thin(cpu, true)).unwrap();
}

#[test]
fn epic_target_checks_the_full_macho_before_repairing_executable_permission() {
    use crate::architecture::Architecture;
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("game");
    let (config, plan) = launch_fixture(root.path());
    std::fs::write(&executable, b"\xcf\xfa\xed\xfe").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        server::native_target(&plan, &config),
        Err(Error::Unsupported)
    ));
    assert_eq!(
        std::fs::metadata(&executable).unwrap().permissions().mode() & 0o100,
        0
    );
    for (cpu, expected) in [(INTEL, Architecture::Intel), (ARM64, Architecture::Arm64)] {
        write_macho(&executable, cpu);
        let (target, architecture, ownership) = server::native_target(&plan, &config).unwrap();
        assert_eq!(architecture, expected);
        assert_eq!(target.executable, executable.canonicalize().unwrap());
        assert!(ownership.is_none());
        assert_ne!(
            std::fs::metadata(&executable).unwrap().permissions().mode() & 0o100,
            0
        );
    }
}

#[tokio::test]
async fn rosetta_installation_refreshes_credentials_and_revalidates_the_new_target() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for refreshed_cpu in [INTEL, ARM64] {
        let root = tempfile::tempdir().unwrap();
        write_macho(&root.path().join("game"), INTEL);
        let control = control(root.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let prepared = {
            let pending = Arc::clone(&control);
            let calls = Arc::clone(&calls);
            let install = root.path().to_owned();
            tokio::spawn(async move {
                let (config, _) = launch_fixture(&install);
                server::prepare_game(&config, &pending, || {
                    let generation = calls.fetch_add(1, Ordering::SeqCst);
                    let (_, mut plan) = launch_fixture(&install);
                    if generation == 1 {
                        write_macho(&install.join("game"), refreshed_cpu);
                        if refreshed_cpu == ARM64 {
                            // The refreshed target is native and no longer needs Rosetta.
                            std::fs::remove_file(&pending.rosetta.helper).unwrap();
                        }
                    }
                    plan.target.arguments.push(format!("code-{generation}"));
                    plan.ownership_token = Some(format!("ownership-{generation}").into_bytes());
                    std::future::ready(Ok(plan))
                })
                .await
            })
        };
        let request = rosetta_request(&control).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let snapshot = control.snapshot();
        std::fs::write(&control.rosetta.helper, b"available").unwrap();
        resume_rosetta(
            &control,
            &snapshot.id,
            &snapshot.account_id,
            request.decision_id,
        )
        .unwrap();
        let prepared = prepared.await.unwrap().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(prepared.target.arguments[0], "code-1");
        let token = PathBuf::from(
            prepared.target.arguments[1]
                .strip_prefix("-epicovt=")
                .unwrap(),
        );
        assert_eq!(std::fs::read(&token).unwrap(), b"ownership-1");
        drop(prepared);
        assert!(!token.exists());
    }
}

#[test]
fn cancellation_after_admission_does_not_mean_game_exit_but_signout_revokes_cloud() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    *control.admission.lock().unwrap() = true;
    control
        .update(|s| {
            s.phase = Phase::Running;
            s.game = Some(identity(42));
        })
        .unwrap();
    let snapshot = control.snapshot();
    send_command(
        &control,
        &Command::Cancel {
            id: snapshot.id.clone(),
        },
    )
    .unwrap();
    assert!(!control.cancel.is_cancelled());
    send_command(
        &control,
        &Command::Revoke {
            id: snapshot.id,
            account: snapshot.account_id,
        },
    )
    .unwrap();
    assert!(control.cancel.is_cancelled());
    assert_eq!(control.snapshot().game, Some(identity(42)));
    assert!(!control.snapshot().finished());
}

#[tokio::test]
async fn preparation_waits_for_explicit_matching_begin_and_honors_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), server::await_begin(&control))
            .await
            .is_err()
    );
    let snapshot = control.snapshot();
    assert!(
        send_command(
            &control,
            &Command::Begin {
                id: snapshot.id.clone(),
                account: "b".repeat(32),
            },
        )
        .is_err()
    );
    assert!(!control.state.lock().unwrap().begun);
    send_command(
        &control,
        &Command::Begin {
            id: snapshot.id,
            account: snapshot.account_id,
        },
    )
    .unwrap();
    server::await_begin(&control).await.unwrap();
    control.cancel.cancel();
    assert!(matches!(
        server::await_begin(&control).await,
        Err(Error::Cancelled)
    ));
}

#[test]
fn revoke_is_serialized_with_the_exec_admission_gate() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    let admission = control.admission.lock().unwrap();
    let snapshot = control.snapshot();
    let (mut send, receive) = UnixStream::pair().unwrap();
    storage::send(
        &mut send,
        &Command::Revoke {
            id: snapshot.id,
            account: snapshot.account_id,
        },
    )
    .unwrap();
    let (ready, started) = std::sync::mpsc::channel();
    let (done, finished) = std::sync::mpsc::channel();
    let running = Arc::clone(&control);
    let worker = std::thread::spawn(move || {
        ready.send(()).unwrap();
        let result = server::handle(receive, &running);
        done.send(result).unwrap();
    });
    started.recv().unwrap();
    assert!(finished.recv_timeout(Duration::from_millis(30)).is_err());
    assert!(!control.cancel.is_cancelled());
    drop(admission);
    finished
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert!(control.cancel.is_cancelled());
}

#[test]
fn recovery_distinguishes_exec_handoff_reused_pid_and_failed_inspection() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    control
        .update(|s| {
            s.starting = Some(StartingGame::Process {
                identity: identity(42),
                executable: "/fixture/game".into(),
            });
        })
        .unwrap();
    let snapshot = control.snapshot();
    assert!(owns_child(&snapshot, |_| Ok(Some(identity(42)))).unwrap());
    let mut executing = identity(42);
    executing.executable = "/fixture/game".into();
    assert!(owns_child(&snapshot, |_| Ok(Some(executing.clone()))).unwrap());
    executing.executable = "/fixture/second-executable".into();
    assert!(owns_child(&snapshot, |_| Ok(Some(executing.clone()))).unwrap());
    let mut game_snapshot = snapshot.clone();
    game_snapshot.starting = None;
    game_snapshot.game = Some(identity(42));
    assert!(owns_child(&game_snapshot, |_| Ok(Some(executing.clone()))).unwrap());
    executing.birth_microseconds += 1;
    assert!(!owns_child(&snapshot, |_| Ok(Some(executing.clone()))).unwrap());
    assert!(owns_child(&snapshot, |_| Err(Error::Session)).is_err());
    assert!(!owns_child(&snapshot, |_| Ok(None)).unwrap());
}

#[test]
fn failed_phase_cannot_release_recorded_process_ownership() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    control
        .update(|s| {
            s.phase = Phase::Failed;
            s.game = Some(identity(42));
        })
        .unwrap();
    assert!(!control.snapshot().finished());
}

// The fixture re-enters the production exec gate in a disposable test process.
#[test]
#[ignore = "subprocess fixture, launched by the supervision tests"]
fn exec_child_fixture() {
    exec_game(std::io::stdin().lock()).unwrap();
}
fn helper(root: &Path) -> PathBuf {
    let path = root.join("helper");
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .replace('\'', "'\\''");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nexec '{executable}' --exact epic::tests::exec_child_fixture --ignored >/dev/null\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}
fn disposable_target(root: &Path) -> Target {
    Target {
        executable: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "printf fixture > marker; sleep 0.1; exit 7".into(),
        ],
        working_directory: root.into(),
        environment: HashMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
    }
}
fn inspect_fixture(pid: u32) -> Result<Option<Identity>> {
    Ok(Some(identity(pid)))
}

#[test]
fn real_supervisor_channel_is_observable_before_game_admission() {
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("inspect-helper");
    let expected = serde_json::to_string(&identity(std::process::id())).unwrap();
    std::fs::write(&helper, format!("#!/bin/sh\nprintf '%s\\n' '{expected}'\n")).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let session_directory = root.path().join("session");
    let request = Request {
        config: Configuration {
            root: root.path().join("vault"),
            home: root.path().into(),
            install: root.path().join("game"),
            session_directory: session_directory.clone(),
            helper: helper.clone(),
            namespace: "fixture".into(),
            catalog_item_id: "fixture".into(),
            app_name: "fixture".into(),
            platform: "Mac".into(),
            language: "en".into(),
        },
        id: uuid::Uuid::new_v4().to_string(),
        account_id: "a".repeat(32),
        exchange_code: "unused-fixture-code".into(),
        launch: true,
    };
    let mut input = Vec::new();
    storage::send(&mut input, &request).unwrap();
    let (writer, reader) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let server =
        std::thread::spawn(move || server::serve(input.as_slice(), writer, inspect_fixture));
    let initial: Snapshot = storage::packet(reader).unwrap();
    let mode = std::fs::metadata(initial.socket.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    let observer = Client::recover(&session_directory, &helper)
        .unwrap()
        .unwrap();
    let observed = observer.snapshot(None).unwrap();
    // The peer is responsive even when the observer rejects its directory.
    let mut direct = UnixStream::connect(&initial.socket).unwrap();
    direct
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    storage::send(
        &mut direct,
        &Command::Observe {
            id: initial.id.clone(),
            after: None,
        },
    )
    .unwrap();
    let peer: Snapshot = storage::packet(direct).unwrap();
    // No Begin is sent: this runs no Epic authentication, transfer or game.
    // Clean up even in the red case, where the ordinary client rejects the socket.
    let mut cleanup = UnixStream::connect(&initial.socket).unwrap();
    storage::send(&mut cleanup, &Command::Cancel { id: initial.id }).unwrap();
    let _: Snapshot = storage::packet(cleanup).unwrap();
    assert!(matches!(server.join().unwrap(), Err(Error::Cancelled)));
    let terminal = observer.snapshot(None).unwrap();
    assert!(terminal.finished());
    assert_eq!(terminal.failure_reason(), Some(FailureReason::Cancelled));
    assert_eq!(peer.phase, Phase::Preparing);
    assert!(peer.game.is_none());
    assert!(
        !observed.reconnecting,
        "live supervisor is stuck reconnecting with socket directory mode {mode:o}"
    );
    assert_eq!(observed.revision, peer.revision);
}

#[tokio::test]
async fn exec_is_admitted_only_after_durable_ownership_and_survives_observer_drop() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    let mut child = server::spawn_game(
        &helper(root.path()),
        disposable_target(root.path()),
        &control,
        inspect_fixture,
    )
    .await
    .unwrap();
    let saved = read_snapshot(root.path(), None).unwrap().unwrap();
    assert_eq!(saved.phase, Phase::Running);
    assert!(saved.starting.is_some());
    drop(control);
    assert_eq!(child.wait().await.unwrap().code(), Some(7));
    assert_eq!(
        std::fs::read(root.path().join("marker")).unwrap(),
        b"fixture"
    );
}

#[tokio::test]
async fn journal_failure_before_exec_never_runs_target() {
    let root = tempfile::tempdir().unwrap();
    let invalid = root.path().join("not-a-directory");
    std::fs::write(&invalid, b"fixture").unwrap();
    let control = control(&invalid);
    let helper = helper(root.path());
    let reaped = root.path().join("helper-exited");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\n/bin/cat >/dev/null\nsleep 0.05\nprintf done > '{}'\n",
            reaped.to_string_lossy().replace('\'', "'\\''")
        ),
    )
    .unwrap();
    assert!(
        server::spawn_game(
            &helper,
            disposable_target(root.path()),
            &control,
            inspect_fixture
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&reaped).unwrap(), b"done");
    assert!(control.snapshot().starting.is_none());
    assert!(!root.path().join("marker").exists());
}

fn inspect_executing_fixture(pid: u32) -> Result<Option<Identity>> {
    let mut executing = identity(pid);
    executing.executable = "/bin/sh".into();
    Ok(Some(executing))
}

#[tokio::test]
async fn game_presence_is_removed_at_exit_before_cloud_finalization() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    let target = disposable_target(root.path());
    let executable = target.executable.clone();
    let mut child = server::spawn_game(&helper(root.path()), target, &control, inspect_fixture)
        .await
        .unwrap();
    let mut publication = Vec::new();
    let exit = server::wait_for_game(
        &mut child,
        &control,
        inspect_executing_fixture,
        &executable,
        |active| publication.push(active),
    )
    .await
    .unwrap();
    assert_eq!(exit, 7);
    assert_eq!(publication, [true, false]);
    assert_eq!(control.snapshot().phase, Phase::Running);
}

#[tokio::test]
async fn game_presence_never_advertises_an_unexecuted_target_or_revoked_account() {
    for revoked in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let control = control(root.path());
        let target = disposable_target(root.path());
        let executable = target.executable.clone();
        let helper = helper(root.path());
        if !revoked {
            std::fs::write(
                &helper,
                "#!/bin/sh\n/bin/cat >/dev/null\nprintf 'exec-failed\\n'\nexit 126\n",
            )
            .unwrap();
        }
        let mut child = server::spawn_game(&helper, target, &control, inspect_fixture)
            .await
            .unwrap();
        if revoked {
            control.cancel.cancel();
        }
        let mut publication = Vec::new();
        let inspect = if revoked {
            inspect_executing_fixture
        } else {
            inspect_fixture
        };
        let result = server::wait_for_game(&mut child, &control, inspect, &executable, |active| {
            publication.push(active)
        })
        .await;
        if revoked {
            assert_eq!(result.unwrap(), 7);
        } else {
            assert!(matches!(result, Err(Error::Failed)));
            assert!(control.snapshot().starting.is_none());
            assert!(!root.path().join("marker").exists());
        }
        assert!(publication.is_empty());
    }
}

#[tokio::test]
async fn game_presence_revocation_withdraws_the_activity_while_the_process_keeps_running() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    let mut target = disposable_target(root.path());
    target.arguments[1] =
        "while [ ! -f release ]; do sleep 0.01; done; printf done > exited; exit 7".into();
    let executable = target.executable.clone();
    let mut child = server::spawn_game(&helper(root.path()), target, &control, inspect_fixture)
        .await
        .unwrap();
    let mut publication = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        server::wait_for_game(
            &mut child,
            &control,
            inspect_executing_fixture,
            &executable,
            |active| {
                publication.push(active);
                if active {
                    control.cancel.cancel();
                } else {
                    assert!(!root.path().join("exited").exists());
                    std::fs::write(root.path().join("release"), b"").unwrap();
                }
            },
        ),
    )
    .await;
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let exit = result.unwrap().unwrap();
    assert_eq!(publication, [true, false]);
    assert_eq!(exit, 7);
    assert_eq!(std::fs::read(root.path().join("exited")).unwrap(), b"done");
}

#[test]
fn snapshot_roundtrip_contains_no_authentication_material() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    write_snapshot(root.path(), &control.snapshot()).unwrap();
    let raw = std::fs::read_to_string(root.path().join("session.json")).unwrap();
    assert!(
        !raw.contains("exchange_code")
            && !raw.contains("refresh_token")
            && !raw.contains("AUTH_PASSWORD")
    );
    assert_eq!(
        read_snapshot(root.path(), None).unwrap().unwrap(),
        control.snapshot()
    );
}
