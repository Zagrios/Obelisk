//! One account's installations across UniFFI, per storefront. Rust keeps the
//! Installation receipts, runs the Transfers, launches, uninstalls and Cloud
//! maintenance, and holds the account's Cloud question; Swift presents the
//! state and forwards the player's actions. A storefront's content is a
//! foreign trait too, so tests can stand in for its session.
use crate::{
    CloudChoice, CloudStatus, CloudSync, EpicCloudConfiguration, EpicCloudIndicator, EpicError,
    EpicInstallation, EpicSession, GameSessionError, GameSupervisor, GameTransfer,
    SailOverLocations, SteamCloudConfiguration, SteamCloudIndicator, SteamCredentials, SteamError,
    SteamInstallation, SteamLaunchPlan, SteamSession, StorefrontSessions,
    game_session::{Foreign, Storefronts},
};
use game_runtime::{
    Locations, cloud, cloud_status, epic,
    installations::{self, BoxFuture, Content, Installations, Report},
    sessions::{Admission, Failure, Handoff, Sessions, Supervised},
    steam,
};
use sailover_core::installation::TransferProgress;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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

/// A Steam account's storefront session, as its installations use it. The
/// real one is Rust's; tests stand in their own. Every call may block.
#[uniffi::export(with_foreign)]
pub trait SteamContent: Send + Sync {
    fn download(
        &self,
        app_id: u32,
        platform: String,
        language: String,
        directory: String,
    ) -> Result<SteamInstallation, SteamError>;
    fn download_progress(&self) -> Option<GameTransfer>;
    fn cancel_download(&self);
    fn launch_plan(
        &self,
        app_id: u32,
        platform: String,
        directory: String,
    ) -> Result<SteamLaunchPlan, SteamError>;
    /// The supervisor that accepted the game `plan` names.
    fn hand_off(&self, plan: SteamLaunchPlan) -> Result<Arc<dyn GameSupervisor>, GameSessionError>;
    fn archive_cloud(&self, config: SteamCloudConfiguration) -> Result<(), SteamError>;
    /// Syncs the game's Cloud saves on the player's request: those `config`
    /// names, or an uninstalled game's when `None`.
    fn maintain_cloud(
        &self,
        app_id: u32,
        config: Option<SteamCloudConfiguration>,
        sync: Arc<CloudSync>,
    ) -> Result<(), SteamError>;
}

/// Saves the Epic account's current credentials, which account calls rotate.
/// It may block.
#[uniffi::export(with_foreign)]
pub trait EpicCredentialSave: Send + Sync {
    fn save(&self);
}

/// An Epic account's storefront session, as its installations use it. The
/// real one is Rust's; tests stand in their own. Every call may block.
#[uniffi::export(with_foreign)]
pub trait EpicContent: Send + Sync {
    /// Renews the account's credentials before a long account call.
    fn prepare(&self) -> Result<(), EpicError>;
    fn download(
        &self,
        namespace: String,
        catalog_item_id: String,
        app_name: String,
        platform: String,
        directory: String,
    ) -> Result<EpicInstallation, EpicError>;
    fn download_progress(&self) -> Option<GameTransfer>;
    fn cancel_download(&self);
    /// The supervisor that accepted the build `configuration` names.
    fn hand_off(
        &self,
        configuration: EpicCloudConfiguration,
    ) -> Result<Arc<dyn GameSupervisor>, GameSessionError>;
    fn archive_cloud(&self, configuration: EpicCloudConfiguration) -> Result<(), EpicError>;
    /// Syncs the Cloud saves of the build `configuration` names, on the
    /// player's request.
    fn maintain_cloud(
        &self,
        configuration: EpicCloudConfiguration,
        sync: Arc<CloudSync>,
    ) -> Result<(), EpicError>;
}

/// A blocking call off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Failure::Failed("backend.steam.runtime.startFailed".into()))
}

fn steam_failure(error: SteamError) -> Failure {
    match error {
        SteamError::Cancelled => Failure::Cancelled,
        error => Failure::Failed(error.to_string()),
    }
}

fn epic_failure(error: impl Into<EpicError>) -> Failure {
    Failure::Failed(error.into().to_string())
}

