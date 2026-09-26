//! A storefront's Game sessions across UniFFI. Rust keeps the Reservation,
//! recovery, Account revocation and the checks on every answer; Swift presents
//! the state and forwards the player's answers. A supervisor is a foreign
//! trait, so tests can stand one in for the real supervisor.
use crate::{SailOverLocations, SteamHostState};
use game_runtime::{
    cloud, epic,
    sessions::{self, Failure, Observation, Sessions, Storefront, Supervised},
    steam, supervisor,
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GamePhase {
    Preparing,
    Running,
    Finalizing,
    Finished,
    Failed,
}
impl From<supervisor::Phase> for GamePhase {
    fn from(phase: supervisor::Phase) -> Self {
        match phase {
            supervisor::Phase::Preparing => Self::Preparing,
            supervisor::Phase::Running => Self::Running,
            supervisor::Phase::Finalizing => Self::Finalizing,
            supervisor::Phase::Finished => Self::Finished,
            supervisor::Phase::Failed => Self::Failed,
        }
    }
}

/// Why a finished session failed. Swift ends a cancellation silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameFailureReason {
    Cancelled,
    Failed,
}
impl From<supervisor::FailureReason> for GameFailureReason {
    fn from(reason: supervisor::FailureReason) -> Self {
        match reason {
            supervisor::FailureReason::Cancelled => Self::Cancelled,
            supervisor::FailureReason::Failed => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudPhase {
    Idle,
    Checking,
    Ready,
    Syncing,
    Synced,
    Conflict,
    Pending,
    Disabled,
    Unsupported,
    Failed,
}
impl From<cloud::Phase> for CloudPhase {
    fn from(phase: cloud::Phase) -> Self {
        match phase {
            cloud::Phase::Idle => Self::Idle,
            cloud::Phase::Checking => Self::Checking,
            cloud::Phase::Ready => Self::Ready,
            cloud::Phase::Syncing => Self::Syncing,
            cloud::Phase::Synced => Self::Synced,
            cloud::Phase::Conflict => Self::Conflict,
            cloud::Phase::Pending => Self::Pending,
            cloud::Phase::Disabled => Self::Disabled,
            cloud::Phase::Unsupported => Self::Unsupported,
            cloud::Phase::Failed => Self::Failed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudChoice {
    Retry,
    Cancel,
    LaunchAnyway,
    Local,
    Cloud,
}
impl From<cloud::Choice> for CloudChoice {
    fn from(choice: cloud::Choice) -> Self {
        match choice {
            cloud::Choice::Retry => Self::Retry,
            cloud::Choice::Cancel => Self::Cancel,
            cloud::Choice::LaunchAnyway => Self::LaunchAnyway,
            cloud::Choice::Local => Self::Local,
            cloud::Choice::Cloud => Self::Cloud,
        }
    }
}
impl From<CloudChoice> for cloud::Choice {
    fn from(choice: CloudChoice) -> Self {
        match choice {
            CloudChoice::Retry => Self::Retry,
            CloudChoice::Cancel => Self::Cancel,
            CloudChoice::LaunchAnyway => Self::LaunchAnyway,
            CloudChoice::Local => Self::Local,
            CloudChoice::Cloud => Self::Cloud,
        }
    }
}

/// A Cloud sync's progress and the Cloud question it waits on, if any.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudStatus {
    pub phase: CloudPhase,
    /// Binds an answer to the question this status shows.
    pub decision_id: u64,
    /// Localization key of the failure the status reports.
    pub error: Option<String>,
    /// Empty unless a question waits.
    pub allowed_choices: Vec<CloudChoice>,
}
impl From<cloud::Status> for CloudStatus {
    fn from(status: cloud::Status) -> Self {
        Self {
            phase: status.phase.into(),
            decision_id: status.decision_id,
            error: status.error,
            allowed_choices: status.allowed_choices.into_iter().map(Into::into).collect(),
        }
    }
}

/// What the storefront's supervisor records about the game.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameStore {
    Steam {
        app_id: u32,
        account_id: u64,
        /// The native host that owns the game's Steam session.
        host: SteamHostState,
    },
    Epic {
        account_id: String,
        namespace: String,
        app_name: String,
    },
}

/// The account an answer or a revocation names.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameAccount {
    Steam { steam_id: u64 },
    Epic { account_id: String },
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct RosettaRequest {
    pub decision_id: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GameSnapshot {
    pub id: String,
    pub phase: GamePhase,
    /// The phase is final and the session owns no game or starting process:
    /// only then is the Reservation released.
    pub finished: bool,
    pub failure_reason: Option<GameFailureReason>,
    /// Localization key of the session's own failure.
    pub error: Option<String>,
    pub revision: u64,
    /// The control connection was lost; the game may still run.
    pub reconnecting: bool,
    pub rosetta: Option<RosettaRequest>,
    pub cloud: CloudStatus,
    pub store: GameStore,
}

/// A refused or failed session call, as a localization key.
#[derive(Clone, Debug, thiserror::Error, uniffi::Error)]
pub enum GameSessionError {
    /// The launch was cancelled before its game started.
    #[error("cancelled")]
    Cancelled,
    #[error("{reason}")]
    Failure { reason: String },
}
impl GameSessionError {
    pub(crate) fn new(error: impl std::fmt::Display) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}
impl From<Failure> for GameSessionError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Cancelled => Self::Cancelled,
            Failure::Failed(reason) => Self::Failure { reason },
        }
    }
}
impl From<GameSessionError> for String {
    fn from(error: GameSessionError) -> Self {
        error.to_string()
    }
}

/// One Game session's supervisor. The real one is Rust's; tests stand in
/// their own. Every call may block on the supervisor's control connection.
#[uniffi::export(with_foreign)]
pub trait GameSupervisor: Send + Sync {
    /// Waits for a revision newer than `after`, or returns at once.
    fn snapshot(&self, after: Option<u64>) -> Result<GameSnapshot, GameSessionError>;
    /// Cancels preparation. After spawn the game keeps running.
    fn cancel_preparation(&self);
    /// Opens the start gate of a supervisor that waits for its UI.
    fn begin(&self) -> Result<(), GameSessionError>;
    fn choose(
        &self,
        account: GameAccount,
        decision_id: u64,
        choice: CloudChoice,
    ) -> Result<(), GameSessionError>;
    fn resume_after_rosetta(
        &self,
        account: GameAccount,
        decision_id: u64,
    ) -> Result<(), GameSessionError>;
    /// Account revocation: network work stops, the game keeps running.
    fn revoke(&self, account: GameAccount) -> Result<(), GameSessionError>;
}

/// Hands an admitted game to a supervisor that code outside Rust starts.
#[uniffi::export(with_foreign)]
pub trait GameHandoff: Send + Sync {
    /// The supervisor that accepted the game. It may block.
    fn hand_off(&self) -> Result<Arc<dyn GameSupervisor>, GameSessionError>;
}

/// What must happen once a new supervisor holds the game and before its
/// start gate opens, such as saving the credentials the handoff rotated. It
/// may block.
#[uniffi::export(with_foreign)]
pub trait BeforeGameStart: Send + Sync {
    fn run(&self);
}

/// Finds the Game session a previous SailOver left running. `None` confirms
/// that none remains.
#[uniffi::export(with_foreign)]
pub trait GameRecovery: Send + Sync {
    fn recover(&self) -> Result<Option<Arc<dyn GameSupervisor>>, GameSessionError>;
}

/// What the bridge adds to each storefront's Game sessions.
trait Bridged: Storefront {
    fn account(account: GameAccount) -> Option<Self::Account>;
    fn game_account(account: &Self::Account) -> GameAccount;
    fn store(store: GameStore) -> Option<(Self::Account, Self::Store)>;
    fn admission(game: GameIdentity) -> Option<(Self::Game, Self::Account)>;
    fn identity(game: &Self::Game, account: &Self::Account) -> GameIdentity;
    fn host(store: &Self::Store) -> Option<SteamHostState>;
}

impl Bridged for steam::Steam {
    fn account(account: GameAccount) -> Option<u64> {
        match account {
            GameAccount::Steam { steam_id } => Some(steam_id),
            GameAccount::Epic { .. } => None,
        }
    }
    fn game_account(account: &u64) -> GameAccount {
        GameAccount::Steam { steam_id: *account }
    }
    fn store(store: GameStore) -> Option<(u64, steam::Store)> {
        match store {
            GameStore::Steam {
                app_id,
                account_id,
                host,
            } => Some((
                account_id,
                steam::Store {
                    app_id,
                    host: host.into(),
                },
            )),
            GameStore::Epic { .. } => None,
        }
    }
    fn admission(game: GameIdentity) -> Option<(u32, u64)> {
        match game {
            GameIdentity::Steam { app_id, account_id } => Some((app_id, account_id)),
            GameIdentity::Epic { .. } => None,
        }
    }
    fn identity(game: &u32, account: &u64) -> GameIdentity {
        GameIdentity::Steam {
            app_id: *game,
            account_id: *account,
        }
    }
    fn host(store: &steam::Store) -> Option<SteamHostState> {
        Some(store.host.into())
    }
}

impl Bridged for epic::Epic {
    fn account(account: GameAccount) -> Option<String> {
        match account {
            GameAccount::Epic { account_id } => Some(account_id),
            GameAccount::Steam { .. } => None,
        }
    }
    fn game_account(account: &String) -> GameAccount {
        GameAccount::Epic {
            account_id: account.clone(),
        }
    }
    fn store(store: GameStore) -> Option<(String, epic::Store)> {
        match store {
            GameStore::Epic {
                account_id,
                namespace,
                app_name,
            } => Some((
                account_id,
                epic::Store {
                    namespace,
                    app_name,
                },
            )),
            GameStore::Steam { .. } => None,
        }
    }
    fn admission(game: GameIdentity) -> Option<(epic::Store, String)> {
        match game {
            GameIdentity::Epic {
                account_id,
                namespace,
                app_name,
            } => Some((
                epic::Store {
                    namespace,
                    app_name,
                },
                account_id,
            )),
            GameIdentity::Steam { .. } => None,
        }
    }
    fn identity(game: &epic::Store, account: &String) -> GameIdentity {
        GameIdentity::Epic {
            account_id: account.clone(),
            namespace: game.namespace.clone(),
            app_name: game.app_name.clone(),
        }
    }
    fn host(_: &epic::Store) -> Option<SteamHostState> {
        None
    }
}

impl From<SteamHostState> for game_runtime::HostState {
    fn from(host: SteamHostState) -> Self {
        match host {
            SteamHostState::Online => Self::Online,
            SteamHostState::Disconnected => Self::Disconnected,
            SteamHostState::Lost => Self::Lost,
            SteamHostState::Stopped => Self::Stopped,
        }
    }
}
impl From<GamePhase> for supervisor::Phase {
    fn from(phase: GamePhase) -> Self {
        match phase {
            GamePhase::Preparing => Self::Preparing,
            GamePhase::Running => Self::Running,
            GamePhase::Finalizing => Self::Finalizing,
            GamePhase::Finished => Self::Finished,
            GamePhase::Failed => Self::Failed,
        }
    }
}
impl From<CloudPhase> for cloud::Phase {
    fn from(phase: CloudPhase) -> Self {
        match phase {
            CloudPhase::Idle => Self::Idle,
            CloudPhase::Checking => Self::Checking,
            CloudPhase::Ready => Self::Ready,
            CloudPhase::Syncing => Self::Syncing,
            CloudPhase::Synced => Self::Synced,
            CloudPhase::Conflict => Self::Conflict,
            CloudPhase::Pending => Self::Pending,
            CloudPhase::Disabled => Self::Disabled,
            CloudPhase::Unsupported => Self::Unsupported,
            CloudPhase::Failed => Self::Failed,
        }
    }
}

/// A foreign supervisor as the Game sessions drive it.
struct Foreign(Arc<dyn GameSupervisor>);
impl<P: Bridged> Supervised<P> for Foreign {
    fn snapshot(&self, after: Option<u64>) -> Result<Observation<P>, String> {
        let snapshot = self.0.snapshot(after)?;
        let (account, store) = P::store(snapshot.store).ok_or(P::INVALID_SESSION)?;
        Ok(Observation {
            id: snapshot.id,
            account,
            store,
            phase: snapshot.phase.into(),
            finished: snapshot.finished,
            failure: snapshot.failure_reason.map(|reason| match reason {
                GameFailureReason::Cancelled => supervisor::FailureReason::Cancelled,
                GameFailureReason::Failed => supervisor::FailureReason::Failed,
            }),
            error: snapshot.error,
            revision: snapshot.revision,
            reconnecting: snapshot.reconnecting,
            rosetta: snapshot.rosetta.map(|request| request.decision_id),
            cloud: cloud::Status {
                phase: snapshot.cloud.phase.into(),
                decision_id: snapshot.cloud.decision_id,
                error: snapshot.cloud.error,
                allowed_choices: snapshot
                    .cloud
                    .allowed_choices
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            },
        })
    }
    fn cancel(&self) {
        self.0.cancel_preparation();
    }
    fn begin(&self) -> Result<(), String> {
        Ok(self.0.begin()?)
    }
    fn choose(
        &self,
        account: &P::Account,
        decision_id: u64,
        choice: cloud::Choice,
    ) -> Result<(), String> {
        Ok(self
            .0
            .choose(P::game_account(account), decision_id, choice.into())?)
    }
    fn resume_after_rosetta(&self, account: &P::Account, decision_id: u64) -> Result<(), String> {
        Ok(self
            .0
            .resume_after_rosetta(P::game_account(account), decision_id)?)
    }
    fn revoke(&self, account: &P::Account) -> Result<(), String> {
        Ok(self.0.revoke(P::game_account(account))?)
    }
}

/// The storefront whose Game sessions an object holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SessionStorefront {
    Steam,
    Epic,
}

/// The game and account a launch is admitted for.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameIdentity {
    Steam {
        app_id: u32,
        account_id: u64,
    },
    Epic {
        account_id: String,
        namespace: String,
        app_name: String,
    },
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct GameSessionState {
    pub id: String,
    pub game: GameIdentity,
    pub phase: GamePhase,
    /// The control connection was lost; the game may still run.
    pub reconnecting: bool,
    pub cloud: CloudStatus,
    /// Steam's native host.
    pub host: Option<SteamHostState>,
    /// A supervisor snapshot backs this session, not only its admission.
    pub observed: bool,
    /// How much of Voile has downloaded while the launch waits for it.
    pub runtime_install: Option<f64>,
}

/// A Rosetta requirement the player can answer.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct RosettaPromptState {
    pub session_id: String,
    pub account: GameAccount,
    pub decision_id: u64,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct GameSessionsState {
    /// Grows with every change.
    pub revision: u64,
    /// The session holding the Reservation.
    pub active: Option<GameSessionState>,
    /// The last session observed, kept after it ends.
    pub last: Option<GameSessionState>,
    /// A session a previous SailOver left may still run.
    pub recovering: bool,
    /// The last recovery attempt failed; it is retried.
    pub recovery_failed: bool,
    /// Accounts signed out since the last admission.
    pub revoked: Vec<GameAccount>,
    pub rosetta: Option<RosettaPromptState>,
}

fn session_state<P: Bridged>(session: sessions::Session<P>) -> GameSessionState {
    GameSessionState {
        game: P::identity(&session.game, &session.account),
        host: session.store.as_ref().and_then(P::host),
        id: session.id,
        phase: session.phase.into(),
        reconnecting: session.reconnecting,
        cloud: session.cloud.into(),
        observed: session.observed,
        runtime_install: session.runtime_install,
    }
}

fn state<P: Bridged>(state: sessions::State<P>) -> GameSessionsState {
    GameSessionsState {
        revision: state.revision,
        active: state.active.map(session_state),
        last: state.last.map(session_state),
        recovering: state.recovering,
        recovery_failed: state.recovery_failed,
        revoked: state.revoked.iter().map(P::game_account).collect(),
        rosetta: state.rosetta.map(|prompt| RosettaPromptState {
            session_id: prompt.session,
            account: P::game_account(&prompt.account),
            decision_id: prompt.decision_id,
        }),
    }
}

fn recovery<P: Bridged>(recovery: Arc<dyn GameRecovery>) -> sessions::Recovery<P> {
    Box::new(move || {
        Ok(recovery
            .recover()?
            .map(|supervisor| Arc::new(Foreign(supervisor)) as Arc<dyn Supervised<P>>))
    })
}

/// The supervisor a previous SailOver left for a game of `P`.
fn supervisor_recovery<P: Bridged>(sessions: &Path, helper: &Path) -> sessions::Recovery<P>
where
    P::Error: std::fmt::Display,
{
    let (sessions, helper) = (sessions.to_owned(), helper.to_owned());
    Box::new(move || {
        Ok(supervisor::Client::<P>::recover(&sessions, &helper)
            .map_err(|error| error.to_string())?
            .map(|client| Arc::new(client) as Arc<dyn Supervised<P>>))
    })
}

enum Storefronts {
    Steam(Sessions<steam::Steam>),
    Epic(Sessions<epic::Epic>),
}
impl Storefronts {
    /// A session call that failed without a reason of its own.
    fn failed(&self) -> &'static str {
        match self {
            Self::Steam(_) => steam::Steam::SESSION_FAILED,
            Self::Epic(_) => epic::Epic::SESSION_FAILED,
        }
    }
}

