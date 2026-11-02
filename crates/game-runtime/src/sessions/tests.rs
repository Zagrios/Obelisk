//! Game sessions through their interface, against a supervisor the test
//! drives. Steam stands in; Epic differs only in names and keys.
use super::*;
use crate::{
    epic::Epic,
    runtimes::tests::{FakeRuntimes, fake_offer, ready, recorded},
    steam::{Steam, Store, host::HostState},
};
use std::{
    sync::Condvar,
    time::{Duration, Instant},
};

/// A supervisor the test drives, as a real one reports through its journal.
struct Fake<P: Provider> {
    state: Mutex<FakeState<P>>,
    changed: Condvar,
}

struct FakeState<P: Provider> {
    current: Observation<P>,
    /// A final phase with a process still owned: not finished yet.
    owns_process: bool,
    observation_failure: bool,
    begin_failure: bool,
    revoke_failure: bool,
    rosetta_unavailable: bool,
    begins: usize,
    cancellations: usize,
    stops: usize,
    /// The player's Quit and Force Quit, in order.
    quits: Vec<&'static str>,
    /// The player's controls, with the account each named.
    controls: Vec<(P::Account, P::Control)>,
    /// The statuses it was told, with the account each named.
    statuses: Vec<(P::Account, OnlineStatus)>,
    revocations: Vec<P::Account>,
    choices: Vec<(P::Account, u64, Choice)>,
    rosetta_resumes: Vec<(P::Account, u64)>,
}

fn choices(phase: CloudPhase) -> Vec<Choice> {
    match phase {
        CloudPhase::Conflict => vec![Choice::Local, Choice::Cloud, Choice::Cancel],
        CloudPhase::Failed => vec![Choice::Retry, Choice::Cancel],
        _ => Vec::new(),
    }
}

impl<P: Provider> Fake<P> {
    fn with(account: P::Account, store: P::Store, phase: Phase) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                current: Observation {
                    id: uuid::Uuid::new_v4().to_string(),
                    account,
                    store,
                    phase,
                    finished: false,
                    failure: None,
                    error: None,
                    revision: 1,
                    reconnecting: false,
                    rosetta: None,
                    cloud: CloudStatus::default(),
                    played: None,
                },
                owns_process: false,
                observation_failure: false,
                begin_failure: false,
                revoke_failure: false,
                rosetta_unavailable: false,
                begins: 0,
                cancellations: 0,
                stops: 0,
                quits: Vec::new(),
                controls: Vec::new(),
                statuses: Vec::new(),
                revocations: Vec::new(),
                choices: Vec::new(),
                rosetta_resumes: Vec::new(),
            }),
            changed: Condvar::new(),
        })
    }

    fn change(&self, change: impl FnOnce(&mut FakeState<P>)) {
        let mut state = self.state.lock().unwrap();
        change(&mut state);
        state.current.revision += 1;
        self.changed.notify_all();
    }

    fn read<T>(&self, read: impl FnOnce(&FakeState<P>) -> T) -> T {
        read(&self.state.lock().unwrap())
    }

    fn cloud(&self, phase: CloudPhase, decision_id: u64) {
        self.change(|state| {
            state.current.cloud = CloudStatus {
                phase,
                decision_id,
                error: None,
                allowed_choices: choices(phase),
            };
        });
    }

    fn phase(&self, phase: Phase) {
        self.change(|state| state.current.phase = phase);
    }

    /// The game exited; `owning_process` keeps a process the supervisor still
    /// owns.
    fn finish(&self, owning_process: bool) {
        self.change(|state| {
            state.current.phase = Phase::Finished;
            state.current.reconnecting = false;
            state.owns_process = owning_process;
            state.current.finished = !owning_process;
        });
    }

    fn release_process(&self) {
        self.change(|state| {
            state.owns_process = false;
            state.current.finished = true;
        });
    }

    fn fail(&self, error: Option<&str>, cancelled: bool) {
        self.change(|state| {
            state.current.phase = Phase::Failed;
            state.current.error = error.map(Into::into);
            state.current.failure = Some(if cancelled {
                FailureReason::Cancelled
            } else {
                FailureReason::Failed
            });
            state.current.finished = !state.owns_process;
        });
    }
}

impl<P: Provider> Supervised<P> for Fake<P> {
    fn snapshot(&self, after: Option<u64>) -> Result<Observation<P>, String> {
        let mut state = self.state.lock().unwrap();
        while after == Some(state.current.revision)
            && !state.current.finished
            && !state.observation_failure
        {
            state = self.changed.wait(state).unwrap();
        }
        if state.observation_failure {
            state.observation_failure = false;
            return Err("backend.steam.runtime.unavailable".into());
        }
        Ok(state.current.clone())
    }
    fn cancel(&self) {
        self.state.lock().unwrap().cancellations += 1;
    }
    fn stop(&self) -> Result<(), String> {
        self.state.lock().unwrap().stops += 1;
        Ok(())
    }
    fn quit(&self) -> Result<(), String> {
        self.state.lock().unwrap().quits.push("quit");
        Ok(())
    }
    fn force_quit(&self) -> Result<(), String> {
        self.state.lock().unwrap().quits.push("force");
        Ok(())
    }
    fn begin(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.begins += 1;
        if state.begin_failure {
            return Err("backend.epic.cloud.session".into());
        }
        Ok(())
    }
    fn choose(&self, account: &P::Account, decision_id: u64, choice: Choice) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.choices.push((account.clone(), decision_id, choice));
        Ok(())
    }
    fn resume_after_rosetta(&self, account: &P::Account, decision_id: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if state.rosetta_unavailable {
            return Err("backend.runtime.rosettaUnavailable".into());
        }
        state.rosetta_resumes.push((account.clone(), decision_id));
        Ok(())
    }
    fn revoke(&self, account: &P::Account) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if state.revoke_failure {
            return Err("backend.steam.runtime.unavailable".into());
        }
        state.revocations.push(account.clone());
        Ok(())
    }
    fn control(&self, account: &P::Account, control: P::Control) -> Result<(), String> {
        self.state
            .lock()
            .unwrap()
            .controls
            .push((account.clone(), control));
        Ok(())
    }
    fn set_status(&self, account: &P::Account, status: OnlineStatus) -> Result<(), String> {
        self.state
            .lock()
            .unwrap()
            .statuses
            .push((account.clone(), status));
        Ok(())
    }
}

