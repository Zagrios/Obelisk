//! The Game session lifecycle through the supervisor's interface: a real
//! supervisor process, its control socket and an exec helper process. Each
//! storefront's harness runs every scenario; its fakes follow one script and
//! record one vocabulary of events. Only process inspection is replaced.
use super::*;
use crate::supervisor::client::Client;
use std::{fs, path::Path};

mod recording;

/// What the storefront's fakes do at each step.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Script {
    /// The game's shell body, run in the test directory.
    pub game: String,
    /// Cloud preparation waits until `release-cloud` exists.
    pub hold_cloud: bool,
    /// Cloud preparation asks a question and waits for its answer.
    pub ask: bool,
    /// Cloud finalization waits until `release-finish` exists.
    pub hold_finish: bool,
    /// The game is an Intel executable.
    pub intel: bool,
    pub helper: Helper,
    /// The admission names an executable that does not exist.
    pub missing: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Helper {
    #[default]
    Exec,
    /// Holds before exec until `release-exec` exists.
    Hold,
    /// Exits without confirming its exec.
    Unconfirmed,
    /// Confirms its exec only after the game has already been reaped.
    Late,
}

/// A storefront under test.
pub(crate) trait Harness: 'static {
    type P: Provider;
    /// Selects this harness in the re-entered supervisor process.
    const NAME: &'static str;
    fn account() -> <Self::P as Provider>::Account;
    fn other_account() -> <Self::P as Provider>::Account;
    /// Writes this storefront's handoff for a launch.
    fn handoff(
        directory: &Path,
        sessions: &Path,
        id: &str,
        pipe: &mut std::os::unix::net::UnixStream,
    ) -> Result<(), Error>;
    /// Runs this storefront's supervisor with its fakes.
    fn serve(
        input: std::io::StdinLock<'static>,
        output: std::io::StderrLock<'static>,
    ) -> Result<(), String>;
    /// Its UI sends Begin after each start, as SailOver does for Epic.
    fn begins() -> bool {
        false
    }
    /// A running record in the format and under the names the previous
    /// build used.
    fn previous_build_record(id: &str, live: &Identity, socket: &Path) -> serde_json::Value;
}

/// The decision a pending Cloud question waits on.
fn question<P: Provider>(record: &Record<P>) -> Option<u64> {
    (!record.cloud.allowed_choices.is_empty()).then_some(record.cloud.decision_id)
}

fn choice(cloud: bool) -> crate::cloud::Choice {
    if cloud {
        crate::cloud::Choice::Cloud
    } else {
        crate::cloud::Choice::Local
    }
}

/// Events the fakes of every storefront record, in the order they occur.
const SHARED: [&str; 8] = [
    "cloud.prepare",
    "answered",
    "target",
    "cloud.started",
    "presence:true",
    "presence:false",
    "cloud.finished",
    "cloud.aborted",
];

pub(crate) fn record(directory: &Path, event: &str) {
    use std::io::Write;
    let mut events = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join("events"))
        .unwrap();
    writeln!(events, "{event}").unwrap();
}

fn events(directory: &Path) -> Vec<String> {
    fs::read_to_string(directory.join("events"))
        .unwrap_or_default()
        .lines()
        .filter(|event| SHARED.iter().any(|shared| event.starts_with(shared)))
        .map(Into::into)
        .collect()
}

/// The test directory of the re-entered supervisor, and its script.
pub(crate) fn fixture_directory() -> (PathBuf, Script) {
    let directory = PathBuf::from(std::env::var_os("SAILOVER_SESSION_FIXTURE").unwrap());
    let script = serde_json::from_slice(&fs::read(directory.join("script.json")).unwrap()).unwrap();
    (directory, script)
}

