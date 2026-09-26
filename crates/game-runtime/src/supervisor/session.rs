//! The supervisor side of a Game session, shared by every storefront. A
//! supervisor process hosts exactly one Game session, from the client's
//! handoff until the game has really exited and its Cloud saves are settled.
//! A storefront supplies its `Launch`; the order of the steps is the core's.
use super::*;
use crate::supervisor::architecture::Architecture;
use sailover_core::transfer::CancelToken;
use std::{
    io::{Read, Write},
    os::fd::OwnedFd,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Command as Process, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// The supervisor's own process inspection. `Ok(None)` proves absence; an
/// error is uncertainty.
pub type Inspect = fn(u32) -> Result<Option<Identity>, Error>;

type Failure<L> = <<L as Launch>::P as Provider>::Error;
/// A storefront's own async step, boxed so its traits stay object safe.
pub(crate) type Operation<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// A Cloud question is abandoned this long after it was published.
const QUESTION_DEADLINE: Duration = Duration::from_secs(600);
/// A Cloud question is abandoned once no observer has asked for this long.
const OBSERVER_DEADLINE: Duration = Duration::from_secs(120);
/// A supervisor abandons a launch that its UI never begins.
pub(crate) const BEGIN_DEADLINE: Duration = Duration::from_secs(120);
/// Concurrent control connections; more are dropped unanswered.
const WORKERS: usize = 16;

/// What the client handed over, as the core needs it.
pub(crate) struct Opening<P: Provider> {
    pub root: PathBuf,
    pub id: String,
    pub account_id: P::Account,
    pub store: P::Store,
    /// The directory that bundles the SailOver Rosetta helper app.
    pub helpers: PathBuf,
}

/// A prepared game, ready for exec admission.
pub(crate) struct Prepared<H> {
    pub host: H,
    /// The exec helper and its subcommand.
    pub helper: PathBuf,
    pub subcommand: &'static str,
    /// The game's environment. It is set on the helper and inherited through
    /// exec, so it reaches the game verbatim.
    pub environment: std::collections::HashMap<String, String>,
}

/// A storefront's launch. The core calls it in this order: authentication,
/// Cloud preparation, target and architecture, the Rosetta gate (then the
/// target again, once Rosetta was just installed), host preparation, then
/// `admitted` inside the admission lock and exec admission.
#[allow(async_fn_in_trait)]
pub(crate) trait Launch: Sized + 'static {
    type P: Provider;
    /// What preparation leaves running until the game exits, such as
    /// Steam's native host.
    type Host: Send + Sync + 'static;
    /// Only a launch that waits for its UI's Begin sets this.
    const BEGIN: bool = false;

    fn open(input: &mut dyn Read) -> Result<(Opening<Self::P>, Self), Failure<Self>>;
    /// Stops the storefront's own preparation outside Cloud, such as Steam's
    /// native host, when the launch is cancelled or revoked before spawn.
    fn interrupt(&self) -> Box<dyn Fn() + Send + Sync> {
        Box::new(|| {})
    }
    async fn authenticate(
        &mut self,
        _session: &Arc<Session<Self::P>>,
    ) -> Result<(), Failure<Self>> {
        Ok(())
    }
    /// `alive` answers for a previous session whose Cloud game guard remains.
    async fn prepare_cloud(
        &mut self,
        session: &Arc<Session<Self::P>>,
        alive: &SessionAlive<'_>,
    ) -> Result<(), Failure<Self>>;
    /// Called again after Rosetta was just installed: short-lived launch
    /// credentials may need refreshing.
    async fn target(
        &mut self,
        session: &Arc<Session<Self::P>>,
    ) -> Result<(Target, Option<Architecture>), Failure<Self>>;
    async fn prepare_host(
        &mut self,
        session: &Arc<Session<Self::P>>,
        target: &Target,
        architecture: Option<Architecture>,
    ) -> Result<Prepared<Self::Host>, Failure<Self>>;
    /// Inside the admission lock, after the cancellation and revocation check.
    fn admitted(&mut self) -> Result<(), Failure<Self>>;
    /// The game runs. Returns how its presence is published.
    fn running(&self, host: &Arc<Self::Host>, pid: u32) -> Box<dyn Fn(bool) + Send + Sync>;
    /// Reports host changes while the game runs, until the host stops.
    fn watch(
        &self,
        _host: &Arc<Self::Host>,
        _session: &Arc<Session<Self::P>>,
    ) -> Option<std::thread::JoinHandle<()>> {
        None
    }
    fn stopped(&self, host: &Self::Host, exit: Option<i32>);
    /// Settles Cloud saves after the game exits. Never fails the session: its
    /// result lives in the Cloud status. `confirmed` is false when exec was
    /// never confirmed; the exit save is then only captured and kept pending.
    async fn finish(&mut self, session: &Arc<Session<Self::P>>, confirmed: bool);
    /// Releases Cloud work of a launch abandoned before spawn.
    fn abort(&mut self, session: &Arc<Session<Self::P>>);
    /// Ends the launch's own network session, whatever the outcome.
    async fn close(&mut self) {}
    /// Storefront facts of a terminal record.
    fn terminal(_record: &mut Record<Self::P>) {}
    /// Whether Rosetta translates Intel games, asked of the bundled helper.
    fn rosetta(helper: &Path) -> Result<bool, Error> {
        crate::supervisor::architecture::rosetta_available(helper)
    }
}

