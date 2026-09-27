//! A storefront's Game sessions across UniFFI. Rust keeps the Reservation,
//! recovery, Account revocation and the checks on every answer; Swift presents
//! the state and forwards the player's answers.
use crate::{SailOverLocations, SteamHostState};
use game_runtime::{
    cloud, epic,
    sessions::{self, Failure, Sessions, Storefront, Supervised},
    steam, supervisor,
};
use std::{path::Path, sync::Arc};

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

/// The account an answer or a revocation names.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameAccount {
    Steam { steam_id: u64 },
    Epic { account_id: String },
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

/// What the bridge adds to each storefront's Game sessions.
pub(crate) trait Bridged: Storefront {
    fn account(account: GameAccount) -> Option<Self::Account>;
    fn game_account(account: &Self::Account) -> GameAccount;
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

pub(crate) enum Storefronts {
    Steam(Arc<Sessions<steam::Steam>>),
    Epic(Arc<Sessions<epic::Epic>>),
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
    pub(crate) sessions: Arc<Storefronts>,
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
            SessionStorefront::Steam => Storefronts::Steam(Arc::new(Sessions::new(
                supervisor_recovery(Path::new(&locations.steam_sessions), helper),
            ))),
            SessionStorefront::Epic => Storefronts::Epic(Arc::new(Sessions::new(
                supervisor_recovery(Path::new(&locations.epic_sessions), helper),
            ))),
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

    /// Ends the running game, as quitting SailOver does; its session still
    /// settles its Cloud saves. A launch still preparing is cancelled.
    pub async fn stop(&self) -> Result<(), GameSessionError> {
        let sessions = self.sessions.clone();
        blocking(self.sessions.failed(), move || match &*sessions {
            Storefronts::Steam(sessions) => sessions.stop(),
            Storefronts::Epic(sessions) => sessions.stop(),
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