fn steam(app_id: u32, phase: Phase) -> Arc<Fake<Steam>> {
    Fake::with(42, Store::new(app_id), phase)
}

/// Sessions with nothing left to recover.
fn sessions() -> Sessions<Steam> {
    recovering(|| Ok(None))
}

fn recovering(
    recover: impl Fn() -> Result<Option<Arc<dyn Supervised<Steam>>>, String> + Send + Sync + 'static,
) -> Sessions<Steam> {
    Sessions::new(Box::new(recover), ready())
}

async fn settled(sessions: &Sessions<Steam>) {
    eventually(|| !sessions.state().recovering).await;
}

/// Waits until `predicate` holds, or fails the test after three seconds.
async fn eventually(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn running(sessions: &Sessions<Steam>) -> bool {
    sessions
        .state()
        .active
        .is_some_and(|active| active.observed && active.phase == Phase::Running)
}

fn protects(sessions: &Sessions<Steam>, game: u32) -> bool {
    let state = sessions.state();
    state.recovering || state.active.is_some_and(|active| active.game == game)
}

/// Launches `game` behind `supervisor`, which accepted it at once.
fn follow(
    sessions: &Sessions<Steam>,
    game: u32,
    supervisor: Arc<Fake<Steam>>,
) -> tokio::task::JoinHandle<Result<(), Failure>> {
    let admission = sessions.admit(game, 42).expect("admitted");
    tokio::spawn(admission.follow(supervisor))
}

type Answer = tokio::sync::oneshot::Sender<Result<Arc<dyn Supervised<Steam>>, Handoff>>;

/// A handoff the test answers.
fn gated() -> (
    Answer,
    impl Future<Output = Result<Arc<dyn Supervised<Steam>>, Handoff>> + Send + 'static,
) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    (sender, async move {
        receiver
            .await
            .unwrap_or(Err(Handoff::Refused(Failure::Cancelled)))
    })
}

#[tokio::test]
async fn admission_reserves_at_once_and_a_refused_handoff_releases_without_recovery() {
    let recoveries = Arc::new(Mutex::new(0));
    let counted = recoveries.clone();
    let sessions = recovering(move || {
        *counted.lock().unwrap() += 1;
        Ok(None)
    });
    settled(&sessions).await;
    let admission = sessions.admit(42, 42).unwrap();
    assert!(protects(&sessions, 42));
    assert!(!protects(&sessions, 570));
    assert!(sessions.admit(570, 42).is_none());

    let outcome = admission
        .launch(async {
            Err(Handoff::Refused(Failure::Failed(
                "backend.voile.downloadFailed".into(),
            )))
        })
        .await;

    assert_eq!(
        outcome,
        Err(Failure::Failed("backend.voile.downloadFailed".into()))
    );
    assert!(!sessions.state().busy());
    assert_eq!(*recoveries.lock().unwrap(), 1, "only startup recovered");
}

#[tokio::test]
async fn an_admission_cancelled_or_dropped_before_its_handoff_releases_the_reservation() {
    let sessions = sessions();
    settled(&sessions).await;
    let admission = sessions.admit(42, 42).unwrap();
    admission.cancel();
    assert!(!sessions.state().busy());
    let handed = Arc::new(AtomicBool::new(false));
    let called = handed.clone();
    let outcome = admission
        .launch(async move {
            called.store(true, Ordering::Release);
            Err(Handoff::Refused(Failure::Cancelled))
        })
        .await;
    assert_eq!(outcome, Err(Failure::Cancelled));
    assert!(!handed.load(Ordering::Acquire));

    drop(sessions.admit(42, 42).unwrap());
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn a_lost_handoff_keeps_the_reservation_until_recovery_finds_the_game_gone() {
    let game = steam(42, Phase::Running);
    let attempts = Arc::new(Mutex::new(0));
    let (recovered, recovery) = (game.clone(), attempts.clone());
    let answer = Arc::new(Mutex::new(None::<bool>));
    let answered = answer.clone();
    let sessions = recovering(move || {
        let mut attempts = recovery.lock().unwrap();
        *attempts += 1;
        match (*attempts, *answered.lock().unwrap()) {
            (1, _) => Ok(None),
            (2, _) => Err("backend.steam.runtime.unavailable".into()),
            (_, None) => Err("not yet".into()),
            (_, Some(true)) => Ok(Some(recovered.clone() as Arc<dyn Supervised<Steam>>)),
            (_, Some(false)) => Ok(None),
        }
    });
    settled(&sessions).await;
    let admission = sessions.admit(42, 42).unwrap();
    let launch = tokio::spawn(
        admission.launch(async { Err(Handoff::Lost("backend.steam.runtime.unavailable".into())) }),
    );
    eventually(|| sessions.state().recovery_failed).await;
    let state = sessions.state();
    assert!(state.recovering);
    assert!(protects(&sessions, 42));
    assert!(protects(&sessions, 570));
    assert!(sessions.admit(570, 42).is_none());

    *answer.lock().unwrap() = Some(true);
    eventually(|| running(&sessions)).await;
    let state = sessions.state();
    assert!(!state.recovering);
    assert!(!state.recovery_failed);
    assert!(!protects(&sessions, 570));
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert!(!sessions.state().busy());

    // No game to recover: the original failure ends the launch.
    *answer.lock().unwrap() = Some(false);
    let admission = sessions.admit(42, 42).unwrap();
    let outcome = admission
        .launch(async { Err(Handoff::Lost("backend.steam.runtime.unavailable".into())) })
        .await;
    assert_eq!(
        outcome,
        Err(Failure::Failed("backend.steam.runtime.unavailable".into()))
    );
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn startup_recovery_reserves_the_storefront_until_the_recovered_game_exits() {
    let game = steam(42, Phase::Running);
    let attempts = Arc::new(Mutex::new(0));
    let (recovered, counted) = (game.clone(), attempts.clone());
    let sessions = recovering(move || {
        let mut attempts = counted.lock().unwrap();
        *attempts += 1;
        if *attempts == 1 {
            return Err("backend.steam.runtime.unavailable".into());
        }
        Ok(Some(recovered.clone() as Arc<dyn Supervised<Steam>>))
    });
    assert!(sessions.state().recovering);
    assert!(sessions.admit(570, 42).is_none());
    eventually(|| running(&sessions)).await;
    assert_eq!(*attempts.lock().unwrap(), 2);
    assert!(!sessions.state().recovery_failed);
    assert!(protects(&sessions, 42));
    assert!(!protects(&sessions, 570));

    game.fail(Some("backend.steam.runtime.startFailed"), false);
    eventually(|| !sessions.state().busy()).await;
    assert!(
        !sessions.state().recovery_failed,
        "a recovered game's end is no lasting message"
    );
}

#[tokio::test]
async fn a_launch_keeps_when_its_game_started_even_unseen_running_but_a_recovered_game_has_none() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Preparing);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.observed)
    })
    .await;
    assert_eq!(sessions.state().active.unwrap().started_at, None);

    // The game ran and exited between two snapshots.
    let before = crate::unix_now();
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    let started = sessions.state().last.unwrap().started_at.expect("started");
    assert!((before..=crate::unix_now()).contains(&started));

    let recovered = steam(570, Phase::Running);
    let found = recovered.clone();
    let sessions = recovering(move || Ok(Some(found.clone() as Arc<dyn Supervised<Steam>>)));
    eventually(|| running(&sessions)).await;
    recovered.finish(false);
    eventually(|| !sessions.state().busy()).await;
    assert_eq!(
        sessions.state().last.unwrap().started_at,
        None,
        "a game recovered running started before this Obelisk saw it"
    );
}

