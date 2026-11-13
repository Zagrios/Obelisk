//! One storefront's Game sessions: the Reservation a launch holds from its
//! admission until its supervisor confirms the game is gone, the recovery of
//! a session Obelisk left running, Account revocation, and the answers to a
//! session's Cloud question, its Rosetta requirement and the newer Voile
//! release its launch offers. Only the supervisor owns the game and its
//! processes; this module observes it and forwards answers.
use crate::{
    cloud::{Choice, Phase as CloudPhase, Status as CloudStatus},
    runtimes::{self, Answer, Runtime, Runtimes, Update},
    supervisor::{self, FailureReason, Phase, Provider, Record},
};
use obelisk_core::{OnlineStatus, Platform, transfer::CancelToken};
use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch};

/// A failed recovery or snapshot is retried after this long.
const PACE: Duration = Duration::from_millis(250);

/// What a storefront adds to its Game sessions.
pub trait Storefront: Provider {
    /// The game a session runs, as the storefront names it.
    type Game: Clone + fmt::Debug + PartialEq + Send + Sync + 'static;
    /// Whether a new supervisor waits for its UI to open its start gate.
    const BEGINS: bool;
    /// A refused session call.
    const INVALID_SESSION: &'static str;
    /// A failed session that reported no reason.
    const SESSION_FAILED: &'static str;
    /// The runtimes every build of the storefront needs, whatever its
    /// platform.
    const RUNTIMES: &'static [Runtime];
    fn game(store: &Self::Store) -> Self::Game;
}

/// One observation of a supervisor, whatever carries it.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation<P: Provider> {
    pub id: String,
    pub account: P::Account,
    pub store: P::Store,
    pub phase: Phase,
    /// The phase is final and the session owns no game or starting process:
    /// only then is the Reservation released.
    pub finished: bool,
    pub failure: Option<FailureReason>,
    /// Localization key of the session's own failure.
    pub error: Option<String>,
    pub revision: u64,
    /// The control connection was lost; the game may still run.
    pub reconnecting: bool,
    /// The decision a Rosetta requirement waits on.
    pub rosetta: Option<u64>,
    pub cloud: CloudStatus,
    /// What the game did, once it ran and exited.
    pub played: Option<supervisor::Played>,
}
impl<P: Provider> From<Record<P>> for Observation<P> {
    fn from(record: Record<P>) -> Self {
        Self {
            finished: record.finished(),
            failure: record.failure_reason(),
            id: record.id,
            account: record.account_id,
            store: record.store,
            phase: record.phase,
            error: record.error,
            revision: record.revision,
            reconnecting: record.reconnecting,
            rosetta: record.rosetta.map(|request| request.decision_id),
            cloud: record.cloud,
            played: record.played,
        }
    }
}

/// One Game session's supervisor, as this module drives it. Every call may
/// block on its control connection; failures are localization keys.
pub trait Supervised<P: Provider>: Send + Sync {
    /// Waits for a revision newer than `after`, or returns at once.
    fn snapshot(&self, after: Option<u64>) -> Result<Observation<P>, String>;
    /// Cancels preparation. After spawn the game keeps running.
    fn cancel(&self);
    /// Ends the game, as quitting Obelisk does; its exit Cloud sync still
    /// runs. Before spawn it cancels preparation.
    fn stop(&self) -> Result<(), String>;
    /// Asks the game to close its windows; nothing is killed. Before spawn it
    /// cancels preparation.
    fn quit(&self) -> Result<(), String>;
    /// Ends the game now, with whatever still runs for its installation; its
    /// exit Cloud sync still runs. Before spawn it cancels preparation.
    fn force_quit(&self) -> Result<(), String>;
    /// Opens the start gate of a supervisor that waits for its UI.
    fn begin(&self) -> Result<(), String>;
    fn choose(&self, account: &P::Account, decision_id: u64, choice: Choice) -> Result<(), String>;
    fn resume_after_rosetta(&self, account: &P::Account, decision_id: u64) -> Result<(), String>;
    /// Account revocation: network work stops, the game keeps running.
    fn revoke(&self, account: &P::Account) -> Result<(), String>;
    /// The player's `control` of the running game.
    fn control(&self, account: &P::Account, control: P::Control) -> Result<(), String>;
    /// How the game shows the account to its friends, from its start on.
    fn set_status(&self, account: &P::Account, status: OnlineStatus) -> Result<(), String>;
}

