//! A storefront's Game sessions across UniFFI. Rust keeps the Reservation,
//! recovery, Account revocation and the checks on every answer; Swift presents
//! the state and forwards the player's answers.
use crate::{ObeliskLocations, SteamHostState, steam_chat::SteamGameInvite};
use game_runtime::{
    Locations, cloud, epic, gog, protocol,
    runtimes::{self, Answer, Runtimes},
    sessions::{self, Failure, Sessions, Storefront, Supervised},
    steam, supervisor,
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
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

/// A runtime a launch downloads before its game is prepared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RuntimeComponent {
    /// Voile, the Wine runtime Windows games run in.
    Voile,
    /// Valve's Steam components, which Obelisk's Steam host loads.
    SteamComponents,
}
impl From<runtimes::Runtime> for RuntimeComponent {
    fn from(runtime: runtimes::Runtime) -> Self {
        match runtime {
            runtimes::Runtime::Voile => Self::Voile,
            runtimes::Runtime::SteamComponents => Self::SteamComponents,
        }
    }
}

/// How much of a runtime has downloaded while the launch waits for it.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct RuntimeInstallState {
    pub component: RuntimeComponent,
    pub fraction: f64,
}
impl From<sessions::RuntimeInstall> for RuntimeInstallState {
    fn from(install: sessions::RuntimeInstall) -> Self {
        Self {
            component: install.runtime.into(),
            fraction: install.fraction,
        }
    }
}

/// A newer Voile release a launch offers before its game starts, which the
/// player answers with `StorefrontSessions::answer_voile_update`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VoileUpdateOffer {
    /// The release offered.
    pub version: String,
    /// The game needs it: the player takes it, or the launch stops.
    pub required: bool,
    /// What each release newer than the one in place brings, up to the one
    /// offered, newest first.
    pub releases: Vec<VoileReleaseNotes>,
}
impl From<runtimes::Update> for VoileUpdateOffer {
    fn from(update: runtimes::Update) -> Self {
        Self {
            version: update.version,
            required: update.required,
            releases: update.releases.into_iter().map(Into::into).collect(),
        }
    }
}

/// What a Voile release brings.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VoileReleaseNotes {
    pub version: String,
    /// In Markdown, as its GitHub release tells it; empty when Voile's feed
    /// has none for it.
    pub notes: String,
}
impl From<game_runtime::voile::Notes> for VoileReleaseNotes {
    fn from(notes: game_runtime::voile::Notes) -> Self {
        Self {
            version: notes.version,
            notes: notes.notes,
        }
    }
}