/// A foreign supervisor, once the handoff that returns it ends.
fn foreign_supervisor<P: crate::game_session::Bridged>(
    handoff: Result<Arc<dyn GameSupervisor>, GameSessionError>,
) -> Result<Arc<dyn Supervised<P>>, Handoff> {
    match handoff {
        Ok(supervisor) => Ok(Arc::new(Foreign(supervisor))),
        Err(GameSessionError::Cancelled) => Err(Handoff::Refused(Failure::Cancelled)),
        Err(GameSessionError::Failure { reason }) => Err(Handoff::Refused(Failure::Failed(reason))),
    }
}

/// The Wine prefix of a Windows installation, where its saves live.
fn prefix(locations: &Locations, windows: bool, directory: &str) -> Option<PathBuf> {
    windows
        .then(|| game_runtime::wine::prefix(&locations.wine_prefixes, Path::new(directory)).ok())
        .flatten()
}

fn remove_prefix(
    locations: &Locations,
    windows: bool,
    directory: &str,
) -> Option<Result<(), String>> {
    windows.then(|| {
        game_runtime::wine::remove(locations, Path::new(directory))
            .map_err(|error| error.to_string())
    })
}

/// What Steam's content adapters share: the account, its locations and the
/// installation's Cloud configuration.
struct SteamLocal {
    account: u64,
    locations: Locations,
}
impl SteamLocal {
    fn cloud(&self, receipt: &steam::Receipt) -> steam::cloud::Config {
        steam::cloud::Config {
            root: self.locations.cloud_archive.clone(),
            home: self.locations.home.clone(),
            install: receipt.directory.clone().into(),
            app_id: receipt.app_id,
            prefix: prefix(
                &self.locations,
                receipt.platform == "windows",
                &receipt.directory,
            ),
        }
    }
    fn configuration(&self, receipt: &steam::Receipt) -> SteamCloudConfiguration {
        let config = self.cloud(receipt);
        SteamCloudConfiguration {
            root: config.root.to_string_lossy().into_owned(),
            home: config.home.to_string_lossy().into_owned(),
            install: receipt.directory.clone(),
            app_id: receipt.app_id,
            prefix: config
                .prefix
                .map(|prefix| prefix.to_string_lossy().into_owned()),
        }
    }
    async fn removed(&self, receipt: &steam::Receipt) -> Result<(), Failure> {
        let (config, account) = (self.cloud(receipt), self.account);
        blocking(move || steam::cloud::removed(&config, account))
            .await?
            .map_err(|failure| Failure::Failed(failure.key().into()))
    }
    fn directory_name(receipt: &steam::Receipt) -> String {
        format!("steam-{}-{}", receipt.app_id, receipt.platform)
    }
    /// What maintenance of `app_id` syncs: its installation, or, uninstalled,
    /// its saved context.
    fn maintenance(
        &self,
        app_id: u32,
        receipt: Option<&steam::Receipt>,
    ) -> steam::cloud::MaintenanceTarget {
        match receipt {
            Some(receipt) => steam::cloud::MaintenanceTarget::Installed(self.cloud(receipt)),
            None => steam::cloud::MaintenanceTarget::Uninstalled {
                root: self.locations.cloud_archive.clone(),
                app_id,
            },
        }
    }
    fn indicators(&self, games: &[u32]) -> Result<Vec<(u32, cloud_status::Indicator)>, Failure> {
        steam::cloud::indicators(&self.locations.cloud_archive, self.account, games)
            .map_err(|failure| Failure::Failed(failure.key().into()))
    }
    fn remove_prefix(&self, receipt: &steam::Receipt) -> Option<Result<(), String>> {
        remove_prefix(
            &self.locations,
            receipt.platform == "windows",
            &receipt.directory,
        )
    }
}

/// The signed-in Steam session.
struct SteamSessionContent {
    session: Arc<SteamSession>,
    credentials: SteamCredentials,
    local: SteamLocal,
}