impl<P: Provider> Supervised<P> for supervisor::Client<P> {
    fn snapshot(&self, after: Option<u64>) -> Result<Observation<P>, String> {
        supervisor::Client::snapshot(self, after)
            .map(Into::into)
            .map_err(|error| error.to_string())
    }
    fn cancel(&self) {
        supervisor::Client::cancel(self);
    }
    fn stop(&self) -> Result<(), String> {
        supervisor::Client::stop(self).map_err(|error| error.to_string())
    }
    fn quit(&self) -> Result<(), String> {
        supervisor::Client::quit(self).map_err(|error| error.to_string())
    }
    fn force_quit(&self) -> Result<(), String> {
        supervisor::Client::force_quit(self).map_err(|error| error.to_string())
    }
    fn begin(&self) -> Result<(), String> {
        supervisor::Client::begin(self).map_err(|error| error.to_string())
    }
    fn choose(&self, account: &P::Account, decision_id: u64, choice: Choice) -> Result<(), String> {
        supervisor::Client::choose(self, account, decision_id, choice)
            .map_err(|error| error.to_string())
    }
    fn resume_after_rosetta(&self, account: &P::Account, decision_id: u64) -> Result<(), String> {
        supervisor::Client::resume_after_rosetta(self, account, decision_id)
            .map_err(|error| error.to_string())
    }
    fn revoke(&self, account: &P::Account) -> Result<(), String> {
        supervisor::Client::revoke(self, account).map_err(|error| error.to_string())
    }
    fn control(&self, account: &P::Account, control: P::Control) -> Result<(), String> {
        supervisor::Client::control(self, account, control).map_err(|error| error.to_string())
    }
    fn set_status(&self, account: &P::Account, status: OnlineStatus) -> Result<(), String> {
        supervisor::Client::set_status(self, account, status).map_err(|error| error.to_string())
    }
}

/// Runs once a new supervisor holds the game and before its start gate opens,
/// such as saving the credentials its handoff rotated.
pub type BeforeBegin = Box<dyn FnOnce() + Send>;

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A storefront's part of handing an admitted game to a new supervisor.
/// `Admission::hand_off` runs it in order: the runtimes its `platform` needs,
/// `prepare`, then `start`.
pub trait Handover<P: Storefront>: Send + Sync + 'static {
    /// What `prepare` readies for `start`.
    type Prepared: Send + 'static;
    /// The platform of the build it starts, which decides the runtimes it
    /// needs before it is prepared.
    fn platform(&self) -> Platform;
    /// How its Windows build runs in Wine, as its launch settings resolved
    /// for this launch: the runtimes it needs wait for the oldest Voile
    /// release they name when an older one is in place. None for a Mac
    /// build.
    fn windows(&self) -> Option<&crate::wine::Installation>;
    /// Readies the game, such as its Wine prefix, which may take minutes, and
    /// then the short-lived credentials it starts with. A failure is definite.
    fn prepare(&self) -> BoxFuture<'_, Result<Self::Prepared, Failure>>;
    /// Runs once the new supervisor holds the game, before its start gate
    /// opens.
    fn before_begin(&self) -> BeforeBegin {
        Box::new(|| {})
    }
    /// Starts the supervisor, whose game shows the account as `status` says;
    /// `cancel` stops a start in progress. It blocks. A failure leaves
    /// unknown whether the supervisor accepted the game.
    fn start(
        self,
        prepared: Self::Prepared,
        status: OnlineStatus,
        cancel: CancelToken,
    ) -> Result<Arc<dyn Supervised<P>>, String>;
}

/// Finds the session a previous Obelisk left running. `None` confirms that
/// none remains.
pub type Recovery<P> =
    Box<dyn Fn() -> Result<Option<Arc<dyn Supervised<P>>>, String> + Send + Sync>;

/// How a launch ended when it did not end with its game.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// Cancelled before its game started, which ends it silently.
    #[error("cancelled")]
    Cancelled,
    /// A localization key.
    #[error("{0}")]
    Failed(String),
}

/// The failure whose localization key `error` displays.
pub(crate) fn failed(error: impl std::fmt::Display) -> Failure {
    Failure::Failed(error.to_string())
}

/// What a handoff to a new supervisor returned instead of its session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// No supervisor accepted the game.
    Refused(Failure),
    /// The supervisor may have accepted the game before its reply was lost,
    /// so only recovery may release the Reservation.
    Lost(String),
}

/// How much of a runtime has downloaded while the launch waits for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RuntimeInstall {
    pub runtime: Runtime,
    pub fraction: f64,
}
impl From<runtimes::Progress> for RuntimeInstall {
    fn from(progress: runtimes::Progress) -> Self {
        let fraction = match progress.total {
            0 => 0.0,
            total => (progress.received as f64 / total as f64).min(1.0),
        };
        Self {
            runtime: progress.runtime,
            fraction,
        }
    }
}

