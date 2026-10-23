//! One account's installations across UniFFI, for either storefront. Rust
//! keeps the Installation receipts, runs the Transfers, launches, uninstalls
//! and Cloud maintenance, and holds the account's Cloud question; Swift
//! presents the state and forwards the player's actions. Games cross as Swift
//! names them: a Steam app id, an Epic `namespace:app` pair or a GOG product
//! id; installations as their receipt's id.
use crate::{
    CloudChoice, CloudIndicator, EpicSession, GameSessionError, GameTransfer, GogSession,
    LaunchChoices, LaunchSettings, ObeliskLocations, SteamCredentials, SteamSession,
    StorefrontSessions,
    game_session::{Storefronts, blocking, shares_runtimes},
    steam_chat::SteamGameInvite,
};
use game_runtime::{
    Locations, epic, gog,
    installations::{self, Installations, Report},
    sessions::{Sessions, Storefront as _},
    steam,
};
use obelisk_core::Platform;
use std::sync::Arc;

/// One installation of the account, as its receipt records it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Installation {
    /// Names the installation in every action.
    pub id: String,
    pub game: String,
    pub name: String,
    pub directory: String,
    pub platform: GamePlatform,
    pub total_bytes: u64,
    /// A Transfer started over it and has not succeeded since.
    pub needs_verification: bool,
    /// The storefront offers a newer build than the installed one.
    pub update_available: bool,
}

/// Which of a game's builds: the Mac build or the Windows build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GamePlatform {
    Mac,
    Windows,
}

impl From<Platform> for GamePlatform {
    fn from(platform: Platform) -> Self {
        match platform {
            Platform::Mac => Self::Mac,
            Platform::Windows => Self::Windows,
        }
    }
}

/// What a storefront offers to install of one game.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameOffer {
    /// With the depots of `language`. Steam picks the build the account can
    /// install as the Transfer starts: the Mac build when its game runs
    /// natively on Apple silicon, else the Windows build.
    Steam {
        app_id: u32,
        name: String,
        language: String,
    },
    /// Epic picks the build as Steam does.
    Epic {
        namespace: String,
        catalog_item_id: String,
        app_name: String,
        title: String,
    },
    /// GOG picks the build as Steam does.
    Gog { product_id: String, title: String },
}

/// A failure the player sees, and what it is about.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum InstallationReport {
    /// A storefront's localization key.
    Storefront { key: String },
    /// The chosen folder could not be created.
    DownloadDirectory { message: String },
    /// An uninstall failed for this reason, a localization key.
    Uninstall { reason: String },
    /// A move into the games folder failed for this reason, a localization
    /// key or the system's message.
    Move { reason: String },
    /// The games folder's disk is not connected: this folder.
    GamesFolderUnavailable { path: String },
}

/// What may start now, of one kind of action.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum GameOpening {
    Closed,
    /// It may start, for any game but these.
    Open {
        except: Vec<String>,
    },
}

/// What the Reservation lets start now.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InstallationGate {
    /// A Transfer, which waits in the download queue.
    pub modify: GameOpening,
    /// An uninstall: no Transfer, launch or other uninstall of the storefront
    /// runs.
    pub uninstall: GameOpening,
    /// A launch: a Transfer holds only its own game, and those whose launch
    /// would need one.
    pub launch: GameOpening,
    pub maintain: GameOpening,
}

/// A storage warning, a localization key; `id` changes each time one is raised.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InstallationWarning {
    pub id: u64,
    pub key: String,
}

/// Why a Cloud sync runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudSyncKind {
    /// Before a launch's game starts.
    Launch,
    /// After its game exited.
    Exit,
    /// On the player's request.
    Maintenance,
}

/// What a Cloud question asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudQuestionKind {
    /// Which saves to keep.
    Conflict,
    /// Whether to retry, cancel or launch anyway after a launch check failed.
    Failed,
}

/// The one Cloud question the account asks, until it has its answer.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudQuestion {
    pub decision_id: u64,
    pub kind: CloudQuestionKind,
    /// Localization key of what the sync reports.
    pub error: Option<String>,
    pub choices: Vec<CloudChoice>,
}

