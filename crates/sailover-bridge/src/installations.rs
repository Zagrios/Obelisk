//! One account's installations across UniFFI, per storefront. Rust keeps the
//! Installation receipts, runs the Transfers, launches, uninstalls and Cloud
//! maintenance, and holds the account's Cloud question; Swift presents the
//! state and forwards the player's actions. Each storefront's part of them is
//! game-runtime's.
use crate::{
    CloudChoice, CloudStatus, EpicCloudIndicator, EpicSession, GameSessionError, GameTransfer,
    SailOverLocations, SteamCloudIndicator, SteamCredentials, SteamSession, StorefrontSessions,
    game_session::Storefronts,
};
use game_runtime::{
    Locations, epic,
    installations::{self, Content, Installations, Report},
    sessions::{Failure, Sessions},
    steam,
};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamReceipt {
    pub app_id: u32,
    pub name: String,
    pub directory: String,
    pub platform: String,
    pub language: String,
    pub total_bytes: u64,
    /// A Transfer started over this installation and has not succeeded since.
    pub needs_verification: Option<bool>,
}

/// A Steam game SailOver can install: its build for `platform`, with the
/// depots of `language`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamOffer {
    pub app_id: u32,
    pub name: String,
    pub platform: String,
    pub language: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct EpicReceipt {
    pub namespace: String,
    pub app_name: String,
    pub name: String,
    pub directory: String,
    pub platform: String,
    pub total_bytes: u64,
    /// A Transfer started over this installation and has not succeeded since.
    pub needs_verification: Option<bool>,
}

/// An Epic game SailOver can install: its build for `platform`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct EpicOffer {
    pub namespace: String,
    pub catalog_item_id: String,
    pub app_name: String,
    pub title: String,
    pub platform: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct EpicGameId {
    pub namespace: String,
    pub app_name: String,
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
}

/// What may start now, of one kind of action. Games are named as Swift names
/// them: a Steam app id, or an Epic `namespace:app` pair.
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
    /// A Transfer or uninstall.
    pub modify: GameOpening,
    /// A launch, of any game a Transfer may touch.
    pub launch: bool,
    pub maintain: GameOpening,
}

fn gate<G>(gate: installations::Gate<G>, name: impl Fn(&G) -> String) -> InstallationGate {
    let opening = |opening: installations::Opening<G>| match opening {
        installations::Opening::Closed => GameOpening::Closed,
        installations::Opening::Open { except } => GameOpening::Open {
            except: except.iter().map(&name).collect(),
        },
    };
    InstallationGate {
        modify: opening(gate.modify),
        launch: gate.launch,
        maintain: opening(gate.maintain),
    }
}

fn epic_name(game: &epic::Store) -> String {
    format!("{}:{}", game.namespace, game.app_name)
}

/// A storage warning, a localization key; `id` changes each time one is raised.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InstallationWarning {
    pub id: u64,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SteamInstallationsState {
    /// Grows with every change.
    pub revision: u64,
    pub receipts: Vec<SteamReceipt>,
    pub transfer: Option<GameTransfer>,
    pub transfer_game: Option<u32>,
    pub downloading: bool,
    pub launching: bool,
    pub verifying_before_launch: bool,
    pub launch_game: Option<u32>,
    pub uninstalling: bool,
    pub uninstall_game: Option<u32>,
    /// The game whose Transfer stopped before its receipt: the player can
    /// resume it.
    pub resumable: Option<String>,
    pub report: Option<InstallationReport>,
    pub warning: Option<InstallationWarning>,
    pub maintenance: Option<SteamCloudMaintenance>,
    pub gate: InstallationGate,
}