/// One Game session as Obelisk presents it.
#[derive(Clone, Debug, PartialEq)]
pub struct Session<P: Storefront> {
    pub id: String,
    pub game: P::Game,
    pub account: P::Account,
    pub phase: Phase,
    /// The control connection was lost; the game may still run.
    pub reconnecting: bool,
    pub cloud: CloudStatus,
    /// What the storefront's supervisor records live, such as Steam's host.
    pub store: Option<P::Store>,
    /// A supervisor snapshot backs this session, not only its admission.
    pub observed: bool,
    /// How much of a runtime has downloaded while the launch waits for it.
    pub runtime_install: Option<RuntimeInstall>,
    /// The newer Voile release the launch waits for the player to take or
    /// skip, before any download.
    pub voile_update: Option<Update>,
    /// When this Obelisk saw the game start, in seconds since the Unix
    /// epoch: once a session it saw preparing is past it, even if its game
    /// already exited. A game recovered running started before, and has none.
    pub started_at: Option<u64>,
    /// What the game did, once it ran and exited.
    pub played: Option<supervisor::Played>,
    rosetta: Option<u64>,
}
impl<P: Storefront> Session<P> {
    fn admitted(game: P::Game, account: P::Account) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            game,
            account,
            phase: Phase::Preparing,
            reconnecting: false,
            cloud: CloudStatus::default(),
            store: None,
            observed: false,
            runtime_install: None,
            voile_update: None,
            started_at: None,
            played: None,
            rosetta: None,
        }
    }
    fn observed(observation: &Observation<P>) -> Self {
        Self {
            id: observation.id.clone(),
            game: P::game(&observation.store),
            account: observation.account.clone(),
            phase: observation.phase,
            reconnecting: observation.reconnecting,
            cloud: observation.cloud.clone(),
            store: Some(observation.store.clone()),
            observed: true,
            runtime_install: None,
            voile_update: None,
            started_at: None,
            played: observation.played.clone(),
            rosetta: observation.rosetta,
        }
    }
}

/// A Rosetta requirement the player can answer: its session, account and
/// decision, so a stale answer never resumes a newer requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct RosettaPrompt<P: Provider> {
    pub session: String,
    pub account: P::Account,
    pub decision_id: u64,
}

/// Everything the storefront's Game sessions present. `revision` grows with
/// every change.
#[derive(Clone, Debug, PartialEq)]
pub struct State<P: Storefront> {
    pub revision: u64,
    /// The session holding the Reservation.
    pub active: Option<Session<P>>,
    /// The last session observed, kept after it ends.
    pub last: Option<Session<P>>,
    /// A session a previous Obelisk left may still run: the storefront
    /// stays reserved until recovery settles.
    pub recovering: bool,
    /// The last recovery attempt failed; it is retried.
    pub recovery_failed: bool,
    /// Accounts signed out since the last admission.
    pub revoked: Vec<P::Account>,
    pub rosetta: Option<RosettaPrompt<P>>,
}
impl<P: Storefront> State<P> {
    pub fn busy(&self) -> bool {
        self.active.is_some() || self.recovering
    }
}

/// One launch or recovery, which cancellation reaches.
struct Operation {
    cancelled: AtomicBool,
    hooks: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
}
impl Operation {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            hooks: Mutex::new(Vec::new()),
        })
    }
    /// Cancels once: a second cancellation has nothing left to reach.
    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        for hook in self.hooks.lock().unwrap().iter() {
            hook();
        }
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

struct Inner<P: Storefront> {
    revision: u64,
    active: Option<Session<P>>,
    last: Option<Session<P>>,
    recovering: bool,
    recovery_failed: bool,
    revoked: Vec<P::Account>,
    /// Revocations the current supervisor confirmed.
    applied: Vec<P::Account>,
    client: Option<Arc<dyn Supervised<P>>>,
    operation: Option<Arc<Operation>>,
    /// The session and decision of a Rosetta requirement the player declined.
    declined_rosetta: Option<(String, u64)>,
    /// Where the player's answer to the active launch's `voile_update` goes.
    voile_answer: Option<oneshot::Sender<Answer>>,
    /// How the player shows to their friends: every game shows it. `None`
    /// until the UI told one, which leaves a recovered game as it was.
    status: Option<OnlineStatus>,
    /// The status the current supervisor started with or confirmed; `None`
    /// for one recovered, which shows what it was last told.
    shown: Option<OnlineStatus>,
}
impl<P: Storefront> Inner<P> {
    fn state(&self) -> State<P> {
        let rosetta = self.active.as_ref().and_then(|active| {
            let decision_id = active.rosetta?;
            (active.phase == Phase::Preparing
                && !active.reconnecting
                && !self.revoked.contains(&active.account)
                && self.declined_rosetta.as_ref() != Some(&(active.id.clone(), decision_id)))
            .then(|| RosettaPrompt {
                session: active.id.clone(),
                account: active.account.clone(),
                decision_id,
            })
        });
        State {
            revision: self.revision,
            active: self.active.clone(),
            last: self.last.clone(),
            recovering: self.recovering,
            recovery_failed: self.recovery_failed,
            revoked: self.revoked.clone(),
            rosetta,
        }
    }
    fn current(&self, operation: &Arc<Operation>) -> bool {
        self.operation
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, operation))
    }
    /// The Reservation ends: nothing is active any more.
    fn finish(&mut self) {
        self.declined_rosetta = None;
        self.voile_answer = None;
        self.active = None;
        self.client = None;
        self.applied.clear();
        self.shown = None;
        self.recovering = false;
        self.recovery_failed = false;
        self.operation = None;
    }
}