/// The player's answer to a newer Voile release a launch offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum VoileUpdateAnswer {
    /// Download it, then start the game in it.
    Update,
    /// Start the game in the release in place; refused for a release the
    /// game needs.
    Skip,
    /// Stop the launch.
    Cancel,
}
impl From<VoileUpdateAnswer> for Answer {
    fn from(answer: VoileUpdateAnswer) -> Self {
        match answer {
            VoileUpdateAnswer::Update => Self::Update,
            VoileUpdateAnswer::Skip => Self::Skip,
            VoileUpdateAnswer::Cancel => Self::Cancel,
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
    Gog { user_id: String },
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
    fn host(_: &Self::Store) -> Option<SteamHostState> {
        None
    }
    fn invite_request(_: &Self::Store) -> Option<SteamInviteRequest> {
        None
    }
    fn lobby(_: &Self::Store) -> Option<u64> {
        None
    }
    fn overlay_request(_: &Self::Store) -> Option<SteamOverlayRequest> {
        None
    }
    fn received_invites(_: &Self::Store) -> Vec<SteamReceivedInvite> {
        Vec::new()
    }
    fn micro_txn(_: &Self::Store) -> Option<SteamMicroTxn> {
        None
    }
}

impl Bridged for steam::Steam {
    fn account(account: GameAccount) -> Option<u64> {
        match account {
            GameAccount::Steam { steam_id } => Some(steam_id),
            _ => None,
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
    fn invite_request(store: &steam::Store) -> Option<SteamInviteRequest> {
        store.requests.invite().map(|request| SteamInviteRequest {
            sequence: request.sequence,
            invite: request.target.into(),
        })
    }
    fn lobby(store: &steam::Store) -> Option<u64> {
        store.requests.lobby
    }
    fn overlay_request(store: &steam::Store) -> Option<SteamOverlayRequest> {
        store.requests.overlay.clone().map(Into::into)
    }
    fn received_invites(store: &steam::Store) -> Vec<SteamReceivedInvite> {
        store
            .requests
            .invites
            .iter()
            .cloned()
            .map(|invite| SteamReceivedInvite::into_game(store.app_id, invite))
            .collect()
    }
    fn micro_txn(store: &steam::Store) -> Option<SteamMicroTxn> {
        store
            .requests
            .microtxn
            .as_ref()
            .map(|pending| SteamMicroTxn {
                order_id: pending.order_id,
                app_id: pending.app_id,
            })
    }
}

impl Bridged for epic::Epic {
    fn account(account: GameAccount) -> Option<String> {
        match account {
            GameAccount::Epic { account_id } => Some(account_id),
            _ => None,
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
}

impl Bridged for gog::Gog {
    fn account(account: GameAccount) -> Option<String> {
        match account {
            GameAccount::Gog { user_id } => Some(user_id),
            _ => None,
        }
    }
    fn game_account(account: &String) -> GameAccount {
        GameAccount::Gog {
            user_id: account.clone(),
        }
    }
    fn identity(game: &String, account: &String) -> GameIdentity {
        GameIdentity::Gog {
            account_id: account.clone(),
            product_id: game.clone(),
        }
    }
}

/// The storefront whose Game sessions an object holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SessionStorefront {
    Steam,
    Epic,
    Gog,
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
    Gog {
        account_id: String,
        product_id: String,
    },
}

/// The game asked Steam's overlay to invite friends into `invite`. A new
/// `sequence` is a new request, even for the same invitation.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamInviteRequest {
    pub sequence: u64,
    pub invite: SteamGameInvite,
}

/// What `ActivateGameOverlayToStore` asks of the store page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamStoreFlag {
    /// Only the page (`k_EOverlayToStoreFlag_None`).
    ShowPage,
    AddToCart,
    AddToCartAndShow,
}

/// What the game asked Steam's overlay to show; Obelisk shows its own.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamOverlayKind {
    /// `ActivateGameOverlay`: "friends", "community", "players", "settings",
    /// "officialgamegroup", "stats", "achievements", "chatroomgroup/<id>", …
    Dialog { dialog: String },
    /// `ActivateGameOverlayToUser`: "steamid", "chat", "jointrade", "stats",
    /// "achievements", "friendadd", … about `steam_id`.
    User { dialog: String, steam_id: u64 },
    /// `ActivateGameOverlayToWebPage`: an http or https URL.
    WebPage { url: String, modal: bool },
    /// `ActivateGameOverlayToStore`; app 0 is the store's front page.
    Store { app_id: u32, flag: SteamStoreFlag },
    /// `ActivateGameOverlayInviteDialog`: invite friends into the lobby.
    LobbyInvite { lobby_id: u64 },
    /// `ActivateGameOverlayRemotePlayTogetherInviteDialog`.
    RemotePlayTogetherInvite { lobby_id: Option<u64> },
    /// `ActivateGameOverlayInviteDialogConnectString`.
    ConnectInvite { connect: String },
    /// Steam's own Remote Play Together dialog.
    RemotePlayTogether,
}
impl From<protocol::OverlayDialog> for SteamOverlayKind {
    fn from(dialog: protocol::OverlayDialog) -> Self {
        use protocol::OverlayDialog as Dialog;
        match dialog {
            Dialog::Dialog { dialog } => Self::Dialog { dialog },
            Dialog::User { dialog, steam_id } => Self::User { dialog, steam_id },
            Dialog::WebPage { url, modal } => Self::WebPage { url, modal },
            Dialog::Store { app_id, flag } => Self::Store {
                app_id,
                flag: match flag {
                    1 => SteamStoreFlag::AddToCart,
                    2 => SteamStoreFlag::AddToCartAndShow,
                    _ => SteamStoreFlag::ShowPage,
                },
            },
            Dialog::LobbyInvite { lobby_id } => Self::LobbyInvite { lobby_id },
            Dialog::RemotePlayTogetherInvite { lobby_id } => {
                Self::RemotePlayTogetherInvite { lobby_id }
            }
            Dialog::ConnectInvite { connect } => Self::ConnectInvite { connect },
            Dialog::RemotePlayTogether {} => Self::RemotePlayTogether,
        }
    }
}

/// The latest overlay the game asked for. A new `sequence` is a new
/// request, even for the same overlay.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamOverlayRequest {
    pub sequence: u64,
    pub kind: SteamOverlayKind,
}
impl From<steam::host::OverlayRequest> for SteamOverlayRequest {
    fn from(request: steam::host::OverlayRequest) -> Self {
        Self {
            sequence: request.sequence,
            kind: request.dialog.into(),
        }
    }
}

/// An invitation a friend sent into a game, as the running game's Steam or
/// the account's session received it. The running game takes one into it
/// with `StorefrontSessions::accept_invite`; another game joins it as it
/// starts, with `AccountInstallations::join`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamReceivedInvite {
    pub friend_steam_id: u64,
    pub app_id: u32,
    /// Never `None`.
    pub invite: SteamGameInvite,
    /// When it was last received, in seconds since the Unix epoch.
    pub received_at: u64,
}
impl SteamReceivedInvite {
    /// An invitation the running game of `app_id` received.
    fn into_game(app_id: u32, invite: steam::host::ReceivedInvite) -> Self {
        Self {
            friend_steam_id: invite.friend,
            app_id,
            invite: invite.target.into(),
            received_at: invite.received_at,
        }
    }
}