/// The account's Cloud sync running now, whichever asks.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudSync {
    pub game: String,
    pub kind: CloudSyncKind,
    /// Binds a cancellation to the sync as it shows now.
    pub decision_id: u64,
    /// It compares or transfers saves now.
    pub working: bool,
    /// Its Game session's control connection was lost: nothing reaches it.
    pub reconnecting: bool,
    /// The player may cancel it: a launch check still in progress.
    pub cancellable: bool,
    pub question: Option<CloudQuestion>,
}

/// A game's Cloud indicator.
#[derive(Clone, Debug, uniffi::Record)]
pub struct GameCloudIndicator {
    pub game: String,
    pub indicator: CloudIndicator,
}

/// Everything the account's installations present.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct InstallationsState {
    /// Grows with every change.
    pub revision: u64,
    pub installations: Vec<Installation>,
    pub transfer: Option<GameTransfer>,
    /// The game the last Transfer progress belongs to.
    pub transfer_game: Option<String>,
    pub downloading: bool,
    pub launching: bool,
    pub verifying_before_launch: bool,
    pub launch_game: Option<String>,
    pub uninstall_game: Option<String>,
    /// The game whose Transfer stopped before its receipt: the player can
    /// resume it.
    pub resumable: Option<String>,
    /// The new installations whose Transfer has not succeeded yet: the player
    /// resumes or discards each. None is an installed game.
    pub incomplete: Vec<Installation>,
    pub report: Option<InstallationReport>,
    pub warning: Option<InstallationWarning>,
    pub gate: InstallationGate,
    pub cloud_sync: Option<CloudSync>,
    /// The games whose Transfer waits in the download queue.
    pub queued: Vec<String>,
}

/// How a storefront's games and offers cross UniFFI.
pub(crate) trait Named: installations::Storefront {
    fn name(game: &Self::Game) -> String;
    /// The game Swift names `name`.
    fn named(name: &str) -> Option<Self::Game>;
    fn offer(offer: GameOffer) -> Option<installations::Offer<Self>>;
}

impl Named for steam::Steam {
    fn name(game: &u32) -> String {
        game.to_string()
    }
    fn named(name: &str) -> Option<u32> {
        name.parse().ok()
    }
    fn offer(offer: GameOffer) -> Option<installations::Offer<Self>> {
        match offer {
            GameOffer::Steam {
                app_id,
                name,
                language,
            } => Some(installations::Offer {
                product: steam::Product { app_id, language },
                name,
            }),
            _ => None,
        }
    }
}

impl Named for epic::Epic {
    fn name(game: &epic::Store) -> String {
        format!("{}:{}", game.namespace, game.app_name)
    }
    /// An Epic namespace has no colon.
    fn named(name: &str) -> Option<epic::Store> {
        let (namespace, app_name) = name.split_once(':')?;
        Some(epic::Store {
            namespace: namespace.into(),
            app_name: app_name.into(),
        })
    }
    fn offer(offer: GameOffer) -> Option<installations::Offer<Self>> {
        match offer {
            GameOffer::Epic {
                namespace,
                catalog_item_id,
                app_name,
                title,
            } => Some(installations::Offer {
                product: epic::Product {
                    namespace,
                    catalog_item_id,
                    app_name,
                },
                name: title,
            }),
            _ => None,
        }
    }
}

impl Named for gog::Gog {
    fn name(game: &String) -> String {
        game.clone()
    }
    fn named(name: &str) -> Option<String> {
        gog_kit::client::valid_product_id(name).then(|| name.to_owned())
    }
    fn offer(offer: GameOffer) -> Option<installations::Offer<Self>> {
        match offer {
            GameOffer::Gog { product_id, title } => Some(installations::Offer {
                product: gog::Product { product_id },
                name: title,
            }),
            _ => None,
        }
    }
}