#[tokio::test]
async fn a_recovered_game_is_told_no_status_until_the_player_has_one() {
    let recovered = steam(570, Phase::Running);
    let found = recovered.clone();
    let sessions = recovering(move || Ok(Some(found.clone() as Arc<dyn Supervised<Steam>>)));
    // Each observation round, the one that saw it run included, would tell it.
    eventually(|| running(&sessions)).await;
    assert!(recovered.read(|state| state.statuses.is_empty()));

    sessions.set_status(OnlineStatus::Away);
    assert_eq!(
        recovered.read(|state| state.statuses.clone()),
        [(42, OnlineStatus::Away)]
    );
    recovered.finish(false);
    eventually(|| !sessions.state().busy()).await;
}

#[tokio::test]
async fn control_loss_is_uncertainty_and_cancelling_never_ends_a_running_game() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    let admission = sessions.admit(42, 42).unwrap();
    let canceller = admission.canceller();
    let launch = tokio::spawn(admission.follow(game.clone()));
    eventually(|| running(&sessions)).await;
    game.change(|state| state.observation_failure = true);
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.reconnecting)
    })
    .await;
    assert_eq!(sessions.state().active.unwrap().phase, Phase::Running);
    assert!(!sessions.state().recovery_failed);
    assert!(protects(&sessions, 42));

    canceller.cancel();
    eventually(|| game.read(|state| state.cancellations == 1)).await;
    assert!(!launch.is_finished());
    assert!(sessions.admit(570, 42).is_none());
    game.change(|state| state.current.store.host = HostState::Lost);
    eventually(|| {
        sessions.state().active.is_some_and(|active| {
            active
                .store
                .is_some_and(|store| store.host == HostState::Lost)
        })
    })
    .await;
    // Games often quit normally with a nonzero code, which is no failure.
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn cancelling_a_handed_off_launch_cancels_preparation_and_follows_the_game_until_it_exits() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    let admission = sessions.admit(42, 42).unwrap();
    let canceller = admission.canceller();
    let (answer, handoff) = gated();
    let launch = tokio::spawn(admission.launch(handoff));
    eventually(|| canceller.launched.load(Ordering::Acquire)).await;
    canceller.cancel();
    assert!(protects(&sessions, 42));
    answer.send(Ok(game.clone())).ok();
    eventually(|| running(&sessions)).await;
    eventually(|| game.read(|state| state.cancellations == 1)).await;
    assert!(protects(&sessions, 42));
    assert!(sessions.admit(570, 42).is_none());
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn the_reservation_holds_until_the_supervisor_owns_no_process() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| running(&sessions)).await;

    game.finish(true);
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.phase == Phase::Finished)
    })
    .await;
    assert!(
        protects(&sessions, 42),
        "a final phase alone does not end the session"
    );
    assert!(sessions.admit(570, 42).is_none());

    game.release_process();
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn a_launch_opens_the_start_gate_of_a_waiting_supervisor_unless_its_account_signed_out() {
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Preparing);
    let launch = tokio::spawn(
        sessions
            .admit(store.clone(), "account".into())
            .unwrap()
            .follow(game.clone()),
    );
    eventually(|| game.read(|state| state.begins == 1)).await;
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));

    let revoked = Fake::<Epic>::with("account".into(), store.clone(), Phase::Preparing);
    let admission = sessions.admit(store, "account".into()).unwrap();
    sessions.revoke("account".into());
    let launch = tokio::spawn(admission.follow(revoked.clone()));
    eventually(|| revoked.read(|state| state.cancellations == 1)).await;
    assert_eq!(
        revoked.read(|state| state.begins),
        0,
        "a signed-out launch never begins"
    );
    revoked.fail(None, true);
    assert_eq!(launch.await.unwrap(), Err(Failure::Cancelled));
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn a_lost_begin_reply_keeps_the_reservation_until_a_terminal_snapshot() {
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Preparing);
    game.change(|state| state.begin_failure = true);
    let launch = tokio::spawn(
        sessions
            .admit(store, "account".into())
            .unwrap()
            .follow(game.clone()),
    );
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.observed)
    })
    .await;
    assert!(sessions.state().busy());
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
}