/// A purchase the running game asks the player to confirm, answered with
/// `StorefrontSessions::authorize_micro_txn`. Steam tells no price here.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamMicroTxn {
    pub order_id: u64,
    pub app_id: u32,
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
    /// How much of a runtime has downloaded while the launch waits for it.
    pub runtime_install: Option<RuntimeInstallState>,
    /// The newer Voile release the launch waits for the player to take or
    /// skip.
    #[uniffi(default = None)]
    pub voile_update: Option<VoileUpdateOffer>,
    /// When this Obelisk saw the game start, in seconds since the Unix epoch,
    /// even if it already exited. A game recovered running has none.
    #[uniffi(default = None)]
    pub started_at: Option<u64>,
    /// The latest invite dialog the Steam game asked for, while its host is
    /// online: the picker it opens invites into its target.
    #[uniffi(default = None)]
    pub invite_request: Option<SteamInviteRequest>,
    /// The Steam lobby the game is in now, as the account's engine shows it:
    /// the friends list invites into it.
    #[uniffi(default = None)]
    pub lobby_id: Option<u64>,
    /// The latest overlay the Steam game asked for, while its host is online,
    /// invite dialogs included: `invite_request` is the same request when it
    /// asks for one, with the same sequence.
    #[uniffi(default = None)]
    pub overlay_request: Option<SteamOverlayRequest>,
    /// Invitations friends sent into the Steam game, newest first, while its
    /// host is online.
    #[uniffi(default = [])]
    pub received_invites: Vec<SteamReceivedInvite>,
    /// The purchase the Steam game waits on the player to confirm.
    #[uniffi(default = None)]
    pub micro_txn: Option<SteamMicroTxn>,
    /// What the game did, once it ran and exited.
    #[uniffi(default = None)]
    pub played: Option<GamePlayed>,
}