/// Waits in a fake's step until `release` exists, or the launch is stopped.
pub(crate) async fn hold<E>(
    directory: &Path,
    release: &str,
    check: impl Fn() -> Result<(), E>,
) -> Result<(), E> {
    while !directory.join(release).exists() {
        check()?;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Re-entered as a disposable supervisor process.
#[test]
fn supervisor_fixture() {
    let Some(directory) = std::env::var_os("SAILOVER_SESSION_FIXTURE") else {
        return;
    };
    let directory = PathBuf::from(directory);
    let harness = std::env::var("SAILOVER_SESSION_HARNESS").unwrap();
    let (input, output) = (std::io::stdin().lock(), std::io::stderr().lock());
    let served = match harness.as_str() {
        recording::Recording::NAME => recording::Recording::serve(input, output),
        recording::Gated::NAME => recording::Gated::serve(input, output),
        crate::steam::launch::tests::SteamHarness::NAME => {
            crate::steam::launch::tests::SteamHarness::serve(input, output)
        }
        crate::epic::launch::tests::EpicHarness::NAME => {
            crate::epic::launch::tests::EpicHarness::serve(input, output)
        }
        other => panic!("unknown harness {other}"),
    };
    if let Err(error) = &served {
        fs::write(directory.join("serve-error"), error).unwrap();
    }
    // Published atomically: a reader never sees an empty code.
    let code = directory.join("exit-code.partial");
    fs::write(&code, if served.is_ok() { "0" } else { "1" }).unwrap();
    fs::rename(code, directory.join("exit-code")).unwrap();
}

// Only the platform identity adapter is replaced.
fn identity(pid: u32) -> Option<Identity> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "uid=,lstart=,comm="])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if !out.status.success() || fields.len() < 7 {
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
pub(crate) fn inspect(pid: u32) -> Result<Option<Identity>, Error> {
    Ok(identity(pid))
}
fn probe(_: &Path, pid: u32) -> Result<Option<Identity>, Error> {
    Ok(identity(pid))
}

/// A shell exec helper: it waits for one complete admission naming an
/// existing executable, enters its working directory, confirms by closing its
/// stdout, then runs the game.
fn helper(directory: &Path, helper: Helper) -> String {
    let directory = directory.to_string_lossy().replace('\'', "'\\''");
    let before = match helper {
        Helper::Exec => String::new(),
        Helper::Hold => format!(
            "printf ready > '{directory}/exec-ready'\nwhile [ ! -f '{directory}/release-exec' ]; do /bin/sleep 0.01; done\n"
        ),
        // A background process keeps the confirmation pipe open past exit.
        Helper::Unconfirmed => "/bin/sleep 2 &\nexit 7\n".into(),
        // Within the reply window that follows reaping, not before it.
        Helper::Late => "/bin/sleep 0.05 &\n".into(),
    };
    let field = |name: &str| {
        format!(
            "\"$(printf '%s' \"$admission\" | /usr/bin/sed -n 's/.*\"{name}\":\"\\([^\"]*\\)\".*/\\1/p')\""
        )
    };
    format!(
        "#!/bin/sh\n\
         IFS= read -r admission || {{ printf 'exec-failed\\n'; exit 126; }}\n\
         executable={}\n\
         [ -x \"$executable\" ] || {{ printf 'exec-failed\\n'; exit 126; }}\n\
         cd {} || {{ printf 'exec-failed\\n'; exit 126; }}\n\
         {before}exec </dev/null >/dev/null\n\
         exec /bin/sh '{directory}/game.sh'\n",
        field("executable"),
        field("working_directory"),
    )
}

/// Installs the script's exec helper at `path`.
pub(crate) fn write_helper(path: &Path, directory: &Path, script: &Script) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, helper(directory, script.helper)).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// The game a fake storefront names in its target.
pub(crate) fn target(directory: &Path, script: &Script) -> Target {
    Target {
        executable: if script.missing {
            "/sailover-missing-game-fixture".into()
        } else {
            "/bin/sh".into()
        },
        arguments: vec![directory.join("game.sh").to_string_lossy().into()],
        working_directory: directory.into(),
        environment: [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
        log: None,
    }
}

pub(crate) struct Run<H: Harness> {
    _root: tempfile::TempDir,
    pub directory: PathBuf,
    pub sessions: PathBuf,
    pub client: Client<H::P>,
}

fn prepare(script: &Script) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let sessions = directory.join("sessions");
    fs::write(
        directory.join("script.json"),
        serde_json::to_vec(script).unwrap(),
    )
    .unwrap();
    fs::write(directory.join("game.sh"), &script.game).unwrap();
    (root, directory, sessions)
}

fn fixture_command<H: Harness>(directory: &Path) -> Process {
    let mut command = Process::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "supervisor::session::tests::supervisor_fixture"])
        .env("SAILOVER_SESSION_FIXTURE", directory)
        .env("SAILOVER_SESSION_HARNESS", H::NAME);
    command
}

fn start<H: Harness>(directory: &Path, sessions: &Path) -> Result<Client<H::P>, Error> {
    let id = uuid::Uuid::new_v4().to_string();
    let account = H::account();
    Client::<H::P>::start_with(
        sessions,
        Path::new("unused"),
        id.clone(),
        fixture_command::<H>(directory),
        probe,
        |pipe| H::handoff(directory, sessions, &id, pipe),
        |ack| ack.account_id == account,
        || false,
    )
}

pub(crate) fn launch<H: Harness>(script: Script) -> Run<H> {
    let run = launch_without_begin::<H>(script);
    if H::begins() {
        run.client.begin().unwrap();
    }
    run
}

fn launch_without_begin<H: Harness>(script: Script) -> Run<H> {
    let (root, directory, sessions) = prepare(&script);
    let client = start::<H>(&directory, &sessions).unwrap_or_else(|error| {
        let served = fs::read_to_string(directory.join("serve-error")).unwrap_or_default();
        panic!("start failed with {error:?}; the supervisor reported {served:?}")
    });
    Run {
        _root: root,
        directory,
        sessions,
        client,
    }
}