/// An installation as Swift shows it, under the id its actions name.
fn installation<S: Named>(receipt: installations::Receipt<S>, updates: &[String]) -> Installation {
    let (id, game) = (receipt.id(), S::name(&receipt.game()));
    Installation {
        update_available: updates.contains(&id),
        id,
        game,
        name: receipt.name,
        directory: receipt.directory,
        platform: receipt.platform.into(),
        total_bytes: receipt.total_bytes,
        needs_verification: receipt.needs_verification,
    }
}

impl From<Report> for InstallationReport {
    fn from(report: Report) -> Self {
        match report {
            Report::Storefront(key) => Self::Storefront { key },
            Report::DownloadDirectory(message) => Self::DownloadDirectory { message },
            Report::Uninstall(reason) => Self::Uninstall { reason },
            Report::Move(reason) => Self::Move { reason },
            Report::GamesFolderUnavailable(path) => Self::GamesFolderUnavailable {
                path: path.to_string_lossy().into_owned(),
            },
        }
    }
}

fn opening<S: Named>(opening: installations::Opening<S::Game>) -> GameOpening {
    match opening {
        installations::Opening::Closed => GameOpening::Closed,
        installations::Opening::Open { except } => GameOpening::Open {
            except: except.iter().map(S::name).collect(),
        },
    }
}

fn state<S: Named>(state: installations::State<S>) -> InstallationsState {
    let name = |game: Option<S::Game>| game.as_ref().map(S::name);
    InstallationsState {
        revision: state.revision,
        installations: state
            .receipts
            .into_iter()
            .map(|receipt| installation(receipt, &state.updates))
            .collect(),
        transfer: state.transfer.map(Into::into),
        transfer_game: name(state.transfer_game),
        downloading: state.downloading,
        launching: state.launching,
        verifying_before_launch: state.verifying_before_launch,
        launch_game: name(state.launch_game),
        uninstall_game: name(state.uninstall_game),
        resumable: name(
            state
                .last_download
                .as_ref()
                .map(installations::Transfer::game),
        ),
        incomplete: state
            .incomplete
            .into_iter()
            .map(|receipt| installation(receipt, &[]))
            .collect(),
        report: state.report.map(Into::into),
        warning: state
            .warning
            .map(|(id, key)| InstallationWarning { id, key }),
        gate: InstallationGate {
            modify: opening::<S>(state.gate.modify),
            uninstall: opening::<S>(state.gate.uninstall),
            launch: opening::<S>(state.gate.launch),
            maintain: opening::<S>(state.gate.maintain),
        },
        cloud_sync: state.cloud_sync.map(|sync| CloudSync {
            game: S::name(&sync.game),
            kind: match sync.kind {
                installations::SyncKind::Launch => CloudSyncKind::Launch,
                installations::SyncKind::Exit => CloudSyncKind::Exit,
                installations::SyncKind::Maintenance => CloudSyncKind::Maintenance,
            },
            decision_id: sync.decision_id,
            working: sync.working,
            reconnecting: sync.reconnecting,
            cancellable: sync.cancellable,
            question: sync.question.map(|question| CloudQuestion {
                decision_id: question.decision_id,
                kind: match question.kind {
                    installations::QuestionKind::Conflict => CloudQuestionKind::Conflict,
                    installations::QuestionKind::Failed => CloudQuestionKind::Failed,
                },
                error: question.error,
                choices: question.choices.into_iter().map(Into::into).collect(),
            }),
        }),
        queued: state.queued.iter().map(S::name).collect(),
    }
}

fn install<S: Named>(installations: &Installations<S>, offer: GameOffer) {
    if let Some(offer) = S::offer(offer) {
        installations.install(offer);
    }
}

fn cancel<S: Named>(installations: &Installations<S>, game: &str) {
    if let Some(game) = S::named(game) {
        installations.cancel(&game);
    }
}

fn maintain_cloud<S: Named>(
    installations: &Installations<S>,
    game: &str,
    installation: Option<&str>,
) {
    if let Some(game) = S::named(game) {
        installations.maintain_cloud(game, installation);
    }
}

/// What a blocking call lost with its thread reports.
const FAILED: &str = "backend.steam.runtime.startFailed";
/// What a launch settings change that could not be kept reports.
const NOT_SAVED: &str = "backend.launchSettings.notSaved";