impl Content<steam::Steam> for SteamSessionContent {
    fn download<'a>(
        &'a self,
        request: &'a steam::Download,
    ) -> BoxFuture<'a, Result<steam::Receipt, Failure>> {
        Box::pin(async move {
            let installed = self
                .session
                .download(
                    request.app_id,
                    request.platform.clone(),
                    request.language.clone(),
                    request.directory.to_string_lossy().into_owned(),
                )
                .await
                .map_err(steam_failure)?;
            Ok(steam_receipt(installed))
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.session.client.progress()
    }
    fn cancel(&self) {
        self.session.client.cancel_download();
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a steam::Receipt,
        _: &'a steam::Download,
        admission: Admission<steam::Steam>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let plan = self
                .session
                .launch_plan(
                    receipt.app_id,
                    receipt.platform.clone(),
                    receipt.directory.clone(),
                )
                .await
                .map_err(steam_failure)?;
            if admission.cancelled() {
                return Err(Failure::Cancelled);
            }
            let handoff = steam::hand_off(
                admission.canceller(),
                self.local.locations.clone(),
                game_runtime::protocol::Credentials {
                    steam_id: self.credentials.steam_id,
                    account_name: self.credentials.account_name.clone(),
                    refresh_token: self.credentials.refresh_token.clone(),
                },
                plan.app_id,
                plan.platform,
                PathBuf::from(plan.install_directory),
                plan.target.into(),
            );
            admission.launch(handoff).await
        })
    }
    fn archive<'a>(
        &'a self,
        receipt: &'a steam::Receipt,
        _: &'a steam::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.session
                .archive_cloud(self.local.configuration(receipt))
                .await
                .map_err(steam_failure)
        })
    }
    fn removed<'a>(
        &'a self,
        receipt: &'a steam::Receipt,
        _: &'a steam::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.local.removed(receipt))
    }
    fn directory_name(&self, receipt: &steam::Receipt, _: &steam::Download) -> String {
        SteamLocal::directory_name(receipt)
    }
    fn remove_prefix(&self, receipt: &steam::Receipt) -> Option<Result<(), String>> {
        self.local.remove_prefix(receipt)
    }
    fn maintain<'a>(
        &'a self,
        game: &'a u32,
        installed: Option<(&'a steam::Receipt, &'a steam::Download)>,
        control: Arc<cloud::Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let target = self
                .local
                .maintenance(*game, installed.map(|(receipt, _)| receipt));
            let credentials = game_runtime::protocol::Credentials {
                steam_id: self.credentials.steam_id,
                account_name: self.credentials.account_name.clone(),
                refresh_token: self.credentials.refresh_token.clone(),
            };
            let locations = self.local.locations.clone();
            blocking(move || {
                // Its idle guard keeps every launch of Steam out meanwhile.
                let idle = steam::Client::idle_guard(&locations.steam_sessions, &locations.helper)
                    .map_err(|error| Failure::Failed(error.to_string()))?;
                steam::cloud::maintain(target, &credentials, control, &idle).map_err(|failure| {
                    match failure {
                        steam::cloud::Failure::Cancelled => Failure::Cancelled,
                        failure => Failure::Failed(failure.key().into()),
                    }
                })
            })
            .await?
        })
    }
    fn indicators(&self, games: &[u32]) -> Result<Vec<(u32, cloud_status::Indicator)>, Failure> {
        self.local.indicators(games)
    }
}

fn steam_receipt(installed: SteamInstallation) -> steam::Receipt {
    steam::Receipt {
        app_id: installed.app_id,
        name: installed.name,
        directory: installed.directory,
        platform: installed.platform,
        language: installed.language,
        total_bytes: installed.total_bytes,
        needs_verification: None,
    }
}

/// A Steam session a test stands in.
struct ForeignSteamContent {
    content: Arc<dyn SteamContent>,
    local: SteamLocal,
}