impl<H: Harness> Run<H> {
    pub(crate) fn until(&self, predicate: impl Fn(&Record<H::P>) -> bool) -> Record<H::P> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let record = self.client.snapshot(None).unwrap();
            if predicate(&record) {
                return record;
            }
            assert!(Instant::now() < deadline, "last state: {record:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    pub(crate) fn wait_for(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.directory.join(name).exists() {
            assert!(Instant::now() < deadline, "{name} never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    pub(crate) fn release(&self, name: &str) {
        fs::write(self.directory.join(name), b"").unwrap();
    }
    fn events(&self) -> Vec<String> {
        events(&self.directory)
    }
    fn presence(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|event| event.starts_with("presence"))
            .collect()
    }
    fn exit_code(&self) -> String {
        self.wait_for("exit-code");
        fs::read_to_string(self.directory.join("exit-code")).unwrap()
    }
    fn ran(&self) -> bool {
        self.directory.join("ran").exists()
    }
}

pub(crate) const QUICK: &str = "printf ran > ran\nexit 5\n";
pub(crate) const HELD: &str =
    "printf ran > ran\nwhile [ ! -f release-game ]; do /bin/sleep 0.01; done\nexit 3\n";

pub(crate) fn script(game: &str) -> Script {
    Script {
        game: game.into(),
        ..Script::default()
    }
}

/// Instantiates every shared scenario for a harness.
macro_rules! lifecycle_suite {
    ($harness:ty) => {
        use $crate::supervisor::session::tests as lifecycle;
        #[test]
        fn a_launch_prepares_in_order_then_runs_its_game_and_settles_cloud() {
            lifecycle::a_launch_prepares_in_order_then_runs_its_game_and_settles_cloud::<$harness>()
        }
        #[test]
        fn running_is_published_only_after_exec_is_confirmed() {
            lifecycle::running_is_published_only_after_exec_is_confirmed::<$harness>()
        }
        #[test]
        fn a_detached_session_keeps_its_game_reserved_and_recovers() {
            lifecycle::a_detached_session_keeps_its_game_reserved_and_recovers::<$harness>()
        }
        #[test]
        fn a_lost_acknowledgement_never_abandons_an_accepted_launch() {
            lifecycle::a_lost_acknowledgement_never_abandons_an_accepted_launch::<$harness>()
        }
        #[test]
        fn cancelling_before_spawn_runs_nothing_and_ends_silently() {
            lifecycle::cancelling_before_spawn_runs_nothing_and_ends_silently::<$harness>()
        }
        #[test]
        fn cancelling_after_spawn_leaves_the_game_running() {
            lifecycle::cancelling_after_spawn_leaves_the_game_running::<$harness>()
        }
        #[test]
        fn a_supervisor_asked_to_quit_stops_its_game_and_still_finishes_the_session() {
            lifecycle::a_supervisor_asked_to_quit_stops_its_game_and_still_finishes_the_session::<
                $harness,
            >()
        }
        #[test]
        fn stopping_a_running_game_ends_it_and_still_finishes_the_session() {
            lifecycle::stopping_a_running_game_ends_it_and_still_finishes_the_session::<$harness>()
        }
        #[test]
        fn revoking_before_admission_abandons_the_launch_without_presence() {
            lifecycle::revoking_before_admission_abandons_the_launch_without_presence::<$harness>()
        }
        #[test]
        fn revoking_a_running_game_withdraws_presence_but_keeps_the_game() {
            lifecycle::revoking_a_running_game_withdraws_presence_but_keeps_the_game::<$harness>()
        }
        #[test]
        fn revocation_fences_presence_of_a_late_exec() {
            lifecycle::revocation_fences_presence_of_a_late_exec::<$harness>()
        }
        #[test]
        fn a_supervisor_crash_keeps_the_game_reserved_until_it_exits() {
            lifecycle::a_supervisor_crash_keeps_the_game_reserved_until_it_exits::<$harness>()
        }
        #[test]
        fn corrupt_metadata_fails_closed() {
            lifecycle::corrupt_metadata_fails_closed::<$harness>()
        }
        #[test]
        fn a_failed_exec_reaps_its_helper_and_never_claims_a_running_game() {
            lifecycle::a_failed_exec_reaps_its_helper_and_never_claims_a_running_game::<$harness>()
        }
        #[test]
        fn an_unconfirmed_exec_only_captures_the_exit_save() {
            lifecycle::an_unconfirmed_exec_only_captures_the_exit_save::<$harness>()
        }
        #[test]
        fn a_game_reaped_before_its_confirmation_still_finishes() {
            lifecycle::a_game_reaped_before_its_confirmation_still_finishes::<$harness>()
        }
        #[test]
        fn a_journal_failure_before_spawn_never_runs_the_target() {
            lifecycle::a_journal_failure_before_spawn_never_runs_the_target::<$harness>()
        }
        #[test]
        fn a_journal_failure_after_exit_still_finalizes_cloud() {
            lifecycle::a_journal_failure_after_exit_still_finalizes_cloud::<$harness>()
        }
        #[test]
        fn cloud_answers_bind_to_the_session_account_and_decision() {
            lifecycle::cloud_answers_bind_to_the_session_account_and_decision::<$harness>()
        }
        #[test]
        fn an_observer_waits_for_a_new_revision_while_commands_still_arrive() {
            lifecycle::an_observer_waits_for_a_new_revision_while_commands_still_arrive::<$harness>(
            )
        }
        #[test]
        fn records_left_by_the_previous_build_never_block_a_launch_or_its_recovery() {
            lifecycle::records_left_by_the_previous_build_never_block_a_launch_or_its_recovery::<
                $harness,
            >()
        }
        #[test]
        fn a_control_failure_is_paced_and_keeps_the_game() {
            lifecycle::a_control_failure_is_paced_and_keeps_the_game::<$harness>()
        }
        #[test]
        fn unreadable_metadata_does_not_regress_an_observed_live_session() {
            lifecycle::unreadable_metadata_does_not_regress_an_observed_live_session::<$harness>()
        }
        #[test]
        fn a_removed_socket_does_not_prevent_teardown() {
            lifecycle::a_removed_socket_does_not_prevent_teardown::<$harness>()
        }
        #[test]
        fn finalization_keeps_the_reservation_until_cloud_settles() {
            lifecycle::finalization_keeps_the_reservation_until_cloud_settles::<$harness>()
        }
        #[cfg(target_arch = "aarch64")]
        #[test]
        fn rosetta_resumes_only_its_decision_after_installation() {
            lifecycle::rosetta_resumes_only_its_decision_after_installation::<$harness>()
        }
        #[cfg(target_arch = "aarch64")]
        #[test]
        fn cancelling_the_rosetta_wait_never_starts_the_game() {
            lifecycle::cancelling_the_rosetta_wait_never_starts_the_game::<$harness>()
        }
        #[cfg(target_arch = "aarch64")]
        #[test]
        fn installed_rosetta_needs_no_requirement() {
            lifecycle::installed_rosetta_needs_no_requirement::<$harness>()
        }
    };
}
pub(crate) use lifecycle_suite;