/// One storefront's Game sessions, for the whole life of SailOver.
#[derive(uniffi::Object)]
pub struct StorefrontSessions {
    sessions: Arc<Storefronts>,
}

impl StorefrontSessions {
    fn with(sessions: Storefronts) -> Arc<Self> {
        Arc::new(Self {
            sessions: Arc::new(sessions),
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl StorefrontSessions {
    /// Starts by recovering the session a previous SailOver left running.
    #[uniffi::constructor]
    pub fn new(storefront: SessionStorefront, locations: SailOverLocations) -> Arc<Self> {
        let helper = Path::new(&locations.helper);
        Self::with(match storefront {
            SessionStorefront::Steam => Storefronts::Steam(Sessions::new(supervisor_recovery(
                Path::new(&locations.steam_sessions),
                helper,
            ))),
            SessionStorefront::Epic => Storefronts::Epic(Sessions::new(supervisor_recovery(
                Path::new(&locations.epic_sessions),
                helper,
            ))),
        })
    }

    /// Recovers through `recovery` instead of the supervisors on disk.
    #[uniffi::constructor]
    pub fn with_recovery(
        storefront: SessionStorefront,
        recovery: Arc<dyn GameRecovery>,
    ) -> Arc<Self> {
        Self::with(match storefront {
            SessionStorefront::Steam => Storefronts::Steam(Sessions::new(self::recovery(recovery))),
            SessionStorefront::Epic => Storefronts::Epic(Sessions::new(self::recovery(recovery))),
        })
    }

    pub fn state(&self) -> GameSessionsState {
        match &*self.sessions {
            Storefronts::Steam(sessions) => state(sessions.state()),
            Storefronts::Epic(sessions) => state(sessions.state()),
        }
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> GameSessionsState {
        match &*self.sessions {
            Storefronts::Steam(sessions) => state(sessions.changed(after).await),
            Storefronts::Epic(sessions) => state(sessions.changed(after).await),
        }
    }

    /// Admits a launch: the Reservation holds from here. Nil while another
    /// session or a recovery holds the storefront.
    pub fn admit(&self, game: GameIdentity) -> Option<Arc<GameAdmission>> {
        let admission = match &*self.sessions {
            Storefronts::Steam(sessions) => {
                let (game, account) = steam::Steam::admission(game)?;
                Admitted::Steam(Admitted::slot(sessions.admit(game, account)?))
            }
            Storefronts::Epic(sessions) => {
                let (game, account) = epic::Epic::admission(game)?;
                Admitted::Epic(Admitted::slot(sessions.admit(game, account)?))
            }
        };
        Some(Arc::new(GameAdmission { admission }))
    }

    /// Account revocation. The game keeps running.
    pub fn revoke(&self, account: GameAccount) {
        match &*self.sessions {
            Storefronts::Steam(sessions) => {
                if let Some(account) = steam::Steam::account(account) {
                    sessions.revoke(account);
                }
            }
            Storefronts::Epic(sessions) => {
                if let Some(account) = epic::Epic::account(account) {
                    sessions.revoke(account);
                }
            }
        }
    }

    /// Answers the Cloud question session `session_id` shows with
    /// `decision_id`. Cancel stops a launch check still in progress.
    pub async fn choose(
        &self,
        session_id: String,
        decision_id: u64,
        choice: CloudChoice,
    ) -> Result<(), GameSessionError> {
        let sessions = self.sessions.clone();
        blocking(self.sessions.failed(), move || match &*sessions {
            Storefronts::Steam(sessions) => {
                sessions.choose(&session_id, decision_id, choice.into())
            }
            Storefronts::Epic(sessions) => sessions.choose(&session_id, decision_id, choice.into()),
        })
        .await
    }

    pub async fn resume_after_rosetta(
        &self,
        session_id: String,
        account: GameAccount,
        decision_id: u64,
    ) -> Result<(), GameSessionError> {
        let sessions = self.sessions.clone();
        blocking(self.sessions.failed(), move || match &*sessions {
            Storefronts::Steam(sessions) => match steam::Steam::account(account) {
                Some(account) => sessions.resume_after_rosetta(&session_id, &account, decision_id),
                None => Err(Failure::Failed(steam::Steam::INVALID_SESSION.into())),
            },
            Storefronts::Epic(sessions) => match epic::Epic::account(account) {
                Some(account) => sessions.resume_after_rosetta(&session_id, &account, decision_id),
                None => Err(Failure::Failed(epic::Epic::INVALID_SESSION.into())),
            },
        })
        .await
    }

    /// The player declined to install Rosetta: the waiting launch stops.
    pub fn decline_rosetta(&self, session_id: String, account: GameAccount, decision_id: u64) {
        match &*self.sessions {
            Storefronts::Steam(sessions) => {
                if let Some(account) = steam::Steam::account(account) {
                    sessions.decline_rosetta(&session_id, &account, decision_id);
                }
            }
            Storefronts::Epic(sessions) => {
                if let Some(account) = epic::Epic::account(account) {
                    sessions.decline_rosetta(&session_id, &account, decision_id);
                }
            }
        }
    }
}

/// Every supervisor call parks on IPC, so it runs off the async runtime.
async fn blocking(
    failed: &'static str,
    work: impl FnOnce() -> Result<(), Failure> + Send + 'static,
) -> Result<(), GameSessionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| GameSessionError::new(failed))?
        .map_err(Into::into)
}

/// An admission, until its launch takes it, and what cancels it after.
pub(crate) struct Slot<P: Storefront> {
    admission: Mutex<Option<sessions::Admission<P>>>,
    canceller: sessions::Canceller<P>,
}
impl<P: Storefront> Slot<P> {
    fn cancel(&self) {
        self.canceller.cancel();
    }
    /// The admission, once: a second launch of it is refused.
    pub(crate) fn take(&self) -> Result<sessions::Admission<P>, GameSessionError> {
        self.admission
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| GameSessionError::new(P::INVALID_SESSION))
    }
}

pub(crate) enum Admitted {
    Steam(Slot<steam::Steam>),
    Epic(Slot<epic::Epic>),
}
impl Admitted {
    fn slot<P: Storefront>(admission: sessions::Admission<P>) -> Slot<P> {
        Slot {
            canceller: admission.canceller(),
            admission: Mutex::new(Some(admission)),
        }
    }
}

/// One admitted launch. It holds the Reservation until its session is
/// finished, or until it is cancelled before its handoff.
#[derive(uniffi::Object)]
pub struct GameAdmission {
    pub(crate) admission: Admitted,
}

#[uniffi::export(async_runtime = "tokio")]
impl GameAdmission {
    /// Cancels preparation. Before the handoff this releases the Reservation;
    /// after it, a game that already started keeps running.
    pub fn cancel(&self) {
        match &self.admission {
            Admitted::Steam(slot) => slot.cancel(),
            Admitted::Epic(slot) => slot.cancel(),
        }
    }

    /// Hands the game off through `handoff`, then follows its session until
    /// it is finished. Cancelling during the handoff keeps the Reservation
    /// until the supervisor it returns confirms the game is gone.
    /// `before_start` runs before a waiting supervisor's start gate opens.
    pub async fn launch(
        &self,
        handoff: Arc<dyn GameHandoff>,
        before_start: Option<Arc<dyn BeforeGameStart>>,
    ) -> Result<(), GameSessionError> {
        let before: sessions::BeforeBegin = Box::new(move || {
            if let Some(before_start) = before_start {
                before_start.run();
            }
        });
        match &self.admission {
            Admitted::Steam(slot) => Ok(slot
                .take()?
                .launch_before(foreign_handoff(handoff), before)
                .await?),
            Admitted::Epic(slot) => Ok(slot
                .take()?
                .launch_before(foreign_handoff(handoff), before)
                .await?),
        }
    }
}

/// A foreign handoff, which blocks, as an admitted launch awaits it.
async fn foreign_handoff<P: Bridged>(
    handoff: Arc<dyn GameHandoff>,
) -> Result<Arc<dyn Supervised<P>>, sessions::Handoff> {
    let supervisor = tokio::task::spawn_blocking(move || handoff.hand_off())
        .await
        .map_err(|_| sessions::Handoff::Refused(Failure::Failed(P::SESSION_FAILED.into())))?
        .map_err(|error| {
            sessions::Handoff::Refused(match error {
                GameSessionError::Cancelled => Failure::Cancelled,
                GameSessionError::Failure { reason } => Failure::Failed(reason),
            })
        })?;
    Ok(Arc::new(Foreign(supervisor)) as Arc<dyn Supervised<P>>)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A supervisor whose game already exited.
    struct Exited(GameStore);
    impl GameSupervisor for Exited {
        fn snapshot(&self, _: Option<u64>) -> Result<GameSnapshot, GameSessionError> {
            Ok(GameSnapshot {
                id: "session".into(),
                phase: GamePhase::Finished,
                finished: true,
                failure_reason: None,
                error: None,
                revision: 2,
                reconnecting: false,
                rosetta: None,
                cloud: CloudStatus {
                    phase: CloudPhase::Pending,
                    decision_id: 0,
                    error: Some("backend.epic.cloud.failed".into()),
                    allowed_choices: Vec::new(),
                },
                store: self.0.clone(),
            })
        }
        fn cancel_preparation(&self) {}
        fn begin(&self) -> Result<(), GameSessionError> {
            Ok(())
        }
        fn choose(&self, _: GameAccount, _: u64, _: CloudChoice) -> Result<(), GameSessionError> {
            Ok(())
        }
        fn resume_after_rosetta(&self, _: GameAccount, _: u64) -> Result<(), GameSessionError> {
            Ok(())
        }
        fn revoke(&self, _: GameAccount) -> Result<(), GameSessionError> {
            Ok(())
        }
    }

    /// Hands off to a supervisor whose game already exited.
    struct Ready(GameStore);
    impl GameHandoff for Ready {
        fn hand_off(&self) -> Result<Arc<dyn GameSupervisor>, GameSessionError> {
            Ok(Arc::new(Exited(self.0.clone())))
        }
    }

    struct Nothing;
    impl GameRecovery for Nothing {
        fn recover(&self) -> Result<Option<Arc<dyn GameSupervisor>>, GameSessionError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn a_foreign_supervisor_is_followed_under_its_storefront_identity() {
        let sessions =
            StorefrontSessions::with_recovery(SessionStorefront::Epic, Arc::new(Nothing));
        let mut state = sessions.state();
        while state.recovering {
            state = sessions.changed(state.revision).await;
        }
        let game = GameIdentity::Epic {
            account_id: "account".into(),
            namespace: "ns".into(),
            app_name: "app".into(),
        };
        assert!(
            sessions
                .admit(GameIdentity::Steam {
                    app_id: 42,
                    account_id: 42
                })
                .is_none(),
            "a Steam game is no Epic session"
        );
        let admission = sessions.admit(game.clone()).unwrap();
        assert!(sessions.state().active.is_some());

        admission
            .launch(
                Arc::new(Ready(GameStore::Epic {
                    account_id: "account".into(),
                    namespace: "ns".into(),
                    app_name: "app".into(),
                })),
                None,
            )
            .await
            .unwrap();

        let state = sessions.state();
        assert!(state.active.is_none());
        let last = state.last.unwrap();
        assert_eq!(last.game, game);
        assert_eq!(last.cloud.phase, CloudPhase::Pending);
        assert_eq!(last.host, None);
        assert!(matches!(
            admission
                .launch(
                    Arc::new(Ready(GameStore::Epic {
                        account_id: "account".into(),
                        namespace: "ns".into(),
                        app_name: "app".into(),
                    })),
                    None
                )
                .await,
            Err(GameSessionError::Failure { .. })
        ));
    }
}