impl Content<steam::Steam> for ForeignSteamContent {
    fn download<'a>(
        &'a self,
        request: &'a steam::Download,
    ) -> BoxFuture<'a, Result<steam::Receipt, Failure>> {
        Box::pin(async move {
            let (content, request) = (self.content.clone(), request.clone());
            let installed = blocking(move || {
                content.download(
                    request.app_id,
                    request.platform,
                    request.language,
                    request.directory.to_string_lossy().into_owned(),
                )
            })
            .await?
            .map_err(steam_failure)?;
            Ok(steam_receipt(installed))
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.content.download_progress().map(Into::into)
    }
    fn cancel(&self) {
        self.content.cancel_download();
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a steam::Receipt,
        _: &'a steam::Download,
        admission: Admission<steam::Steam>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let (content, receipt) = (self.content.clone(), receipt.clone());
            let plan = blocking(move || {
                content.launch_plan(receipt.app_id, receipt.platform, receipt.directory)
            })
            .await?
            .map_err(steam_failure)?;
            if admission.cancelled() {
                return Err(Failure::Cancelled);
            }
            let content = self.content.clone();
            admission
                .launch(async move {
                    let handoff = tokio::task::spawn_blocking(move || content.hand_off(plan))
                        .await
                        .map_err(|_| Handoff::Refused(Failure::Cancelled))?;
                    foreign_supervisor(handoff)
                })
                .await
        })
    }
    fn archive<'a>(
        &'a self,
        receipt: &'a steam::Receipt,
        _: &'a steam::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let (content, config) = (self.content.clone(), self.local.configuration(receipt));
            blocking(move || content.archive_cloud(config))
                .await?
                .map_err(steam_failure)
        })
    }
    fn removed<'a>(
        &'a self,
        receipt: &'a steam::Receipt,
        _: &'a steam::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.local.removed(receipt))
    }
    fn directory_name(&self, receipt: &steam::Receipt, _: &steam::Download) -> String {
        SteamLocal::directory_name(receipt)
    }
    fn remove_prefix(&self, receipt: &steam::Receipt) -> Option<Result<(), String>> {
        self.local.remove_prefix(receipt)
    }
    fn maintain<'a>(
        &'a self,
        game: &'a u32,
        installed: Option<(&'a steam::Receipt, &'a steam::Download)>,
        control: Arc<cloud::Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let config = installed.map(|(receipt, _)| self.local.configuration(receipt));
            let (content, app_id) = (self.content.clone(), *game);
            blocking(move || content.maintain_cloud(app_id, config, Arc::new(CloudSync(control))))
                .await?
                .map_err(steam_failure)
        })
    }
    fn indicators(&self, games: &[u32]) -> Result<Vec<(u32, cloud_status::Indicator)>, Failure> {
        self.local.indicators(games)
    }
}

/// What Epic's content adapters share.
struct EpicLocal {
    account: String,
    language: String,
    locations: Locations,
    credentials: Arc<dyn EpicCredentialSave>,
    /// A Transfer was cancelled while its credentials were being renewed.
    cancelled: std::sync::atomic::AtomicBool,
}
impl EpicLocal {
    fn proceed(&self) -> Result<(), Failure> {
        match self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            true => Err(Failure::Cancelled),
            false => Ok(()),
        }
    }
    /// Saves the credentials an account call rotated, waiting for the save.
    async fn save(&self) -> Result<(), Failure> {
        let credentials = self.credentials.clone();
        blocking(move || credentials.save()).await
    }
    /// Saves them once the caller moved on: a completed Transfer's receipt
    /// never waits for Keychain.
    fn save_later(&self) {
        let credentials = self.credentials.clone();
        tokio::task::spawn_blocking(move || credentials.save());
    }
    fn configuration(
        &self,
        receipt: &epic::Receipt,
        request: &epic::Download,
    ) -> epic::Configuration {
        epic::Configuration {
            root: self.locations.cloud_archive.clone(),
            home: self.locations.home.clone(),
            install: receipt.directory.clone().into(),
            session_directory: self.locations.epic_sessions.clone(),
            helper: self.locations.helper.clone(),
            namespace: receipt.namespace.clone(),
            catalog_item_id: request.catalog_item_id.clone(),
            app_name: receipt.app_name.clone(),
            platform: receipt.platform.clone(),
            language: self.language.clone(),
            prefix: prefix(
                &self.locations,
                receipt.platform == "Windows",
                &receipt.directory,
            ),
        }
    }
    async fn removed(
        &self,
        receipt: &epic::Receipt,
        request: &epic::Download,
    ) -> Result<(), Failure> {
        let (config, account) = (self.configuration(receipt, request), self.account.clone());
        blocking(move || epic::removed(&config, &account))
            .await?
            .map_err(epic_failure)
    }
    fn directory_name(receipt: &epic::Receipt, request: &epic::Download) -> String {
        epic_kit::client::installation_directory_name(
            &receipt.namespace,
            &request.catalog_item_id,
            &receipt.app_name,
            &receipt.platform,
        )
    }
    fn remove_prefix(&self, receipt: &epic::Receipt) -> Option<Result<(), String>> {
        remove_prefix(
            &self.locations,
            receipt.platform == "Windows",
            &receipt.directory,
        )
    }
    /// The saved credentials the exchange rotated, before the game starts.
    fn before_begin(&self) -> game_runtime::sessions::BeforeBegin {
        let credentials = self.credentials.clone();
        Box::new(move || credentials.save())
    }
    fn indicators(
        &self,
        games: &[epic::Store],
    ) -> Result<Vec<(epic::Store, cloud_status::Indicator)>, Failure> {
        let games: Vec<_> = games
            .iter()
            .map(|game| (game.namespace.clone(), game.app_name.clone()))
            .collect();
        Ok(
            epic::indicators(&self.locations.cloud_archive, &self.account, &games)
                .map_err(epic_failure)?
                .into_iter()
                .map(|shown| {
                    let game = epic::Store {
                        namespace: shown.namespace,
                        app_name: shown.app_name,
                    };
                    (game, shown.indicator)
                })
                .collect(),
        )
    }
    /// What maintenance of `game` syncs: its installation, or, uninstalled,
    /// the configuration its journal saved, with this SailOver's paths.
    async fn maintenance(
        &self,
        game: &epic::Store,
        installed: Option<(&epic::Receipt, &epic::Download)>,
    ) -> Result<epic::Configuration, Failure> {
        if let Some((receipt, request)) = installed {
            return Ok(self.configuration(receipt, request));
        }
        let (root, account, wanted) = (
            self.locations.cloud_archive.clone(),
            self.account.clone(),
            (game.namespace.clone(), game.app_name.clone()),
        );
        let saved = blocking(move || epic::indicators(&root, &account, &[wanted]))
            .await?
            .map_err(epic_failure)?
            .into_iter()
            .find(|shown| shown.namespace == game.namespace && shown.app_name == game.app_name)
            .and_then(|shown| shown.configuration);
        let mut configuration =
            saved.ok_or_else(|| Failure::Failed("backend.epic.cloud.failed".into()))?;
        configuration.root = self.locations.cloud_archive.clone();
        configuration.session_directory = self.locations.epic_sessions.clone();
        configuration.helper = self.locations.helper.clone();
        Ok(configuration)
    }
}