#[tokio::test]
async fn epic_sessions_report_epic_failures() {
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Running);
    let launch = tokio::spawn(
        sessions
            .admit(store.clone(), "account".into())
            .unwrap()
            .follow(game.clone()),
    );
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.observed)
    })
    .await;
    assert_eq!(sessions.state().active.unwrap().game, store);
    game.fail(None, false);
    assert_eq!(
        launch.await.unwrap(),
        Err(Failure::Failed("backend.epic.cloud.failed".into()))
    );
}

#[tokio::test]
async fn signing_out_during_the_handoff_revokes_before_the_cloud_question_shows() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Preparing);
    game.cloud(CloudPhase::Conflict, 17);
    let (answer, handoff) = gated();
    let launch = tokio::spawn(sessions.admit(42, 42).unwrap().launch(handoff));

    sessions.revoke(42);
    assert!(protects(&sessions, 42));
    answer.send(Ok(game.clone())).ok();
    eventually(|| game.read(|state| state.revocations == [42])).await;
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.cloud.phase == CloudPhase::Conflict)
    })
    .await;
    let state = sessions.state();
    assert_eq!(state.revoked, [42]);
    let active = state.active.unwrap();
    assert!(
        sessions.choose(&active.id, 17, Choice::Local).is_err(),
        "a revoked account never answers"
    );

    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert_eq!(game.read(|state| state.revocations.clone()), [42]);
}

#[tokio::test]
async fn a_failed_revocation_still_observes_the_real_exit() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    game.change(|state| state.revoke_failure = true);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| running(&sessions)).await;

    sessions.revoke(42);
    game.change(|_| {});
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.reconnecting)
    })
    .await;
    assert_eq!(sessions.state().active.unwrap().phase, Phase::Running);

    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn a_cloud_answer_goes_only_to_the_question_it_was_shown_and_only_for_its_account() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Preparing);
    game.cloud(CloudPhase::Conflict, 17);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.observed && active.cloud.decision_id == 17)
    })
    .await;
    let id = sessions.state().active.unwrap().id;

    assert!(sessions.choose(&id, 16, Choice::Local).is_err());
    assert!(sessions.choose(&id, 17, Choice::Retry).is_err());
    assert!(
        sessions
            .choose("another session", 17, Choice::Local)
            .is_err()
    );
    assert!(game.read(|state| state.choices.is_empty()));

    sessions.choose(&id, 17, Choice::Cloud).unwrap();
    assert_eq!(
        game.read(|state| state.choices.clone()),
        [(42, 17, Choice::Cloud)]
    );
    sessions.revoke(42);
    assert!(sessions.choose(&id, 17, Choice::Local).is_err());
    assert_eq!(game.read(|state| state.choices.len()), 1);

    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
}

#[tokio::test]
async fn an_exit_conflict_is_answered_while_finalizing_without_cancelling_anything() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| running(&sessions)).await;
    game.phase(Phase::Finalizing);
    game.cloud(CloudPhase::Conflict, 5);
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.cloud.decision_id == 5)
    })
    .await;
    let id = sessions.state().active.unwrap().id;

    sessions.choose(&id, 5, Choice::Cancel).unwrap();

    assert_eq!(
        game.read(|state| state.choices.clone()),
        [(42, 5, Choice::Cancel)]
    );
    assert_eq!(game.read(|state| state.cancellations), 0);
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
}

#[tokio::test]
async fn cancelling_a_cloud_check_or_sync_cancels_preparation_without_inventing_an_answer() {
    for phase in [CloudPhase::Checking, CloudPhase::Syncing] {
        let sessions = sessions();
        settled(&sessions).await;
        let game = steam(42, Phase::Preparing);
        game.cloud(phase, 0);
        let launch = follow(&sessions, 42, game.clone());
        eventually(|| {
            sessions
                .state()
                .active
                .is_some_and(|active| active.cloud.phase == phase)
        })
        .await;
        let id = sessions.state().active.unwrap().id;

        sessions.choose(&id, 0, Choice::Cancel).unwrap();

        assert_eq!(game.read(|state| state.cancellations), 1);
        assert!(game.read(|state| state.choices.is_empty()));
        assert!(protects(&sessions, 42));
        game.finish(false);
        assert_eq!(launch.await.unwrap(), Ok(()));
    }
}

#[tokio::test]
async fn finalizing_reserves_the_game_and_the_last_session_keeps_its_pending_cloud_status() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| running(&sessions)).await;
    game.phase(Phase::Finalizing);
    game.cloud(CloudPhase::Syncing, 0);
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.phase == Phase::Finalizing)
    })
    .await;
    assert!(protects(&sessions, 42));
    assert!(sessions.admit(570, 42).is_none());

    game.change(|state| {
        state.current.cloud.phase = CloudPhase::Pending;
        state.current.cloud.error = Some("backend.steam.cloud.network".into());
    });
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    let state = sessions.state();
    assert!(!state.busy());
    let last = state.last.unwrap();
    assert_eq!(last.game, 42);
    assert_eq!(last.cloud.phase, CloudPhase::Pending);
    assert_eq!(
        last.cloud.error.as_deref(),
        Some("backend.steam.cloud.network")
    );
}