async fn launch_settings<S: Named>(
    installations: Installations<S>,
    installation: String,
) -> Result<Option<LaunchSettings>, GameSessionError> {
    let view = blocking(FAILED, move || {
        Ok(installations.launch_settings(&installation))
    })
    .await?;
    Ok(view.map(Into::into))
}

async fn set_launch_settings<S: Named>(
    installations: Installations<S>,
    installation: String,
    choices: LaunchChoices,
) -> Result<Option<LaunchSettings>, GameSessionError> {
    let view = blocking(NOT_SAVED, move || {
        installations
            .set_launch_settings(&installation, choices.into())
            .map_err(|_| game_runtime::sessions::Failure::Failed(NOT_SAVED.into()))
    })
    .await?;
    Ok(view.map(Into::into))
}

async fn answer<S: Named>(
    installations: Installations<S>,
    game: String,
    decision_id: u64,
    choice: CloudChoice,
) -> Result<(), GameSessionError> {
    let game = S::named(&game).ok_or_else(|| GameSessionError::new(S::INVALID_SESSION))?;
    blocking(FAILED, move || {
        installations.answer_cloud(&game, decision_id, choice.into())
    })
    .await
}

async fn indicators<S: Named>(
    installations: Installations<S>,
    games: Vec<String>,
) -> Result<Vec<GameCloudIndicator>, GameSessionError> {
    let games: Vec<_> = games.iter().filter_map(|game| S::named(game)).collect();
    let indicators = blocking(FAILED, move || installations.cloud_indicators(&games)).await?;
    Ok(indicators
        .into_iter()
        .map(|(game, indicator)| GameCloudIndicator {
            game: S::name(&game),
            indicator: indicator.into(),
        })
        .collect())
}

fn account<S: Named>(
    content: impl installations::Content<S> + 'static,
    sessions: Arc<Sessions<S>>,
    account: S::Account,
    receipts: String,
    locations: &Locations,
) -> Installations<S>
where
    S::Account: std::fmt::Display,
{
    debug_assert!(
        shares_runtimes(locations),
        "an account's installations name the runtime places of their sessions"
    );
    let queueing = installations::Queueing {
        queue: crate::downloads::queue().clone(),
        account: account.to_string(),
        name: <S as Named>::name,
    };
    Installations::new(
        Arc::new(content),
        sessions,
        account,
        receipts.into(),
        locations.home.clone(),
        crate::games_folder::shared(&locations.library),
        queueing,
    )
}

enum Account {
    Steam(Installations<steam::Steam>),
    Epic(Installations<epic::Epic>),
    Gog(Installations<gog::Gog>),
}

/// Runs `$body` with the account's installations as `$installations`,
/// whichever storefront keeps them.
macro_rules! each {
    ($account:expr, $installations:ident => $body:expr) => {
        match &$account.account {
            Account::Steam($installations) => $body,
            Account::Epic($installations) => $body,
            Account::Gog($installations) => $body,
        }
    };
}

/// One signed-in account's installed games and the actions on them.
#[derive(uniffi::Object)]
pub struct AccountInstallations {
    account: Account,
}

#[uniffi::export(async_runtime = "tokio")]
impl AccountInstallations {
    /// The installations of the Steam account `session` signed in, whose
    /// receipts `receipts` keeps, for the whole sign-in: they reach Steam
    /// through the session's CM connection of the moment.
    #[uniffi::constructor]
    pub fn steam(
        session: Arc<SteamSession>,
        credentials: SteamCredentials,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: ObeliskLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let Storefronts::Steam(sessions) = &*sessions.sessions else {
            return Err(GameSessionError::new(steam::Steam::INVALID_SESSION));
        };
        let locations: Locations = (&locations).into();
        let steam_id = credentials.steam_id;
        let content = steam::content::Content::new(
            session.account.clone(),
            game_runtime::protocol::Credentials {
                steam_id,
                account_name: credentials.account_name,
                refresh_token: credentials.refresh_token,
            },
            locations.clone(),
        );
        let installations = account(content, sessions.clone(), steam_id, receipts, &locations);
        Ok(Arc::new(Self {
            account: Account::Steam(installations),
        }))
    }

