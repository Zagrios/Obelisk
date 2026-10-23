//! The supervisor side of a Game session, shared by every storefront. A
//! supervisor process hosts exactly one Game session, from the client's
//! handoff until the game has really exited and its Cloud saves are settled.
//! A storefront supplies its `Launch`; the order of the steps is the core's.
use super::*;
use crate::supervisor::architecture::Architecture;
use obelisk_core::transfer::CancelToken;
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
/// How the player's controls reach a running game.
pub(crate) type Control<P> =
    Box<dyn Fn(<P as Provider>::Control) -> Result<(), Error> + Send + Sync>;
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
/// How long a game asked to stop gets to quit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// What the client handed over, as the core needs it.
pub(crate) struct Opening<P: Provider> {
    pub root: PathBuf,
    pub id: String,
    pub account_id: P::Account,
    pub store: P::Store,
    /// The directory that bundles the Obelisk Rosetta helper app.
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

/// A launch's Cloud sync, which the core drives around the game: prepared
/// before it, started inside the admission lock, finished once the game
/// exits, or abandoned when it never starts.
pub(crate) trait LaunchCloud<E>: Send {
    /// `alive` answers for a previous session whose Cloud game guard remains.
    fn prepare<'a>(&'a mut self, alive: &'a SessionAlive<'a>) -> Operation<'a, Result<(), E>>;
    fn started(&mut self) -> Result<(), E>;
    /// Settles the exit save. Never fails the session: its result lives in
    /// the Cloud status. `confirmed` is false when exec was never confirmed;
    /// the exit save is then only captured and kept pending.
    fn finished(&mut self, confirmed: bool) -> Operation<'_, ()>;
    /// Releases the Cloud work of a launch abandoned before spawn.
    fn aborted(&mut self);
}
impl<E, C: LaunchCloud<E> + ?Sized> LaunchCloud<E> for Box<C> {
    fn prepare<'a>(&'a mut self, alive: &'a SessionAlive<'a>) -> Operation<'a, Result<(), E>> {
        (**self).prepare(alive)
    }
    fn started(&mut self) -> Result<(), E> {
        (**self).started()
    }
    fn finished(&mut self, confirmed: bool) -> Operation<'_, ()> {
        (**self).finished(confirmed)
    }
    fn aborted(&mut self) {
        (**self).aborted();
    }
}
impl<S, E> LaunchCloud<E> for crate::cloud::sync::CloudSync<S>
where
    S: crate::cloud::sync::Storefront + Send,
    E: From<S::Failure>,
    Self: Send,
{
    fn prepare<'a>(&'a mut self, alive: &'a SessionAlive<'a>) -> Operation<'a, Result<(), E>> {
        Box::pin(async move { Ok(crate::cloud::sync::CloudSync::prepare(self, alive).await?) })
    }
    fn started(&mut self) -> Result<(), E> {
        Ok(crate::cloud::sync::CloudSync::started(self)?)
    }
    fn finished(&mut self, confirmed: bool) -> Operation<'_, ()> {
        Box::pin(crate::cloud::sync::CloudSync::finished(self, confirmed))
    }
    fn aborted(&mut self) {
        crate::cloud::sync::CloudSync::aborted(self);
    }
}