/// How an Epic maintenance sync ended.
fn epic_maintenance(result: Result<(), epic::Error>) -> Result<(), Failure> {
    match result {
        Err(epic::Error::Cancelled) => Err(Failure::Cancelled),
        result => result.map_err(epic_failure),
    }
}

fn epic_receipt(installed: EpicInstallation, request: &epic::Download) -> epic::Receipt {
    epic::Receipt {
        namespace: request.namespace.clone(),
        app_name: installed.app_name,
        name: request.title.clone(),
        directory: installed.directory,
        platform: installed.platform,
        total_bytes: installed.total_bytes,
        needs_verification: Some(false),
    }
}

/// Waits until the session's persistence saved its rotated credentials.
struct Persisted(Arc<EpicSession>);
impl EpicCredentialSave for Persisted {
    fn save(&self) {
        let client = self.0.client.clone();
        self.0.persistence.wait(|| client.credentials_revision());
    }
}

/// The signed-in Epic session.
struct EpicSessionContent {
    session: Arc<EpicSession>,
    local: EpicLocal,
}
impl EpicSessionContent {
    /// As every Epic account call: renewed and saved credentials first. A
    /// cancellation meanwhile stops the call before it starts.
    async fn prepare(&self) -> Result<(), Failure> {
        self.local
            .cancelled
            .store(false, std::sync::atomic::Ordering::Release);
        self.session.prepare().await.map_err(epic_failure)?;
        self.local.save().await?;
        self.local.proceed()
    }
}