#[tokio::test]
async fn a_rosetta_requirement_is_answered_once_for_its_decision_or_declined() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Preparing);
    game.change(|state| state.current.rosetta = Some(3));
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| sessions.state().rosetta.is_some()).await;
    let prompt = sessions.state().rosetta.unwrap();
    assert_eq!((prompt.account, prompt.decision_id), (42, 3));

    assert!(
        sessions
            .resume_after_rosetta(&prompt.session, &42, 2)
            .is_err()
    );
    assert!(
        sessions
            .resume_after_rosetta(&prompt.session, &43, 3)
            .is_err()
    );
    assert!(
        sessions
            .resume_after_rosetta("another session", &42, 3)
            .is_err()
    );
    sessions.decline_rosetta(&prompt.session, &43, 3);
    assert!(
        sessions.state().rosetta.is_some(),
        "another account never declines"
    );
    game.change(|state| state.rosetta_unavailable = true);
    assert_eq!(
        sessions.resume_after_rosetta(&prompt.session, &42, 3),
        Err(Failure::Failed("backend.runtime.rosettaUnavailable".into()))
    );
    game.change(|state| state.rosetta_unavailable = false);
    sessions
        .resume_after_rosetta(&prompt.session, &42, 3)
        .unwrap();
    assert_eq!(game.read(|state| state.rosetta_resumes.clone()), [(42, 3)]);

    sessions.decline_rosetta(&prompt.session, &42, 3);
    assert!(sessions.state().rosetta.is_none());
    eventually(|| game.read(|state| state.cancellations == 1)).await;
    game.fail(None, true);
    assert_eq!(launch.await.unwrap(), Err(Failure::Cancelled));
}

#[tokio::test]
async fn the_start_gate_opens_only_after_the_step_before_it_and_never_once_cancelled() {
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Preparing);
    let (saved, save) = std::sync::mpsc::channel::<()>();
    let (entered, entering) = std::sync::mpsc::channel::<()>();
    let admission = sessions.admit(store, "account".into()).unwrap();
    let canceller = admission.canceller();
    let supervisor = game.clone();
    let launch = tokio::spawn(admission.launch_before(
        async move { Ok(supervisor as Arc<dyn Supervised<Epic>>) },
        Box::new(move || {
            entered.send(()).unwrap();
            save.recv().unwrap();
        }),
    ));
    tokio::task::spawn_blocking(move || entering.recv().unwrap())
        .await
        .unwrap();
    assert_eq!(game.read(|state| state.begins), 0);

    canceller.cancel();
    // Cancellation reaches the supervisor while the step still runs.
    eventually(|| game.read(|state| state.cancellations == 1)).await;
    saved.send(()).unwrap();
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.observed)
    })
    .await;
    assert_eq!(game.read(|state| state.begins), 0);
    game.fail(None, true);
    assert_eq!(launch.await.unwrap(), Err(Failure::Cancelled));
}

#[tokio::test]
async fn stopping_asks_the_running_game_to_end_and_cancels_a_launch_still_preparing() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| running(&sessions)).await;
    sessions.stop().unwrap();
    assert_eq!(game.read(|state| state.stops), 1);
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));

    let admission = sessions.admit(42, 42).unwrap();
    let canceller = admission.canceller();
    sessions.stop().unwrap();
    assert!(canceller.cancelled());
    drop(admission);
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn quitting_reaches_only_the_session_named_and_cancels_a_launch_still_preparing() {
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Running);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| running(&sessions)).await;
    let id = sessions.state().active.unwrap().id;
    assert_eq!(
        sessions.quit("another-session"),
        Err(Failure::Failed(Steam::INVALID_SESSION.into()))
    );
    assert_eq!(
        sessions.force_quit("another-session"),
        Err(Failure::Failed(Steam::INVALID_SESSION.into()))
    );
    sessions.quit(&id).unwrap();
    sessions.force_quit(&id).unwrap();
    assert_eq!(game.read(|state| state.quits.clone()), ["quit", "force"]);
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert!(sessions.quit(&id).is_err(), "the session ended");

    let admission = sessions.admit(42, 42).unwrap();
    let canceller = admission.canceller();
    let id = sessions.state().active.unwrap().id;
    sessions.force_quit(&id).unwrap();
    assert!(canceller.cancelled());
    drop(admission);
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn the_players_controls_reach_only_the_running_game_of_a_signed_in_account() {
    use crate::{protocol::InviteTarget, steam::Control};
    const FRIEND: u64 = 76_561_197_960_265_729;
    const LOBBY: u64 = 109_775_244_421_126_053;
    let refused = || Err(Failure::Failed(Steam::INVALID_SESSION.into()));
    let sessions = sessions();
    settled(&sessions).await;
    let game = steam(42, Phase::Preparing);
    let launch = follow(&sessions, 42, game.clone());
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.observed)
    })
    .await;
    let id = sessions.state().active.unwrap().id;
    assert_eq!(
        sessions.overlay_state(&id, true, false),
        refused(),
        "the game does not run yet"
    );
    game.phase(Phase::Running);
    eventually(|| running(&sessions)).await;
    assert_eq!(
        sessions.overlay_state("another-session", true, false),
        refused()
    );
    let lobby = InviteTarget::Lobby { lobby_id: LOBBY };
    assert_eq!(
        sessions.accept_invite(&id, LOBBY, lobby.clone()),
        refused(),
        "a lobby is no friend"
    );
    assert_eq!(
        sessions.accept_invite(
            &id,
            FRIEND,
            InviteTarget::Connect {
                connect: String::new()
            }
        ),
        refused()
    );
    sessions.overlay_state(&id, true, false).unwrap();
    sessions.accept_invite(&id, FRIEND, lobby.clone()).unwrap();
    sessions.authorize_microtxn(&id, 7, true).unwrap();
    assert_eq!(
        game.read(|state| state.controls.clone()),
        [
            (
                42,
                Control::Overlay {
                    active: true,
                    user_initiated: false
                }
            ),
            (
                42,
                Control::AcceptInvite {
                    friend: FRIEND,
                    target: lobby
                }
            ),
            (
                42,
                Control::AuthorizeMicroTxn {
                    order_id: 7,
                    approve: true
                }
            ),
        ]
    );
    sessions.revoke(42);
    assert_eq!(
        sessions.overlay_state(&id, false, false),
        refused(),
        "the account signed out"
    );
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
    assert_eq!(sessions.overlay_state(&id, false, false), refused());
    assert_eq!(game.read(|state| state.controls.len()), 3);
}

/// What a scripted start returns.
type Started<P> = Result<Arc<dyn Supervised<P>>, String>;

