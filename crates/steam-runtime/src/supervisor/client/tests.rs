//! The client half of every storefront, through the client's interface. A
//! synthetic kernel table keeps these checks independent of PID reuse, `ps`
//! formatting and any real game, helper or account session.
use super::*;
use crate::{epic::Epic, steam::Steam};

trait Sample: Provider {
    fn account() -> Self::Account;
    fn other_account() -> Self::Account;
    fn store() -> Self::Store;
    fn choice() -> Self::Choice;
}
impl Sample for Steam {
    fn account() -> u64 {
        123
    }
    fn other_account() -> u64 {
        456
    }
    fn store() -> crate::steam::Store {
        crate::steam::Store {
            app_id: 42,
            host: crate::HostState::Online,
        }
    }
    fn choice() -> crate::cloud::Choice {
        crate::cloud::Choice::Local
    }
}
impl Sample for Epic {
    fn account() -> String {
        "a".repeat(32)
    }
    fn other_account() -> String {
        "b".repeat(32)
    }
    fn store() -> crate::epic::Store {
        crate::epic::Store {
            namespace: "namespace".into(),
            app_name: "app".into(),
        }
    }
    fn choice() -> crate::epic::Choice {
        crate::epic::Choice::Local
    }
}

/// Instantiates each scenario once per storefront.
macro_rules! every_storefront {
    ($($scenario:ident),* $(,)?) => {
        mod steam {
            $(#[test]
            fn $scenario() {
                super::$scenario::<crate::steam::Steam>()
            })*
        }
        mod epic {
            $(#[test]
            fn $scenario() {
                super::$scenario::<crate::epic::Epic>()
            })*
        }
    };
}
every_storefront!(
    a_live_supervisor_or_game_reserves_the_session,
    a_starting_helper_or_its_replacement_outlives_its_supervisor,
    a_reused_pid_releases_the_old_game_but_an_exec_replacement_does_not,
    a_failed_inspection_keeps_the_reservation,
    a_running_record_without_a_process_refuses_maintenance,
    the_maintenance_guard_excludes_maintenance_and_launch_until_dropped,
    commands_check_the_account_and_the_supervisor_before_sending,
    a_terminal_session_refuses_answers_but_not_cancel_or_revoke,
    a_game_guard_session_is_alive_only_while_it_owns_a_process,
);

const SUPERVISOR: u32 = 1001;
const GAME: u32 = 1002;

fn process(pid: u32) -> Identity {
    Identity {
        pid,
        uid: 501,
        birth_seconds: 123,
        birth_microseconds: u64::from(pid),
        executable: format!("/disposable/process-{pid}"),
    }
}

/// The helper path names a file holding the live process table.
fn inspect_table(helper: &Path, pid: u32) -> Result<Option<Identity>, Error> {
    let table: Vec<Identity> =
        serde_json::from_slice(&fs::read(helper)?).map_err(|_| Error::InvalidSession)?;
    Ok(table.into_iter().find(|identity| identity.pid == pid))
}

fn must_not_probe(_: &Path, _: u32) -> Result<Option<Identity>, Error> {
    panic!("a mismatched account or session fails before process inspection");
}

struct Fixture<P: Provider> {
    _root: tempfile::TempDir,
    sessions: PathBuf,
    helper: PathBuf,
    record: Record<P>,
}
impl<P: Sample> Fixture<P> {
    fn new(phase: Phase, game: Option<Identity>, live: &[Identity]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        crate::files::private_directory(&sessions).unwrap();
        let record = Record::<P> {
            version: journal::VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            account_id: P::account(),
            store: P::store(),
            phase,
            cloud: P::Cloud::default(),
            exit_code: None,
            error: None,
            failure_reason: None,
            revision: 1,
            rosetta: None,
            reconnecting: false,
            supervisor: process(SUPERVISOR),
            game,
            starting: None,
            socket: root.path().join("unused-socket"),
        };
        journal::write(&sessions, &record).unwrap();
        let helper = root.path().join("process-table.json");
        let fixture = Self {
            _root: root,
            sessions,
            helper,
            record,
        };
        fixture.live(live);
        fixture
    }