/// The session's decisions: one counter for Cloud questions and the Rosetta
/// requirement, and the deadlines after which a Cloud question is abandoned.
pub struct Questions {
    next: AtomicU64,
    /// When an observer last asked. Only a supervised session has one.
    observed: Option<Mutex<Instant>>,
}
impl Questions {
    /// Cloud maintenance in SailOver: the UI itself waits for the answer.
    pub fn in_app() -> Self {
        Self {
            next: AtomicU64::new(0),
            observed: None,
        }
    }
    fn supervised() -> Self {
        Self {
            next: AtomicU64::new(0),
            observed: Some(Mutex::new(Instant::now())),
        }
    }
    pub fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::AcqRel) + 1
    }
    fn observe(&self) {
        if let Some(observed) = &self.observed {
            *observed.lock().unwrap() = Instant::now();
        }
    }
    /// A question published at `published` has nobody left to answer it.
    pub fn abandoned(&self, published: Instant) -> bool {
        published.elapsed() >= QUESTION_DEADLINE
            || self
                .observed
                .as_ref()
                .is_some_and(|observed| observed.lock().unwrap().elapsed() >= OBSERVER_DEADLINE)
    }
}

/// Presence goes Active once, after exec is confirmed, and stops for good:
/// a revocation before exec prevents any later publication.
struct Presence(Mutex<PresenceState>);
enum PresenceState {
    Pending,
    Active(Box<dyn Fn(bool) + Send + Sync>),
    Stopped,
}
impl Presence {
    fn start(&self, publish: Box<dyn Fn(bool) + Send + Sync>) {
        let mut state = self.0.lock().unwrap();
        if matches!(*state, PresenceState::Pending) {
            publish(true);
            *state = PresenceState::Active(publish);
        }
    }
    fn stop(&self) {
        let mut state = self.0.lock().unwrap();
        if let PresenceState::Active(publish) = &*state {
            publish(false);
        }
        *state = PresenceState::Stopped;
    }
}

struct State<P: Provider> {
    record: Record<P>,
    spawned: bool,
    begun: bool,
    revoked: bool,
    /// A journal write failed before spawn: the launch fails closed.
    unsaved: bool,
}