impl Content<epic::Epic> for EpicSessionContent {
    fn download<'a>(
        &'a self,
        request: &'a epic::Download,
    ) -> BoxFuture<'a, Result<epic::Receipt, Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            let installed = self
                .session
                .download(
                    request.namespace.clone(),
                    request.catalog_item_id.clone(),
                    request.app_name.clone(),
                    request.platform.clone(),
                    request.directory.to_string_lossy().into_owned(),
                )
                .await;
            self.local.save_later();
            Ok(epic_receipt(installed.map_err(epic_failure)?, request))
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.session.client.progress()
    }
    fn cancel(&self) {
        self.local
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.session.client.cancel_download();
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a epic::Receipt,
        request: &'a epic::Download,
        admission: Admission<epic::Epic>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let handoff = crate::epic_cloud::hand_off(
                self.session.client.clone(),
                self.local.configuration(receipt, request),
                self.local.locations.clone(),
                admission.canceller(),
            );
            let outcome = admission
                .launch_before(handoff, self.local.before_begin())
                .await;
            self.local.save_later();
            outcome
        })
    }
    fn archive<'a>(
        &'a self,
        receipt: &'a epic::Receipt,
        request: &'a epic::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            let archived = epic::archive(
                self.local.configuration(receipt, request),
                &self.session.client,
            )
            .await;
            self.local.save_later();
            archived.map_err(epic_failure)
        })
    }
    fn removed<'a>(
        &'a self,
        receipt: &'a epic::Receipt,
        request: &'a epic::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.local.removed(receipt, request))
    }
    fn directory_name(&self, receipt: &epic::Receipt, request: &epic::Download) -> String {
        EpicLocal::directory_name(receipt, request)
    }
    fn remove_prefix(&self, receipt: &epic::Receipt) -> Option<Result<(), String>> {
        self.local.remove_prefix(receipt)
    }
    fn maintain<'a>(
        &'a self,
        game: &'a epic::Store,
        installed: Option<(&'a epic::Receipt, &'a epic::Download)>,
        control: Arc<cloud::Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let config = self.local.maintenance(game, installed).await?;
            let (sessions, helper) = (config.session_directory.clone(), config.helper.clone());
            // Its idle guard keeps every launch of Epic out meanwhile.
            let idle = blocking(move || epic::Client::idle_guard(&sessions, &helper))
                .await?
                .map_err(epic_failure)?;
            epic_maintenance(
                epic::maintain(config, self.session.client.clone(), control, &idle).await,
            )
        })
    }
    fn indicators(
        &self,
        games: &[epic::Store],
    ) -> Result<Vec<(epic::Store, cloud_status::Indicator)>, Failure> {
        self.local.indicators(games)
    }
}

/// An Epic session a test stands in.
struct ForeignEpicContent {
    content: Arc<dyn EpicContent>,
    local: EpicLocal,
}
impl ForeignEpicContent {
    /// As every Epic account call: renewed and saved credentials first.
    async fn prepare(&self) -> Result<(), Failure> {
        self.local
            .cancelled
            .store(false, std::sync::atomic::Ordering::Release);
        let content = self.content.clone();
        blocking(move || content.prepare())
            .await?
            .map_err(epic_failure)?;
        self.local.save().await?;
        self.local.proceed()
    }
}