struct Shared<P: Storefront> {
    inner: Mutex<Inner<P>>,
    published: watch::Sender<State<P>>,
    recover: Recovery<P>,
    runtimes: Arc<Runtimes>,
}
impl<P: Storefront> Shared<P> {
    fn update<T>(&self, change: impl FnOnce(&mut Inner<P>) -> T) -> T {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.state();
        let result = change(&mut inner);
        let mut after = inner.state();
        after.revision = before.revision;
        if after != before {
            inner.revision += 1;
            after.revision = inner.revision;
            self.published.send_replace(after);
        }
        result
    }

    fn finish(&self, operation: &Arc<Operation>) {
        self.update(|inner| {
            if inner.current(operation) {
                inner.finish();
            }
        });
    }

    /// Tells `client`, the current supervisor, the status the player chose
    /// last, unless it already shows it.
    fn apply_status(&self, account: &P::Account, client: &Arc<dyn Supervised<P>>) {
        let status = {
            let inner = self.inner.lock().unwrap();
            match inner.status {
                Some(status) if inner.shown != Some(status) => status,
                _ => return,
            }
        };
        if client.set_status(account, status).is_ok() {
            let mut inner = self.inner.lock().unwrap();
            if inner
                .client
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, client))
            {
                inner.shown = Some(status);
            }
        }
    }

    /// Revokes `account` through `client` once, when it was signed out.
    fn apply_revocation(
        &self,
        account: &P::Account,
        client: &Arc<dyn Supervised<P>>,
    ) -> Result<(), String> {
        let pending = {
            let inner = self.inner.lock().unwrap();
            inner.revoked.contains(account) && !inner.applied.contains(account)
        };
        if !pending {
            return Ok(());
        }
        client.revoke(account)?;
        // Success stays tied to this exact supervisor.
        self.update(|inner| {
            if inner
                .client
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, client))
                && !inner.applied.contains(account)
            {
                inner.applied.push(account.clone());
            }
        });
        Ok(())
    }

    /// Follows `client` until its session is finished, and reports how it
    /// ended. `begin` opens the start gate of a new supervisor.
    fn observe(
        &self,
        operation: &Arc<Operation>,
        client: Arc<dyn Supervised<P>>,
        begin: Option<BeforeBegin>,
    ) -> Result<(), Failure> {
        let revoked = self.update(|inner| {
            if !inner.current(operation) {
                return None;
            }
            inner.client = Some(client.clone());
            inner.applied.clear();
            if begin.is_none() {
                // Recovered: it may show an older status.
                inner.shown = None;
            }
            Some(
                inner
                    .active
                    .as_ref()
                    .is_some_and(|active| inner.revoked.contains(&active.account)),
            )
        });
        let Some(revoked) = revoked else {
            client.cancel();
            return Err(Failure::Cancelled);
        };
        let begin_gate = begin.is_some();
        {
            // Cancel parks on the control connection, which the caller, such
            // as the UI, must not wait for.
            let cancel = client.clone();
            operation.hooks.lock().unwrap().push(Box::new(move || {
                let cancel = cancel.clone();
                std::thread::spawn(move || cancel.cancel());
            }));
        }
        if let Some(before) = begin.filter(|_| P::BEGINS && !revoked && !operation.cancelled()) {
            before();
        }
        if operation.cancelled() {
            client.cancel();
        } else if begin_gate && P::BEGINS {
            if revoked {
                client.cancel();
            } else if client.begin().is_err() {
                // A lost Begin reply cannot establish whether admission
                // happened. Keep ownership until a terminal snapshot.
                self.update(|inner| {
                    if let Some(active) = &mut inner.active {
                        active.reconnecting = true;
                    }
                });
            }
        }
        let mut revision = None;
        loop {
            let mut revocation_failed = false;
            let account = self
                .inner
                .lock()
                .unwrap()
                .active
                .as_ref()
                .map(|active| active.account.clone());
            if let Some(account) = account {
                if self.apply_revocation(&account, &client).is_err() {
                    revocation_failed = true;
                }
                self.apply_status(&account, &client);
            }
            let observation = match client.snapshot(revision) {
                Ok(observation) => observation,
                Err(_) => {
                    // Control loss is uncertainty, not evidence of game exit.
                    self.update(|inner| {
                        if let Some(active) = &mut inner.active {
                            active.reconnecting = true;
                        }
                    });
                    std::thread::sleep(PACE);
                    continue;
                }
            };
            if !observation.finished
                && self
                    .apply_revocation(&observation.account, &client)
                    .is_err()
            {
                revocation_failed = true;
            }
            revision = Some(observation.revision);
            let mut session = Session::observed(&observation);
            // A failed revocation cannot suppress the snapshot that
            // establishes real exit after a supervisor loses its socket.
            if revocation_failed && !observation.finished {
                session.reconnecting = true;
            }
            self.update(|inner| {
                if let Some(active) = &inner.active {
                    let started = matches!(
                        session.phase,
                        Phase::Running | Phase::Finalizing | Phase::Finished
                    );
                    session.started_at = active.started_at.or_else(|| {
                        (active.phase == Phase::Preparing && started).then(crate::unix_now)
                    });
                }
                inner.active = Some(session.clone());
                inner.last = Some(session);
                inner.recovering = false;
                inner.recovery_failed = false;
            });
            if observation.finished {
                return match (observation.phase, observation.failure) {
                    (Phase::Failed, Some(FailureReason::Cancelled)) => Err(Failure::Cancelled),
                    (Phase::Failed, _) => Err(Failure::Failed(
                        observation
                            .error
                            .unwrap_or_else(|| P::SESSION_FAILED.to_owned()),
                    )),
                    _ => Ok(()),
                };
            }
        }
    }

    /// Recovers a session a previous Obelisk left running and follows it.
    /// `Ok(false)` confirms that none remains.
    fn recover(&self, operation: &Arc<Operation>) -> Result<bool, Failure> {
        loop {
            match (self.recover)() {
                // Cancellation must not turn unknown process ownership into exit.
                Err(_) => {
                    self.update(|inner| inner.recovery_failed = true);
                    std::thread::sleep(PACE);
                }
                Ok(None) => {
                    self.update(|inner| inner.recovery_failed = false);
                    return Ok(false);
                }
                Ok(Some(client)) => {
                    return self.observe(operation, client, None).map(|()| true);
                }
            }
        }
    }
}