/// A storefront's handover the test scripts, which records its steps.
struct Scripted<P: Provider> {
    /// The platform of the build it starts: a Mac one unless the test says.
    platform: Platform,
    /// How its Windows build runs in Wine: none unless the test says.
    windows: Option<crate::wine::Installation>,
    steps: Arc<Mutex<Vec<&'static str>>>,
    /// Preparing waits until the test notifies it.
    held: Option<Arc<tokio::sync::Notify>>,
    prepared: Result<(), Failure>,
    /// Starting waits for its cancellation instead of returning.
    awaits_cancellation: bool,
    started: Mutex<Option<Started<P>>>,
    /// The status it started the game showing.
    status: Arc<Mutex<Option<OnlineStatus>>>,
}

impl<P: Provider> Scripted<P> {
    fn starting(supervisor: Started<P>) -> Self {
        Self {
            platform: Platform::Mac,
            windows: None,
            steps: Arc::default(),
            held: None,
            prepared: Ok(()),
            awaits_cancellation: false,
            started: Mutex::new(Some(supervisor)),
            status: Arc::default(),
        }
    }
    fn steps(&self) -> Arc<Mutex<Vec<&'static str>>> {
        self.steps.clone()
    }
}

impl<P: Storefront> Handover<P> for Scripted<P> {
    type Prepared = ();
    fn platform(&self) -> Platform {
        self.platform
    }
    fn windows(&self) -> Option<&crate::wine::Installation> {
        self.windows.as_ref()
    }
    fn prepare(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            self.steps.lock().unwrap().push("prepare");
            if let Some(held) = &self.held {
                held.notified().await;
            }
            self.prepared.clone()
        })
    }
    fn before_begin(&self) -> BeforeBegin {
        let steps = self.steps.clone();
        Box::new(move || steps.lock().unwrap().push("before begin"))
    }
    fn start(
        self,
        (): (),
        status: OnlineStatus,
        cancel: CancelToken,
    ) -> Result<Arc<dyn Supervised<P>>, String> {
        self.steps.lock().unwrap().push("start");
        *self.status.lock().unwrap() = Some(status);
        if self.awaits_cancellation {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !cancel.is_cancelled() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if cancel.is_cancelled() {
                self.steps.lock().unwrap().push("start cancelled");
            }
            return Err("backend.steam.runtime.cancelled".into());
        }
        self.started.lock().unwrap().take().unwrap()
    }
}

#[tokio::test]
async fn a_handover_prepares_starts_the_supervisor_and_opens_its_start_gate_after_the_step_before_it()
 {
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Preparing);
    let handover = Scripted::starting(Ok(game.clone() as Arc<dyn Supervised<Epic>>));
    let steps = handover.steps();
    let admission = sessions.admit(store, "account".into()).unwrap();

    let launch = tokio::spawn(admission.hand_off(handover));
    eventually(|| game.read(|state| state.begins) == 1).await;
    assert_eq!(*steps.lock().unwrap(), ["prepare", "start", "before begin"]);
    game.finish(false);

    assert_eq!(launch.await.unwrap(), Ok(()));
    assert!(sessions.state().active.is_none());
}

#[tokio::test]
async fn a_game_starts_showing_the_chosen_status_and_shows_each_change_at_once() {
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    sessions.set_status(OnlineStatus::Invisible);
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Running);
    let handover = Scripted::starting(Ok(game.clone() as Arc<dyn Supervised<Epic>>));
    let started_with = handover.status.clone();
    let admission = sessions.admit(store, "account".into()).unwrap();

    let launch = tokio::spawn(admission.hand_off(handover));
    eventually(|| game.read(|state| state.begins) == 1).await;
    assert_eq!(*started_with.lock().unwrap(), Some(OnlineStatus::Invisible));
    sessions.set_status(OnlineStatus::Invisible);
    sessions.set_status(OnlineStatus::Away);
    assert_eq!(
        game.read(|state| state.statuses.clone()),
        [("account".to_owned(), OnlineStatus::Away)],
        "it started showing Invisible"
    );
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));

    // Without a game, it waits for the next one.
    sessions.set_status(OnlineStatus::Online);
    assert_eq!(game.read(|state| state.statuses.len()), 1);
}

#[tokio::test]
async fn a_failed_preparation_refuses_the_launch_and_gives_its_reservation_back_without_recovery() {
    let recoveries = Arc::new(Mutex::new(0));
    let counted = recoveries.clone();
    let sessions = recovering(move || {
        *counted.lock().unwrap() += 1;
        Ok(None)
    });
    settled(&sessions).await;
    let mut handover = Scripted::starting(Err("never started".into()));
    handover.prepared = Err(Failure::Failed("backend.runtime.wineUnavailable".into()));
    let steps = handover.steps();

    let outcome = sessions.admit(42, 42).unwrap().hand_off(handover).await;

    assert_eq!(
        outcome,
        Err(Failure::Failed("backend.runtime.wineUnavailable".into()))
    );
    assert_eq!(*steps.lock().unwrap(), ["prepare"]);
    assert!(!sessions.state().busy());
    assert_eq!(*recoveries.lock().unwrap(), 1, "only startup recovered");
}