    fn starting(live: &[Identity]) -> Self {
        let mut fixture = Self::new(Phase::Preparing, None, live);
        fixture.record.starting = Some(starting_process());
        journal::write(&fixture.sessions, &fixture.record).unwrap();
        fixture
    }

    fn live(&self, live: &[Identity]) {
        fs::write(&self.helper, serde_json::to_vec(live).unwrap()).unwrap();
    }

    fn guard(&self) -> Result<IdleGuard<P>, Error> {
        Client::<P>::idle_guard_with(&self.sessions, &self.helper, inspect_table)
    }

    fn recover(&self) -> Result<Option<Client<P>>, Error> {
        Client::<P>::recover_with(&self.sessions, &self.helper, inspect_table)
    }

    fn client(&self, probe: Probe) -> Client<P> {
        Client::new(&self.sessions, &self.helper, self.record.id.clone(), probe)
    }

    /// A launch that must be refused before any supervisor is spawned.
    fn start(&self) -> Result<Client<P>, Error> {
        Client::<P>::start_with(
            &self.sessions,
            &self.helper,
            uuid::Uuid::new_v4().to_string(),
            Process::new("/forge-no-supervisor-fixture"),
            inspect_table,
            |_| panic!("a refused launch never hands anything over"),
            |_| false,
            || false,
        )
    }

    /// A concurrently spawned test process can briefly hold an inherited
    /// lock descriptor; wait for it rather than racing it.
    fn settled(&self) -> &Self {
        crate::tests::wait_for_lock_release(&self.sessions.join("start.lock"));
        crate::tests::wait_for_lock_release(&self.sessions.join("owner.lock"));
        self
    }
}

fn starting_process() -> StartingGame {
    StartingGame::Process {
        identity: process(GAME),
        executable: "/disposable/intended-game".into(),
    }
}

fn replaced(mut identity: Identity) -> Identity {
    identity.executable = "/disposable/another-binary".into();
    identity
}

fn a_live_supervisor_or_game_reserves_the_session<P: Sample>() {
    for (phase, game, live) in [
        (Phase::Finalizing, None, vec![process(SUPERVISOR)]),
        (Phase::Running, Some(process(GAME)), vec![process(GAME)]),
    ] {
        let fixture = Fixture::<P>::new(phase, game, &live);
        assert!(matches!(fixture.guard(), Err(Error::Busy)));
        // A refused guard releases both locks it took.
        fixture.settled();
    }
}

fn a_starting_helper_or_its_replacement_outlives_its_supervisor<P: Sample>() {
    for live in [process(GAME), replaced(process(GAME))] {
        let fixture = Fixture::<P>::starting(&[live]);
        assert!(matches!(fixture.guard(), Err(Error::Busy)));
        let client = fixture
            .settled()
            .recover()
            .unwrap()
            .expect("an accepted helper stays recoverable without its supervisor");
        let orphan = client.snapshot(None).unwrap();
        assert!(!orphan.finished());
        assert_eq!(orphan.starting, Some(starting_process()));
        assert!(matches!(fixture.settled().start(), Err(Error::Busy)));

        // Only a fresh negative inspection releases the orphan, in memory:
        // nothing but a supervisor writes the journal.
        let before = fs::read(
            fixture
                .sessions
                .join(format!("{}.v2.json", fixture.record.id)),
        );
        fixture.live(&[]);
        let finished = client.snapshot(Some(orphan.revision)).unwrap();
        assert!(finished.finished());
        assert!(finished.starting.is_none());
        let after = fs::read(
            fixture
                .sessions
                .join(format!("{}.v2.json", fixture.record.id)),
        );
        assert_eq!(before.unwrap(), after.unwrap());
        drop(fixture.settled().guard().unwrap());
    }
}