/// One storefront's Game sessions.
pub struct Sessions<P: Storefront> {
    shared: Arc<Shared<P>>,
}

impl<P: Storefront> Sessions<P> {
    /// Starts by recovering a session a previous Obelisk left running: the
    /// storefront stays reserved until recovery settles. Its launches get
    /// their runtimes from `runtimes`.
    pub fn new(recover: Recovery<P>, runtimes: Arc<Runtimes>) -> Self {
        let operation = Operation::new();
        let inner = Inner {
            revision: 1,
            active: None,
            last: None,
            recovering: true,
            recovery_failed: false,
            revoked: Vec::new(),
            applied: Vec::new(),
            client: None,
            operation: Some(operation.clone()),
            declined_rosetta: None,
            voile_answer: None,
            status: None,
            shown: None,
        };
        let (published, _) = watch::channel(inner.state());
        let shared = Arc::new(Shared {
            inner: Mutex::new(inner),
            published,
            recover,
            runtimes: runtimes.clone(),
        });
        // A game an earlier Obelisk started may run from a replaced release;
        // once recovered, it shows as any observed session. Every game this
        // process starts runs from the pinned one, and a launch is observed
        // only after its handoff, so it never counts itself.
        let watched = Arc::downgrade(&shared);
        runtimes.watch_sessions(move || {
            watched.upgrade().is_some_and(|shared| {
                let state = shared.published.borrow();
                state.recovering || state.active.as_ref().is_some_and(|active| active.observed)
            })
        });
        let recovering = shared.clone();
        std::thread::spawn(move || {
            let _ = recovering.recover(&operation);
            recovering.finish(&operation);
        });
        Self { shared }
    }

    pub fn state(&self) -> State<P> {
        self.shared.published.borrow().clone()
    }