#[tokio::test]
async fn a_launch_cancelled_while_it_prepares_never_starts_its_supervisor() {
    let sessions = sessions();
    settled(&sessions).await;
    let held = Arc::new(tokio::sync::Notify::new());
    let mut handover = Scripted::starting(Err("never started".into()));
    handover.held = Some(held.clone());
    let steps = handover.steps();
    let admission = sessions.admit(42, 42).unwrap();
    let canceller = admission.canceller();

    let launch = tokio::spawn(admission.hand_off(handover));
    eventually(|| steps.lock().unwrap().contains(&"prepare")).await;
    canceller.cancel();
    held.notify_one();

    assert_eq!(launch.await.unwrap(), Err(Failure::Cancelled));
    assert_eq!(*steps.lock().unwrap(), ["prepare"]);
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn a_start_that_failed_may_have_reached_the_supervisor_so_recovery_settles_the_reservation() {
    let recoveries = Arc::new(Mutex::new(0));
    let counted = recoveries.clone();
    let sessions = recovering(move || {
        *counted.lock().unwrap() += 1;
        Ok(None)
    });
    settled(&sessions).await;
    let handover = Scripted::starting(Err("backend.steam.runtime.startFailed".into()));

    let outcome = sessions.admit(42, 42).unwrap().hand_off(handover).await;

    assert_eq!(
        outcome,
        Err(Failure::Failed("backend.steam.runtime.startFailed".into()))
    );
    assert_eq!(
        *recoveries.lock().unwrap(),
        2,
        "recovery confirmed no game runs"
    );
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn cancelling_a_launch_reaches_its_supervisor_while_it_starts() {
    let sessions = sessions();
    settled(&sessions).await;
    let mut handover = Scripted::starting(Err("never returned".into()));
    handover.awaits_cancellation = true;
    let steps = handover.steps();
    let admission = sessions.admit(42, 42).unwrap();
    let canceller = admission.canceller();

    let launch = tokio::spawn(admission.hand_off(handover));
    eventually(|| steps.lock().unwrap().contains(&"start")).await;
    canceller.cancel();

    assert_eq!(launch.await.unwrap(), Err(Failure::Cancelled));
    assert_eq!(
        *steps.lock().unwrap(),
        ["prepare", "start", "start cancelled"]
    );
    assert!(!sessions.state().busy());
}

/// Epic Game sessions with nothing left to recover, which get their
/// runtimes from `runtimes`, and the game their launches start.
async fn epic_launches(runtimes: Arc<Runtimes>) -> (Sessions<Epic>, crate::epic::Store) {
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), runtimes);
    eventually(|| !sessions.state().recovering).await;
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    (sessions, store)
}

#[tokio::test]
async fn a_launch_gets_the_runtimes_its_build_needs_and_shows_their_download_before_it_prepares() {
    let runtimes = FakeRuntimes::new(&[], |_| {});
    runtimes.hold();
    let (sessions, store) = epic_launches(Runtimes::from_source(runtimes.clone())).await;
    let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Preparing);
    let mut handover = Scripted::starting(Ok(game.clone() as Arc<dyn Supervised<Epic>>));
    handover.platform = Platform::Windows;
    let held = Arc::new(tokio::sync::Notify::new());
    handover.held = Some(held.clone());
    let steps = handover.steps();
    let admission = sessions.admit(store, "account".into()).unwrap();

    let launch = tokio::spawn(admission.hand_off(handover));
    let install = || {
        sessions
            .state()
            .active
            .and_then(|active| active.runtime_install)
    };
    eventually(|| {
        install()
            == Some(RuntimeInstall {
                runtime: Runtime::Voile,
                fraction: 0.5,
            })
    })
    .await;
    assert!(steps.lock().unwrap().is_empty(), "nothing is prepared yet");

    runtimes.release();
    eventually(|| steps.lock().unwrap().contains(&"prepare")).await;
    assert_eq!(install(), None, "the download no longer shows");
    assert_eq!(runtimes.in_place(), [Runtime::Voile]);
    held.notify_one();
    eventually(|| game.read(|state| state.begins) == 1).await;
    game.finish(false);
    assert_eq!(launch.await.unwrap(), Ok(()));
}

#[tokio::test]
async fn cancelling_a_launch_while_a_runtime_downloads_cancels_it_and_gives_the_reservation_back() {
    let runtimes = FakeRuntimes::new(&[], |_| {});
    runtimes.hold();
    let (sessions, store) = epic_launches(Runtimes::from_source(runtimes.clone())).await;
    let mut handover = Scripted::starting(Err("never started".into()));
    handover.platform = Platform::Windows;
    let steps = handover.steps();
    let admission = sessions.admit(store, "account".into()).unwrap();
    let canceller = admission.canceller();

    let launch = tokio::spawn(admission.hand_off(handover));
    eventually(|| {
        sessions
            .state()
            .active
            .is_some_and(|active| active.runtime_install.is_some())
    })
    .await;
    canceller.cancel();

    assert_eq!(launch.await.unwrap(), Err(Failure::Cancelled));
    assert!(steps.lock().unwrap().is_empty(), "nothing is prepared");
    assert!(runtimes.in_place().is_empty());
    assert!(!sessions.state().busy());
}

#[tokio::test]
async fn a_runtime_that_fails_to_download_refuses_the_launch_before_it_prepares() {
    let runtimes = FakeRuntimes::new(&[], |_| {});
    let failure = Failure::Failed("backend.voile.downloadFailed".into());
    runtimes.fail(failure.clone());
    let (sessions, store) = epic_launches(Runtimes::from_source(runtimes)).await;
    let mut handover = Scripted::starting(Err("never started".into()));
    handover.platform = Platform::Windows;
    let steps = handover.steps();

    let outcome = sessions
        .admit(store, "account".into())
        .unwrap()
        .hand_off(handover)
        .await;

    assert_eq!(outcome, Err(failure));
    assert!(steps.lock().unwrap().is_empty(), "nothing is prepared");
    assert!(!sessions.state().busy());
}

/// Rocket League's Windows build as a launch resolves it, whose profile
/// names the oldest Voile release it runs in.
fn rocket_league_on_windows() -> crate::wine::Installation {
    let root = tempfile::tempdir().unwrap();
    let windows = crate::wine::tests::windows_of(
        &crate::wine::tests::locations(root.path()),
        crate::profile::Game::Steam { app_id: 252950 },
        &root.path().join("Rocket League"),
    );
    assert!(windows.minimum_voile().is_some());
    windows
}

/// A Windows build's launch from Epic sessions whose Voile is in place while
/// a newer release is offered, waiting for the player's answer.
struct OfferingVoile {
    sessions: Sessions<Epic>,
    launch: tokio::task::JoinHandle<Result<(), Failure>>,
    /// The steps its handover took.
    steps: Arc<Mutex<Vec<&'static str>>>,
    runtimes: Arc<FakeRuntimes>,
    /// The game it starts.
    game: Arc<Fake<Epic>>,
}