fn a_reused_pid_releases_the_old_game_but_an_exec_replacement_does_not<P: Sample>() {
    for change in 0..3 {
        let mut live = vec![process(SUPERVISOR), process(GAME)];
        for identity in &mut live {
            match change {
                0 => identity.birth_seconds += 1,
                1 => identity.birth_microseconds += 1,
                _ => identity.uid += 1,
            }
        }
        let fixture = Fixture::<P>::new(Phase::Running, Some(process(GAME)), &live);
        assert!(fixture.recover().unwrap().is_none(), "change {change}");
        drop(fixture.settled().guard().unwrap());
    }
    let fixture = Fixture::<P>::new(
        Phase::Running,
        Some(process(GAME)),
        &[replaced(process(GAME))],
    );
    assert!(matches!(fixture.guard(), Err(Error::Busy)));
    assert!(fixture.settled().recover().unwrap().is_some());
}

fn a_failed_inspection_keeps_the_reservation<P: Sample>() {
    fn failed_child(helper: &Path, pid: u32) -> Result<Option<Identity>, Error> {
        if pid == GAME {
            Err(Error::Unavailable)
        } else {
            inspect_table(helper, pid)
        }
    }
    let fixture = Fixture::<P>::starting(&[]);
    assert!(matches!(
        Client::<P>::idle_guard_with(&fixture.sessions, &fixture.helper, failed_child),
        Err(Error::Unavailable)
    ));
    let client =
        Client::<P>::recover_with(&fixture.settled().sessions, &fixture.helper, failed_child)
            .unwrap()
            .unwrap();
    let snapshot = client.snapshot(None).unwrap();
    assert!(!snapshot.finished());
    assert!(snapshot.reconnecting);
    assert_eq!(snapshot.starting, Some(starting_process()));
}

fn a_running_record_without_a_process_refuses_maintenance<P: Sample>() {
    let fixture = Fixture::<P>::new(Phase::Running, None, &[]);
    assert!(matches!(fixture.guard(), Err(Error::Busy)));
    fs::write(&fixture.settled().helper, b"unavailable identity response").unwrap();
    assert!(matches!(fixture.guard(), Err(Error::InvalidSession)));
}

fn the_maintenance_guard_excludes_maintenance_and_launch_until_dropped<P: Sample>() {
    let fixture = Fixture::<P>::new(Phase::Running, Some(process(GAME)), &[]);
    let guard = fixture.guard().unwrap();
    assert!(matches!(fixture.guard(), Err(Error::Busy)));
    for name in ["start.lock", "owner.lock"] {
        assert!(matches!(
            storage::lock(&fixture.sessions, name),
            Err(Error::Busy)
        ));
    }
    assert!(matches!(fixture.start(), Err(Error::Busy)));
    drop(guard);
    drop(fixture.settled().guard().unwrap());
}

fn commands_check_the_account_and_the_supervisor_before_sending<P: Sample>() {
    let fixture = Fixture::<P>::new(Phase::Preparing, None, &[]);
    let client = fixture.client(must_not_probe);
    assert!(matches!(
        client.command(
            Some(&P::other_account()),
            Command::Revoke {
                id: fixture.record.id.clone(),
                account_id: P::other_account(),
            },
            false,
        ),
        Err(Error::InvalidSession)
    ));

    let mut replacement = fixture.record.clone();
    replacement.id = uuid::Uuid::new_v4().to_string();
    journal::write(&fixture.sessions, &replacement).unwrap();
    fs::copy(
        fixture.sessions.join(format!("{}.v2.json", replacement.id)),
        fixture
            .sessions
            .join(format!("{}.v2.json", fixture.record.id)),
    )
    .unwrap();
    assert!(matches!(
        client.command(
            Some(&P::account()),
            Command::Revoke {
                id: fixture.record.id.clone(),
                account_id: P::account(),
            },
            false,
        ),
        Err(Error::InvalidSession)
    ));

    // A recycled supervisor PID is never sent a command.
    let fixture = Fixture::<P>::new(Phase::Preparing, None, &[replaced_birth()]);
    let client = fixture.client(inspect_table);
    for answer in [false, true] {
        let command = if answer {
            Command::CloudChoice {
                id: fixture.record.id.clone(),
                account_id: P::account(),
                decision_id: 7,
                choice: P::choice(),
            }
        } else {
            Command::Revoke {
                id: fixture.record.id.clone(),
                account_id: P::account(),
            }
        };
        assert!(matches!(
            client.command(Some(&P::account()), command, answer),
            Err(Error::InvalidSession)
        ));
    }
}