/// What a session's game did once it ran, for the player's performance
/// reports. Frame rates are absent when Metal reported no frame.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct GamePlayed {
    pub seconds: u64,
    pub frames: u64,
    pub median_fps: Option<f64>,
    pub low_1pct_fps: Option<f64>,
    pub hitches: u64,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    pub ended_by_obelisk: bool,
    /// `rosetta` or `linux` for a Windows game, absent for a Mac one.
    pub engine: Option<String>,
    /// `dxmt` or `d3dmetal` for a Windows game, absent for a Mac one.
    pub graphics: Option<String>,
    pub voile: Option<String>,
}
impl From<game_runtime::supervisor::Played> for GamePlayed {
    fn from(played: game_runtime::supervisor::Played) -> Self {
        use game_runtime::wine::Engine;
        Self {
            seconds: played.seconds,
            frames: played.frames.count,
            median_fps: played.frames.median_fps(),
            low_1pct_fps: played.frames.low_1pct_fps(),
            hitches: played.frames.hitches,
            exit_code: played.exit_code,
            exit_signal: played.exit_signal,
            ended_by_obelisk: played.ended_by_obelisk,
            engine: played.engine.map(|engine| {
                match engine {
                    Engine::Rosetta => "rosetta",
                    Engine::Linux => "linux",
                }
                .to_owned()
            }),
            graphics: played.graphics.map(|graphics| graphics.name().to_owned()),
            voile: played.voile,
        }
    }
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
    /// A session a previous Obelisk left may still run.
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
        invite_request: session.store.as_ref().and_then(P::invite_request),
        lobby_id: session.store.as_ref().and_then(P::lobby),
        overlay_request: session.store.as_ref().and_then(P::overlay_request),
        received_invites: session
            .store
            .as_ref()
            .map(P::received_invites)
            .unwrap_or_default(),
        micro_txn: session.store.as_ref().and_then(P::micro_txn),
        id: session.id,
        phase: session.phase.into(),
        reconnecting: session.reconnecting,
        cloud: session.cloud.into(),
        observed: session.observed,
        runtime_install: session.runtime_install.map(Into::into),
        voile_update: session.voile_update.map(Into::into),
        started_at: session.started_at,
        played: session.played.map(Into::into),
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

/// The supervisor a previous Obelisk left for a game of `P`.
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
    Gog(Arc<Sessions<gog::Gog>>),
}

/// Runs `$body` with the Game sessions as `$sessions`, whichever storefront
/// they serve.
macro_rules! each {
    ($storefronts:expr, $sessions:ident => $body:expr) => {
        match $storefronts {
            Storefronts::Steam($sessions) => $body,
            Storefronts::Epic($sessions) => $body,
            Storefronts::Gog($sessions) => $body,
        }
    };
}

impl Storefronts {
    /// A session call that failed without a reason of its own.
    fn failed(&self) -> &'static str {
        fn failed<P: Storefront>(_: &Sessions<P>) -> &'static str {
            P::SESSION_FAILED
        }
        each!(self, sessions => failed(sessions))
    }
}

/// Account revocation, for an account of the sessions' storefront.
fn revoke<P: Bridged>(sessions: &Sessions<P>, account: GameAccount) {
    if let Some(account) = P::account(account) {
        sessions.revoke(account);
    }
}

fn resume_after_rosetta<P: Bridged>(
    sessions: &Sessions<P>,
    session_id: &str,
    account: GameAccount,
    decision_id: u64,
) -> Result<(), Failure> {
    match P::account(account) {
        Some(account) => sessions.resume_after_rosetta(session_id, &account, decision_id),
        None => Err(Failure::Failed(P::INVALID_SESSION.into())),
    }
}

fn decline_rosetta<P: Bridged>(
    sessions: &Sessions<P>,
    session_id: &str,
    account: GameAccount,
    decision_id: u64,
) {
    if let Some(account) = P::account(account) {
        sessions.decline_rosetta(session_id, &account, decision_id);
    }
}

/// One storefront's Game sessions, for the whole life of Obelisk.
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

    /// Runs `call` on the Steam Game sessions; any other storefront's
    /// sessions refuse it.
    async fn steam(
        &self,
        call: impl FnOnce(&Sessions<steam::Steam>) -> Result<(), Failure> + Send + 'static,
    ) -> Result<(), GameSessionError> {
        let Storefronts::Steam(sessions) = &*self.sessions else {
            let refused = each!(&*self.sessions, sessions => invalid_session(sessions));
            return Err(GameSessionError::new(refused));
        };
        let sessions = sessions.clone();
        blocking(self.sessions.failed(), move || call(&sessions)).await
    }
}

