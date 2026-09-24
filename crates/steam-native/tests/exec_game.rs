//! Both exec helpers through their process boundary: the arm64
//! `forge-steam-host game-exec` and the Intel `forge-steam-sdk exec-game`.
//! Each becomes the game only after the supervisor's admission message.
use game_runtime::supervisor::exec::Admission;
use std::{
    ffi::OsStr,
    io::{Read, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::Duration,
};

/// The two helpers, as the supervisor spawns them.
fn helpers() -> [Command; 2] {
    let mut arm = Command::new(env!("CARGO_BIN_EXE_forge-steam-host"));
    arm.arg("game-exec");
    let mut intel = Command::new(env!("CARGO_BIN_EXE_forge-steam-sdk"));
    intel.arg("exec-game");
    [arm, intel]
}

fn spawn(mut helper: Command) -> Child {
    helper
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn admit(child: &mut Child, admission: &Admission) {
    let mut input = child.stdin.take().unwrap();
    game_runtime::supervisor::exec::send(&mut input, admission).unwrap();
}

/// Waits for the helper's exec confirmation, then releases the replacement.
/// Returns the handshake bytes, whether the replacement was still running when
/// they arrived, and its final exit status.
fn confirm_replacement(child: &mut Child, directory: &Path) -> (Vec<u8>, bool, ExitStatus) {
    let mut control = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        control.read_to_end(&mut bytes).unwrap();
        let _ = sender.send(bytes);
    });
    // A successful exec closes its control descriptor while the replacement is
    // still alive. The fixture cannot exit successfully until this test releases it.
    let handshake = receiver.recv_timeout(Duration::from_secs(3));
    let running = child.try_wait().unwrap().is_none();
    std::fs::write(directory.join("finish"), b"").unwrap();
    let status = child.wait().unwrap();
    reader.join().unwrap();
    (handshake.unwrap(), running, status)
}

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("forge-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

#[test]
fn a_failed_exec_reports_failure_without_starting_a_replacement() {
    for helper in helpers() {
        let mut child = spawn(helper);
        admit(
            &mut child,
            &Admission {
                executable: "/forge-missing-game-fixture".into(),
                arguments: vec![],
                working_directory: "/tmp".into(),
            },
        );
        let result = child.wait_with_output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(result.stdout, game_runtime::protocol::EXEC_FAILED);
    }
}

#[test]
fn a_missing_or_invalid_admission_never_executes_the_target() {
    let marker = scratch("missing-admission").join("started");
    let complete = serde_json::to_vec(&Admission {
        executable: "/usr/bin/touch".into(),
        arguments: vec![marker.to_string_lossy().into_owned()],
        working_directory: "/tmp".into(),
    })
    .unwrap();
    let relative = serde_json::to_vec(&Admission {
        executable: "touch".into(),
        arguments: vec![marker.to_string_lossy().into_owned()],
        working_directory: "/tmp".into(),
    })
    .unwrap();
    // Losing the supervisor before admission closes the pipe mid-message.
    let inputs: [&[u8]; 4] = [b"", b"1", &complete[..complete.len() - 1], &relative];
    for input in inputs {
        for helper in helpers() {
            let mut child = spawn(helper);
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(input).unwrap();
            drop(pipe);
            let result = child.wait_with_output().unwrap();
            let started = marker.exists();
            if started {
                std::fs::remove_file(&marker).unwrap();
            }
            assert!(!started, "target executed without supervisor admission");
            assert_eq!(result.status.code(), Some(1));
            assert_eq!(result.stdout, game_runtime::protocol::EXEC_FAILED);
        }
    }
}

#[test]
fn the_game_keeps_the_helper_pid_its_environment_and_default_signals() {
    let directory = scratch("exec-game");
    let executable = directory.join("game-fixture");
    let adapter = directory.join("adapter-fixture.dylib");
    let source = directory.join("adapter.c");
    std::fs::write(&source, "int forge_fixture(void) { return 0; }\n").unwrap();
    for (source, output, library) in [
        (
            Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/exec_game.c"
            )),
            &executable,
            false,
        ),
        (source.as_path(), &adapter, true),
    ] {
        let mut compiler = Command::new("/usr/bin/clang");
        if library {
            compiler.arg("-dynamiclib");
        }
        assert!(
            compiler
                .arg(source)
                .arg("-o")
                .arg(output)
                .status()
                .unwrap()
                .success()
        );
    }
    for mut helper in helpers() {
        let _ = std::fs::remove_file(directory.join("finish"));
        // The supervisor sets the game's environment when it spawns the
        // helper; only the admission message names the target.
        helper
            .current_dir("/")
            .env_clear()
            .env("EXPECTED_CWD", &directory)
            .env("EXPECTED_ADAPTER", &adapter)
            .env("DYLD_INSERT_LIBRARIES", &adapter)
            .env(
                "FORGE_PRIVATE_STEAM_DISCOVERY",
                "com.zagrios.forge.discovery.fixture",
            )
            .env("FORGE_FIXTURE_BYTES", OsStr::from_bytes(b"\xff\xfe"));
        let mut child = spawn(helper);
        let pid = child.id();
        admit(
            &mut child,
            &Admission {
                executable: executable.clone(),
                arguments: vec!["a b".into(), "$not-a-shell".into()],
                working_directory: directory.clone(),
            },
        );
        let (handshake, running, status) = confirm_replacement(&mut child, &directory);
        let proof = std::fs::read_to_string(directory.join("game-proof.txt")).unwrap();
        assert_eq!(status.code(), Some(23));
        assert!(handshake.is_empty());
        assert!(running);
        assert_eq!(proof.trim(), pid.to_string());
    }
    std::fs::remove_dir_all(directory).unwrap();
}
