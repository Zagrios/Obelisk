use super::*;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// A shell exec helper in `directory`: it records its subcommand, becomes
/// the game only on an admission that follows its `recorded` identity, and
/// then exits with 7.
fn helper(directory: &Path) -> Helper {
    let program = directory.join("helper");
    fs::write(
        &program,
        "#!/bin/sh\n\
         printf '%s' \"$1\" > \"$FIXTURE/subcommand\"\n\
         IFS= read -r admission || { printf 'exec-failed\\n'; exit 126; }\n\
         [ -f \"$FIXTURE/recorded\" ] || { printf 'exec-failed\\n'; exit 125; }\n\
         printf '%s\\n' \"$admission\" > \"$FIXTURE/admission\"\n\
         exec </dev/null >/dev/null\n\
         exit 7\n",
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let environment = HashMap::from([
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        (
            "FIXTURE".to_owned(),
            directory.to_string_lossy().into_owned(),
        ),
    ]);
    Helper::new(&program, &environment, admission(directory)).unwrap()
}

fn admission(directory: &Path) -> Admission {
    Admission {
        executable: "/bin/sh".into(),
        arguments: vec!["game.sh".into()],
        working_directory: directory.into(),
        log: Some(directory.join("game.log")),
        log_header: Some("obelisk: engine=wine-macos\n".into()),
    }
}

fn inspect(pid: u32) -> Result<Option<Identity>, Error> {
    Ok(Some(Identity {
        pid,
        uid: 501,
        birth_seconds: 1,
        birth_microseconds: 0,
        executable: "/fixture/helper".into(),
    }))
}

#[tokio::test]
async fn a_helper_is_admitted_as_game_exec_once_its_starting_identity_is_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    let mut game = helper(&directory).spawn().unwrap();
    let spawned = game.pid().unwrap();
    let mut starting = None;

    let admitted = game.admit(inspect, |recorded| {
        fs::write(directory.join("recorded"), b"").unwrap();
        starting = Some(recorded);
        Ok(())
    });

    assert_eq!(admitted, Ok(spawned));
    assert_eq!(
        starting,
        Some(StartingGame::Process {
            identity: inspect(spawned).unwrap().unwrap(),
            executable: Path::new("/bin/sh").canonicalize().unwrap(),
        })
    );
    assert_eq!(game.confirm(inspect).await, Execution::Started);
    assert_eq!(game.exit().await.code(), Some(7));
    assert_eq!(
        fs::read_to_string(directory.join("subcommand")).unwrap(),
        "game-exec"
    );
    let sent = fs::read(directory.join("admission")).unwrap();
    assert_eq!(receive(sent.as_slice()).unwrap(), admission(&directory));
}

#[tokio::test]
async fn a_helper_whose_identity_is_not_recorded_never_becomes_the_game() {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path().canonicalize().unwrap();
    let mut game = helper(&directory).spawn().unwrap();

    let admitted = game.admit(inspect, |_| Err(Error::Storage));

    assert_eq!(admitted, Err(Error::Storage));
    assert_eq!(game.confirm(inspect).await, Execution::Failed);
    assert_eq!(game.exit().await.code(), Some(126));
    assert!(!directory.join("admission").exists());
}

fn child(script: &str, output: bool) -> tokio::process::Child {
    tokio::process::Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(if output {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn()
        .unwrap()
}

#[tokio::test]
async fn pipe_confirmation_precedes_actual_exit_and_keeps_runtime_available() {
    let mut game = child("exec 1>/dev/null; sleep 0.1; exit 7", true);
    assert_eq!(
        confirm_execution(&mut game, Path::new("/bin/sh"), |_| None).await,
        Execution::Started
    );
    assert!(game.try_wait().unwrap().is_none());
    let progressed = Arc::new(AtomicBool::new(false));
    let progress = progressed.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        progress.store(true, Ordering::Release);
    });
    assert_eq!(wait_for_exit(&mut game).await.code(), Some(7));
    assert!(progressed.load(Ordering::Acquire));
}

#[tokio::test]
async fn explicit_exec_failure_and_missing_confirmation_remain_distinct() {
    let mut failed = child("printf 'exec-failed\\n'; exit 126", true);
    assert_eq!(
        confirm_execution(&mut failed, Path::new("/bin/sh"), |_| None).await,
        Execution::Failed
    );
    assert_eq!(wait_for_exit(&mut failed).await.code(), Some(126));
    let mut unknown = child("exit 0", false);
    assert_eq!(
        confirm_execution(&mut unknown, Path::new("/bin/sh"), |_| None).await,
        Execution::ExitedUnconfirmed
    );
    assert_eq!(wait_for_exit(&mut unknown).await.code(), Some(0));
}

#[tokio::test]
async fn unknown_reply_does_not_release_a_live_child() {
    let mut game = child("printf unknown; exec 1>/dev/null; sleep 0.1; exit 4", true);
    let start = Instant::now();
    assert_eq!(
        confirm_execution(&mut game, Path::new("/bin/sh"), |_| None).await,
        Execution::ExitedUnconfirmed
    );
    assert!(start.elapsed() >= Duration::from_millis(80));
    assert_eq!(wait_for_exit(&mut game).await.code(), Some(4));
}

#[tokio::test]
async fn helper_identity_is_never_published_as_the_game() {
    let mut game = child("sleep 0.1", false);
    let identity = wait_identity(&mut game, Path::new("/bin/sh"), |pid| {
        Some(Identity {
            pid,
            uid: 0,
            birth_seconds: 1,
            birth_microseconds: 0,
            executable: "/not-the-game".into(),
        })
    })
    .await;
    assert!(identity.is_none());
    assert_eq!(wait_for_exit(&mut game).await.code(), Some(0));
}
