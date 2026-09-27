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

/// A Steam Transfer: `language` picks the depots; `directory` is the parent
/// folder the installation goes in.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamDownloadRequest {
    pub app_id: u32,
    pub name: String,
    pub platform: String,
    pub language: String,
    pub directory: String,
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

/// An Epic Transfer of one game's build; `directory` is the parent folder the
/// installation goes in.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct EpicDownloadRequest {
    pub namespace: String,
    pub catalog_item_id: String,
    pub app_name: String,
    pub title: String,
    pub platform: String,
    pub directory: String,
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
    /// A Transfer that stopped before its receipt, which the player can resume.
    pub last_download: Option<SteamDownloadRequest>,
    pub report: Option<InstallationReport>,
    pub warning: Option<InstallationWarning>,
    pub maintenance: Option<SteamCloudMaintenance>,
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
    /// A Transfer that stopped before its receipt, which the player can resume.
    pub last_download: Option<EpicDownloadRequest>,
    pub report: Option<InstallationReport>,
    pub warning: Option<InstallationWarning>,
    pub maintenance: Option<EpicCloudMaintenance>,
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
impl From<SteamReceipt> for steam::Receipt {
    fn from(receipt: SteamReceipt) -> Self {
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
impl From<steam::Download> for SteamDownloadRequest {
    fn from(request: steam::Download) -> Self {
        Self {
            app_id: request.app_id,
            name: request.name,
            platform: request.platform,
            language: request.language,
            directory: request.directory.to_string_lossy().into_owned(),
        }
    }
}
impl From<SteamDownloadRequest> for steam::Download {
    fn from(request: SteamDownloadRequest) -> Self {
        Self {
            app_id: request.app_id,
            name: request.name,
            platform: request.platform,
            language: request.language,
            directory: request.directory.into(),
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
impl From<EpicReceipt> for epic::Receipt {
    fn from(receipt: EpicReceipt) -> Self {
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
impl From<epic::Download> for EpicDownloadRequest {
    fn from(request: epic::Download) -> Self {
        Self {
            namespace: request.namespace,
            catalog_item_id: request.catalog_item_id,
            app_name: request.app_name,
            title: request.title,
            platform: request.platform,
            directory: request.directory.to_string_lossy().into_owned(),
        }
    }
}
impl From<EpicDownloadRequest> for epic::Download {
    fn from(request: EpicDownloadRequest) -> Self {
        Self {
            namespace: request.namespace,
            catalog_item_id: request.catalog_item_id,
            app_name: request.app_name,
            title: request.title,
            platform: request.platform,
            directory: request.directory.into(),
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

    /// Whether an action may touch the game's installation now.
    pub fn can_modify(&self, app_id: u32) -> bool {
        self.installations.can_modify(&app_id)
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        self.installations.set_diagnostic(running);
    }

    pub fn download(&self, request: SteamDownloadRequest) {
        self.installations.download(request.into());
    }

    pub fn resume_download(&self) {
        self.installations.resume_download();
    }

    /// Stops the Transfer, and a launch still preparing.
    pub fn cancel(&self) {
        self.installations.cancel();
    }

    /// Launches the installed game, verifying it first with `verification`
    /// when `verify` asks for it or its receipt needs it.
    pub fn launch(&self, receipt: SteamReceipt, verification: SteamDownloadRequest, verify: bool) {
        self.installations
            .launch(receipt.into(), verification.into(), verify);
    }

    pub fn uninstall(&self, receipt: SteamReceipt, request: SteamDownloadRequest) {
        self.installations.uninstall(receipt.into(), request.into());
    }

    /// Stops the account's Transfer, Cloud maintenance and launch still
    /// preparing.
    pub fn signed_out(&self) {
        self.installations.signed_out();
    }

    /// Whether the player may sync the game's Cloud saves now.
    pub fn can_maintain_cloud(&self, app_id: u32) -> bool {
        self.installations.can_maintain_cloud(&app_id)
    }

    /// Syncs the game's Cloud saves: those of the installation `receipt`
    /// records, with the Transfer `request` names, or an uninstalled game's
    /// when both are `None`.
    pub fn maintain_cloud(
        &self,
        app_id: u32,
        receipt: Option<SteamReceipt>,
        request: Option<SteamDownloadRequest>,
    ) {
        let installed = receipt.map(Into::into).zip(request.map(Into::into));
        self.installations.maintain_cloud(app_id, installed);
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
        last_download: state.last_download.map(Into::into),
        report: state.report.map(Into::into),
        warning: warning(state.warning),
        maintenance: state.maintenance.map(|maintenance| SteamCloudMaintenance {
            app_id: maintenance.game,
            cloud: maintenance.cloud.into(),
        }),
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

    /// Whether an action may touch the game's installation now.
    pub fn can_modify(&self, game: EpicGameId) -> bool {
        self.installations.can_modify(&game.into())
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        self.installations.set_diagnostic(running);
    }

    pub fn download(&self, request: EpicDownloadRequest) {
        self.installations.download(request.into());
    }

    pub fn resume_download(&self) {
        self.installations.resume_download();
    }

    /// Stops the Transfer, and a launch still preparing.
    pub fn cancel(&self) {
        self.installations.cancel();
    }

    /// Launches the installed build, verifying it first with `verification`
    /// when `verify` asks for it or its receipt needs it.
    pub fn launch(&self, receipt: EpicReceipt, verification: EpicDownloadRequest, verify: bool) {
        self.installations
            .launch(receipt.into(), verification.into(), verify);
    }

    pub fn uninstall(&self, receipt: EpicReceipt, request: EpicDownloadRequest) {
        self.installations.uninstall(receipt.into(), request.into());
    }

    /// Stops the account's Transfer, Cloud maintenance and launch still
    /// preparing.
    pub fn signed_out(&self) {
        self.installations.signed_out();
    }

    /// Whether the player may sync the game's Cloud saves now.
    pub fn can_maintain_cloud(&self, game: EpicGameId) -> bool {
        self.installations.can_maintain_cloud(&game.into())
    }

    /// Syncs the game's Cloud saves: those of the build `receipt` records,
    /// with the Transfer `request` names, or, when both are `None`, those the
    /// uninstalled game's journal saved.
    pub fn maintain_cloud(
        &self,
        game: EpicGameId,
        receipt: Option<EpicReceipt>,
        request: Option<EpicDownloadRequest>,
    ) {
        let installed = receipt.map(Into::into).zip(request.map(Into::into));
        self.installations.maintain_cloud(game.into(), installed);
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
        last_download: state.last_download.map(Into::into),
        report: state.report.map(Into::into),
        warning: warning(state.warning),
        maintenance: state.maintenance.map(|maintenance| EpicCloudMaintenance {
            game: maintenance.game.into(),
            cloud: maintenance.cloud.into(),
        }),
    }
}
