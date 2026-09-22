use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    time::Duration,
};

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

#[test]
fn game_exec_failure_returns_failure_without_starting_a_replacement() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-steam-sdk"))
        .args(["exec-game", "/forge-missing-game-fixture"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"1").unwrap();
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(result.stdout, steam_runtime::protocol::EXEC_FAILED);
}

#[test]
fn missing_or_invalid_admission_never_executes_the_target() {
    let marker =
        std::env::temp_dir().join(format!("forge-missing-admission-{}", std::process::id()));
    for input in [b"".as_slice(), b"0"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_forge-steam-sdk"))
            .args(["exec-game", "/usr/bin/touch"])
            .arg(&marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let result = child.wait_with_output().unwrap();
        let started = marker.exists();
        if started {
            std::fs::remove_file(&marker).unwrap();
        }
        assert!(!started, "target executed without supervisor admission");
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(result.stdout, steam_runtime::protocol::EXEC_FAILED);
    }
}

#[test]
fn game_exec_preserves_pid_arguments_environment_and_working_directory() {
    let directory = std::env::temp_dir().join(format!("forge-exec-game-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let executable = directory.join("game-fixture");
    let adapter = directory.join("adapter-fixture.dylib");
    let source = directory.join("adapter.c");
    std::fs::write(&source, "int forge_fixture(void) { return 0; }\n").unwrap();
    for (source, output, library) in [
        (
            std::path::Path::new(concat!(
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
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-steam-sdk"))
        .args(["exec-game"])
        .arg(&executable)
        .args(["a b", "$not-a-shell"])
        .current_dir(&directory)
        .env_clear()
        .env("EXPECTED_CWD", &directory)
        .env("EXPECTED_ADAPTER", &adapter)
        .env("DYLD_INSERT_LIBRARIES", &adapter)
        .env(
            "FORGE_PRIVATE_STEAM_DISCOVERY",
            "com.zagrios.forge.discovery.fixture",
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    child.stdin.take().unwrap().write_all(b"1").unwrap();
    let (handshake, running, status) = confirm_replacement(&mut child, &directory);
    let proof = std::fs::read_to_string(directory.join("game-proof.txt")).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(status.code(), Some(23));
    assert!(handshake.is_empty());
    assert!(running);
    assert_eq!(proof.trim(), pid.to_string());
}

fn epic_child(target: &steam_runtime::supervisor::Target) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_forge-steam-host"))
        .arg("epic-exec")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    serde_json::to_writer(&mut input, target).unwrap();
    input.write_all(b"\n").unwrap();
    child
}

#[test]
fn epic_exec_failure_uses_the_shared_confirmation_protocol() {
    let result = epic_child(&steam_runtime::supervisor::Target {
        executable: "/forge-missing-epic-game-fixture".into(),
        arguments: vec![],
        working_directory: "/tmp".into(),
        environment: Default::default(),
    })
    .wait_with_output()
    .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(result.stdout, steam_runtime::protocol::EXEC_FAILED);
}

#[test]
fn epic_exec_confirms_replacement_before_exit_and_preserves_parentage() {
    let directory = std::env::temp_dir().join(format!("forge-epic-exec-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let directory = directory.canonicalize().unwrap();
    let mut child = epic_child(&steam_runtime::supervisor::Target {
        executable: "/bin/sh".into(),
        arguments: vec!["-c".into(), "printf '%s' \"$$\" > proof; i=0; while [ ! -e finish ] && [ \"$i\" -lt 500 ]; do /bin/sleep 0.01; i=$((i + 1)); done; exit 23".into()],
        working_directory: directory.clone(),
        environment: Default::default(),
    });
    let pid = child.id();
    let (handshake, running, status) = confirm_replacement(&mut child, &directory);
    let proof = std::fs::read_to_string(directory.join("proof")).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    assert_eq!(status.code(), Some(23));
    assert!(handshake.is_empty());
    assert!(running);
    assert_eq!(proof, pid.to_string());
}