pub(crate) fn a_launch_prepares_in_order_then_runs_its_game_and_settles_cloud<H: Harness>() {
    let run = launch::<H>(script(QUICK));
    let finished = run.until(Record::finished);
    assert_eq!(finished.phase, Phase::Finished);
    assert_eq!(finished.exit_code, Some(5));
    assert!(finished.failure_reason().is_none());
    assert!(run.ran());
    assert_eq!(
        run.events(),
        [
            "cloud.prepare",
            "target",
            "cloud.started",
            "presence:true",
            "presence:false",
            "cloud.finished:true",
        ]
    );
    assert_eq!(run.exit_code(), "0");
}

pub(crate) fn running_is_published_only_after_exec_is_confirmed<H: Harness>() {
    let run = launch::<H>(Script {
        helper: Helper::Hold,
        ..script(HELD)
    });
    run.wait_for("exec-ready");
    let admitted = run.client.snapshot(None).unwrap();
    assert_eq!(admitted.phase, Phase::Preparing);
    assert!(
        admitted.starting.is_some(),
        "the helper is owned before exec"
    );
    assert!(run.presence().is_empty());
    run.release("release-exec");
    let running = run.until(|r| r.phase == Phase::Running);
    assert!(running.game.is_some() && running.starting.is_none());
    run.release("release-game");
    assert_eq!(run.until(Record::finished).exit_code, Some(3));
}