    /// The installations of the Epic account `account_id`, which `session`
    /// signed in. `language` names the game's language at launch. The
    /// session's own persistence saves the credentials their calls rotate.
    #[uniffi::constructor]
    pub fn epic(
        session: Arc<EpicSession>,
        account_id: String,
        language: String,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: ObeliskLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let Storefronts::Epic(sessions) = &*sessions.sessions else {
            return Err(GameSessionError::new(epic::Epic::INVALID_SESSION));
        };
        let locations: Locations = (&locations).into();
        // Every account call waits until the credentials it rotated are saved.
        let content = epic::content::Content::new(
            session.client.clone(),
            session.persistence.waiter(session.client.clone()),
            account_id.clone(),
            language,
            locations.clone(),
        );
        let installations = account(content, sessions.clone(), account_id, receipts, &locations);
        Ok(Arc::new(Self {
            account: Account::Epic(installations),
        }))
    }

    /// The installations of the GOG account `user_id`, which `session`
    /// signed in. `language` names the depots Transfers install, such as
    /// `fr-FR`. The session's own persistence saves the credentials their
    /// calls rotate.
    #[uniffi::constructor]
    pub fn gog(
        session: Arc<GogSession>,
        user_id: String,
        language: String,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: ObeliskLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let Storefronts::Gog(sessions) = &*sessions.sessions else {
            return Err(GameSessionError::new(gog::Gog::INVALID_SESSION));
        };
        let locations: Locations = (&locations).into();
        let content = gog::content::Content::new(
            session.client.clone(),
            session.persistence.waiter(session.client.clone()),
            language,
            locations.clone(),
        );
        let installations = account(content, sessions.clone(), user_id, receipts, &locations);
        Ok(Arc::new(Self {
            account: Account::Gog(installations),
        }))
    }