    /// The runtimes its launches get, which the storefront's Transfers get
    /// too.
    pub(crate) fn runtimes(&self) -> &Arc<Runtimes> {
        &self.shared.runtimes
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> State<P> {
        let mut receiver = self.shared.published.subscribe();
        let _ = receiver.wait_for(|state| state.revision > after).await;
        receiver.borrow().clone()
    }

    /// Admits a launch of `game` for `account`: the Reservation holds from
    /// here, before any verification or authentication can suspend. `None`
    /// while another session or a recovery holds the storefront.
    pub fn admit(&self, game: P::Game, account: P::Account) -> Option<Admission<P>> {
        let operation = Operation::new();
        let admitted = self.shared.update(|inner| {
            if inner.active.is_some() || inner.recovering {
                return false;
            }
            // A newly admitted launch belongs to the currently authenticated
            // account. Revocations from a finished session must not disable it.
            inner.revoked.clear();
            inner.applied.clear();
            inner.active = Some(Session::admitted(game, account));
            inner.recovery_failed = false;
            inner.operation = Some(operation.clone());
            true
        });
        admitted.then(|| Admission {
            canceller: Canceller {
                shared: self.shared.clone(),
                operation,
                launched: Arc::new(AtomicBool::new(false)),
            },
        })
    }

    /// Account revocation. Admission checks it at once; the supervisor learns
    /// it as soon as it can be reached, and the game keeps running.
    pub fn revoke(&self, account: P::Account) {
        let client = self.shared.update(|inner| {
            if !inner.revoked.contains(&account) {
                inner.revoked.push(account.clone());
            }
            inner
                .active
                .as_ref()
                .filter(|active| active.account == account)
                .and(inner.client.clone())
        });
        if let Some(client) = client {
            let shared = self.shared.clone();
            std::thread::spawn(move || {
                let _ = shared.apply_revocation(&account, &client);
            });
        }
    }

    /// Ends the running game, as quitting Obelisk does: it gets a few
    /// seconds to quit, and its session still settles its Cloud saves. A
    /// launch still preparing is cancelled.
    pub fn stop(&self) -> Result<(), Failure> {
        let (client, operation) = {
            let inner = self.shared.inner.lock().unwrap();
            (inner.client.clone(), inner.operation.clone())
        };
        Self::call_or_cancel(client, operation, |client| client.stop())
    }

    /// The player's Quit of session `id`'s game: it is asked to close its
    /// windows, and nothing is killed. A launch still preparing is cancelled.
    pub fn quit(&self, id: &str) -> Result<(), Failure> {
        self.end(id, |client| client.quit())
    }

    /// The player's Force Quit of session `id`'s game, which ignored Quit: it
    /// ends now, and its session still settles its Cloud saves. A launch
    /// still preparing is cancelled.
    pub fn force_quit(&self, id: &str) -> Result<(), Failure> {
        self.end(id, |client| client.force_quit())
    }

    fn end(
        &self,
        id: &str,
        call: impl FnOnce(&dyn Supervised<P>) -> Result<(), String>,
    ) -> Result<(), Failure> {
        let (client, operation) = {
            let inner = self.shared.inner.lock().unwrap();
            match &inner.active {
                Some(active) if active.id == id => (inner.client.clone(), inner.operation.clone()),
                _ => return Err(Failure::Failed(P::INVALID_SESSION.to_owned())),
            }
        };
        Self::call_or_cancel(client, operation, call)
    }

    /// Makes `call` on the session's supervisor, or cancels the launch still
    /// preparing before it has one.
    fn call_or_cancel(
        client: Option<Arc<dyn Supervised<P>>>,
        operation: Option<Arc<Operation>>,
        call: impl FnOnce(&dyn Supervised<P>) -> Result<(), String>,
    ) -> Result<(), Failure> {
        match client {
            Some(client) => call(&*client).map_err(Failure::Failed),
            None => {
                if let Some(operation) = operation {
                    operation.cancel();
                }
                Ok(())
            }
        }
    }

    /// How the player shows to their friends: the next game starts showing
    /// it, and the one under way shows it at once.
    pub fn set_status(&self, status: OnlineStatus) {
        let (client, account) = {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.status = Some(status);
            match (&inner.client, &inner.active) {
                (Some(client), Some(active)) => (client.clone(), active.account.clone()),
                _ => return,
            }
        };
        // Otherwise its session's observation tells it again.
        self.shared.apply_status(&account, &client);
    }

    /// The player's `control` of session `id`'s running game. Refused while
    /// the game does not run, for a signed-out account, and while the
    /// control connection is lost.
    pub fn control(&self, id: &str, control: P::Control) -> Result<(), Failure> {
        let (client, account) = {
            let inner = self.shared.inner.lock().unwrap();
            match (&inner.active, &inner.client) {
                (Some(active), Some(client))
                    if active.id == id
                        && active.phase == Phase::Running
                        && !active.reconnecting
                        && !inner.revoked.contains(&active.account) =>
                {
                    (client.clone(), active.account.clone())
                }
                _ => return Err(Failure::Failed(P::INVALID_SESSION.to_owned())),
            }
        };
        client.control(&account, control).map_err(Failure::Failed)
    }

    /// Answers the Cloud question the session `id` shows with `decision_id`.
    /// Cancel stops a launch check still in progress.
    pub fn choose(&self, id: &str, decision_id: u64, choice: Choice) -> Result<(), Failure> {
        let refused = || Failure::Failed(P::INVALID_SESSION.to_owned());
        let (client, active) = {
            let inner = self.shared.inner.lock().unwrap();
            match (&inner.active, &inner.client) {
                (Some(active), Some(client))
                    if active.id == id
                        && active.cloud.decision_id == decision_id
                        && !inner.revoked.contains(&active.account)
                        && !active.reconnecting =>
                {
                    (client.clone(), active.clone())
                }
                _ => return Err(refused()),
            }
        };
        if choice == Choice::Cancel
            && active.phase == Phase::Preparing
            && matches!(
                active.cloud.phase,
                CloudPhase::Checking | CloudPhase::Syncing
            )
        {
            client.cancel();
            return Ok(());
        }
        if !active.cloud.allowed_choices.contains(&choice) {
            return Err(refused());
        }
        client
            .choose(&active.account, decision_id, choice)
            .map_err(Failure::Failed)
    }

    /// Resumes the launch waiting for Rosetta, once it is installed.
    pub fn resume_after_rosetta(
        &self,
        id: &str,
        account: &P::Account,
        decision_id: u64,
    ) -> Result<(), Failure> {
        let (client, account) = {
            let inner = self.shared.inner.lock().unwrap();
            match (inner.state().rosetta, &inner.client) {
                (Some(prompt), Some(client))
                    if prompt.session == id
                        && prompt.account == *account
                        && prompt.decision_id == decision_id =>
                {
                    (client.clone(), prompt.account)
                }
                _ => return Err(Failure::Failed(P::INVALID_SESSION.to_owned())),
            }
        };
        client
            .resume_after_rosetta(&account, decision_id)
            .map_err(Failure::Failed)
    }

    /// The player declined to install Rosetta: the waiting launch is
    /// cancelled and its requirement no longer shows.
    pub fn decline_rosetta(&self, id: &str, account: &P::Account, decision_id: u64) {
        let operation = self.shared.update(|inner| {
            let prompt = inner.state().rosetta?;
            if prompt.session != id
                || prompt.account != *account
                || prompt.decision_id != decision_id
            {
                return None;
            }
            inner.declined_rosetta = Some((prompt.session, decision_id));
            inner.operation.clone()
        });
        if let Some(operation) = operation {
            operation.cancel();
        }
    }

    /// The player's answer to the newer Voile release session `id`'s launch
    /// offers: Cancel stops the launch. A release its game needs cannot be
    /// skipped.
    pub fn answer_voile_update(&self, id: &str, answer: Answer) -> Result<(), Failure> {
        let answered = self.shared.update(|inner| {
            let active = inner.active.as_mut().filter(|active| active.id == id)?;
            if active.voile_update.as_ref()?.required && answer == Answer::Skip {
                return None;
            }
            active.voile_update = None;
            inner.voile_answer.take()?.send(answer).ok()
        });
        answered.ok_or_else(|| Failure::Failed(P::INVALID_SESSION.to_owned()))
    }
}

/// One admitted launch. It holds the Reservation until its session is
/// finished, or until it is cancelled or dropped before its handoff.
pub struct Admission<P: Storefront> {
    canceller: Canceller<P>,
}

/// Cancels one admitted launch, before or after its handoff.
#[derive(Clone)]
pub struct Canceller<P: Storefront> {
    shared: Arc<Shared<P>>,
    operation: Arc<Operation>,
    launched: Arc<AtomicBool>,
}

impl<P: Storefront> Canceller<P> {
    /// Cancels preparation. Before the handoff this releases the Reservation;
    /// after it, a game that already started keeps running.
    pub fn cancel(&self) {
        self.operation.cancel();
        if !self.launched.load(Ordering::Acquire) {
            self.shared.finish(&self.operation);
        }
    }