/// Cloud maintenance of one game: its sync's progress and the Cloud question
/// it waits on.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamCloudMaintenance {
    pub app_id: u32,
    pub cloud: CloudStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct EpicCloudMaintenance {
    pub game: EpicGameId,
    pub cloud: CloudStatus,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct EpicInstallationsState {
    /// Grows with every change.
    pub revision: u64,
    pub receipts: Vec<EpicReceipt>,
    pub transfer: Option<GameTransfer>,
    pub transfer_game: Option<EpicGameId>,
    pub downloading: bool,
    pub launching: bool,
    pub verifying_before_launch: bool,
    pub launch_game: Option<EpicGameId>,
    pub uninstalling: bool,
    pub uninstall_game: Option<EpicGameId>,
    /// The game whose Transfer stopped before its receipt: the player can
    /// resume it.
    pub resumable: Option<String>,
    pub report: Option<InstallationReport>,
    pub warning: Option<InstallationWarning>,
    pub maintenance: Option<EpicCloudMaintenance>,
    pub gate: InstallationGate,
}

impl From<Report> for InstallationReport {
    fn from(report: Report) -> Self {
        match report {
            Report::Storefront(key) => Self::Storefront { key },
            Report::DownloadDirectory(message) => Self::DownloadDirectory { message },
            Report::Uninstall(reason) => Self::Uninstall { reason },
        }
    }
}

fn warning(warning: Option<(u64, String)>) -> Option<InstallationWarning> {
    warning.map(|(id, key)| InstallationWarning { id, key })
}

impl From<steam::Receipt> for SteamReceipt {
    fn from(receipt: steam::Receipt) -> Self {
        Self {
            app_id: receipt.app_id,
            name: receipt.name,
            directory: receipt.directory,
            platform: receipt.platform,
            language: receipt.language,
            total_bytes: receipt.total_bytes,
            needs_verification: receipt.needs_verification,
        }
    }
}
impl From<SteamOffer> for steam::Offer {
    fn from(offer: SteamOffer) -> Self {
        Self {
            app_id: offer.app_id,
            name: offer.name,
            platform: offer.platform,
            language: offer.language,
        }
    }
}
impl From<epic::Receipt> for EpicReceipt {
    fn from(receipt: epic::Receipt) -> Self {
        Self {
            namespace: receipt.namespace,
            app_name: receipt.app_name,
            name: receipt.name,
            directory: receipt.directory,
            platform: receipt.platform,
            total_bytes: receipt.total_bytes,
            needs_verification: receipt.needs_verification,
        }
    }
}
impl From<EpicOffer> for epic::Offer {
    fn from(offer: EpicOffer) -> Self {
        Self {
            namespace: offer.namespace,
            catalog_item_id: offer.catalog_item_id,
            app_name: offer.app_name,
            title: offer.title,
            platform: offer.platform,
        }
    }
}
impl From<epic::Store> for EpicGameId {
    fn from(store: epic::Store) -> Self {
        Self {
            namespace: store.namespace,
            app_name: store.app_name,
        }
    }
}
impl From<EpicGameId> for epic::Store {
    fn from(game: EpicGameId) -> Self {
        Self {
            namespace: game.namespace,
            app_name: game.app_name,
        }
    }
}

/// A blocking call off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Failure::Failed("backend.steam.runtime.startFailed".into()))
}

fn steam_sessions(
    sessions: &StorefrontSessions,
) -> Result<Arc<Sessions<steam::Steam>>, GameSessionError> {
    match &*sessions.sessions {
        Storefronts::Steam(sessions) => Ok(sessions.clone()),
        Storefronts::Epic(_) => Err(GameSessionError::new(
            "backend.steam.runtime.invalidSession",
        )),
    }
}

fn epic_sessions(
    sessions: &StorefrontSessions,
) -> Result<Arc<Sessions<epic::Epic>>, GameSessionError> {
    match &*sessions.sessions {
        Storefronts::Epic(sessions) => Ok(sessions.clone()),
        Storefronts::Steam(_) => Err(GameSessionError::new("backend.epic.cloud.invalidSession")),
    }
}

fn account_installations<S: installations::Storefront>(
    content: Arc<dyn Content<S>>,
    sessions: Arc<Sessions<S>>,
    account: S::Account,
    receipts: String,
    locations: &Locations,
) -> Installations<S> {
    Installations::new(
        content,
        sessions,
        account,
        receipts.into(),
        locations.library.clone(),
        locations.home.clone(),
    )
}