impl OfferingVoile {
    /// Its game needs the newer release when `required`.
    async fn launch(required: bool) -> Self {
        let runtimes = FakeRuntimes::new(&[], |_| {});
        runtimes.outdate(Runtime::Voile);
        let (sessions, store) = epic_launches(Runtimes::from_source(runtimes.clone())).await;
        let game = Fake::<Epic>::with("account".into(), store.clone(), Phase::Preparing);
        let mut handover = Scripted::starting(Ok(game.clone() as Arc<dyn Supervised<Epic>>));
        handover.platform = Platform::Windows;
        if required {
            handover.windows = Some(rocket_league_on_windows());
        }
        let steps = handover.steps();
        let admission = sessions.admit(store, "account".into()).unwrap();
        let launch = tokio::spawn(admission.hand_off(handover));
        let offering = Self {
            sessions,
            launch,
            steps,
            runtimes,
            game,
        };
        eventually(|| offering.shown_update().is_some()).await;
        offering
    }

    /// The newer Voile release the session shows.
    fn shown_update(&self) -> Option<Update> {
        self.sessions
            .state()
            .active
            .and_then(|active| active.voile_update)
    }

    fn answer(&self, answer: runtimes::Answer) -> Result<(), Failure> {
        let id = self.sessions.state().active.unwrap().id;
        self.sessions.answer_voile_update(&id, answer)
    }

    /// Lets the game start, then exit.
    async fn play(self) {
        eventually(|| self.game.read(|state| state.begins) == 1).await;
        self.game.finish(false);
        assert_eq!(self.launch.await.unwrap(), Ok(()));
    }
}

#[tokio::test]
async fn a_launch_shows_a_newer_voile_release_until_the_player_skips_it() {
    let offering = OfferingVoile::launch(false).await;
    assert_eq!(offering.shown_update(), Some(fake_offer(false)));
    assert!(
        offering.steps.lock().unwrap().is_empty(),
        "nothing is prepared yet"
    );
    let refused = Err(Failure::Failed(Epic::INVALID_SESSION.into()));
    assert_eq!(
        offering
            .sessions
            .answer_voile_update("another session", runtimes::Answer::Skip),
        refused
    );

    offering.answer(runtimes::Answer::Skip).unwrap();

    eventually(|| offering.steps.lock().unwrap().contains(&"prepare")).await;
    assert_eq!(offering.shown_update(), None);
    assert!(
        offering.runtimes.in_place().is_empty(),
        "nothing downloaded"
    );
    assert_eq!(
        offering.answer(runtimes::Answer::Update),
        refused,
        "answered once"
    );
    offering.play().await;
}

#[tokio::test]
async fn a_newer_voile_release_the_player_takes_downloads_before_the_game_is_prepared() {
    let offering = OfferingVoile::launch(false).await;

    offering.answer(runtimes::Answer::Update).unwrap();

    eventually(|| offering.steps.lock().unwrap().contains(&"prepare")).await;
    assert_eq!(offering.runtimes.in_place(), [Runtime::Voile]);
    offering.play().await;
}

#[tokio::test]
async fn a_voile_release_the_game_needs_cannot_be_skipped_and_cancelling_stops_the_launch() {
    let offering = OfferingVoile::launch(true).await;
    assert_eq!(offering.shown_update(), Some(fake_offer(true)));
    assert_eq!(
        offering.answer(runtimes::Answer::Skip),
        Err(Failure::Failed(Epic::INVALID_SESSION.into()))
    );
    assert!(offering.shown_update().is_some(), "still asked");

    offering.answer(runtimes::Answer::Cancel).unwrap();

    assert_eq!(offering.launch.await.unwrap(), Err(Failure::Cancelled));
    assert!(
        offering.steps.lock().unwrap().is_empty(),
        "nothing is prepared"
    );
    assert!(
        offering.runtimes.in_place().is_empty(),
        "nothing downloaded"
    );
    assert!(!offering.sessions.state().busy());
}

#[tokio::test]
async fn a_launch_keeps_the_replaced_voile_releases_while_another_storefronts_game_may_run_from_one()
 {
    let (events, record) = recorded();
    let runtimes = Runtimes::from_source(FakeRuntimes::new(
        &[Runtime::SteamComponents, Runtime::Voile],
        record,
    ));
    // An Epic game an earlier Obelisk started still runs.
    let store = crate::epic::Store {
        namespace: "ns".into(),
        app_name: "app".into(),
    };
    let recovered = Fake::<Epic>::with("account".into(), store, Phase::Running);
    let found = recovered.clone();
    let epic_sessions = Sessions::<Epic>::new(
        Box::new(move || Ok(Some(found.clone() as Arc<dyn Supervised<Epic>>))),
        runtimes.clone(),
    );
    let steam_sessions = Sessions::<Steam>::new(Box::new(|| Ok(None)), runtimes);
    settled(&steam_sessions).await;
    eventually(|| {
        epic_sessions
            .state()
            .active
            .is_some_and(|active| active.observed)
    })
    .await;
    let launch = |game: Arc<Fake<Steam>>| {
        let mut handover = Scripted::starting(Ok(game as Arc<dyn Supervised<Steam>>));
        handover.platform = Platform::Windows;
        steam_sessions.admit(42, 42).unwrap().hand_off(handover)
    };

    let game = steam(42, Phase::Running);
    let first = tokio::spawn(launch(game.clone()));
    eventually(|| running(&steam_sessions)).await;
    assert!(
        events.lock().unwrap().is_empty(),
        "the Epic game may run from one"
    );
    game.finish(false);
    assert_eq!(first.await.unwrap(), Ok(()));

    recovered.finish(false);
    eventually(|| !epic_sessions.state().busy()).await;
    let game = steam(42, Phase::Running);
    let second = tokio::spawn(launch(game.clone()));
    eventually(|| running(&steam_sessions)).await;
    assert_eq!(*events.lock().unwrap(), ["remove replaced"]);
    game.finish(false);
    assert_eq!(second.await.unwrap(), Ok(()));
}