fn replaced_birth() -> Identity {
    let mut supervisor = process(SUPERVISOR);
    supervisor.birth_seconds += 1;
    supervisor
}

fn a_terminal_session_refuses_answers_but_not_cancel_or_revoke<P: Sample>() {
    let fixture = Fixture::<P>::new(Phase::Finished, None, &[]);
    let client = fixture.client(must_not_probe);
    let answer = Command::ResumeAfterRosetta {
        id: fixture.record.id.clone(),
        account_id: P::account(),
        decision_id: 3,
    };
    assert!(matches!(
        client.command(Some(&P::account()), answer, true),
        Err(Error::InvalidSession)
    ));
    let revoke = Command::Revoke {
        id: fixture.record.id.clone(),
        account_id: P::account(),
    };
    assert!(
        client
            .command(Some(&P::account()), revoke, false)
            .unwrap()
            .finished()
    );
    client.cancel();
}

fn a_game_guard_session_is_alive_only_while_it_owns_a_process<P: Sample>() {
    let fixture = Fixture::<P>::new(Phase::Running, Some(process(GAME)), &[process(GAME)]);
    let guard = fixture.guard_after_supervisor_loss();
    assert!(guard.session_alive(&fixture.record.id).unwrap());
    fixture.live(&[]);
    assert!(!guard.session_alive(&fixture.record.id).unwrap());
    // A session that never produced a current record, such as one launched by
    // a previous build, is dead once no supervisor of this storefront runs.
    assert!(
        !guard
            .session_alive(&uuid::Uuid::new_v4().to_string())
            .unwrap()
    );
    drop(guard);

    let unknown = Fixture::<P>::new(Phase::Running, None, &[]);
    let owner = OwnerLock::<P>::take(&unknown.sessions).unwrap();
    assert!(
        owner
            .session_alive(&unknown.record.id, |_| Ok(None))
            .unwrap(),
        "a running record without a process cannot be proven dead"
    );
}

impl<P: Sample> Fixture<P> {
    /// The guard refuses a live game, so model a session whose supervisor and
    /// game were both inspected dead when the guard was taken, then check the
    /// question maintenance asks about the vault's recorded session.
    fn guard_after_supervisor_loss(&self) -> IdleGuard<P> {
        let live = fs::read(&self.helper).unwrap();
        self.live(&[]);
        let guard = self.guard().unwrap();
        fs::write(&self.helper, live).unwrap();
        guard
    }
}

#[test]
fn an_owner_wait_that_times_out_keeps_other_starters_out() {
    let root = tempfile::tempdir().unwrap();
    let owner = storage::lock(root.path(), "owner.lock").unwrap();
    let starting = storage::lock(root.path(), "start.lock").unwrap();
    assert!(matches!(
        storage::owner_released(root.path(), starting),
        Err(Error::Busy)
    ));
    assert!(matches!(
        storage::lock(root.path(), "start.lock"),
        Err(Error::Busy)
    ));
    drop(owner);
    let deadline = Instant::now() + Duration::from_secs(2);
    while storage::lock(root.path(), "start.lock").is_err() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}
