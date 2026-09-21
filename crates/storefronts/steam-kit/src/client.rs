//! Account-scoped orchestration. Authentication, CM messages and content formats
//! remain independently usable; no UI or platform keychain code lives here.
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use forge_core::{
    install::{self, InstallError, LOCK_FILE, RECEIPT_FILE},
    transfer::{
        Cancelled, DownloadProgress, DownloadSummary, TransferBusy, TransferContext,
        TransferStatus, Transfers,
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    cm::{AppInfo, ContentLicenses, ContentServer, DepotInfo, SteamConnection, supports_platform},
    content::{CancelToken, ContentClient, ContentError, DepotDownload},
};
mod cloud;

pub use forge_core::transfer::TransferState;

/// Receipts are small JSON documents; anything larger is not one of ours.
const MAX_RECEIPT_BYTES: usize = 1024 * 1024;

/// Opening a game and then downloading it should not pay two PICS token and
/// product round trips; publisher metadata rarely changes within a minute.
const APP_INFO_TTL: Duration = Duration::from_secs(60);

pub use crate::cm::OwnedGameInfo as OwnedGame;
pub use crate::cm::achievements::Achievement;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamePlatforms {
    pub app_id: u32,
    pub platforms: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ClientError(pub String);

#[derive(Debug, thiserror::Error)]
pub enum ClientConnectError {
    #[error("{0}")]
    AuthenticationRequired(ClientError),
    #[error("{0}")]
    Other(ClientError),
}

impl From<crate::cm::CmError> for ClientConnectError {
    fn from(error: crate::cm::CmError) -> Self {
        if error.authentication_required() {
            Self::AuthenticationRequired(client_error(error))
        } else {
            Self::Other(client_error(error))
        }
    }
}

pub fn http_client() -> Result<reqwest::Client, ClientError> {
    reqwest::Client::builder()
        .user_agent(concat!("Forge/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ClientError("backend.steam.client.connectionInitFailed".into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyError {
    Network,
    TooLarge,
}

/// Reads a response body while refusing anything above `limit` bytes, whether
/// advertised up front or discovered while streaming.
pub(crate) async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, BodyError> {
    let advertised = response.content_length();
    if advertised.is_some_and(|length| length > limit as u64) {
        return Err(BodyError::TooLarge);
    }
    let mut body = Vec::with_capacity(advertised.map_or(0, |length| length as usize));
    while let Some(chunk) = response.chunk().await.map_err(|_| BodyError::Network)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(BodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Clone, Debug)]
pub struct GameDetails {
    pub app_id: u32,
    pub name: String,
    pub windows: bool,
    pub macos: bool,
    pub windows_bytes: u64,
    pub macos_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct TransferProgress {
    pub app_id: u32,
    pub state: TransferState,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub network_bytes: u64,
    pub content_bytes: u64,
    pub current_file: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub app_id: u32,
    pub name: String,
    pub directory: String,
    pub platform: String,
    pub language: String,
    pub total_bytes: u64,
    pub manifests: Vec<InstalledDepot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledDepot {
    pub depot_id: u32,
    pub manifest_id: u64,
}

// Intentionally no Debug: depot keys are credentials.
struct PreparedDepot {
    depot_id: u32,
    manifest_id: u64,
    authorizing_app: u32,
    key: [u8; 32],
    estimated_bytes: u64,
}

/// Everything resolved under the CM lock before the installation lock is taken.
struct PreparedInstall {
    name: String,
    /// Validated HTTPS CDN/SteamCache hosts allowed to serve this app.
    servers: Vec<ContentServer>,
    depots: Vec<PreparedDepot>,
}

/// Counters accumulated by depots already installed in this invocation, so
/// per-depot engine progress can be presented as one transfer.
#[derive(Clone, Copy)]
struct ProgressBase {
    app_id: u32,
    completed: u64,
    total_estimate: u64,
    network: u64,
    content_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TransferBytes {
    network: u64,
    content: u64,
}

/// Content callbacks contain cumulative counters within one invocation. Keep
/// prior attempts separate so repeated snapshots and mirror retries add once.
#[derive(Default)]
struct DepotTransferBytes {
    previous: TransferBytes,
    current: TransferBytes,
}

impl DepotTransferBytes {
    fn begin_attempt(&mut self) {
        self.previous = self.total();
        self.current = TransferBytes::default();
    }

    fn observe(&mut self, network: u64, content: u64) -> TransferBytes {
        self.current = TransferBytes { network, content };
        self.total()
    }

    fn total(&self) -> TransferBytes {
        TransferBytes {
            network: self.previous.network + self.current.network,
            content: self.previous.content + self.current.content,
        }
    }
}

pub struct Client {
    http: reqwest::Client,
    cm: AsyncMutex<SteamConnection>,
    friends: crate::friends::FriendsEventSource,
    connection_abort: tokio::task::AbortHandle,
    cloud_authorized: std::sync::atomic::AtomicBool,
    /// App and depot IDs expanded through PICS licenses, filled on first content query.
    catalog: AsyncMutex<Option<Arc<ContentLicenses>>>,
    app_infos: Mutex<HashMap<u32, (Instant, Arc<AppInfo>)>>,
    downloads: Transfers<TransferProgress>,
}

impl Client {
    /// Reads the connected account's achievements without initializing a game SDK.
    /// Releases the CM lock between the two reads so foreground content can
    /// proceed; a schema change in that window is already rejected by the join.
    pub async fn achievements(
        &self,
        app_id: u32,
        language: &str,
    ) -> Result<Vec<Achievement>, ClientError> {
        let schema = self
            .cm
            .lock()
            .await
            .game_achievements(app_id, language)
            .await
            .map_err(client_error)?;
        let user = self
            .cm
            .lock()
            .await
            .user_achievements(app_id)
            .await
            .map_err(client_error)?;
        crate::cm::achievements::combine(app_id, schema, user).map_err(client_error)
    }
    #[cfg(test)]
    pub(crate) fn with_test_connection(cm: SteamConnection) -> Self {
        Self {
            http: http_client().unwrap(),
            friends: cm.friends_source(),
            connection_abort: cm.abort_handle(),
            cloud_authorized: std::sync::atomic::AtomicBool::new(true),
            cm: AsyncMutex::new(cm),
            catalog: AsyncMutex::new(None),
            app_infos: Mutex::new(HashMap::new()),
            downloads: Transfers::default(),
        }
    }

    pub async fn connect(
        account_name: &str,
        steam_id: u64,
        refresh_token: &str,
    ) -> Result<Self, ClientConnectError> {
        let http = http_client().map_err(ClientConnectError::Other)?;
        let mut cm = SteamConnection::connect(http.clone())
            .await
            .map_err(ClientConnectError::from)?;
        cm.log_on(account_name, steam_id, refresh_token)
            .await
            .map_err(ClientConnectError::from)?;
        Ok(Self {
            http,
            friends: cm.friends_source(),
            connection_abort: cm.abort_handle(),
            cloud_authorized: std::sync::atomic::AtomicBool::new(true),
            cm: AsyncMutex::new(cm),
            catalog: AsyncMutex::new(None),
            app_infos: Mutex::new(HashMap::new()),
            downloads: Transfers::default(),
        })
    }

    pub async fn next_friends(&self, after_revision: u64) -> crate::friends::FriendsSnapshot {
        self.friends.next(after_revision).await
    }

    /// Works even while a download owns the CM query mutex. Observers wake
    /// before the worker is aborted, and the old socket cannot keep reconnecting.
    pub fn shutdown(&self) {
        self.cloud_authorized
            .store(false, std::sync::atomic::Ordering::Release);
        self.friends.disconnect(false);
        self.connection_abort.abort();
        self.downloads.cancel();
    }

    pub async fn profile(&self) -> Result<crate::cm::AccountProfile, ClientError> {
        // Include any wait for a content query in the optional profile's budget.
        tokio::time::timeout(crate::cm::PROFILE_TIMEOUT, async {
            let request = self
                .cm
                .lock()
                .await
                .request_profile()
                .await
                .map_err(client_error)?;
            request.receive().await.map_err(client_error)
        })
        .await
        .map_err(|_| client_error(crate::cm::CmError::Timeout))?
    }

    /// Ordered by the CM layer: most recently played first, then name.
    pub async fn games(&self) -> Result<Vec<OwnedGame>, ClientError> {
        let games = self
            .cm
            .lock()
            .await
            .owned_games()
            .await
            .map_err(client_error)?;
        // Rendering the library does not require expanding every license through
        // PICS. Refresh license-derived download rights only when next needed.
        *self.catalog.lock().await = None;
        self.app_infos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        Ok(games)
    }

    /// Call with the CM lock held. Serves PICS metadata from a short-lived cache.
    async fn app_info(
        &self,
        cm: &mut SteamConnection,
        app_id: u32,
    ) -> Result<Arc<AppInfo>, ClientError> {
        if let Some(app) = self.cached_app(app_id) {
            return Ok(app);
        }
        let app = Arc::new(cm.app_info(app_id).await.map_err(client_error)?);
        self.remember_app(&app);
        Ok(app)
    }

    fn cached_app(&self, app_id: u32) -> Option<Arc<AppInfo>> {
        self.app_infos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&app_id)
            .filter(|(fetched, _)| fetched.elapsed() < APP_INFO_TTL)
            .map(|(_, app)| Arc::clone(app))
    }

    fn remember_app(&self, app: &Arc<AppInfo>) {
        self.app_infos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(app.app_id, (Instant::now(), Arc::clone(app)));
    }

    /// Call with the CM lock held: every path acquires CM before catalog.
    /// Player.GetOwnedGames is presentation data, not proof of depot/DLC rights.
    async fn licensed_content(
        &self,
        cm: &mut SteamConnection,
    ) -> Result<Arc<ContentLicenses>, ClientError> {
        let mut catalog = self.catalog.lock().await;
        if let Some(owned) = catalog.as_ref() {
            return Ok(Arc::clone(owned));
        }
        let owned = Arc::new(cm.content_licenses().await.map_err(client_error)?);
        *catalog = Some(Arc::clone(&owned));
        Ok(owned)
    }

    pub fn progress(&self) -> Option<TransferProgress> {
        self.downloads.snapshot()
    }

    /// Declared platforms in bounded PICS batches, without expanding licenses.
    /// Returns each nonzero ID once, in request order. Empty means unknown;
    /// transport, access and malformed metadata errors remain request failures.
    pub async fn platforms(&self, app_ids: &[u32]) -> Result<Vec<GamePlatforms>, ClientError> {
        let mut seen = HashSet::new();
        let app_ids: Vec<_> = app_ids
            .iter()
            .copied()
            .filter(|id| *id != 0 && seen.insert(*id))
            .collect();
        let mut result = Vec::with_capacity(app_ids.len());
        for batch in app_ids.chunks(50) {
            let mut apps: HashMap<_, _> = batch
                .iter()
                .filter_map(|id| self.cached_app(*id).map(|app| (*id, app)))
                .collect();
            let mut missing: Vec<_> = batch
                .iter()
                .copied()
                .filter(|id| !apps.contains_key(id))
                .collect();
            if !missing.is_empty() {
                let mut cm = self.cm.lock().await;
                // Another content query may have populated the cache while we waited.
                missing.retain(|id| {
                    if let Some(app) = self.cached_app(*id) {
                        apps.insert(*id, app);
                        false
                    } else {
                        true
                    }
                });
                if !missing.is_empty() {
                    for app in cm.app_infos(&missing).await.map_err(client_error)? {
                        if missing.contains(&app.app_id) {
                            let app = Arc::new(app);
                            self.remember_app(&app);
                            apps.insert(app.app_id, app);
                        }
                    }
                }
            }
            // Release the CM lock between batches so foreground content can proceed.
            result.extend(batch.iter().map(|id| {
                GamePlatforms {
                    app_id: *id,
                    platforms: apps
                        .get(id)
                        .map_or_else(Vec::new, |app| app.os_list.clone()),
                }
            }));
        }
        Ok(result)
    }

    pub async fn details(&self, app_id: u32, language: &str) -> Result<GameDetails, ClientError> {
        let mut cm = self.cm.lock().await;
        let owned = self.licensed_content(&mut cm).await?;
        let app = self.app_info(&mut cm, app_id).await?;
        drop(cm);
        let windows = select_depots(&app, &owned, "windows", language);
        let macos = select_depots(&app, &owned, "macos", language);
        Ok(GameDetails {
            app_id,
            name: app.name.clone(),
            windows: !windows.is_empty(),
            macos: !macos.is_empty(),
            windows_bytes: windows.iter().filter_map(|d| d.size_bytes).sum(),
            macos_bytes: macos.iter().filter_map(|d| d.size_bytes).sum(),
        })
    }

    pub fn cancel_download(&self) {
        self.downloads.cancel();
    }

    /// Resolve only a completed installation belonging to this account. PICS is
    /// fetched again so executable paths and launch arguments follow the publisher.
    pub async fn launch_plan(
        &self,
        app_id: u32,
        platform: &str,
        directory: &Path,
    ) -> Result<crate::launch::LaunchPlan, ClientError> {
        if !directory.is_absolute() || !directory.is_dir() {
            return Err(ClientError("backend.steam.launch.directoryMissing".into()));
        }
        let receipt = install::read_file_bounded(&directory.join(RECEIPT_FILE), MAX_RECEIPT_BYTES)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Installation>(&bytes).ok())
            .filter(|receipt| receipt.app_id == app_id && receipt.platform == platform)
            .ok_or_else(|| ClientError("backend.steam.client.incompleteInstallation".into()))?;
        let mut cm = self.cm.lock().await;
        let owned = self.licensed_content(&mut cm).await?;
        if !owned.apps.contains(&receipt.app_id) {
            return Err(ClientError("backend.steam.client.gameNotOwned".into()));
        }
        let app = Arc::new(cm.app_info(app_id).await.map_err(client_error)?);
        drop(cm);
        self.remember_app(&app);
        crate::launch::plan(&app, platform, directory).map_err(client_error)
    }

    pub async fn download(
        &self,
        app_id: u32,
        platform: &str,
        language: &str,
        parent: &Path,
    ) -> Result<Installation, ClientError> {
        self.downloads
            .run(
                TransferProgress {
                    app_id,
                    state: TransferState::Preparing,
                    completed_bytes: 0,
                    total_bytes: 0,
                    network_bytes: 0,
                    content_bytes: 0,
                    current_file: String::new(),
                    message: "backend.steam.client.preparingDepots".into(),
                },
                |transfer| async move {
                    self.download_inner(app_id, platform, language, parent, &transfer)
                        .await
                },
            )
            .await
    }

    async fn download_inner(
        &self,
        app_id: u32,
        platform: &str,
        language: &str,
        parent: &Path,
        transfer: &TransferContext<TransferProgress>,
    ) -> Result<Installation, ClientError> {
        let cancel = transfer.cancel_token();
        if !matches!(platform, "windows" | "macos") || !crate::language::is_supported(language) {
            return Err(ClientError(
                "backend.steam.client.unsupportedPlatformOrLanguage".into(),
            ));
        }
        // The chosen folder is the only caller-controlled path. App IDs and depot
        // manifest validation determine every path beneath it.
        if !parent.is_absolute() || !parent.is_dir() {
            return Err(ClientError(
                "backend.steam.client.chooseExistingFolder".into(),
            ));
        }
        let parent = parent.canonicalize().map_err(storage_error)?;
        let destination = parent.join(format!("steam-{app_id}-{platform}"));
        reject_symlink(&destination)?;
        let prepared = self
            .prepare_depots(app_id, platform, language, cancel)
            .await?;
        cancel.check()?;
        // Hold this lock across every depot and the final receipt, including waits
        // for refreshed request codes. Other sessions/processes must not interleave.
        let _installation_lock = lock_installation(&destination)?;
        let mut base = ProgressBase {
            app_id,
            completed: 0,
            total_estimate: prepared
                .depots
                .iter()
                .map(|depot| depot.estimated_bytes)
                .sum(),
            network: 0,
            content_bytes: 0,
        };
        let mut manifests = Vec::new();
        let content = ContentClient::new(self.http.clone());
        for depot in &prepared.depots {
            cancel.check()?;
            let (summary, received) = self
                .install_depot(
                    &content,
                    depot,
                    &prepared.servers,
                    &destination,
                    transfer,
                    base,
                )
                .await?;
            base.total_estimate =
                base.total_estimate.saturating_sub(depot.estimated_bytes) + summary.total_bytes;
            base.completed += summary.total_bytes;
            base.network += received.network;
            base.content_bytes += received.content;
            manifests.push(InstalledDepot {
                depot_id: depot.depot_id,
                manifest_id: depot.manifest_id,
            });
        }
        cancel.check()?;
        let installation = Installation {
            app_id,
            name: prepared.name,
            directory: destination.to_string_lossy().into_owned(),
            platform: platform.into(),
            language: language.into(),
            total_bytes: base.completed,
            manifests,
        };
        save_installation(&destination, &installation)?;
        transfer.update(|progress| {
            *progress = TransferProgress {
                app_id,
                state: TransferState::Downloading,
                completed_bytes: base.completed,
                total_bytes: base.completed,
                network_bytes: base.network,
                content_bytes: base.content_bytes,
                current_file: String::new(),
                message: String::new(),
            }
        });
        Ok(installation)
    }

    /// Resolves ownership, depot selection, manifest IDs, depot keys and CDN
    /// candidates under the CM lock, before any installation file exists.
    async fn prepare_depots(
        &self,
        app_id: u32,
        platform: &str,
        language: &str,
        cancel: &CancelToken,
    ) -> Result<PreparedInstall, ClientError> {
        let mut cm = self.cm.lock().await;
        let owned = self.licensed_content(&mut cm).await?;
        if !owned.apps.contains(&app_id) {
            return Err(ClientError("backend.steam.client.gameNotOwned".into()));
        }
        let app = self.app_info(&mut cm, app_id).await?;
        let selected = select_depots(&app, &owned, platform, language);
        if selected.is_empty() {
            return Err(ClientError("backend.steam.client.noCompatibleDepot".into()));
        }
        // The CM layer already keeps only validated HTTPS CDN/SteamCache hosts.
        let servers: Vec<ContentServer> = cm
            .content_servers()
            .await
            .map_err(client_error)?
            .into_iter()
            .filter(|server| {
                server.allowed_app_ids.is_empty() || server.allowed_app_ids.contains(&app_id)
            })
            .take(8)
            .collect();
        if servers.is_empty() {
            return Err(ClientError("backend.steam.client.noContentServer".into()));
        }
        let mut depots = Vec::new();
        for depot in selected {
            cancel.check()?;
            let manifest_app = depot.shared_from_app.unwrap_or(app_id);
            let authorizing_app = content_authorizing_app(&app, depot);
            let manifest_id = match depot.public_manifest {
                Some(id) => id,
                None if manifest_app != app_id => self
                    .app_info(&mut cm, manifest_app)
                    .await?
                    .depots
                    .iter()
                    .find(|other| other.depot_id == depot.depot_id)
                    .and_then(|other| other.public_manifest)
                    .ok_or_else(|| {
                        ClientError(crate::message::format(
                            "backend.steam.client.sharedManifestMissing",
                            &[depot.depot_id.to_string()],
                        ))
                    })?,
                None => continue,
            };
            let key = cm
                .depot_key(depot.depot_id, app_id)
                .await
                .map_err(client_error)?;
            let key: [u8; 32] = key
                .try_into()
                .map_err(|_| ClientError("backend.steam.client.invalidDepotKey".into()))?;
            depots.push(PreparedDepot {
                depot_id: depot.depot_id,
                manifest_id,
                authorizing_app,
                key,
                estimated_bytes: depot.size_bytes.unwrap_or(0),
            });
        }
        if depots.is_empty() {
            return Err(ClientError("backend.steam.client.noPublicManifest".into()));
        }
        Ok(PreparedInstall {
            name: app.name.clone(),
            servers,
            depots,
        })
    }

    /// Installs one depot from the first mirror that serves it, refreshing CDN
    /// authorization once after a 403. Returns the engine summary and the bytes
    /// received across every attempt on this depot.
    async fn install_depot(
        &self,
        content: &ContentClient,
        depot: &PreparedDepot,
        servers: &[ContentServer],
        destination: &Path,
        transfer: &TransferContext<TransferProgress>,
        base: ProgressBase,
    ) -> Result<(DownloadSummary, TransferBytes), ClientError> {
        let cancel = transfer.cancel_token();
        let received = Arc::new(Mutex::new(DepotTransferBytes::default()));
        // The current manifest ID and depot key came from authorized Steam
        // metadata above. Keep one validated snapshot across all CDN attempts.
        let cached_manifest = content
            .cached_manifest(destination, depot.depot_id, depot.manifest_id, &depot.key)
            .await
            .map_err(client_error)?;
        let depot_label =
            crate::message::format("backend.steam.client.depot", &[depot.depot_id.to_string()]);
        // Progress arrives from the content module's network and disk
        // threads, so the callback owns everything it touches.
        let report: Arc<dyn Fn(DownloadProgress) + Send + Sync> = {
            let received = Arc::clone(&received);
            let transfer = transfer.clone();
            let size = depot.estimated_bytes;
            Arc::new(move |progress: DownloadProgress| {
                let bytes = received
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .observe(progress.network_bytes, progress.content_bytes);
                transfer.update(|status| {
                    *status = TransferProgress {
                        app_id: base.app_id,
                        state: progress.phase.into(),
                        completed_bytes: base.completed
                            + progress
                                .verification_bytes
                                .unwrap_or(progress.completed_bytes),
                        total_bytes: base.total_estimate.saturating_sub(size)
                            + progress.total_bytes,
                        network_bytes: base.network + bytes.network,
                        content_bytes: base.content_bytes + bytes.content,
                        current_file: progress.current_file,
                        message: depot_label.clone(),
                    }
                });
            })
        };
        let mut last_error = None;
        for server in servers {
            cancel.check()?;
            // No request code is needed for an already validated cached
            // archive. A miss always gets a fresh code just before the CDN.
            let request_code = if cached_manifest.is_some() {
                0
            } else {
                self.cm
                    .lock()
                    .await
                    .manifest_request_code(depot.depot_id, depot.authorizing_app, depot.manifest_id)
                    .await
                    .map_err(client_error)?
            };
            let mut plan = DepotDownload {
                depot_id: depot.depot_id,
                manifest_id: depot.manifest_id,
                request_code,
                depot_key: depot.key,
                server: server.vhost.clone(),
                cdn_token: None,
                destination: destination.to_owned(),
                cached_manifest: cached_manifest.clone(),
            };
            let mut result = attempt_depot(content, &plan, cancel, &received, &report).await;
            if matches!(result, Err(ContentError::Http(403))) {
                cancel.check()?;
                plan.cdn_token = Some(
                    self.cm
                        .lock()
                        .await
                        .cdn_auth(depot.authorizing_app, depot.depot_id, &server.host)
                        .await
                        .map_err(client_error)?,
                );
                result = attempt_depot(content, &plan, cancel, &received, &report).await;
            }
            match result {
                Ok(summary) => {
                    let received = received
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .total();
                    return Ok((summary, received));
                }
                Err(error @ (ContentError::Network | ContentError::Http(_))) => {
                    last_error = Some(error.to_string());
                }
                Err(error) => return Err(client_error(error)),
            }
        }
        Err(ClientError(last_error.unwrap_or_else(|| {
            "backend.steam.client.noDepotServer".into()
        })))
    }
}

fn select_depots<'a>(
    app: &'a AppInfo,
    owned: &ContentLicenses,
    platform: &str,
    language: &str,
) -> Vec<&'a DepotInfo> {
    if !supports_platform(&app.os_list, platform) {
        return Vec::new();
    }
    let eligible: Vec<_> = app
        .depots
        .iter()
        .filter(|depot| {
            supports_platform(&depot.os_list, platform)
                && (app.free_to_download || owned.depots.contains(&depot.depot_id))
                && depot.dlc_app_id.is_none_or(|dlc| owned.apps.contains(&dlc))
                && (depot.public_manifest.is_some() || depot.shared_from_app.is_some())
        })
        .collect();
    // Shared asset depots often have no OS restriction. They do not establish
    // that the application ships an executable for an otherwise unknown OS.
    if app.os_list.is_empty()
        && !eligible
            .iter()
            .any(|depot| depot.os_list.iter().any(|os| os == platform))
    {
        return Vec::new();
    }
    let has_language = eligible
        .iter()
        .any(|depot| depot.language.as_deref() == Some(language));
    let language = if has_language { language } else { "english" };
    let eligible: Vec<_> = eligible
        .into_iter()
        .filter(|depot| {
            depot
                .language
                .as_deref()
                .is_none_or(|value| value.is_empty() || value == language)
        })
        .collect();
    let arch = if eligible
        .iter()
        .any(|depot| depot.os_arch.as_deref() == Some("64"))
    {
        "64"
    } else {
        "32"
    };
    eligible
        .into_iter()
        .filter(|depot| {
            depot
                .os_arch
                .as_deref()
                .is_none_or(|value| value == arch || value.is_empty())
        })
        .collect()
}

fn content_authorizing_app(app: &AppInfo, depot: &DepotInfo) -> u32 {
    // FreeToDownload titles authorize shared depot requests through their own app,
    // even when the manifest itself must be looked up on the source app.
    if app.free_to_download {
        app.app_id
    } else {
        depot.shared_from_app.unwrap_or(app.app_id)
    }
}

/// One engine attempt on a depot. Counters restart so mirror retries add once.
async fn attempt_depot(
    content: &ContentClient,
    plan: &DepotDownload,
    cancel: &CancelToken,
    received: &Mutex<DepotTransferBytes>,
    report: &Arc<dyn Fn(DownloadProgress) + Send + Sync>,
) -> Result<DownloadSummary, ContentError> {
    received
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .begin_attempt();
    let report = Arc::clone(report);
    content
        .download_depot(plan, cancel, move |progress| report(progress))
        .await
}

fn lock_installation(destination: &Path) -> Result<std::fs::File, ClientError> {
    reject_symlink(destination)?;
    std::fs::create_dir_all(destination).map_err(storage_error)?;
    reject_symlink(destination)?;
    let lock_path = destination.join(LOCK_FILE);
    install::reject_link_or_special_file(&lock_path).map_err(|error| match error {
        InstallError::UnsafePath(_) => {
            ClientError("backend.steam.client.invalidInstallationLock".into())
        }
        InstallError::Io(error) => storage_error(error),
        InstallError::Cancelled => client_error(error),
    })?;
    install::try_lock_file(&lock_path)
        .map_err(storage_error)?
        .ok_or_else(|| ClientError("backend.steam.client.installationLocked".into()))
}

fn reject_symlink(path: &Path) -> Result<(), ClientError> {
    match path.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => Err(ClientError(
            "backend.steam.client.installationDirectorySymlink".into(),
        )),
        Ok(meta) if !meta.is_dir() => Err(ClientError(
            "backend.steam.client.installationDirectoryIsFile".into(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(error)),
    }
}

fn save_installation(destination: &Path, installation: &Installation) -> Result<(), ClientError> {
    let state = destination.join(RECEIPT_FILE);
    if state
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(ClientError(
            "backend.steam.client.installationFileSymlink".into(),
        ));
    }
    let bytes = serde_json::to_vec_pretty(installation)
        .map_err(|_| ClientError("backend.steam.client.receiptFailed".into()))?;
    forge_core::write_atomic(&state, &bytes).map_err(storage_error)
}

/// Lower layers keep typed errors; the client passes message keys to the host.
impl From<Cancelled> for ClientError {
    fn from(_: Cancelled) -> Self {
        ClientError("backend.steam.client.downloadStopped".into())
    }
}

fn client_error(error: impl std::fmt::Display) -> ClientError {
    ClientError(error.to_string())
}
fn storage_error(_: std::io::Error) -> ClientError {
    ClientError("backend.steam.client.storageFailed".into())
}
impl From<TransferBusy> for ClientError {
    fn from(_: TransferBusy) -> Self {
        ClientError("backend.steam.client.downloadInProgress".into())
    }
}

impl TransferStatus for TransferProgress {
    fn finish(&mut self, state: TransferState, error: Option<String>) {
        self.state = state;
        self.message = error.unwrap_or_else(|| {
            match state {
                TransferState::Complete if self.content_bytes == 0 => {
                    "backend.steam.client.filesUpToDate"
                }
                TransferState::Complete => "backend.steam.client.downloadComplete",
                _ => "backend.steam.client.downloadStopped",
            }
            .into()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfer_bytes_accumulate_attempts_without_counting_repeated_snapshots() {
        let mut depot = DepotTransferBytes::default();
        depot.begin_attempt();
        assert_eq!(
            depot.observe(253, 0),
            TransferBytes {
                network: 253,
                content: 0
            }
        );
        assert_eq!(
            depot.observe(253, 0),
            TransferBytes {
                network: 253,
                content: 0
            }
        );
        assert_eq!(
            depot.observe(573, 320),
            TransferBytes {
                network: 573,
                content: 320
            }
        );
        // A CDN authentication retry or mirror starts a fresh set of counters.
        depot.begin_attempt();
        assert_eq!(
            depot.observe(0, 0),
            TransferBytes {
                network: 573,
                content: 320
            }
        );
        assert_eq!(
            depot.observe(253, 0),
            TransferBytes {
                network: 826,
                content: 320
            }
        );
        assert_eq!(
            depot.observe(253, 0),
            TransferBytes {
                network: 826,
                content: 320
            }
        );
        assert_eq!(
            depot.observe(573, 320),
            TransferBytes {
                network: 1146,
                content: 640
            }
        );
        assert_eq!(
            depot.total(),
            TransferBytes {
                network: 1146,
                content: 640
            }
        );
    }
    fn depot(id: u32, os: &str, language: Option<&str>, dlc: Option<u32>) -> DepotInfo {
        DepotInfo {
            depot_id: id,
            shared_from_app: None,
            dlc_app_id: dlc,
            os_list: if os.is_empty() {
                vec![]
            } else {
                vec![os.into()]
            },
            os_arch: None,
            language: language.map(Into::into),
            public_manifest: Some(1),
            size_bytes: None,
        }
    }
    fn licenses(
        apps: impl IntoIterator<Item = u32>,
        depots: impl IntoIterator<Item = u32>,
    ) -> ContentLicenses {
        ContentLicenses {
            apps: apps.into_iter().collect(),
            depots: depots.into_iter().collect(),
        }
    }

    #[test]
    fn unlicensed_depot_without_dlc_tag_is_not_selected() {
        // Dead Cells publishes this additional Mac depot without dlcappid.
        // Its base package grants 588652, but not 1046443.
        let mut app = app(vec![
            depot(588652, "macos", None, None),
            depot(1046443, "macos", None, None),
        ]);
        app.app_id = 588650;
        let rights = licenses([588650], [588651, 588652, 588653, 588654]);
        let selected: Vec<_> = select_depots(&app, &rights, "macos", "english")
            .iter()
            .map(|d| d.depot_id)
            .collect();
        assert_eq!(selected, [588652]);
        let rights = licenses([588650], [588652, 1046443]);
        assert_eq!(select_depots(&app, &rights, "macos", "english").len(), 2);
    }

    #[test]
    fn depot_rights_do_not_grant_dlc_app_ownership() {
        let app = app(vec![depot(2, "macos", None, Some(99))]);
        assert!(select_depots(&app, &licenses([1], [2, 99]), "macos", "english").is_empty());
        assert_eq!(
            select_depots(&app, &licenses([1, 99], [2]), "macos", "english").len(),
            1
        );
    }

    #[test]
    fn free_to_download_depots_do_not_require_package_membership() {
        let mut app = app(vec![depot(2, "macos", None, None)]);
        assert!(select_depots(&app, &licenses([1], []), "macos", "english").is_empty());
        app.free_to_download = true;
        assert_eq!(
            select_depots(&app, &licenses([1], []), "macos", "english").len(),
            1
        );
    }

    #[test]
    fn unlicensed_depots_cannot_choose_language_or_architecture() {
        let mut english = depot(2, "macos", Some("english"), None);
        english.os_arch = Some("32".into());
        let mut unlicensed = depot(3, "macos", Some("french"), None);
        unlicensed.os_arch = Some("64".into());
        let app = app(vec![english, unlicensed]);
        let rights = licenses([1], [2]);
        let selected = select_depots(&app, &rights, "macos", "french");
        assert_eq!(selected.iter().map(|d| d.depot_id).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn selection_respects_platform_language_and_dlc_ownership() {
        let app = AppInfo {
            app_id: 1,
            name: "Game".into(),
            free_to_download: false,
            os_list: vec!["windows".into(), "macos".into()],
            launch_configs: vec![],
            cloud: None,
            depots: vec![
                depot(1, "", None, None),
                depot(2, "windows", Some("english"), None),
                depot(3, "windows", Some("french"), None),
                depot(4, "macos", None, None),
                depot(5, "windows", None, Some(99)),
            ],
        };
        let owned = licenses([1], [1, 2, 3, 4, 5]);
        assert_eq!(
            select_depots(&app, &owned, "windows", "french")
                .iter()
                .map(|d| d.depot_id)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            select_depots(&app, &owned, "macos", "french")
                .iter()
                .map(|d| d.depot_id)
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
    }
    fn app(depots: Vec<DepotInfo>) -> AppInfo {
        AppInfo {
            app_id: 1,
            name: "Game".into(),
            free_to_download: false,
            os_list: vec!["windows".into(), "macos".into()],
            launch_configs: vec![],
            cloud: None,
            depots,
        }
    }
    #[test]
    fn unavailable_depots_cannot_change_language_or_architecture_selection() {
        let mut english = depot(2, "windows", Some("english"), None);
        english.os_arch = Some("32".into());
        let mut unavailable_dlc = depot(3, "windows", Some("french"), Some(99));
        unavailable_dlc.os_arch = Some("64".into());
        let mut unpublished = depot(4, "windows", Some("french"), None);
        unpublished.public_manifest = None;
        unpublished.os_arch = Some("64".into());
        let mut other_platform = depot(5, "macos", Some("french"), None);
        other_platform.os_arch = Some("64".into());
        let app = app(vec![
            depot(1, "", None, None),
            english,
            unavailable_dlc,
            unpublished,
            other_platform,
        ]);
        let selected: Vec<_> =
            select_depots(&app, &licenses([1], [1, 2, 3, 4, 5]), "windows", "french")
                .into_iter()
                .map(|depot| depot.depot_id)
                .collect();
        assert_eq!(selected, [1, 2]);
    }
    #[test]
    fn neutral_depots_cannot_create_unsupported_or_unknown_platform_support() {
        let owned = licenses([1], [1, 2, 3, 4, 5]);
        let mut app = app(vec![
            depot(1, "", None, None),
            depot(2, "windows", None, None),
        ]);
        app.os_list = vec!["windows".into()];
        assert!(select_depots(&app, &owned, "macos", "english").is_empty());
        assert_eq!(select_depots(&app, &owned, "windows", "english").len(), 2);
        app.os_list.clear();
        assert!(select_depots(&app, &owned, "macos", "english").is_empty());
        assert_eq!(select_depots(&app, &owned, "windows", "english").len(), 2);
        app.depots.retain(|depot| depot.os_list.is_empty());
        assert!(select_depots(&app, &owned, "windows", "english").is_empty());
    }
    #[test]
    fn unowned_platform_specific_dlc_cannot_establish_unknown_app_platform() {
        let mut app = app(vec![
            depot(1, "", None, None),
            depot(2, "macos", None, Some(99)),
        ]);
        app.os_list.clear();
        assert!(
            select_depots(&app, &licenses([1], [1, 2, 3, 4, 5]), "macos", "english").is_empty()
        );
        assert_eq!(
            select_depots(
                &app,
                &licenses([1, 99], [1, 2, 3, 4, 5]),
                "macos",
                "english"
            )
            .len(),
            2
        );
    }
    #[test]
    fn architecture_is_chosen_among_depots_for_the_selected_language() {
        let mut english = depot(2, "windows", Some("english"), None);
        english.os_arch = Some("64".into());
        let mut french = depot(3, "windows", Some("french"), None);
        french.os_arch = Some("32".into());
        let app = app(vec![english, french]);
        assert_eq!(
            select_depots(&app, &licenses([1], [1, 2, 3, 4, 5]), "windows", "french")[0].depot_id,
            3
        );
    }
    #[test]
    fn free_to_download_shared_depots_use_the_requesting_app_for_authorization() {
        let mut shared = depot(2, "windows", None, None);
        shared.shared_from_app = Some(99);
        let mut app = app(vec![]);
        assert_eq!(content_authorizing_app(&app, &shared), 99);
        app.free_to_download = true;
        assert_eq!(content_authorizing_app(&app, &shared), 1);
    }
    #[test]
    fn installation_lock_prevents_another_session_and_releases_on_drop() {
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("game");
        let first = lock_installation(&destination).unwrap();
        assert!(lock_installation(&destination).is_err());
        // Holding the lock does not prevent the installer itself from recording success.
        assert!(destination.join(".forge-install.lock").is_file());
        drop(first);
        assert!(lock_installation(&destination).is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn installation_lock_rejects_symlinks_and_hardlinks() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("outside");
        std::fs::write(&target, "unchanged").unwrap();
        let symlink_root = parent.path().join("symlink");
        std::fs::create_dir(&symlink_root).unwrap();
        std::os::unix::fs::symlink(&target, symlink_root.join(".forge-install.lock")).unwrap();
        assert!(lock_installation(&symlink_root).is_err());
        let hardlink_root = parent.path().join("hardlink");
        std::fs::create_dir(&hardlink_root).unwrap();
        std::fs::hard_link(&target, hardlink_root.join(".forge-install.lock")).unwrap();
        assert!(lock_installation(&hardlink_root).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
    }
    #[test]
    fn only_successful_download_receipt_is_written_atomically() {
        let root = tempfile::tempdir().unwrap();
        let install = Installation {
            app_id: 10,
            name: "Test".into(),
            directory: root.path().to_string_lossy().into_owned(),
            platform: "windows".into(),
            language: "english".into(),
            total_bytes: 2,
            manifests: vec![InstalledDepot {
                depot_id: 11,
                manifest_id: 12,
            }],
        };
        save_installation(root.path(), &install).unwrap();
        let loaded: Installation = serde_json::from_slice(
            &std::fs::read(root.path().join(".forge-install.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(loaded.manifests[0].manifest_id, 12);
    }
}