    pub fn cancelled(&self) -> bool {
        self.operation.cancelled()
    }

    /// Runs `hook` when the launch is cancelled, at once if it already was.
    pub fn on_cancel(&self, hook: impl Fn() + Send + Sync + 'static) {
        if self.cancelled() {
            hook();
        }
        self.operation.hooks.lock().unwrap().push(Box::new(hook));
    }

    /// Shows `update` on the session until the player answers it, which the
    /// receiver gets. A launch no longer current never gets an answer.
    fn offer_voile_update(&self, update: Update) -> oneshot::Receiver<Answer> {
        let (answer, answered) = oneshot::channel();
        self.shared.update(|inner| {
            if inner.current(&self.operation)
                && let Some(active) = &mut inner.active
            {
                active.voile_update = Some(update);
                inner.voile_answer = Some(answer);
            }
        });
        answered
    }

    /// How much of a runtime has downloaded while the launch waits for it.
    fn runtime_install(&self, install: Option<RuntimeInstall>) {
        self.shared.update(|inner| {
            if inner.current(&self.operation)
                && let Some(active) = &mut inner.active
            {
                active.runtime_install = install;
            }
        });
    }
}

impl<P: Storefront> std::ops::Deref for Admission<P> {
    type Target = Canceller<P>;
    fn deref(&self) -> &Canceller<P> {
        &self.canceller
    }
}

impl<P: Storefront> Admission<P> {
    /// What cancels this launch once it is handed off.
    pub fn canceller(&self) -> Canceller<P> {
        self.canceller.clone()
    }

