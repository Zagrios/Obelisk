//! Account-scoped orchestration. Authentication, CM messages and content formats
//! remain independently usable; no UI or platform keychain code lives here.
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use sailover_core::{
    installation::{self, Destination, InstallFailure, InstalledFiles, TransferError},
    transfer::{
        Cancelled, DownloadProgress, DownloadSummary, TransferBusy, TransferContext, Transfers,
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    cm::{
        AppInfo, ContentLicenses, ContentServer, DepotInfo, PICS_BATCH, SteamConnection,
        supports_platform,
    },
    content::{CancelToken, ContentClient, ContentError, DepotDownload},
};
mod cloud;

pub use sailover_core::{installation::TransferProgress, transfer::TransferState};

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
        .user_agent(concat!("SailOver/", env!("CARGO_PKG_VERSION")))
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
/// Steamworks Common Redistributables, free to every account. Windows games
/// name its depots, such as Visual C++ or DirectX, for Steam to install and run
/// before their first launch.
const COMMON_REDISTRIBUTABLES: u32 = 228_980;
/// Its depots a download leaves out: .NET Framework and XNA, which needs it,
/// 229000 to 229012. Their installers do not work in Wine, and .NET games run
/// on Wine Mono instead. The others, such as Visual C++, install in the prefix
/// as Steam installs them under Proton.
const SKIPPED_REDISTRIBUTABLES: std::ops::RangeInclusive<u32> = 229_000..=229_012;

struct PreparedDepot {
    depot_id: u32,
    manifest_id: u64,
    authorizing_app: u32,
    key: [u8; 32],
    estimated_bytes: u64,
}

/// Everything resolved over the CM before the installation lock is taken.
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
    cm: SteamConnection,
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
    /// Other requests may run between the two reads; a schema change in that
    /// window is already rejected by the join.
    pub async fn achievements(
        &self,
        app_id: u32,
        language: &str,
    ) -> Result<Vec<Achievement>, ClientError> {
        let schema = self
            .cm
            .game_achievements(app_id, language)
            .await
            .map_err(client_error)?;
        let user = self
            .cm
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
            cm,
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
            cm,
            catalog: AsyncMutex::new(None),
            app_infos: Mutex::new(HashMap::new()),
            downloads: Transfers::default(),
        })
    }

    pub async fn next_friends(&self, after_revision: u64) -> crate::friends::FriendsSnapshot {
        // Publish presence immediately. Optional title lookup must never hold up
        // a new status, game exit or disconnect, even while content owns CM.
        tokio::select! {
            biased;
            snapshot = self.friends.next(after_revision) => snapshot,
            () = self.resolve_friend_game_names() => self.friends.next(after_revision).await,
        }
    }

    /// One pass over the currently unknown titles: apps that appear while the
    /// lookup is in flight are picked up by the next call. An unresolved app
    /// caches as `None`, so a failed lookup is retried only on a later event.
    async fn resolve_friend_game_names(&self) {
        let ids = self.friends.missing_game_names();
        if ids.is_empty() {
            return;
        }
        let account_id = self.friends.account_id();
        // Keep presence usable; a lookup failure must not stall the snapshot.
        let Ok(apps) = self.cached_app_infos(&ids).await else {
            return;
        };
        let names: Vec<_> = ids
            .iter()
            .map(|id| (*id, apps.get(id).map(|app| app.name.clone())))
            .collect();
        self.friends.resolve_game_names(account_id, &names);
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
            let request = self.cm.request_profile().await.map_err(client_error)?;
            request.receive().await.map_err(client_error)
        })
        .await
        .map_err(|_| client_error(crate::cm::CmError::Timeout))?
    }

    /// Ordered by the CM layer: most recently played first, then name.
    pub async fn games(&self) -> Result<Vec<OwnedGame>, ClientError> {
        let games = self.cm.owned_games().await.map_err(client_error)?;
        // Rendering the library does not require expanding every license through
        // PICS. Refresh license-derived download rights only when next needed.
        *self.catalog.lock().await = None;
        self.app_infos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        Ok(games)
    }

    /// Serves PICS metadata from a short-lived cache.
    async fn app_info(
        &self,
        cm: &SteamConnection,
        app_id: u32,
    ) -> Result<Arc<AppInfo>, ClientError> {
        if let Some(app) = self.cached_app(app_id) {
            return Ok(app);
        }
        let app = Arc::new(cm.app_info(app_id).await.map_err(client_error)?);
        self.remember_app(&app);
        Ok(app)
    }

    /// PICS metadata for many apps, served from the short-lived cache and
    /// fetched in bounded batches. Apps PICS does not return are absent.
    async fn cached_app_infos(
        &self,
        app_ids: &[u32],
    ) -> Result<HashMap<u32, Arc<AppInfo>>, ClientError> {
        let mut apps = HashMap::with_capacity(app_ids.len());
        for batch in app_ids.chunks(PICS_BATCH) {
            let mut missing = Vec::new();
            for id in batch {
                if let Some(app) = self.cached_app(*id) {
                    apps.insert(*id, app);
                } else {
                    missing.push(*id);
                }
            }
            if missing.is_empty() {
                continue;
            }
            for app in self.cm.app_infos(&missing).await.map_err(client_error)? {
                if missing.contains(&app.app_id) {
                    let app = Arc::new(app);
                    self.remember_app(&app);
                    apps.insert(app.app_id, app);
                }
            }
        }
        Ok(apps)
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

    /// One caller expands the licenses; the others wait for its catalog.
    /// Player.GetOwnedGames is presentation data, not proof of depot/DLC rights.
    async fn licensed_content(
        &self,
        cm: &SteamConnection,
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
        let apps = self.cached_app_infos(&app_ids).await?;
        Ok(app_ids
            .into_iter()
            .map(|app_id| GamePlatforms {
                app_id,
                platforms: apps
                    .get(&app_id)
                    .map_or_else(Vec::new, |app| app.os_list.clone()),
            })
            .collect())
    }

    pub async fn details(&self, app_id: u32, language: &str) -> Result<GameDetails, ClientError> {
        let cm = &self.cm;
        let owned = self.licensed_content(cm).await?;
        let app = self.app_info(cm, app_id).await?;
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
        // No Transfer of this installation can start until the plan returns.
        let installed = installation::open::<Installation>(directory)?;
        let receipt = installed
            .receipt
            .as_ref()
            .filter(|receipt| receipt.app_id == app_id && receipt.platform == platform)
            .ok_or_else(|| ClientError("backend.steam.client.incompleteInstallation".into()))?;
        let cm = &self.cm;
        let owned = self.licensed_content(cm).await?;
        if !owned.apps.contains(&receipt.app_id) {
            return Err(ClientError("backend.steam.client.gameNotOwned".into()));
        }
        let app = Arc::new(cm.app_info(app_id).await.map_err(client_error)?);
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
        if !matches!(platform, "windows" | "macos") || !crate::language::is_supported(language) {
            return Err(ClientError(
                "backend.steam.client.unsupportedPlatformOrLanguage".into(),
            ));
        }
        // The chosen folder is the only caller-controlled path. App IDs and depot
        // manifest validation determine every path beneath it.
        let directory = format!("steam-{app_id}-{platform}");
        installation::install(
            &self.downloads,
            app_id.to_string(),
            Destination {
                parent,
                directory: &directory,
            },
            |_, cancel| async move {
                self.prepare_depots(app_id, platform, language, &cancel)
                    .await
            },
            |prepared, destination, transfer| async move {
                self.install_depots(
                    app_id,
                    platform,
                    language,
                    prepared,
                    &destination,
                    &transfer,
                )
                .await
            },
        )
        .await
    }

    /// Installs every prepared depot under the installation lock, including
    /// waits for refreshed request codes.
    async fn install_depots(
        &self,
        app_id: u32,
        platform: &str,
        language: &str,
        prepared: PreparedInstall,
        destination: &Path,
        transfer: &TransferContext<TransferProgress>,
    ) -> Result<InstalledFiles<Installation>, ClientError> {
        let cancel = transfer.cancel_token();
        let mut base = ProgressBase {
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
                    destination,
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
        let mut scripts = Vec::new();
        for depot in &prepared.depots {
            scripts.extend(
                content
                    .install_scripts(destination, depot.depot_id, depot.manifest_id, &depot.key)
                    .await
                    .map_err(client_error)?,
            );
        }
        scripts.sort();
        sailover_core::install::write_cached_file(
            destination,
            Path::new(crate::install_script::LIST),
            scripts.join("\n").as_bytes(),
        )
        .map_err(|error| client_error(ContentError::from(error)))?;
        Ok(InstalledFiles {
            receipt: Installation {
                app_id,
                name: prepared.name,
                directory: destination.to_string_lossy().into_owned(),
                platform: platform.into(),
                language: language.into(),
                total_bytes: base.completed,
                manifests,
            },
            total_bytes: base.completed,
            network_bytes: base.network,
            content_bytes: base.content_bytes,
        })
    }

    /// Resolves ownership, depot selection, manifest IDs, depot keys and CDN
    /// candidates over the CM, before any installation file exists.
    async fn prepare_depots(
        &self,
        app_id: u32,
        platform: &str,
        language: &str,
        cancel: &CancelToken,
    ) -> Result<PreparedInstall, ClientError> {
        let cm = &self.cm;
        let owned = self.licensed_content(cm).await?;
        if !owned.apps.contains(&app_id) {
            return Err(ClientError("backend.steam.client.gameNotOwned".into()));
        }
        let app = self.app_info(cm, app_id).await?;
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
                    .app_info(cm, manifest_app)
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
            let key_app = if depot.shared_from_app == Some(COMMON_REDISTRIBUTABLES) {
                COMMON_REDISTRIBUTABLES
            } else {
                app_id
            };
            let key = cm
                .depot_key(depot.depot_id, key_app)
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
                    status.state = progress.phase.into();
                    status.completed_bytes = base.completed
                        + progress
                            .verification_bytes
                            .unwrap_or(progress.completed_bytes);
                    status.total_bytes =
                        base.total_estimate.saturating_sub(size) + progress.total_bytes;
                    status.network_bytes = base.network + bytes.network;
                    status.content_bytes = base.content_bytes + bytes.content;
                    status.current_file = progress.current_file;
                    status.message.clone_from(&depot_label);
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
            let redistributable = depot.shared_from_app == Some(COMMON_REDISTRIBUTABLES);
            supports_platform(&depot.os_list, platform)
                && !(redistributable && SKIPPED_REDISTRIBUTABLES.contains(&depot.depot_id))
                && (app.free_to_download
                    || owned.depots.contains(&depot.depot_id)
                    || redistributable)
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

/// Lower layers keep typed errors; the client passes message keys to the host.
impl From<Cancelled> for ClientError {
    fn from(_: Cancelled) -> Self {
        ClientError("backend.steam.client.downloadStopped".into())
    }
}

fn client_error(error: impl std::fmt::Display) -> ClientError {
    ClientError(error.to_string())
}
impl From<TransferBusy> for ClientError {
    fn from(_: TransferBusy) -> Self {
        ClientError("backend.steam.client.downloadInProgress".into())
    }
}

impl From<InstallFailure> for ClientError {
    fn from(failure: InstallFailure) -> Self {
        ClientError(
            match failure {
                InstallFailure::ParentMissing => "backend.steam.client.chooseExistingFolder",
                InstallFailure::UnsafeDirectory(_) => {
                    "backend.steam.client.unsupportedPlatformOrLanguage"
                }
                InstallFailure::DestinationIsLink(_) => {
                    "backend.steam.client.installationDirectorySymlink"
                }
                InstallFailure::DestinationIsFile(_) => {
                    "backend.steam.client.installationDirectoryIsFile"
                }
                InstallFailure::UnsafeLock(_) => "backend.steam.client.invalidInstallationLock",
                InstallFailure::Locked => "backend.steam.client.installationLocked",
                InstallFailure::UnsafeReceipt(_) => "backend.steam.client.installationFileSymlink",
                InstallFailure::Io(_) => "backend.steam.client.storageFailed",
            }
            .into(),
        )
    }
}

impl TransferError for ClientError {
    fn is_cancellation(&self) -> bool {
        self.0 == "backend.steam.client.downloadStopped"
            || self.0 == "backend.steam.content.cancelled"
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
    fn a_windows_game_gets_the_common_redistributables_it_names_without_owning_them() {
        let mut directx = depot(228_990, "windows", None, None);
        directx.shared_from_app = Some(COMMON_REDISTRIBUTABLES);
        directx.public_manifest = None;
        let mut visual_cpp = depot(228_989, "windows", None, None);
        visual_cpp.shared_from_app = Some(COMMON_REDISTRIBUTABLES);
        let mut dotnet = depot(229_005, "windows", None, None);
        dotnet.shared_from_app = Some(COMMON_REDISTRIBUTABLES);
        let mut other = depot(3, "windows", None, None);
        other.shared_from_app = Some(99);
        other.public_manifest = None;
        let app = app(vec![
            depot(2, "windows", None, None),
            directx,
            visual_cpp,
            dotnet,
            other,
        ]);
        let selected: Vec<_> = select_depots(
            &app,
            &licenses([1], [2, 228_989, 229_005]),
            "windows",
            "english",
        )
        .iter()
        .map(|depot| depot.depot_id)
        .collect();
        assert_eq!(selected, [2, 228_990, 228_989]);
        assert!(select_depots(&app, &licenses([1], [2]), "macos", "english").is_empty());
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
}