/// A refused session call of `sessions`' storefront.
fn invalid_session<P: Storefront>(_: &Sessions<P>) -> &'static str {
    P::INVALID_SESSION
}

/// The runtimes every storefront's Game sessions and Transfers share, and the
/// places the first sessions named for them.
static RUNTIMES: OnceLock<((PathBuf, PathBuf), Arc<Runtimes>)> = OnceLock::new();

/// Where `locations` puts the runtimes: Voile's folder and Valve's Steam
/// components.
fn runtime_places(locations: &Locations) -> (PathBuf, PathBuf) {
    (
        locations.voile.clone(),
        locations.steam_runtime_package.clone(),
    )
}

/// The shared runtimes, in the places the first sessions' `locations` name.
/// The sessions of every other storefront name the same places.
fn runtimes(locations: &Locations) -> Arc<Runtimes> {
    let (places, runtimes) =
        RUNTIMES.get_or_init(|| (runtime_places(locations), Runtimes::new(locations)));
    debug_assert_eq!(
        *places,
        runtime_places(locations),
        "every storefront's sessions name the same runtime places"
    );
    runtimes.clone()
}

/// Whether `locations` names the places of the shared runtimes, as an
/// account's installations must: they get their runtimes through their
/// sessions, and their launches read them from `locations`.
pub(crate) fn shares_runtimes(locations: &Locations) -> bool {
    RUNTIMES
        .get()
        .is_none_or(|(places, _)| *places == runtime_places(locations))
}

#[uniffi::export(async_runtime = "tokio")]
impl StorefrontSessions {
    /// Starts by recovering the session a previous Obelisk left running. Its
    /// launches and Transfers get their runtimes into the places `locations`
    /// names, and the installations given it read them back from their own
    /// locations: those must name the same places.
    #[uniffi::constructor]
    pub fn new(storefront: SessionStorefront, locations: ObeliskLocations) -> Arc<Self> {
        let helper = Path::new(&locations.helper);
        let runtimes = runtimes(&(&locations).into());
        Self::with(match storefront {
            SessionStorefront::Steam => Storefronts::Steam(Arc::new(Sessions::new(
                supervisor_recovery(Path::new(&locations.steam_sessions), helper),
                runtimes,
            ))),
            SessionStorefront::Epic => Storefronts::Epic(Arc::new(Sessions::new(
                supervisor_recovery(Path::new(&locations.epic_sessions), helper),
                runtimes,
            ))),
            SessionStorefront::Gog => Storefronts::Gog(Arc::new(Sessions::new(
                supervisor_recovery(Path::new(&locations.gog_sessions), helper),
                runtimes,
            ))),
        })
    }