impl Content<epic::Epic> for ForeignEpicContent {
    fn download<'a>(
        &'a self,
        request: &'a epic::Download,
    ) -> BoxFuture<'a, Result<epic::Receipt, Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            let (content, asked) = (self.content.clone(), request.clone());
            let installed = blocking(move || {
                content.download(
                    asked.namespace,
                    asked.catalog_item_id,
                    asked.app_name,
                    asked.platform,
                    asked.directory.to_string_lossy().into_owned(),
                )
            })
            .await?;
            self.local.save_later();
            Ok(epic_receipt(installed.map_err(epic_failure)?, request))
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.content.download_progress().map(Into::into)
    }
    fn cancel(&self) {
        self.local
            .cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        self.content.cancel_download();
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a epic::Receipt,
        request: &'a epic::Download,
        admission: Admission<epic::Epic>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let content = self.content.clone();
            let configuration: EpicCloudConfiguration =
                self.local.configuration(receipt, request).into();
            let outcome = admission
                .launch_before(
                    async move {
                        let handoff =
                            tokio::task::spawn_blocking(move || content.hand_off(configuration))
                                .await
                                .map_err(|_| Handoff::Refused(Failure::Cancelled))?;
                        foreign_supervisor(handoff)
                    },
                    self.local.before_begin(),
                )
                .await;
            self.local.save_later();
            outcome
        })
    }
    fn archive<'a>(
        &'a self,
        receipt: &'a epic::Receipt,
        request: &'a epic::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.prepare().await?;
            let content = self.content.clone();
            let configuration: EpicCloudConfiguration =
                self.local.configuration(receipt, request).into();
            let archived = blocking(move || content.archive_cloud(configuration)).await?;
            self.local.save_later();
            archived.map_err(epic_failure)
        })
    }
    fn removed<'a>(
        &'a self,
        receipt: &'a epic::Receipt,
        request: &'a epic::Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(self.local.removed(receipt, request))
    }
    fn directory_name(&self, receipt: &epic::Receipt, request: &epic::Download) -> String {
        EpicLocal::directory_name(receipt, request)
    }
    fn remove_prefix(&self, receipt: &epic::Receipt) -> Option<Result<(), String>> {
        self.local.remove_prefix(receipt)
    }
    fn maintain<'a>(
        &'a self,
        game: &'a epic::Store,
        installed: Option<(&'a epic::Receipt, &'a epic::Download)>,
        control: Arc<cloud::Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            let configuration: EpicCloudConfiguration =
                self.local.maintenance(game, installed).await?.into();
            let content = self.content.clone();
            blocking(move || content.maintain_cloud(configuration, Arc::new(CloudSync(control))))
                .await?
                .map_err(|error| match error {
                    EpicError::Service { message } if message == "backend.epic.cancelled" => {
                        Failure::Cancelled
                    }
                    error => epic_failure(error),
                })
        })
    }
    fn indicators(
        &self,
        games: &[epic::Store],
    ) -> Result<Vec<(epic::Store, cloud_status::Indicator)>, Failure> {
        self.local.indicators(games)
    }
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
        let content = SteamSessionContent {
            session,
            credentials,
            local: SteamLocal {
                account,
                locations: locations.clone(),
            },
        };
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

    /// The installations of `account_id` behind `content`, which a test
    /// stands in.
    #[uniffi::constructor]
    pub fn with_content(
        content: Arc<dyn SteamContent>,
        account_id: u64,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: SailOverLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let locations: Locations = (&locations).into();
        let content = ForeignSteamContent {
            content,
            local: SteamLocal {
                account: account_id,
                locations: locations.clone(),
            },
        };
        Ok(Arc::new(Self {
            installations: account_installations(
                Arc::new(content),
                steam_sessions(&sessions)?,
                account_id,
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
        let credentials = Arc::new(Persisted(session.clone()));
        let content = EpicSessionContent {
            session,
            local: EpicLocal {
                account: account_id.clone(),
                language,
                locations: locations.clone(),
                credentials,
                cancelled: Default::default(),
            },
        };
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

    /// The installations of `account_id` behind `content`, which a test
    /// stands in.
    #[uniffi::constructor]
    pub fn with_content(
        content: Arc<dyn EpicContent>,
        account_id: String,
        language: String,
        credentials: Arc<dyn EpicCredentialSave>,
        sessions: Arc<StorefrontSessions>,
        receipts: String,
        locations: SailOverLocations,
    ) -> Result<Arc<Self>, GameSessionError> {
        let locations: Locations = (&locations).into();
        let content = ForeignEpicContent {
            content,
            local: EpicLocal {
                account: account_id.clone(),
                language,
                locations: locations.clone(),
                credentials,
                cancelled: Default::default(),
            },
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    struct NothingToSave;
    impl EpicCredentialSave for NothingToSave {
        fn save(&self) {}
    }

    #[test]
    fn a_windows_build_keeps_its_saves_in_its_wine_prefix_and_a_mac_build_has_none() {
        let locations = Locations::new(
            Path::new("/fixture"),
            Path::new("/Applications/SailOver.app"),
            Path::new("/Users/fixture"),
        );
        let local = EpicLocal {
            account: "account".into(),
            language: "fr".into(),
            locations: locations.clone(),
            credentials: Arc::new(NothingToSave),
            cancelled: Default::default(),
        };
        let build = |platform: &str| {
            let receipt = epic::Receipt {
                namespace: "sandbox".into(),
                app_name: "game".into(),
                name: "Fixture".into(),
                directory: format!("/fixture/Games/epic-hash-{platform}"),
                platform: platform.into(),
                total_bytes: 0,
                needs_verification: None,
            };
            let request = epic::Download {
                namespace: "sandbox".into(),
                catalog_item_id: "catalog".into(),
                app_name: "game".into(),
                title: "Fixture".into(),
                platform: platform.into(),
                directory: "/fixture/Games".into(),
            };
            local.configuration(&receipt, &request)
        };

        let windows = build("Windows");
        let mac = build("Mac");

        assert_eq!(
            windows.prefix,
            Some(locations.wine_prefixes.join("epic-hash-Windows"))
        );
        assert_eq!(windows.catalog_item_id, "catalog");
        assert_eq!(windows.language, "fr");
        assert_eq!(windows.root, locations.cloud_archive);
        assert_eq!(mac.prefix, None);
    }
}