/// A storefront's launch. The core calls it in this order: authentication,
/// the Cloud sync it opens and prepares, target and architecture, the Rosetta
/// gate (then the target again, once Rosetta was just installed), host
/// preparation, then exec admission, where the Cloud sync starts.
#[allow(async_fn_in_trait)]
pub(crate) trait Launch: Sized + 'static {
    type P: Provider;
    /// What preparation leaves running until the game exits, such as
    /// Steam's native host.
    type Host: Send + Sync + 'static;
    /// The launch's Cloud sync.
    type Cloud: LaunchCloud<Failure<Self>>;
    /// Only a launch that waits for its UI's Begin sets this.
    const BEGIN: bool = false;

    fn open(input: &mut dyn Read) -> Result<(Opening<Self::P>, Self), Failure<Self>>;
    /// Stops the storefront's own preparation outside Cloud, such as Steam's
    /// native host, when the launch is cancelled or revoked before spawn.
    fn interrupt(&self) -> Box<dyn Fn() + Send + Sync> {
        Box::new(|| {})
    }
    /// How a Windows game runs, whose prefix stopping the game empties too.
    fn wine(&self) -> Option<crate::wine::Config> {
        None
    }
    async fn authenticate(
        &mut self,
        _session: &Arc<Session<Self::P>>,
    ) -> Result<(), Failure<Self>> {
        Ok(())
    }
    /// Opens the launch's Cloud sync, if its game has one.
    async fn cloud(
        &mut self,
        session: &Arc<Session<Self::P>>,
    ) -> Result<Option<Self::Cloud>, Failure<Self>>;
    /// Called again after Rosetta was just installed: short-lived launch
    /// credentials may need refreshing.
    async fn target(
        &mut self,
        session: &Arc<Session<Self::P>>,
    ) -> Result<(Target, Option<Architecture>), Failure<Self>>;
    /// `cloud` is the prepared Cloud sync.
    async fn prepare_host(
        &mut self,
        session: &Arc<Session<Self::P>>,
        target: &Target,
        architecture: Option<Architecture>,
        cloud: Option<&Self::Cloud>,
    ) -> Result<Prepared<Self::Host>, Failure<Self>>;
    /// The game runs. Returns how its presence is published.
    fn running(&self, host: &Arc<Self::Host>, pid: u32) -> Box<dyn Fn(bool) + Send + Sync>;
    /// The game runs: how the player's controls reach it, if the storefront
    /// offers any. They stop once the game exits.
    fn control(&self, _host: &Arc<Self::Host>) -> Option<Control<Self::P>> {
        None
    }
    /// Reports host changes while the game runs, until the host stops.
    fn watch(
        &self,
        _host: &Arc<Self::Host>,
        _session: &Arc<Session<Self::P>>,
    ) -> Option<std::thread::JoinHandle<()>> {
        None
    }
    fn stopped(&self, host: &Self::Host, exit: Option<i32>);
    /// Ends the launch's own network connections, whatever the outcome.
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
    /// Cloud maintenance in Obelisk: the UI itself waits for the answer.
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
    /// The spawned helper that becomes the game, until it is reaped.
    game: Option<u32>,
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
    wine: OnceLock<Option<crate::wine::Config>>,
    /// Set before spawn, for the player's Quit and Force Quit.
    launched: OnceLock<super::quit::Game>,
    /// A Quit, or a Force Quit, is underway: the same request again adds
    /// nothing to it.
    quitting: AtomicBool,
    forcing: AtomicBool,
    /// The player quit or stopped the game from Obelisk, which its exit
    /// then reports.
    ended_by_obelisk: AtomicBool,
    /// The running game's controls, from its start until its exit.
    control: Mutex<Option<Control<P>>>,
    /// The Force Quit underway, which the session waits for before it ends:
    /// it kills what outlives the game, after the game.
    forced: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl<P: Provider> Session<P> {
    /// Obelisk is ending: macOS asks every process it started to quit, the
    /// Wine server of a Windows game included. The game stops as on a quit,
    /// and the Windows processes left frozen without their server go too; the
    /// session then settles its Cloud saves.
    fn terminated(&self) {
        if !self.state.lock().unwrap().spawned {
            self.stop_preparation();
            self.notify();
            return;
        }
        self.stop_game();
        if let Some(wine) = self.wine.get().cloned().flatten() {
            crate::wine::end_orphans(&wine.runtime);
        }
    }

    /// Ends the running game: it gets `STOP_GRACE` to quit before it is
    /// killed. A Windows game's prefix is emptied, which also ends the
    /// processes Wine started apart from it.
    fn stop_game(&self) {
        let Some(pid) = self.state.lock().unwrap().game else {
            return;
        };
        self.ended_by_obelisk.store(true, Ordering::Release);
        let wine = self.wine.get().cloned().flatten();
        self.signal(pid, "-TERM");
        if let Some(wine) = &wine {
            crate::wine::stop(wine);
        }
        self.wait_reaped(pid, STOP_GRACE);
        self.signal(pid, "-KILL");
    }

    /// The player's Quit: the game is asked to close as its player closing
    /// its windows would, and nothing is killed. A Windows game is asked
    /// through Wine; a Mac game gets SIGTERM, which it may handle to quit
    /// cleanly, the closest the supervisor has to the app's own Quit.
    fn quit_game(&self) {
        let Some(pid) = self.state.lock().unwrap().game else {
            return;
        };
        self.ended_by_obelisk.store(true, Ordering::Release);
        match (self.wine.get().cloned().flatten(), self.launched.get()) {
            (Some(wine), Some(game)) => {
                crate::wine::quit(&wine, &game.environment, game.program.as_deref(), pid)
            }
            _ => self.signal(pid, "-TERM"),
        }
    }

    /// The player's Force Quit: the game and what it started are killed at
    /// once, then whatever still runs for its installation or its prefix.
    /// The session then ends as after any exit, Cloud saves included.
    fn force_quit_game(&self) {
        let wine = self.wine.get().cloned().flatten();
        let pid = self.state.lock().unwrap().game;
        self.ended_by_obelisk.store(true, Ordering::Release);
        if let Some(pid) = pid {
            // An engine's launcher stops its window, render and input servers
            // itself: they are not frozen with the game, and SIGTERM lets the
            // input server give each gamepad back its own behaviour.
            if let Some(grace) = wine.as_ref().and_then(crate::wine::Config::launcher_grace) {
                self.signal(pid, "-TERM");
                self.wait_reaped(pid, grace);
            }
            let started = super::quit::started_by(pid);
            self.signal(pid, "-KILL");
            super::quit::kill(&started);
        }
        if let Some(wine) = &wine {
            crate::wine::kill(wine);
        }
        if let (Some(game), Ok(own)) = (self.launched.get(), std::env::current_exe()) {
            super::quit::sweep(&super::quit::Scope::of(game, wine.as_ref(), pid, &own));
        }
    }

    /// Waits at most `grace` for the game's helper `pid` to be reaped.
    fn wait_reaped(&self, pid: u32, grace: Duration) {
        let deadline = Instant::now() + grace;
        while self.state.lock().unwrap().game == Some(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Waits for the Force Quit underway, if any: the supervisor would
    /// otherwise exit before it killed what the game left running.
    async fn forced(&self) {
        let Some(forcing) = self.forced.lock().unwrap().take() else {
            return;
        };
        while !forcing.is_finished() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = forcing.join();
    }

    /// Whether a Force Quit, or else a Quit, is underway.
    fn underway(&self, force: bool) -> &AtomicBool {
        if force { &self.forcing } else { &self.quitting }
    }

    /// Signals the game's helper while the supervisor has not reaped it.
    fn signal(&self, pid: u32, signal: &str) {
        // The helper stays unreaped until the supervisor waits for it, so its
        // pid cannot name another process meanwhile.
        if self.state.lock().unwrap().game == Some(pid) {
            let _ = std::process::Command::new("/bin/kill")
                .args([signal, &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
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
        .prefix("obelisk-session-")
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
        played: None,
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
            game: None,
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
        wine: OnceLock::new(),
        launched: OnceLock::new(),
        quitting: AtomicBool::new(false),
        forcing: AtomicBool::new(false),
        ended_by_obelisk: AtomicBool::new(false),
        control: Mutex::new(None),
        forced: Mutex::new(None),
    });
    let _ = session.interrupt.set(launch.interrupt());
    let _ = session.wine.set(launch.wine());
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
        // Catching SIGTERM keeps this supervisor alive when Obelisk ends,
        // long enough to stop its game and settle its Cloud saves.
        let terminated = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .ok()
            .map(|mut terminate| {
                let session = session.clone();
                tokio::spawn(async move {
                    if terminate.recv().await.is_some() {
                        std::thread::spawn(move || session.terminated());
                    }
                })
            });
        // Kept even if preparation fails, so an abandoned launch still
        // releases its Cloud work.
        let mut cloud = None;
        let outcome = run(&mut launch, &mut cloud, &session, inspect, &alive).await;
        if let Some(terminated) = terminated {
            terminated.abort();
        }
        if outcome.is_err()
            && !session.state.lock().unwrap().spawned
            && let Some(cloud) = &mut cloud
        {
            cloud.aborted();
        }
        // A Force Quit asked once the game was gone still ends what it
        // left running.
        session.forced().await;
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
    cloud: &mut Option<L::Cloud>,
    session: &Arc<Session<L::P>>,
    inspect: Inspect,
    alive: &SessionAlive<'_>,
) -> Result<(), Failure<L>> {
    use super::child::{Execution, confirm_execution, wait_for_exit, wait_identity};

    if L::BEGIN {
        session.begun().await?;
    }
    launch.authenticate(session).await?;
    if let Some(opened) = launch.cloud(session).await? {
        cloud.insert(opened).prepare(alive).await?;
    }
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
    let mut prepared = launch
        .prepare_host(session, &target, architecture, cloud.as_ref())
        .await?;
    let wine = launch.wine();
    // A game in Wine for macOS reads the Mac's gamepads through the input
    // service while it runs.
    let input = wine
        .as_ref()
        .and_then(|wine| {
            let mapping = target.environment.get(crate::input::MAPPING);
            crate::input::Service::start(wine, mapping.map(String::as_str))
        })
        .map(|(service, socket)| {
            prepared.environment.insert(
                crate::input::SOCKET.to_owned(),
                socket.to_string_lossy().into_owned(),
            );
            service
        });
    // Metal reports the game's frames to its log, and shows them over the
    // game only to a player who asked in its launch settings.
    let shown = wine.is_some_and(|wine| wine.settings.performance_hud);
    prepared.environment.extend(
        crate::metal_hud::environment(shown)
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
    );
    let _ = session.launched.set(super::quit::Game {
        environment: target.environment.clone(),
        installation: target.installation.clone(),
        program: target.program.clone(),
    });
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
        if let Some(cloud) = cloud.as_mut() {
            cloud.started()?;
        }
        let game = tokio::process::Command::from(command)
            .kill_on_drop(false)
            .spawn()
            .map_err(|_| Error::StartFailed)?;
        {
            let mut state = session.state.lock().unwrap();
            state.spawned = true;
            state.game = game.id();
        }
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
                    log_header: session
                        .wine
                        .get()
                        .and_then(Option::as_ref)
                        .map(crate::wine::Config::log_header),
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
        let exit = wait_for_exit(&mut game).await.code().unwrap_or(-1);
        session.state.lock().unwrap().game = None;
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
        *session.control.lock().unwrap() = launch.control(&host);
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
    let began = Instant::now();
    let status = wait_for_exit(&mut game).await;
    let seconds = began.elapsed().as_secs();
    let exit = status.code().unwrap_or(-1);
    drop(input);
    session.control.lock().unwrap().take();
    session.state.lock().unwrap().game = None;
    session.presence.stop();
    launch.stopped(&host, Some(exit));
    if let Some(watching) = watching {
        let _ = watching.join();
    }
    // What a Force Quit kills after the game is gone before its saves are
    // read.
    session.forced().await;
    let played = if started {
        let log = target.log.clone();
        let frames = tokio::task::spawn_blocking(move || {
            log.map(|log| crate::metal_hud::read(&log))
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        let wine = session.wine.get().cloned().flatten();
        Some(Played {
            seconds,
            frames,
            exit_code: status.code(),
            exit_signal: std::os::unix::process::ExitStatusExt::signal(&status),
            ended_by_obelisk: session.ended_by_obelisk.load(Ordering::Acquire),
            graphics: wine.as_ref().map(|wine| wine.settings.graphics),
            voile: wine
                .as_ref()
                .and_then(|wine| crate::voile::release_of(&wine.runtime)),
            engine: wine.map(|wine| wine.settings.engine),
        })
    } else {
        None
    };
    // After exit every write is best effort: finalization always runs, and a
    // success never turns into a failure.
    let _ = session.update(|r| {
        r.phase = Phase::Finalizing;
        r.exit_code = Some(exit);
        r.played = played;
        r.game = None;
        r.starting = None;
    });
    if let Some(cloud) = cloud.as_mut() {
        cloud.finished(started).await;
    }
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

fn respond<L: Launch>(mut stream: UnixStream, session: &Arc<Session<L::P>>) -> Result<(), Error> {
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
        Command::Stop { id } => {
            let _admission = session.admission.lock().unwrap();
            let state = session.state.lock().unwrap();
            if !same(&state, &id, None) {
                return Err(Error::InvalidSession);
            }
            let spawned = state.spawned;
            drop(state);
            if spawned {
                // The reply does not wait for the game to go.
                let session = session.clone();
                std::thread::spawn(move || session.stop_game());
            } else {
                session.stop_preparation();
                session.notify();
            }
        }
        Command::Quit { id } => end(session, &id, false)?,
        Command::ForceQuit { id } => end(session, &id, true)?,
        Command::Control {
            id,
            account_id,
            control,
        } => {
            if !same(&session.state.lock().unwrap(), &id, Some(&account_id)) {
                return Err(Error::InvalidSession);
            }
            let controls = session.control.lock().unwrap();
            controls.as_ref().ok_or(Error::InvalidSession)?(control)?;
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

/// The player's Quit, or Force Quit. Before spawn either one cancels
/// preparation. After it, the work runs apart: the reply never waits for the
/// game, and a repeated request joins the one underway.
fn end<P: Provider>(session: &Arc<Session<P>>, id: &str, force: bool) -> Result<(), Error> {
    let _admission = session.admission.lock().unwrap();
    let state = session.state.lock().unwrap();
    if state.record.id != id {
        return Err(Error::InvalidSession);
    }
    let spawned = state.spawned;
    drop(state);
    if !spawned {
        session.stop_preparation();
        session.notify();
        return Ok(());
    }
    if !session.underway(force).swap(true, Ordering::AcqRel) {
        // Held from spawn on: the session waits for a Force Quit only once
        // it can find it.
        let mut forced = session.forced.lock().unwrap();
        let ending = session.clone();
        let thread = std::thread::spawn(move || {
            if force {
                ending.force_quit_game();
            } else {
                ending.quit_game();
            }
            ending.underway(force).store(false, Ordering::Release);
        });
        if force && let Some(previous) = forced.replace(thread) {
            // A previous Force Quit is over, or about to be.
            let _ = previous.join();
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