    pub fn state(&self) -> GameSessionsState {
        each!(&*self.sessions, sessions => state(sessions.state()))
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> GameSessionsState {
        each!(&*self.sessions, sessions => state(sessions.changed(after).await))
    }

    /// How the player shows to their friends: the next game starts showing
    /// it, and the one under way shows it at once. Nothing for a storefront
    /// whose games show no presence.
    pub async fn set_status(&self, status: crate::OnlineStatus) {
        let (sessions, status) = (self.sessions.clone(), status.into());
        // Telling a running game parks on its supervisor's connection.
        let _ = blocking(self.sessions.failed(), move || {
            each!(&*sessions, sessions => sessions.set_status(status));
            Ok(())
        })
        .await;
    }

    /// Account revocation. The game keeps running.
    pub fn revoke(&self, account: GameAccount) {
        each!(&*self.sessions, sessions => revoke(sessions, account))
    }

    pub async fn resume_after_rosetta(
        &self,
        session_id: String,
        account: GameAccount,
        decision_id: u64,
    ) -> Result<(), GameSessionError> {
        let sessions = self.sessions.clone();
        blocking(self.sessions.failed(), move || {
            each!(&*sessions, sessions => resume_after_rosetta(sessions, &session_id, account, decision_id))
        })
        .await
    }

    /// Ends the running game, as quitting Obelisk does; its session still
    /// settles its Cloud saves. A launch still preparing is cancelled.
    pub async fn stop(&self) -> Result<(), GameSessionError> {
        let sessions = self.sessions.clone();
        blocking(
            self.sessions.failed(),
            move || each!(&*sessions, sessions => sessions.stop()),
        )
        .await
    }

    /// The player's Quit of session `session_id`'s game: it is asked to close
    /// its windows, and nothing is killed. A launch still preparing is
    /// cancelled.
    pub async fn quit(&self, session_id: String) -> Result<(), GameSessionError> {
        let sessions = self.sessions.clone();
        blocking(
            self.sessions.failed(),
            move || each!(&*sessions, sessions => sessions.quit(&session_id)),
        )
        .await
    }

    /// The player's Force Quit of session `session_id`'s game, which ignored
    /// Quit: it ends now, with whatever still runs for its installation, and
    /// its session still settles its Cloud saves. A launch still preparing is
    /// cancelled.
    pub async fn force_quit(&self, session_id: String) -> Result<(), GameSessionError> {
        let sessions = self.sessions.clone();
        blocking(
            self.sessions.failed(),
            move || each!(&*sessions, sessions => sessions.force_quit(&session_id)),
        )
        .await
    }

    /// Obelisk's overlay opened (`active`) or closed over session
    /// `session_id`'s running Steam game, as Steam's overlay would tell it;
    /// `user_initiated` when the player opened it rather than the game. The
    /// game sees a close only after an open.
    pub async fn overlay_state(
        &self,
        session_id: String,
        active: bool,
        user_initiated: bool,
    ) -> Result<(), GameSessionError> {
        self.steam(move |sessions| sessions.overlay_state(&session_id, active, user_initiated))
            .await
    }

    /// The player accepted `friend_steam_id`'s invitation into session
    /// `session_id`'s running Steam game, from `received_invites` or from
    /// Steam's chat: the game joins as if Steam's overlay had accepted it.
    pub async fn accept_invite(
        &self,
        session_id: String,
        friend_steam_id: u64,
        invite: SteamGameInvite,
    ) -> Result<(), GameSessionError> {
        self.steam(move |sessions| {
            let target = protocol::InviteTarget::try_from(invite)
                .map_err(|()| Failure::Failed(steam::Steam::INVALID_SESSION.into()))?;
            sessions.accept_invite(&session_id, friend_steam_id, target)
        })
        .await
    }

    /// The player's answer to `micro_txn`, the purchase session
    /// `session_id`'s Steam game waits on: only that order is answered.
    pub async fn authorize_micro_txn(
        &self,
        session_id: String,
        order_id: u64,
        approve: bool,
    ) -> Result<(), GameSessionError> {
        self.steam(move |sessions| sessions.authorize_microtxn(&session_id, order_id, approve))
            .await
    }

    /// The player declined to install Rosetta: the waiting launch stops.
    pub fn decline_rosetta(&self, session_id: String, account: GameAccount, decision_id: u64) {
        each!(&*self.sessions, sessions => decline_rosetta(sessions, &session_id, account, decision_id))
    }

    /// The player's answer to the newer Voile release session `session_id`'s
    /// launch offers.
    pub fn answer_voile_update(
        &self,
        session_id: String,
        answer: VoileUpdateAnswer,
    ) -> Result<(), GameSessionError> {
        each!(&*self.sessions, sessions => sessions.answer_voile_update(&session_id, answer.into()))
            .map_err(Into::into)
    }
}

/// Every supervisor call parks on IPC, so it runs off the async runtime, as
/// do an installation's answer to a Cloud question and its journal reads.
pub(crate) async fn blocking<T: Send + 'static>(
    failed: &'static str,
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, GameSessionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| GameSessionError::new(failed))?
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOBBY: u64 = 109_775_244_421_126_053;
    const FRIEND: u64 = 76_561_197_960_265_729;

    #[test]
    fn only_a_steam_session_carries_its_games_invite_request_and_lobby() {
        let mut store = steam::Store::new(3527290);
        assert_eq!(steam::Steam::invite_request(&store), None);
        assert_eq!(steam::Steam::lobby(&store), None);
        store.requests.lobby = Some(LOBBY);
        assert_eq!(steam::Steam::lobby(&store), Some(LOBBY));
        store.requests.overlay = Some(steam::host::OverlayRequest {
            sequence: 2,
            dialog: protocol::OverlayDialog::LobbyInvite { lobby_id: LOBBY },
        });
        assert_eq!(
            steam::Steam::invite_request(&store),
            Some(SteamInviteRequest {
                sequence: 2,
                invite: SteamGameInvite::Lobby { lobby_id: LOBBY },
            })
        );
        store.requests.overlay = Some(steam::host::OverlayRequest {
            sequence: 3,
            dialog: protocol::OverlayDialog::ConnectInvite {
                connect: "+connect_lobby 1".into(),
            },
        });
        assert_eq!(
            steam::Steam::invite_request(&store).map(|request| request.invite),
            Some(SteamGameInvite::Connect {
                connect: "+connect_lobby 1".into()
            })
        );
        let gog = gog::Store {
            product_id: "1207658924".into(),
        };
        assert_eq!(gog::Gog::overlay_request(&gog), None);
        assert!(gog::Gog::received_invites(&gog).is_empty());
        assert_eq!(gog::Gog::micro_txn(&gog), None);
    }

    #[test]
    fn every_overlay_request_reaches_swift_with_its_sequence() {
        use protocol::OverlayDialog as Dialog;
        for (dialog, kind) in [
            (
                Dialog::Dialog {
                    dialog: "friends".into(),
                },
                SteamOverlayKind::Dialog {
                    dialog: "friends".into(),
                },
            ),
            (
                Dialog::User {
                    dialog: "chat".into(),
                    steam_id: FRIEND,
                },
                SteamOverlayKind::User {
                    dialog: "chat".into(),
                    steam_id: FRIEND,
                },
            ),
            (
                Dialog::WebPage {
                    url: "https://example.com".into(),
                    modal: true,
                },
                SteamOverlayKind::WebPage {
                    url: "https://example.com".into(),
                    modal: true,
                },
            ),
            (
                Dialog::Store {
                    app_id: 620,
                    flag: 0,
                },
                SteamOverlayKind::Store {
                    app_id: 620,
                    flag: SteamStoreFlag::ShowPage,
                },
            ),
            (
                Dialog::Store {
                    app_id: 620,
                    flag: 1,
                },
                SteamOverlayKind::Store {
                    app_id: 620,
                    flag: SteamStoreFlag::AddToCart,
                },
            ),
            (
                Dialog::Store {
                    app_id: 620,
                    flag: 2,
                },
                SteamOverlayKind::Store {
                    app_id: 620,
                    flag: SteamStoreFlag::AddToCartAndShow,
                },
            ),
            (
                Dialog::LobbyInvite { lobby_id: LOBBY },
                SteamOverlayKind::LobbyInvite { lobby_id: LOBBY },
            ),
            (
                Dialog::RemotePlayTogetherInvite { lobby_id: None },
                SteamOverlayKind::RemotePlayTogetherInvite { lobby_id: None },
            ),
            (
                Dialog::ConnectInvite {
                    connect: "+connect 1".into(),
                },
                SteamOverlayKind::ConnectInvite {
                    connect: "+connect 1".into(),
                },
            ),
            (
                Dialog::RemotePlayTogether {},
                SteamOverlayKind::RemotePlayTogether,
            ),
        ] {
            let store = steam::Store {
                requests: steam::host::GameRequests {
                    overlay: Some(steam::host::OverlayRequest {
                        sequence: 9,
                        dialog,
                    }),
                    ..Default::default()
                },
                ..steam::Store::new(620)
            };
            assert_eq!(
                steam::Steam::overlay_request(&store),
                Some(SteamOverlayRequest { sequence: 9, kind })
            );
        }
    }

    #[test]
    fn received_invites_and_the_pending_purchase_reach_swift() {
        let store = steam::Store {
            requests: steam::host::GameRequests {
                invites: vec![
                    steam::host::ReceivedInvite {
                        friend: FRIEND,
                        target: protocol::InviteTarget::Connect {
                            connect: "+connect 10.0.0.1".into(),
                        },
                        received_at: 1_790_000_100,
                    },
                    steam::host::ReceivedInvite {
                        friend: FRIEND + 1,
                        target: protocol::InviteTarget::Lobby { lobby_id: LOBBY },
                        received_at: 1_790_000_000,
                    },
                ],
                microtxn: Some(steam::host::MicroTxn {
                    order_id: 4242,
                    app_id: 620,
                }),
                ..Default::default()
            },
            ..steam::Store::new(620)
        };
        assert_eq!(
            steam::Steam::received_invites(&store),
            [
                SteamReceivedInvite {
                    friend_steam_id: FRIEND,
                    app_id: 620,
                    invite: SteamGameInvite::Connect {
                        connect: "+connect 10.0.0.1".into()
                    },
                    received_at: 1_790_000_100,
                },
                SteamReceivedInvite {
                    friend_steam_id: FRIEND + 1,
                    app_id: 620,
                    invite: SteamGameInvite::Lobby { lobby_id: LOBBY },
                    received_at: 1_790_000_000,
                },
            ]
        );
        assert_eq!(
            steam::Steam::micro_txn(&store),
            Some(SteamMicroTxn {
                order_id: 4242,
                app_id: 620
            })
        );
    }

    #[tokio::test]
    async fn another_storefronts_sessions_refuse_steam_controls() {
        let root = tempfile::tempdir().unwrap();
        let runtimes = Runtimes::new(&Locations::new(root.path(), root.path(), root.path()));
        let sessions = StorefrontSessions::with(Storefronts::Gog(Arc::new(Sessions::new(
            Box::new(|| Ok(None)),
            runtimes,
        ))));
        let refused = |result: Result<(), GameSessionError>| match result {
            Err(GameSessionError::Failure { reason }) => reason,
            other => panic!("{other:?}"),
        };
        let reason = refused(sessions.overlay_state("id".into(), true, false).await);
        assert_eq!(reason, gog::Gog::INVALID_SESSION);
        let reason = refused(
            sessions
                .accept_invite(
                    "id".into(),
                    FRIEND,
                    SteamGameInvite::Lobby { lobby_id: LOBBY },
                )
                .await,
        );
        assert_eq!(reason, gog::Gog::INVALID_SESSION);
        let reason = refused(sessions.authorize_micro_txn("id".into(), 1, true).await);
        assert_eq!(reason, gog::Gog::INVALID_SESSION);
    }

    #[tokio::test]
    async fn steam_sessions_refuse_controls_without_a_running_game() {
        let root = tempfile::tempdir().unwrap();
        let runtimes = Runtimes::new(&Locations::new(root.path(), root.path(), root.path()));
        let sessions = StorefrontSessions::with(Storefronts::Steam(Arc::new(Sessions::new(
            Box::new(|| Ok(None)),
            runtimes,
        ))));
        let refused = |result: Result<(), GameSessionError>| match result {
            Err(GameSessionError::Failure { reason }) => reason,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            refused(
                sessions
                    .accept_invite("id".into(), FRIEND, SteamGameInvite::None)
                    .await
            ),
            steam::Steam::INVALID_SESSION
        );
        assert_eq!(
            refused(sessions.overlay_state("id".into(), true, true).await),
            steam::Steam::INVALID_SESSION
        );
        assert_eq!(
            refused(sessions.authorize_micro_txn("id".into(), 1, false).await),
            steam::Steam::INVALID_SESSION
        );
    }
}
