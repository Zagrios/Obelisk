use forge_steam_native::native;
use std::process::{Command, Stdio};

#[test]
fn identity_helper_flushes_its_bounded_response_before_immediate_exit() {
    let expected = native::process_identity(std::process::id()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_forge-steam-host"))
        .args(["process-identity", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    let actual: Option<game_runtime::supervisor::Identity> =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, Some(expected));
    let output = Command::new(env!("CARGO_BIN_EXE_forge-steam-host"))
        .args(["process-identity", "2147483647"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"null\n");
}

#[test]
fn native_supervisor_acknowledges_and_reports_preparation_failure_without_valve() {
    use game_runtime::{
        Config, Launch,
        protocol::Credentials,
        steam::Request,
        supervisor::{Phase, Target},
    };
    use std::{
        path::PathBuf,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Scratch(std::env::temp_dir().join(format!(
            "forge-ack-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    std::fs::create_dir(&root.0).unwrap();
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_forge-steam-host"));
    let launch = Launch::new(Credentials {
        steam_id: 123,
        account_name: "fixture".into(),
        refresh_token: "fixture-no-steam-account".into(),
    });
    let client = launch
        .start_supervised(Request {
            cloud: None,
            id: String::new(),
            root: root.0.join("sessions"),
            config: Config {
                cloud_save_roots: vec![],
                app_id: 42,
                helpers: helper.parent().unwrap().into(),
                package: root.0.join("missing.zip"),
                cache: root.0.join("cache"),
                diagnostics: root.0.join("logs"),
                steam_api: root.0.join("missing-api.dylib"),
                wine: None,
                home: root.0.clone(),
            },
            target: Target {
                executable: root.0.join("must-not-run"),
                arguments: vec![],
                working_directory: root.0.clone(),
                environment: Default::default(),
                log: None,
            },
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut after = None;
    loop {
        let snapshot = client.snapshot(after).unwrap();
        assert_eq!(snapshot.store.app_id, 42);
        assert_eq!(snapshot.account_id, 123);
        assert!(snapshot.game.is_none());
        if snapshot.finished() {
            assert_eq!(snapshot.phase, Phase::Failed);
            assert_eq!(
                snapshot.error.as_deref(),
                Some("backend.steam.runtime.unavailable")
            );
            break;
        }
        assert!(Instant::now() < deadline);
        after = Some(snapshot.revision);
    }
}
