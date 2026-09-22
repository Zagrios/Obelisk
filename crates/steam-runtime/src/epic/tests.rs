use super::*;
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
        let (mut send, receive) = UnixStream::pair().unwrap();
        storage::send(
            &mut send,
            &Command::Choose {
                id,
                account,
                decision,
                choice,
            },
        )
        .unwrap();
        assert!(server::handle(receive, &control).is_err());
        assert!(control.state.lock().unwrap().choice.is_none());
    }
    let (mut send, receive) = UnixStream::pair().unwrap();
    storage::send(
        &mut send,
        &Command::Choose {
            id: snapshot.id,
            account: snapshot.account_id,
            decision: 1,
            choice: Choice::Cloud,
        },
    )
    .unwrap();
    server::handle(receive, &control).unwrap();
    assert_eq!(control.state.lock().unwrap().choice, Some(Choice::Cloud));
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
    let (mut send, receive) = UnixStream::pair().unwrap();
    storage::send(
        &mut send,
        &Command::Cancel {
            id: snapshot.id.clone(),
        },
    )
    .unwrap();
    server::handle(receive, &control).unwrap();
    assert!(!control.cancel.is_cancelled());
    let (mut send, receive) = UnixStream::pair().unwrap();
    storage::send(
        &mut send,
        &Command::Revoke {
            id: snapshot.id,
            account: snapshot.account_id,
        },
    )
    .unwrap();
    server::handle(receive, &control).unwrap();
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
    let (mut send, receive) = UnixStream::pair().unwrap();
    storage::send(
        &mut send,
        &Command::Begin {
            id: snapshot.id.clone(),
            account: "b".repeat(32),
        },
    )
    .unwrap();
    assert!(server::handle(receive, &control).is_err());
    assert!(!control.state.lock().unwrap().begun);
    let (mut send, receive) = UnixStream::pair().unwrap();
    storage::send(
        &mut send,
        &Command::Begin {
            id: snapshot.id,
            account: snapshot.account_id,
        },
    )
    .unwrap();
    server::handle(receive, &control).unwrap();
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
            "#!/bin/sh\nexec '{executable}' --exact epic::tests::exec_child_fixture --ignored\n"
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

#[test]
fn exec_is_admitted_only_after_durable_ownership_and_survives_observer_drop() {
    let root = tempfile::tempdir().unwrap();
    let control = control(root.path());
    let mut child = server::spawn_game(
        &helper(root.path()),
        disposable_target(root.path()),
        &control,
        inspect_fixture,
    )
    .unwrap();
    let saved = read_snapshot(root.path(), None).unwrap().unwrap();
    assert_eq!(saved.phase, Phase::Running);
    assert!(saved.starting.is_some());
    drop(control);
    assert_eq!(child.wait().unwrap().code(), Some(7));
    assert_eq!(
        std::fs::read(root.path().join("marker")).unwrap(),
        b"fixture"
    );
}

#[test]
fn journal_failure_before_exec_never_runs_target() {
    let root = tempfile::tempdir().unwrap();
    let invalid = root.path().join("not-a-directory");
    std::fs::write(&invalid, b"fixture").unwrap();
    let control = control(&invalid);
    assert!(
        server::spawn_game(
            &helper(root.path()),
            disposable_target(root.path()),
            &control,
            inspect_fixture
        )
        .is_err()
    );
    assert!(!root.path().join("marker").exists());
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