pub(crate) fn a_detached_session_keeps_its_game_reserved_and_recovers<H: Harness>() {
    let run = launch::<H>(script(HELD));
    let running = run.until(|r| r.phase == Phase::Running);
    drop(run.client);
    let recovered = Client::<H::P>::recover_with(&run.sessions, Path::new("unused"), probe)
        .unwrap()
        .expect("the running session is recoverable");
    assert_eq!(recovered.id(), running.id);
    assert!(matches!(
        Client::<H::P>::idle_guard_with(&run.sessions, Path::new("unused"), probe),
        Err(Error::Busy)
    ));
    assert!(matches!(
        start::<H>(&run.directory, &run.sessions),
        Err(Error::Busy)
    ));
    fs::write(run.directory.join("release-game"), b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let finished = loop {
        let record = recovered.snapshot(None).unwrap();
        if record.finished() {
            break record;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(finished.exit_code, Some(3));
}

pub(crate) fn a_lost_acknowledgement_never_abandons_an_accepted_launch<H: Harness>() {
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    let (_root, directory, sessions) = prepare(&script(QUICK));
    let id = uuid::Uuid::new_v4().to_string();
    let (mut pipe, child_pipe) = UnixStream::pair().unwrap();
    let mut child = fixture_command::<H>(&directory)
        .stdin(Stdio::from(OwnedFd::from(child_pipe.try_clone().unwrap())))
        .stdout(Stdio::null())
        .stderr(Stdio::from(OwnedFd::from(child_pipe)))
        .spawn()
        .unwrap();
    H::handoff(&directory, &sessions, &id, &mut pipe).unwrap();
    // The UI goes away before reading the acknowledgement.
    drop(pipe);
    if H::begins() {
        // The next UI recovers the accepted launch and begins it.
        let deadline = Instant::now() + Duration::from_secs(10);
        let recovered = loop {
            if let Ok(Some(client)) =
                Client::<H::P>::recover_with(&sessions, Path::new("unused"), probe)
            {
                break client;
            }
            assert!(Instant::now() < deadline, "the accepted launch vanished");
            std::thread::sleep(Duration::from_millis(20));
        };
        recovered.begin().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while journal::by_id::<Record<H::P>>(&sessions, &id)
        .ok()
        .flatten()
        .is_none_or(|record| !record.finished())
    {
        assert!(
            Instant::now() < deadline,
            "the accepted launch was abandoned"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait().unwrap();
    assert!(directory.join("ran").exists());
    let record = journal::by_id::<Record<H::P>>(&sessions, &id)
        .unwrap()
        .unwrap();
    assert_eq!(record.phase, Phase::Finished);
}

pub(crate) fn cancelling_before_spawn_runs_nothing_and_ends_silently<H: Harness>() {
    let run = launch::<H>(Script {
        hold_cloud: true,
        ..script(QUICK)
    });
    run.wait_for("cloud-ready");
    run.client.cancel();
    let cancelled = run.until(Record::finished);
    assert_eq!(cancelled.phase, Phase::Failed);
    assert_eq!(cancelled.failure_reason(), Some(FailureReason::Cancelled));
    assert!(!run.ran());
    let events = run.events();
    assert_eq!(events.last().map(String::as_str), Some("cloud.aborted"));
    assert!(!events.contains(&"cloud.started".to_string()));
    assert_eq!(
        run.exit_code(),
        "0",
        "a cancellation is not a supervisor failure"
    );
}

pub(crate) fn cancelling_after_spawn_leaves_the_game_running<H: Harness>() {
    let run = launch::<H>(script(HELD));
    run.until(|r| r.phase == Phase::Running);
    run.client.cancel();
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(run.client.snapshot(None).unwrap().phase, Phase::Running);
    run.release("release-game");
    let finished = run.until(Record::finished);
    assert_eq!(finished.phase, Phase::Finished);
    assert_eq!(finished.exit_code, Some(3));
}

/// macOS asks every process SailOver started to quit when SailOver ends.
pub(crate) fn a_supervisor_asked_to_quit_stops_its_game_and_still_finishes_the_session<
    H: Harness,
>() {
    let run = launch::<H>(script(HELD));
    let running = run.until(|r| r.phase == Phase::Running);
    assert!(
        Process::new("/bin/kill")
            .args(["-TERM", &running.supervisor.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let finished = run.until(Record::finished);
    assert_eq!(
        finished.phase,
        Phase::Finished,
        "the supervisor settled the session"
    );
    // Only a supervisor that saw its game go records its exit.
    assert!(finished.exit_code.is_some());
    assert!(finished.game.is_none());
}

pub(crate) fn stopping_a_running_game_ends_it_and_still_finishes_the_session<H: Harness>() {
    let run = launch::<H>(script(HELD));
    run.until(|r| r.phase == Phase::Running);
    run.client.stop().unwrap();
    let finished = run.until(Record::finished);
    assert_eq!(
        finished.phase,
        Phase::Finished,
        "a stopped game is no failure"
    );
    assert!(finished.game.is_none());
}

pub(crate) fn revoking_before_admission_abandons_the_launch_without_presence<H: Harness>() {
    let run = launch::<H>(Script {
        hold_cloud: true,
        ..script(QUICK)
    });
    run.wait_for("cloud-ready");
    assert!(
        run.client.revoke(&H::other_account()).is_err(),
        "another account is refused"
    );
    run.client.revoke(&H::account()).unwrap();
    let revoked = run.until(Record::finished);
    assert_eq!(revoked.failure_reason(), Some(FailureReason::Cancelled));
    assert!(!run.ran());
    assert!(run.presence().is_empty());
}

pub(crate) fn revoking_a_running_game_withdraws_presence_but_keeps_the_game<H: Harness>() {
    let run = launch::<H>(script(HELD));
    run.until(|r| r.phase == Phase::Running);
    run.client.revoke(&H::account()).unwrap();
    // Presence is withdrawn before the reply.
    assert_eq!(run.presence(), ["presence:true", "presence:false"]);
    assert_eq!(run.client.snapshot(None).unwrap().phase, Phase::Running);
    run.release("release-game");
    let finished = run.until(Record::finished);
    assert_eq!(finished.phase, Phase::Finished);
    assert_eq!(finished.exit_code, Some(3));
}

pub(crate) fn revocation_fences_presence_of_a_late_exec<H: Harness>() {
    let run = launch::<H>(Script {
        helper: Helper::Hold,
        ..script(HELD)
    });
    run.wait_for("exec-ready");
    run.client.revoke(&H::account()).unwrap();
    run.release("release-exec");
    run.until(|r| r.phase == Phase::Running);
    run.release("release-game");
    run.until(Record::finished);
    assert!(run.presence().is_empty());
}

pub(crate) fn a_supervisor_crash_keeps_the_game_reserved_until_it_exits<H: Harness>() {
    let run = launch::<H>(script(HELD));
    let running = run.until(|r| r.phase == Phase::Running);
    assert!(
        Process::new("/bin/kill")
            .args(["-KILL", &running.supervisor.pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while identity(running.supervisor.pid).is_some() {
        assert!(Instant::now() < deadline, "the supervisor survived SIGKILL");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Each storefront presents the orphan its own way; none releases it.
    let orphan = run.client.snapshot(None).unwrap();
    assert!(!orphan.finished(), "a live game keeps its Reservation");
    assert_eq!(orphan.game, running.game);
    assert!(matches!(
        Client::<H::P>::idle_guard_with(&run.sessions, Path::new("unused"), probe),
        Err(Error::Busy)
    ));
    run.release("release-game");
    let finished = run.until(Record::finished);
    assert!(finished.game.is_none());
}

pub(crate) fn corrupt_metadata_fails_closed<H: Harness>() {
    let (_root, directory, sessions) = prepare(&script(QUICK));
    crate::files::private_directory(&sessions).unwrap();
    fs::write(sessions.join("session.v2.json"), "invalid").unwrap();
    fs::set_permissions(
        sessions.join("session.v2.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(Client::<H::P>::recover_with(&sessions, Path::new("unused"), probe).is_err());
    assert!(start::<H>(&directory, &sessions).is_err());
    assert!(!directory.join("ran").exists());
}

pub(crate) fn a_failed_exec_reaps_its_helper_and_never_claims_a_running_game<H: Harness>() {
    let run = launch::<H>(Script {
        missing: true,
        ..script(QUICK)
    });
    let failed = run.until(Record::finished);
    assert_eq!(failed.phase, Phase::Failed);
    assert_eq!(failed.failure_reason(), Some(FailureReason::Failed));
    assert!(failed.starting.is_none() && failed.game.is_none());
    assert!(run.presence().is_empty());
    assert_eq!(
        run.events().last().map(String::as_str),
        Some("cloud.aborted")
    );
    assert_eq!(run.exit_code(), "1", "a failed launch exits non-zero");
}

pub(crate) fn an_unconfirmed_exec_only_captures_the_exit_save<H: Harness>() {
    let run = launch::<H>(Script {
        helper: Helper::Unconfirmed,
        ..script(QUICK)
    });
    let failed = run.until(Record::finished);
    assert_eq!(failed.phase, Phase::Failed);
    assert_eq!(failed.exit_code, Some(7));
    assert!(run.events().contains(&"cloud.finished:false".to_string()));
    assert!(run.presence().is_empty());
    assert_eq!(
        run.exit_code(),
        "1",
        "an unconfirmed exec is a failed launch"
    );
}

pub(crate) fn a_game_reaped_before_its_confirmation_still_finishes<H: Harness>() {
    let run = launch::<H>(Script {
        helper: Helper::Late,
        ..script(QUICK)
    });
    let finished = run.until(Record::finished);
    assert_eq!(finished.phase, Phase::Finished);
    assert_eq!(finished.exit_code, Some(5));
    assert!(run.events().contains(&"cloud.finished:true".to_string()));
    assert_eq!(run.exit_code(), "0");
}

pub(crate) fn a_journal_failure_before_spawn_never_runs_the_target<H: Harness>() {
    let run = launch::<H>(Script {
        ask: true,
        ..script(QUICK)
    });
    let asked = run.until(|r| question(r).is_some());
    fs::set_permissions(&run.sessions, fs::Permissions::from_mode(0o500)).unwrap();
    run.client
        .choose(&H::account(), question(&asked).unwrap(), choice(false))
        .unwrap();
    run.wait_for("exit-code");
    fs::set_permissions(&run.sessions, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!run.ran());
    assert!(!run.events().contains(&"cloud.started".to_string()));
}

pub(crate) fn a_journal_failure_after_exit_still_finalizes_cloud<H: Harness>() {
    let run = launch::<H>(script(HELD));
    run.until(|r| r.phase == Phase::Running);
    fs::set_permissions(&run.sessions, fs::Permissions::from_mode(0o500)).unwrap();
    run.release("release-game");
    run.wait_for("exit-code");
    fs::set_permissions(&run.sessions, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(run.events().contains(&"cloud.finished:true".to_string()));
    assert_eq!(run.exit_code(), "0", "a success never turns into a failure");
}

pub(crate) fn cloud_answers_bind_to_the_session_account_and_decision<H: Harness>() {
    let run = launch::<H>(Script {
        ask: true,
        ..script(QUICK)
    });
    let decision = question(&run.until(|r| question(r).is_some())).unwrap();
    let cloud = choice(true);
    assert!(
        run.client
            .choose(&H::other_account(), decision, cloud)
            .is_err()
    );
    assert!(
        run.client
            .choose(&H::account(), decision + 1, cloud)
            .is_err()
    );
    run.client.choose(&H::account(), decision, cloud).unwrap();
    run.until(Record::finished);
    assert!(run.events().contains(&format!("answered:{cloud:?}")));
    assert!(
        run.client.choose(&H::account(), decision, cloud).is_err(),
        "an answer to a terminal session is refused"
    );
}

pub(crate) fn an_observer_waits_for_a_new_revision_while_commands_still_arrive<H: Harness>() {
    let run = launch::<H>(Script {
        hold_cloud: true,
        ..script(QUICK)
    });
    run.wait_for("cloud-ready");
    let current = run.client.snapshot(None).unwrap();
    let observed = std::thread::scope(|scope| {
        let observer = scope.spawn(|| run.client.snapshot(Some(current.revision)).unwrap());
        std::thread::sleep(Duration::from_millis(200));
        assert!(!observer.is_finished(), "Observe waits for a change");
        run.client.cancel();
        observer.join().unwrap()
    });
    assert!(observed.revision > current.revision);
    run.until(Record::finished);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn rosetta_resumes_only_its_decision_after_installation<H: Harness>() {
    let run = launch::<H>(Script {
        intel: true,
        ..script(QUICK)
    });
    let requested = run.until(|r| r.rosetta.is_some()).rosetta.unwrap();
    let account = H::account();
    assert!(
        run.client
            .resume_after_rosetta(&account, requested.decision_id + 1)
            .is_err()
    );
    assert!(
        run.client
            .resume_after_rosetta(&account, requested.decision_id)
            .is_err(),
        "resuming before installation keeps the requirement"
    );
    let rosetta = crate::supervisor::rosetta::helper(&run.directory);
    fs::create_dir_all(rosetta.parent().unwrap()).unwrap();
    fs::write(&rosetta, b"installed").unwrap();
    run.client
        .resume_after_rosetta(&account, requested.decision_id)
        .unwrap();
    assert_eq!(run.until(Record::finished).phase, Phase::Finished);
    // After an installation, the target is prepared again.
    assert_eq!(run.events().iter().filter(|e| *e == "target").count(), 2);
    assert!(run.ran());
}

/// The player accepted that a game the previous build launched, and that
/// still runs across the upgrade, is not protected: its records never block.
pub(crate) fn records_left_by_the_previous_build_never_block_a_launch_or_its_recovery<
    H: Harness,
>() {
    let (root, directory, sessions) = prepare(&script(QUICK));
    crate::files::private_directory(&sessions).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let live = identity(std::process::id()).unwrap();
    let bytes = serde_json::to_vec(&H::previous_build_record(
        &id,
        &live,
        &sessions.join("old.sock"),
    ))
    .unwrap();
    let old: Vec<_> = ["session.json".to_string(), format!("{id}.json")]
        .into_iter()
        .map(|name| sessions.join(name))
        .collect();
    for path in &old {
        fs::write(path, &bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    assert!(
        Client::<H::P>::recover_with(&sessions, Path::new("unused"), probe)
            .unwrap()
            .is_none()
    );
    drop(Client::<H::P>::idle_guard_with(&sessions, Path::new("unused"), probe).unwrap());
    crate::tests::wait_for_lock_release(&sessions.join("start.lock"));
    let run = Run::<H> {
        client: start::<H>(&directory, &sessions).unwrap(),
        _root: root,
        directory,
        sessions,
    };
    if H::begins() {
        run.client.begin().unwrap();
    }
    assert_eq!(run.until(Record::finished).phase, Phase::Finished);
    assert!(run.ran());
    for path in old {
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "{path:?} is left untouched"
        );
    }
}

pub(crate) fn a_control_failure_is_paced_and_keeps_the_game<H: Harness>() {
    let run = launch::<H>(script(HELD));
    let running = run.until(|r| r.phase == Phase::Running);
    let mut unreachable = running.clone();
    unreachable.socket = unreachable.socket.with_file_name("missing");
    journal::write(&run.sessions, &unreachable).unwrap();
    let started = Instant::now();
    let state = run.client.snapshot(Some(running.revision)).unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "retries are paced"
    );
    assert!(state.reconnecting);
    assert_eq!(state.phase, Phase::Running);
    assert_eq!(state.game, running.game);
    journal::write(&run.sessions, &running).unwrap();
    assert!(!run.client.snapshot(None).unwrap().reconnecting);
    run.release("release-game");
    assert_eq!(run.until(Record::finished).exit_code, Some(3));
}

pub(crate) fn unreadable_metadata_does_not_regress_an_observed_live_session<H: Harness>() {
    let run = launch::<H>(script(HELD));
    let running = run.until(|r| r.phase == Phase::Running);
    fs::write(
        run.sessions.join(format!("{}.v2.json", running.id)),
        b"truncated",
    )
    .unwrap();
    let state = run.client.snapshot(None).unwrap();
    assert_eq!(state.phase, Phase::Running);
    assert_eq!(state.game, running.game);
    run.release("release-game");
    run.until(Record::finished);
}

pub(crate) fn a_removed_socket_does_not_prevent_teardown<H: Harness>() {
    let run = launch::<H>(script(HELD));
    let running = run.until(|r| r.phase == Phase::Running);
    fs::remove_file(&running.socket).unwrap();
    run.release("release-game");
    run.wait_for("exit-code");
    let deadline = Instant::now() + Duration::from_secs(5);
    while identity(running.supervisor.pid).is_some() {
        assert!(Instant::now() < deadline, "the supervisor never exited");
        std::thread::sleep(Duration::from_millis(10));
    }
    crate::tests::wait_for_lock_release(&run.sessions.join("owner.lock"));
    assert!(storage::lock(&run.sessions, "owner.lock").is_ok());
}

pub(crate) fn finalization_keeps_the_reservation_until_cloud_settles<H: Harness>() {
    let run = launch::<H>(Script {
        hold_finish: true,
        ..script(QUICK)
    });
    run.wait_for("finish-ready");
    let Run {
        _root,
        directory,
        sessions,
        client,
    } = run;
    drop(client);
    let recovered = Client::<H::P>::recover_with(&sessions, Path::new("unused"), probe)
        .unwrap()
        .expect("a finalizing session is recoverable");
    assert_eq!(recovered.snapshot(None).unwrap().phase, Phase::Finalizing);
    assert!(matches!(
        start::<H>(&directory, &sessions),
        Err(Error::Busy)
    ));
    assert!(matches!(
        Client::<H::P>::idle_guard_with(&sessions, Path::new("unused"), probe),
        Err(Error::Busy)
    ));
    fs::write(directory.join("release-finish"), b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !directory.join("exit-code").exists() {
        assert!(Instant::now() < deadline, "finalization never ended");
        std::thread::sleep(Duration::from_millis(10));
    }
    let finished = journal::current::<Record<H::P>>(&sessions)
        .unwrap()
        .unwrap();
    assert_eq!(finished.phase, Phase::Finished);
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn cancelling_the_rosetta_wait_never_starts_the_game<H: Harness>() {
    for revoke in [false, true] {
        let run = launch::<H>(Script {
            intel: true,
            ..script(QUICK)
        });
        let requested = run.until(|r| r.rosetta.is_some()).rosetta.unwrap();
        if revoke {
            run.client.revoke(&H::account()).unwrap();
        } else {
            run.client.cancel();
        }
        let ended = run.until(Record::finished);
        assert_eq!(ended.failure_reason(), Some(FailureReason::Cancelled));
        assert!(ended.rosetta.is_none());
        assert!(
            run.client
                .resume_after_rosetta(&H::account(), requested.decision_id)
                .is_err()
        );
        assert!(!run.ran());
    }
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn installed_rosetta_needs_no_requirement<H: Harness>() {
    let (root, directory, sessions) = prepare(&Script {
        intel: true,
        ..script(QUICK)
    });
    let rosetta = crate::supervisor::rosetta::helper(&directory);
    fs::create_dir_all(rosetta.parent().unwrap()).unwrap();
    fs::write(&rosetta, b"installed").unwrap();
    let run = Run::<H> {
        client: start::<H>(&directory, &sessions).unwrap(),
        _root: root,
        directory,
        sessions,
    };
    if H::begins() {
        run.client.begin().unwrap();
    }
    let finished = run.until(Record::finished);
    assert_eq!(finished.phase, Phase::Finished);
    assert_eq!(finished.rosetta, None);
    assert_eq!(run.events().iter().filter(|e| *e == "target").count(), 1);
}

pub(crate) fn begin_opens_the_start_gate_once_and_only_for_its_account<H: Harness>() {
    let run = launch_without_begin::<H>(script(QUICK));
    std::thread::sleep(Duration::from_millis(200));
    assert!(run.events().is_empty(), "nothing runs before Begin");
    let session = run.client.snapshot(None).unwrap();
    let wrong = Command::<H::P>::Begin {
        id: session.id.clone(),
        account_id: H::other_account(),
    };
    assert!(run.client.command(None, wrong, false).is_err());
    run.client.begin().unwrap();
    run.client.begin().unwrap();
    assert_eq!(run.until(Record::finished).phase, Phase::Finished);
}

#[test]
fn a_cloud_question_is_abandoned_after_its_deadline_or_its_observer() {
    let now = Instant::now();
    let long_ago = |seconds| now.checked_sub(Duration::from_secs(seconds)).unwrap();

    let in_app = Questions::in_app();
    assert!(!in_app.abandoned(long_ago(599)));
    assert!(in_app.abandoned(long_ago(601)));

    let supervised = Questions::supervised();
    assert!(!supervised.abandoned(now));
    *supervised.observed.as_ref().unwrap().lock().unwrap() = long_ago(121);
    assert!(
        supervised.abandoned(now),
        "nobody has observed for two minutes"
    );
    supervised.observe();
    assert!(!supervised.abandoned(now));

    // Cloud questions and the Rosetta requirement share one counter.
    assert_eq!((supervised.next(), supervised.next()), (1, 2));
}