    /// Hands the admitted game to a new supervisor through `handover`, then
    /// follows its session until it is finished. The runtimes its build
    /// misses come first, their download showing on the session, after the
    /// player's answer to a newer Voile release, which it shows. A failure
    /// or cancellation before the supervisor starts refuses the launch and
    /// gives the Reservation back; once it starts, only its session does.
    pub async fn hand_off<H: Handover<P>>(self, handover: H) -> Result<(), Failure> {
        let admission = self.canceller();
        let before_begin = handover.before_begin();
        let handoff = async move {
            let cancel = CancelToken::new();
            let token = cancel.clone();
            admission.on_cancel(move || token.cancel());
            let got = admission
                .shared
                .runtimes
                .get_for_launch::<P>(
                    handover.platform(),
                    handover
                        .windows()
                        .and_then(crate::wine::Installation::minimum_voile),
                    &cancel,
                    |progress| admission.runtime_install(Some(progress.into())),
                    |update| admission.offer_voile_update(update),
                )
                .await;
            admission.runtime_install(None);
            got.map_err(Handoff::Refused)?;
            let prepared = handover.prepare().await.map_err(Handoff::Refused)?;
            if admission.cancelled() {
                return Err(Handoff::Refused(Failure::Cancelled));
            }
            let status = {
                let mut inner = admission.shared.inner.lock().unwrap();
                let status = inner.status.unwrap_or_default();
                inner.shown = Some(status);
                status
            };
            tokio::task::spawn_blocking(move || handover.start(prepared, status, cancel))
                .await
                .map_err(|_| Handoff::Lost(P::SESSION_FAILED.to_owned()))?
                .map_err(Handoff::Lost)
        };
        self.launch_before(handoff, before_begin).await
    }

    /// Launches as `launch_before` does, with nothing before the start gate.
    #[cfg(test)]
    async fn launch<F>(self, handoff: F) -> Result<(), Failure>
    where
        F: Future<Output = Result<Arc<dyn Supervised<P>>, Handoff>> + Send + 'static,
    {
        self.launch_before(handoff, Box::new(|| {})).await
    }

    /// Hands the admitted game to its supervisor with `handoff`, runs
    /// `before_begin` before the supervisor's start gate opens, then follows
    /// the session until it is finished. The handoff and the observation run
    /// to their end even when the caller stops waiting, since the supervisor
    /// may accept the game at any point: only its session releases the
    /// Reservation then.
    async fn launch_before<F>(self, handoff: F, before_begin: BeforeBegin) -> Result<(), Failure>
    where
        F: Future<Output = Result<Arc<dyn Supervised<P>>, Handoff>> + Send + 'static,
    {
        if self.cancelled() {
            self.shared.finish(&self.operation);
            return Err(Failure::Cancelled);
        }
        self.launched.store(true, Ordering::Release);
        let (shared, operation) = (self.shared.clone(), self.operation.clone());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let handoff = handoff.await;
            std::thread::spawn(move || {
                let outcome = match handoff {
                    Ok(client) => shared.observe(&operation, client, Some(before_begin)),
                    Err(Handoff::Refused(failure)) => Err(failure),
                    Err(Handoff::Lost(failure)) => {
                        shared.update(|inner| {
                            if inner.current(&operation) {
                                inner.recovering = true;
                                if let Some(active) = &mut inner.active {
                                    active.reconnecting = true;
                                }
                            }
                        });
                        match shared.recover(&operation) {
                            Ok(true) => Ok(()),
                            Ok(false) if operation.cancelled() => Err(Failure::Cancelled),
                            Ok(false) => Err(Failure::Failed(failure)),
                            Err(failure) => Err(failure),
                        }
                    }
                };
                shared.finish(&operation);
                let _ = sender.send(outcome);
            });
        });
        receiver
            .await
            .unwrap_or(Err(Failure::Failed(P::SESSION_FAILED.to_owned())))
    }

    /// Follows a supervisor that already accepted the game.
    #[cfg(test)]
    pub(crate) async fn follow(self, client: Arc<dyn Supervised<P>>) -> Result<(), Failure> {
        self.launch(async move { Ok(client) }).await
    }
}

impl<P: Storefront> Drop for Admission<P> {
    /// An admission never handed off releases its Reservation.
    fn drop(&mut self) {
        if !self.launched.load(Ordering::Acquire) {
            self.shared.finish(&self.operation);
        }
    }
}

#[cfg(test)]
mod tests;