pub(crate) struct Session<P: Provider> {
    state: Mutex<State<P>>,
    changed: Condvar,
    /// Spawn, cancellation, revocation, Rosetta publication and the Running
    /// publication linearize here. Take it before `state`, never after.
    admission: Mutex<()>,
    cancel: CancelToken,
    root: PathBuf,
    questions: Arc<Questions>,
    presence: Presence,
    rosetta: crate::supervisor::rosetta::Gate,
    /// The session's Cloud sync publishes into the record through it.
    cloud: Arc<crate::cloud::Control>,
    interrupt: OnceLock<Box<dyn Fn() + Send + Sync>>,
}
impl<P: Provider> Session<P> {
    pub(crate) fn record(&self) -> Record<P> {
        self.state.lock().unwrap().record.clone()
    }
    pub(crate) fn cloud(&self) -> &Arc<crate::cloud::Control> {
        &self.cloud
    }
    /// Wakes blocking observers and the Rosetta wait.
    pub(crate) fn notify(&self) {
        self.changed.notify_all();
        self.rosetta.wake();
    }
    /// Changes the record and journals it. Observers always see the change;
    /// a failed write before spawn makes the launch fail closed.
    pub(crate) fn update(&self, change: impl FnOnce(&mut Record<P>)) -> Result<(), Error> {
        let mut state = self.state.lock().unwrap();
        change(&mut state.record);
        state.record.revision += 1;
        let written = journal::write(&self.root, &state.record);
        if written.is_err() && !state.spawned {
            state.unsaved = true;
        }
        drop(state);
        self.notify();
        written
    }
    /// Whether preparation may go on.
    pub(crate) fn check(&self) -> Result<(), Error> {
        let state = self.state.lock().unwrap();
        if self.cancel.is_cancelled() || state.revoked {
            Err(Error::Cancelled)
        } else if state.unsaved {
            Err(Error::Storage)
        } else {
            Ok(())
        }
    }
    async fn begun(&self) -> Result<(), Error> {
        let deadline = Instant::now() + BEGIN_DEADLINE;
        loop {
            self.check()?;
            if self.state.lock().unwrap().begun {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Cancelled);
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(25)) => (),
                _ = self.cancel.cancelled() => return Err(Error::Cancelled),
            }
        }
    }

    /// Returns true when Rosetta was just installed.
    async fn wait_for_rosetta(&self) -> Result<bool, Error> {
        self.rosetta
            .wait(
                || {
                    let _admission = self.admission.lock().unwrap();
                    self.check()?;
                    let decision_id = self.questions.next();
                    self.update(|r| r.rosetta = Some(RosettaRequest { decision_id }))
                },
                || {
                    self.check()?;
                    Ok(self.state.lock().unwrap().record.rosetta.is_some())
                },
            )
            .await
    }

    /// Stops preparation before spawn: the core, the storefront and Cloud.
    fn stop_preparation(&self) {
        self.cancel.cancel();
        if let Some(interrupt) = self.interrupt.get() {
            interrupt();
        }
        self.cloud.cancel();
    }
}