    pub fn state(&self) -> InstallationsState {
        each!(self, installations => state(installations.state()))
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> InstallationsState {
        each!(self, installations => state(installations.changed(after).await))
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        each!(self, installations => installations.set_diagnostic(running))
    }

    /// Installs what `offer` names, or updates the game's installation; a
    /// Transfer of the game that stopped resumes instead. An offer of the
    /// other storefront does nothing.
    pub fn install(&self, offer: GameOffer) {
        each!(self, installations => install(installations, offer))
    }

    /// Verifies `installation`'s files and repairs or updates them in place;
    /// a Transfer of the game that stopped resumes instead. The player asked:
    /// every file is hashed again, whatever the last Transfer recorded.
    pub fn update(&self, installation: String) {
        each!(self, installations => installations.update(&installation))
    }

    /// Asks the storefront which installations have a newer build, unless it
    /// was asked a short while ago.
    pub fn check_updates(&self) {
        each!(self, installations => installations.check_updates())
    }

    /// Whether each newer build installs as soon as it is known, rather
    /// than at the game's next launch.
    pub fn set_automatic_updates(&self, enabled: bool) {
        each!(self, installations => installations.set_automatic_updates(enabled))
    }

    /// Stops `game`'s Transfer, or its launch still preparing.
    pub fn cancel(&self, game: String) {
        each!(self, installations => cancel(installations, &game))
    }

    /// Launches `installation`, verifying it first only when it needs it.
    pub fn launch(&self, installation: String) {
        each!(self, installations => installations.launch(&installation))
    }

    /// Launches the Steam `installation` as `launch` does, its game joining
    /// `invite`: a friend's lobby or rich presence `connect` string.
    pub fn join(&self, installation: String, invite: SteamGameInvite) {
        let Account::Steam(installations) = &self.account else {
            return;
        };
        let Ok(join) = steam::Join::try_from(invite) else {
            return;
        };
        if join.valid() {
            installations.join(&installation, join);
        }
    }

    pub fn uninstall(&self, installation: String) {
        each!(self, installations => installations.uninstall(&installation))
    }

    /// Deletes the files of the incomplete `installation` and forgets it.
    pub fn discard(&self, installation: String) {
        each!(self, installations => installations.discard(&installation))
    }

    /// The launch settings of the Windows `installation`; `None` for a Mac
    /// build.
    pub async fn launch_settings(
        &self,
        installation: String,
    ) -> Result<Option<LaunchSettings>, GameSessionError> {
        each!(self, installations => launch_settings(installations.clone(), installation).await)
    }

    /// Keeps `choices` as the player's launch settings of the Windows
    /// `installation`, from its game's next launch, and returns them as they
    /// then apply.
    pub async fn set_launch_settings(
        &self,
        installation: String,
        choices: LaunchChoices,
    ) -> Result<Option<LaunchSettings>, GameSessionError> {
        each!(self, installations => {
            set_launch_settings(installations.clone(), installation, choices).await
        })
    }

    /// Stops the account's Transfer, Cloud maintenance and launch still
    /// preparing.
    pub fn signed_out(&self) {
        each!(self, installations => installations.signed_out())
    }

    /// Syncs the game's Cloud saves: those of `installation`, or, when
    /// `None`, those the uninstalled game's saved context names.
    pub fn maintain_cloud(&self, game: String, installation: Option<String>) {
        each!(self, installations => maintain_cloud(installations, &game, installation.as_deref()))
    }

    /// Answers the Cloud question the game shows with `decision_id`, its
    /// maintenance's or its Game session's.
    pub async fn answer_cloud(
        &self,
        game: String,
        decision_id: u64,
        choice: CloudChoice,
    ) -> Result<(), GameSessionError> {
        each!(self, installations => answer(installations.clone(), game, decision_id, choice).await)
    }

    /// The Cloud indicators of `games`, and of every other game of the
    /// account with pending work, which Swift does not know about.
    pub async fn cloud_indicators(
        &self,
        games: Vec<String>,
    ) -> Result<Vec<GameCloudIndicator>, GameSessionError> {
        each!(self, installations => indicators(installations.clone(), games).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn games_cross_as_swift_names_them_and_back() {
        let store = epic::Store {
            namespace: "sandbox".into(),
            app_name: "game:with:colons".into(),
        };
        let name = <epic::Epic as Named>::name(&store);
        assert_eq!(name, "sandbox:game:with:colons");
        assert_eq!(<epic::Epic as Named>::named(&name), Some(store));
        assert_eq!(<epic::Epic as Named>::named("no-colon"), None);
        assert_eq!(<steam::Steam as Named>::named("620"), Some(620));
        assert_eq!(<steam::Steam as Named>::named("sandbox:game"), None);
        assert_eq!(
            <gog::Gog as Named>::named("1207658924").as_deref(),
            Some("1207658924")
        );
        assert_eq!(<gog::Gog as Named>::named("../1207658924"), None);
        assert_eq!(<gog::Gog as Named>::named("sandbox:game"), None);
    }

    #[test]
    fn an_installation_crosses_under_the_id_its_actions_name() {
        let receipt = installations::Receipt {
            product: epic::Product {
                namespace: "sandbox".into(),
                catalog_item_id: "catalog".into(),
                app_name: "game".into(),
            },
            name: "Fixture".into(),
            directory: "/library/Games/Epic/epic-hash-Windows".into(),
            platform: Platform::Windows,
            total_bytes: 7,
            needs_verification: true,
        };
        let installation = installation::<epic::Epic>(receipt, &["sandbox:game:Windows".into()]);
        assert_eq!(installation.id, "sandbox:game:Windows");
        assert_eq!(installation.game, "sandbox:game");
        assert_eq!(installation.platform, GamePlatform::Windows);
        assert!(installation.needs_verification);
        assert!(installation.update_available);
        let offer = GameOffer::Steam {
            app_id: 620,
            name: "Portal 2".into(),
            language: "french".into(),
        };
        assert_eq!(
            <epic::Epic as Named>::offer(offer.clone()),
            None,
            "an offer of the other storefront is refused"
        );
        assert_eq!(
            <steam::Steam as Named>::offer(offer).map(|offer| offer.product.app_id),
            Some(620)
        );
    }
}