/// One Steam account's installed games and the actions on them.
#[derive(uniffi::Object)]
pub struct SteamAccountInstallations {
    installations: Installations<steam::Steam>,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamAccountInstallations {
    /// The installations of the account `session` signed in, whose receipts
    /// `receipts` keeps.
    #[uniffi::constructor]
    pub fn new(
        session: Arc<SteamSession>,
        credentials: SteamCredentials,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: SailOverLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let locations: Locations = (&locations).into();
        let account = credentials.steam_id;
        let content = steam::content::Content::new(
            session.client.clone(),
            game_runtime::protocol::Credentials {
                steam_id: credentials.steam_id,
                account_name: credentials.account_name,
                refresh_token: credentials.refresh_token,
            },
            locations.clone(),
        );
        Ok(Arc::new(Self {
            installations: account_installations(
                Arc::new(content),
                steam_sessions(&sessions)?,
                account,
                receipts,
                &locations,
            ),
        }))
    }

    pub fn state(&self) -> SteamInstallationsState {
        steam_state(self.installations.state())
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> SteamInstallationsState {
        steam_state(self.installations.changed(after).await)
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        self.installations.set_diagnostic(running);
    }

    /// Installs what `offer` names, or updates the installation of its
    /// platform; a Transfer of the game that stopped resumes instead.
    pub fn install(&self, offer: SteamOffer) {
        self.installations.install(offer.into());
    }

    /// Updates the installation `receipt` names in place.
    pub fn update(&self, receipt: String) {
        self.installations.update(&receipt);
    }

    /// Stops the Transfer, and a launch still preparing.
    pub fn cancel(&self) {
        self.installations.cancel();
    }

    /// Launches the installation `receipt` names, verifying it first when
    /// `verify` asks for it or its receipt needs it.
    pub fn launch(&self, receipt: String, verify: bool) {
        self.installations.launch(&receipt, verify);
    }

    pub fn uninstall(&self, receipt: String) {
        self.installations.uninstall(&receipt);
    }

    /// Stops the account's Transfer, Cloud maintenance and launch still
    /// preparing.
    pub fn signed_out(&self) {
        self.installations.signed_out();
    }

    /// Syncs the game's Cloud saves: those of the installation `receipt`
    /// names, or an uninstalled game's when `None`.
    pub fn maintain_cloud(&self, app_id: u32, receipt: Option<String>) {
        self.installations
            .maintain_cloud(app_id, receipt.as_deref());
    }

    /// Answers the Cloud question the game shows with `decision_id`, its
    /// maintenance's or its Game session's.
    pub async fn answer_cloud(
        &self,
        app_id: u32,
        decision_id: u64,
        choice: CloudChoice,
    ) -> Result<(), GameSessionError> {
        let installations = self.installations.clone();
        call(move || installations.answer_cloud(&app_id, decision_id, choice.into())).await
    }

    /// The Cloud indicators of `app_ids`, and of every other game of the
    /// account with pending work, which Swift does not know about.
    pub async fn cloud_indicators(
        &self,
        app_ids: Vec<u32>,
    ) -> Result<Vec<SteamCloudIndicator>, GameSessionError> {
        let installations = self.installations.clone();
        let indicators = call(move || installations.cloud_indicators(&app_ids)).await?;
        Ok(indicators
            .into_iter()
            .map(|(app_id, indicator)| SteamCloudIndicator {
                app_id,
                indicator: indicator.into(),
            })
            .collect())
    }
}

/// A blocking call off the async runtime: an answer to a Game session's
/// question reaches its supervisor, an indicator read reads journals.
async fn call<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, GameSessionError> {
    Ok(blocking(work).await??)
}

fn steam_state(state: installations::State<steam::Steam>) -> SteamInstallationsState {
    SteamInstallationsState {
        revision: state.revision,
        receipts: state.receipts.into_iter().map(Into::into).collect(),
        transfer: state.transfer.map(Into::into),
        transfer_game: state.transfer_game,
        downloading: state.downloading,
        launching: state.launching,
        verifying_before_launch: state.verifying_before_launch,
        launch_game: state.launch_game,
        uninstalling: state.uninstalling,
        uninstall_game: state.uninstall_game,
        resumable: state.last_download.map(|last| last.app_id.to_string()),
        report: state.report.map(Into::into),
        warning: warning(state.warning),
        maintenance: state.maintenance.map(|maintenance| SteamCloudMaintenance {
            app_id: maintenance.game,
            cloud: maintenance.cloud.into(),
        }),
        gate: gate(state.gate, u32::to_string),
    }
}

/// One Epic account's installed games and the actions on them.
#[derive(uniffi::Object)]
pub struct EpicAccountInstallations {
    installations: Installations<epic::Epic>,
}

#[uniffi::export(async_runtime = "tokio")]
impl EpicAccountInstallations {
    /// The installations of `account_id`, which `session` signed in.
    /// `language` names the game's language at launch. The session's own
    /// persistence saves the credentials their calls rotate.
    #[uniffi::constructor]
    pub fn new(
        session: Arc<EpicSession>,
        account_id: String,
        language: String,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: SailOverLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let locations: Locations = (&locations).into();
        // Every account call waits until the credentials it rotated are saved.
        let (persistence, client) = (session.persistence.clone(), session.client.clone());
        let saved = Arc::new(move || persistence.wait(|| client.credentials_revision()));
        let content = epic::content::Content::new(
            session.client.clone(),
            saved,
            account_id.clone(),
            language,
            locations.clone(),
        );
        Ok(Arc::new(Self {
            installations: account_installations(
                Arc::new(content),
                epic_sessions(&sessions)?,
                account_id,
                receipts,
                &locations,
            ),
        }))
    }

    pub fn state(&self) -> EpicInstallationsState {
        epic_state(self.installations.state())
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> EpicInstallationsState {
        epic_state(self.installations.changed(after).await)
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        self.installations.set_diagnostic(running);
    }

    /// Installs what `offer` names, or updates the installation of its
    /// platform; a Transfer of the game that stopped resumes instead.
    pub fn install(&self, offer: EpicOffer) {
        self.installations.install(offer.into());
    }

    /// Updates the installation `receipt` names in place.
    pub fn update(&self, receipt: String) {
        self.installations.update(&receipt);
    }

    /// Stops the Transfer, and a launch still preparing.
    pub fn cancel(&self) {
        self.installations.cancel();
    }

    /// Launches the installation `receipt` names, verifying it first when
    /// `verify` asks for it or its receipt needs it.
    pub fn launch(&self, receipt: String, verify: bool) {
        self.installations.launch(&receipt, verify);
    }

    pub fn uninstall(&self, receipt: String) {
        self.installations.uninstall(&receipt);
    }

    /// Stops the account's Transfer, Cloud maintenance and launch still
    /// preparing.
    pub fn signed_out(&self) {
        self.installations.signed_out();
    }

    /// Syncs the game's Cloud saves: those of the installation `receipt`
    /// names, or, when `None`, those the uninstalled game's journal saved.
    pub fn maintain_cloud(&self, game: EpicGameId, receipt: Option<String>) {
        self.installations
            .maintain_cloud(game.into(), receipt.as_deref());
    }

    /// Answers the Cloud question the game shows with `decision_id`, its
    /// maintenance's or its Game session's.
    pub async fn answer_cloud(
        &self,
        game: EpicGameId,
        decision_id: u64,
        choice: CloudChoice,
    ) -> Result<(), GameSessionError> {
        let installations = self.installations.clone();
        call(move || installations.answer_cloud(&game.into(), decision_id, choice.into())).await
    }

    /// The Cloud indicators of `games`, and of every other game of the
    /// account with pending work, which Swift does not know about.
    pub async fn cloud_indicators(
        &self,
        games: Vec<EpicGameId>,
    ) -> Result<Vec<EpicCloudIndicator>, GameSessionError> {
        let installations = self.installations.clone();
        let games: Vec<epic::Store> = games.into_iter().map(Into::into).collect();
        let indicators = call(move || installations.cloud_indicators(&games)).await?;
        Ok(indicators
            .into_iter()
            .map(|(game, indicator)| EpicCloudIndicator {
                game: game.into(),
                indicator: indicator.into(),
            })
            .collect())
    }
}

fn epic_state(state: installations::State<epic::Epic>) -> EpicInstallationsState {
    EpicInstallationsState {
        revision: state.revision,
        receipts: state.receipts.into_iter().map(Into::into).collect(),
        transfer: state.transfer.map(Into::into),
        transfer_game: state.transfer_game.map(Into::into),
        downloading: state.downloading,
        launching: state.launching,
        verifying_before_launch: state.verifying_before_launch,
        launch_game: state.launch_game.map(Into::into),
        uninstalling: state.uninstalling,
        uninstall_game: state.uninstall_game.map(Into::into),
        resumable: state.last_download.map(|last| {
            epic_name(&epic::Store {
                namespace: last.namespace,
                app_name: last.app_name,
            })
        }),
        report: state.report.map(Into::into),
        warning: warning(state.warning),
        maintenance: state.maintenance.map(|maintenance| EpicCloudMaintenance {
            game: maintenance.game.into(),
            cloud: maintenance.cloud.into(),
        }),
        gate: gate(state.gate, epic_name),
    }
}