/// Runs one supervisor. It returns an error, and the process exits non-zero,
/// only when the session ends Failed for a reason other than cancellation.
pub(crate) fn serve<L: Launch>(
    mut input: impl Read,
    mut output: impl Write,
    inspect: Inspect,
) -> Result<(), Failure<L>> {
    // The executor exists before any durable write.
    let executor = crate::executor().map_err(Error::from)?;
    let (opening, mut launch) = L::open(&mut input)?;
    let owner = OwnerLock::<L::P>::take(&opening.root)?;
    // No other supervisor of this storefront lives, but its game may.
    if let Some(previous) = journal::current::<Record<L::P>>(&opening.root)?
        && !previous.finished()
        && previous.liveness(inspect)? != ownership::Liveness::Dead
    {
        return Err(Error::Busy.into());
    }
    // A short socket path avoids macOS's 104-byte sockaddr_un limit, including
    // when the account home is long. tempfile creates an unpredictable 0700 dir.
    let socket_directory = tempfile::Builder::new()
        .prefix("sailover-session-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .map_err(Error::from)?;
    let socket = socket_directory.path().join("control");
    let listener = UnixListener::bind(&socket).map_err(Error::from)?;
    let record = Record::<L::P> {
        version: journal::VERSION,
        id: opening.id,
        account_id: opening.account_id,
        store: opening.store,
        phase: Phase::Preparing,
        cloud: Default::default(),
        exit_code: None,
        error: None,
        failure_reason: None,
        revision: 1,
        rosetta: None,
        reconnecting: false,
        supervisor: inspect(std::process::id())?.ok_or(Error::StartFailed)?,
        game: None,
        starting: None,
        socket: socket.clone(),
    };
    journal::write(&opening.root, &record)?;
    let questions = Arc::new(Questions::supervised());
    let session = Arc::new_cyclic(|session: &std::sync::Weak<Session<L::P>>| Session {
        state: Mutex::new(State {
            record,
            spawned: false,
            begun: !L::BEGIN,
            revoked: false,
            unsaved: false,
        }),
        changed: Condvar::new(),
        admission: Mutex::new(()),
        cancel: CancelToken::new(),
        root: opening.root,
        questions: questions.clone(),
        presence: Presence(Mutex::new(PresenceState::Pending)),
        rosetta: crate::supervisor::rosetta::Gate::new(
            crate::supervisor::rosetta::helper(&opening.helpers),
            L::rosetta,
        ),
        cloud: Arc::new(crate::cloud::Control::with_questions(questions, {
            let session = session.clone();
            // Observers read the session itself, so a failed journal write
            // hides nothing: the core fails closed before spawn.
            move |status| {
                if let Some(session) = session.upgrade() {
                    let _ = session.update(|record| record.cloud = status);
                }
            }
        })),
        interrupt: OnceLock::new(),
    });
    let _ = session.interrupt.set(launch.interrupt());
    let finished = Arc::new(AtomicBool::new(false));
    let listening = {
        let session = session.clone();
        let finished = finished.clone();
        std::thread::spawn(move || listen::<L>(listener, session, finished))
    };
    // Journal, then listener, then acknowledgement. Losing the parent here
    // never revokes an accepted launch or ownership of its game.
    let _ = storage::send(&mut output, &session.record());
    drop(output);
    let alive = |id: &str| owner.session_alive(id, inspect);
    let outcome = executor.block_on(async {
        let outcome = run(&mut launch, &session, inspect, &alive).await;
        if outcome.is_err() && !session.state.lock().unwrap().spawned {
            launch.abort(&session);
        }
        launch.close().await;
        outcome
    });
    session.presence.stop();
    let result = match outcome {
        // An exec that never became the game is a failed launch.
        Ok(()) if session.record().phase == Phase::Failed => Err(Error::StartFailed.into()),
        Ok(()) => Ok(()),
        Err(failure) => {
            let cancelled = <L::P as Provider>::cancelled(&failure);
            let _ = session.update(|r| {
                r.phase = Phase::Failed;
                r.failure_reason = Some(if cancelled {
                    FailureReason::Cancelled
                } else {
                    FailureReason::Failed
                });
                r.error = Some(failure.to_string());
                r.rosetta = None;
                r.game = None;
                r.starting = None;
                L::terminal(r);
            });
            if cancelled { Ok(()) } else { Err(failure) }
        }
    };
    finished.store(true, Ordering::Release);
    // Wake the blocking accept even when no UI is connected. If the socket
    // was removed, this dedicated process exits next and closes its listener.
    if UnixStream::connect(&socket).is_ok() || listening.is_finished() {
        let _ = listening.join();
    }
    // The socket directory and owner lock go only after game and host
    // teardown, and after the executor's bounded shutdown.
    drop(executor);
    drop(socket_directory);
    drop(owner);
    result
}

async fn run<L: Launch>(
    launch: &mut L,
    session: &Arc<Session<L::P>>,
    inspect: Inspect,
    alive: &SessionAlive<'_>,
) -> Result<(), Failure<L>> {
    use super::child::{Execution, confirm_execution, wait_for_exit, wait_identity};

    if L::BEGIN {
        session.begun().await?;
    }
    launch.authenticate(session).await?;
    launch.prepare_cloud(session, alive).await?;
    session.check()?;
    let mut refreshed = false;
    let (target, architecture) = loop {
        let (target, architecture) = launch.target(session).await?;
        session.check()?;
        if architecture == Some(Architecture::Intel) && cfg!(target_arch = "aarch64") {
            if refreshed {
                // A second interruption cannot loop over fresh launch
                // credentials indefinitely. Fail if availability changed.
                if !session.rosetta.probe().await? {
                    return Err(Error::RosettaUnavailable.into());
                }
            } else if session.wait_for_rosetta().await? {
                refreshed = true;
                continue;
            }
        }
        break (target, architecture);
    };
    session.check()?;
    let prepared = launch.prepare_host(session, &target, architecture).await?;
    let host = Arc::new(prepared.host);
    let mut command = Process::new(&prepared.helper);
    let (mut pipe, child_pipe) = UnixStream::pair().map_err(Error::from)?;
    pipe.set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(Error::from)?;
    command
        .arg(prepared.subcommand)
        .env_clear()
        .envs(&prepared.environment)
        .stdin(Stdio::from(OwnedFd::from(child_pipe)))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let expected = target
        .executable
        .canonicalize()
        .map_err(|_| Error::StartFailed)?;
    let (mut game, admitted) = {
        // Cancellation and spawn have one linearization point. Cancellation
        // after it never ends a game, including sign-out and UI loss.
        let _admission = session.admission.lock().unwrap();
        session.check()?;
        launch.admitted()?;
        let game = tokio::process::Command::from(command)
            .kill_on_drop(false)
            .spawn()
            .map_err(|_| Error::StartFailed)?;
        session.state.lock().unwrap().spawned = true;
        let admitted = (|| {
            // Read now: a game reaped during confirmation no longer has one.
            let pid = game.id().ok_or(Error::StartFailed)?;
            let identity = inspect(pid)?.ok_or(Error::StartFailed)?;
            // The helper cannot exec before its identity and destination are
            // durable: this write must succeed.
            session.update(|r| {
                r.starting = Some(StartingGame::Process {
                    identity,
                    executable: expected.clone(),
                })
            })?;
            exec::send(
                &mut pipe,
                &exec::Admission {
                    executable: target.executable.clone(),
                    arguments: target.arguments.clone(),
                    working_directory: target.working_directory.clone(),
                    log: target.log.clone(),
                },
            )
            .map_err(|_| Error::StartFailed)?;
            Ok(pid)
        })();
        (game, admitted)
    };
    drop(pipe);
    // A helper that never became the game keeps its ownership until it is
    // reaped, outside the admission lock.
    let reap = async |mut game: tokio::process::Child, error: Error| {
        let exit = wait_for_exit(&mut game).await;
        launch.stopped(&host, Some(exit));
        session.state.lock().unwrap().spawned = false;
        let _ = session.update(|r| r.starting = None);
        Err(error.into())
    };
    // Without a complete admission the helper exits without exec.
    let pid = match admitted {
        Ok(pid) => pid,
        Err(error) => return reap(game, error).await,
    };
    let inspect_game = |pid| inspect(pid).ok().flatten();
    let execution = confirm_execution(&mut game, &target.executable, inspect_game).await;
    if execution == Execution::Failed {
        return reap(game, Error::StartFailed).await;
    }
    let started = execution == Execution::Started;
    if started {
        let identity = wait_identity(&mut game, &target.executable, inspect_game).await;
        // Revocation takes this same lock: its permanent stop fences a late exec.
        let _admission = session.admission.lock().unwrap();
        session.presence.start(launch.running(&host, pid));
        let _ = session.update(|r| {
            // A path the helper could not confirm keeps its starting identity,
            // which the birth-time rule still owns.
            let starting = r
                .starting
                .take()
                .map(|StartingGame::Process { identity, .. }| identity);
            r.game = identity.or(starting);
            r.phase = Phase::Running;
        });
    }
    let watching = launch.watch(&host, session);
    let exit = wait_for_exit(&mut game).await;
    session.presence.stop();
    launch.stopped(&host, Some(exit));
    if let Some(watching) = watching {
        let _ = watching.join();
    }
    // After exit every write is best effort: finalization always runs, and a
    // success never turns into a failure.
    let _ = session.update(|r| {
        r.phase = Phase::Finalizing;
        r.exit_code = Some(exit);
        r.game = None;
        r.starting = None;
    });
    launch.finish(session, started).await;
    let _ = session.update(|r| {
        r.phase = if started {
            Phase::Finished
        } else {
            Phase::Failed
        };
        r.exit_code = Some(exit);
        if !started {
            r.failure_reason = Some(FailureReason::Failed);
            r.error = Some(<L::P as Provider>::Error::from(Error::StartFailed).to_string());
        }
        L::terminal(r);
    });
    Ok(())
}

fn listen<L: Launch>(
    listener: UnixListener,
    session: Arc<Session<L::P>>,
    finished: Arc<AtomicBool>,
) {
    let workers = Arc::new(AtomicUsize::new(0));
    while !finished.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if finished.load(Ordering::Acquire) {
                    break;
                }
                if workers.load(Ordering::Acquire) >= WORKERS {
                    continue;
                }
                workers.fetch_add(1, Ordering::AcqRel);
                let workers = workers.clone();
                let session = session.clone();
                std::thread::spawn(move || {
                    let _ = respond::<L>(stream, &session);
                    workers.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn respond<L: Launch>(mut stream: UnixStream, session: &Session<L::P>) -> Result<(), Error> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let command: Command<L::P> = storage::command(&stream)?;
    let same = |state: &State<L::P>, id: &str, account_id: Option<&<L::P as Provider>::Account>| {
        state.record.id == id
            && account_id.is_none_or(|account_id| *account_id == state.record.account_id)
    };
    match command {
        Command::Observe { id, after } => {
            let state = session.state.lock().unwrap();
            if !same(&state, &id, None) {
                return Err(Error::InvalidSession);
            }
            session.questions.observe();
            let waiting = |state: &mut State<L::P>| {
                after == Some(state.record.revision) && !state.record.finished()
            };
            let (state, _) = session
                .changed
                .wait_timeout_while(state, Duration::from_secs(15), waiting)
                .unwrap();
            let record = state.record.clone();
            drop(state);
            return storage::send(&mut stream, &record);
        }
        Command::Begin { id, account_id } => {
            if !L::BEGIN || !same(&session.state.lock().unwrap(), &id, Some(&account_id)) {
                return Err(Error::InvalidSession);
            }
            session.check()?;
            session.state.lock().unwrap().begun = true;
            session.notify();
        }
        Command::Cancel { id } => {
            let _admission = session.admission.lock().unwrap();
            let state = session.state.lock().unwrap();
            if !same(&state, &id, None) {
                return Err(Error::InvalidSession);
            }
            let preparing = !state.spawned;
            drop(state);
            if preparing {
                session.stop_preparation();
                session.notify();
            }
        }
        Command::CloudChoice {
            id,
            account_id,
            decision_id,
            choice,
        } => {
            if !same(&session.state.lock().unwrap(), &id, Some(&account_id)) {
                return Err(Error::InvalidSession);
            }
            session
                .cloud
                .answer(decision_id, choice)
                .map_err(|_| Error::InvalidSession)?;
            session.notify();
        }
        Command::Revoke { id, account_id } => {
            // Serialized with spawn: an acknowledged revocation cannot precede
            // a later launch, and presence goes offline before the reply.
            let _admission = session.admission.lock().unwrap();
            let mut state = session.state.lock().unwrap();
            if !same(&state, &id, Some(&account_id)) {
                return Err(Error::InvalidSession);
            }
            let preparing = !state.spawned;
            if preparing {
                state.revoked = true;
            }
            drop(state);
            if preparing {
                session.stop_preparation();
            }
            session.presence.stop();
            // Cloud network work stops; the exit save stays local and pending.
            session.cloud.revoke();
            session.notify();
        }
        Command::ResumeAfterRosetta {
            id,
            account_id,
            decision_id,
        } => {
            let requested = |state: &State<L::P>| {
                state.record.rosetta == Some(RosettaRequest { decision_id })
                    && !state.spawned
                    && !state.revoked
                    && !session.cancel.is_cancelled()
            };
            {
                let state = session.state.lock().unwrap();
                if !same(&state, &id, Some(&account_id)) || !requested(&state) {
                    return Err(Error::InvalidSession);
                }
            }
            let available = session.rosetta.available()?;
            let _admission = session.admission.lock().unwrap();
            if !requested(&session.state.lock().unwrap()) {
                return Err(Error::InvalidSession);
            }
            if available {
                let _ = session.update(|r| r.rosetta = None);
            }
        }
    }
    let record = session.record();
    storage::send(&mut stream, &record)
}

#[cfg(test)]
pub(crate) mod tests;
