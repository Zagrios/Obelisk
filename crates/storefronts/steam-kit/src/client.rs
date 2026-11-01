//! Account-scoped orchestration. Authentication, CM messages and content formats
//! remain independently usable; no UI or platform keychain code lives here.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;
use obelisk_core::{
    build_choice::{BuildChoice, NOT_NATIVE, Unavailable},
    installation::{self, Destination, InstallFailure, InstalledFiles, TransferError},
    platform::{Platform, Spelling},
    transfer::{
        self, BodyError, BodySize, Cancelled, DownloadProgress, TransferBusy, TransferContext,
        Transfers,
    },
};
use serde::{Deserialize, Serialize, Serializer};
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    chat::{self, ChatEventSource},
    cm::{AppInfo, ChatMode, ContentLicenses, ContentServer, SteamConnection},
    content::{
        CancelToken, CdnAuthorization, ContentClient, ContentError, DepotDownload,
        DepotInstallation,
    },
    invites::{InviteSource, ReceivedInvites},
    own_game::{GameInvite, OwnGame, OwnGameSource},
};
mod build;
mod build_choice;
mod cloud;
#[cfg(test)]
mod depots_tests;

use build::{Kept, PreparedDepot, Revision, Selection};
use build_choice::{Licensed, OwnedApp};

pub use obelisk_core::{installation::TransferProgress, transfer::TransferState};

/// Opening a game and then downloading it should not pay two PICS token and
/// product round trips; publisher metadata rarely changes within a minute.
const APP_INFO_TTL: Duration = Duration::from_secs(60);
/// Valve's web chat tells a friend about typing at most this often.
const TYPING_INTERVAL: Duration = Duration::from_secs(10);

pub use crate::cm::OwnedGameInfo as OwnedGame;
pub use crate::cm::achievements::Achievement;
pub use crate::cm::{GameInviteKind, History, HistoryMessage, MessageSession, SentMessage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GamePlatforms {
    pub app_id: u32,
    pub platforms: Vec<String>,
    /// The game's whole PICS app info, when PICS returned the game.
    pub pics: Option<serde_json::Value>,
}

/// A message key for the host. A stopped download keeps its own variant so
/// its Transfer ends cancelled rather than failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    Failure(String),
    #[error("{0}")]
    Cancelled(String),
}

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
            Self::AuthenticationRequired(error.into())
        } else {
            Self::Other(error.into())
        }
    }
}

pub fn http_client() -> Result<reqwest::Client, ClientError> {
    reqwest::Client::builder()
        .user_agent(concat!("Obelisk/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(90))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        // A depot download opens connections to many CDN servers; those it
        // stops using close soon rather than hold descriptors.
        .pool_idle_timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| ClientError::Failure("backend.steam.client.connectionInitFailed".into()))
}

/// Reads a response body while refusing anything above `limit` bytes, whether
/// advertised up front or discovered while streaming. Nothing cancels it, so
/// it fails only with `Network` or `Size`.
pub(crate) async fn read_bounded(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, BodyError> {
    transfer::read_body(
        response.content_length(),
        response.bytes_stream(),
        BodySize::AtMost(limit as u64),
        &CancelToken::new(),
        |_| {},
    )
    .await
}

#[derive(Clone, Debug)]
pub struct GameDetails {
    pub app_id: u32,
    pub name: String,
    /// The build the game's page shows: the Mac build, unless a download
    /// found that its game does not run natively while it has a Windows
    /// build. None when the account can install neither.
    pub platform: Option<Platform>,
    pub windows_bytes: u64,
    pub macos_bytes: u64,
}

/// How Steam names the two builds, as PICS oslists do; also its receipts',
/// folders' and ids' spelling.
pub const PLATFORM: Spelling = Spelling::new("macos", "windows");

/// Writes a platform as Steam spells it.
pub fn write_platform<S: Serializer>(
    platform: &Platform,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    PLATFORM.serialize(*platform, serializer)
}

/// The folder an installation of `app_id`'s `platform` build gets under its parent.
pub fn installation_directory_name(app_id: u32, platform: Platform) -> String {
    format!("steam-{app_id}-{}", PLATFORM.name(platform))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub app_id: u32,
    pub name: String,
    pub directory: String,
    #[serde(serialize_with = "write_platform")]
    pub platform: Platform,
    pub language: String,
    pub total_bytes: u64,
    pub manifests: Vec<InstalledDepot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledDepot {
    pub depot_id: u32,
    pub manifest_id: u64,
}

/// Everything resolved over the CM before the installation lock is taken.
struct PreparedInstall {
    name: String,
    /// Validated HTTPS CDN/SteamCache hosts allowed to serve this app, in
    /// Steam's order of preference.
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

pub struct Client {
    http: reqwest::Client,
    cm: SteamConnection,
    friends: crate::friends::FriendsEventSource,
    chat: ChatEventSource,
    own_game: OwnGameSource,
    invites: InviteSource,
    /// When each friend last heard that the account is typing.
    typing: Mutex<HashMap<u64, Instant>>,
    connection_abort: tokio::task::AbortHandle,
    cloud_authorized: std::sync::atomic::AtomicBool,
    /// App and depot IDs expanded through PICS licenses, filled on first content query.
    catalog: AsyncMutex<Option<Arc<ContentLicenses>>>,
    app_infos: Mutex<HashMap<u32, (Instant, Arc<AppInfo>)>>,
    /// Each Mac build's verdict, and the depot manifests the last native one
    /// read, by depot and manifest, which its installation starts from.
    build_choice: BuildChoice<Revision, Kept>,
    downloads: Transfers<TransferProgress>,
    /// The session's own token, to ask Steam for web access tokens.
    refresh_token: String,
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
        // A game without achievements needs no request of the player's.
        let Some(schema) = self
            .cm
            .game_achievements(app_id, language)
            .await?
            .filter(|schema| !schema.is_empty())
        else {
            return Ok(Vec::new());
        };
        let user = self.cm.user_achievements(app_id).await?;
        crate::cm::achievements::combine(app_id, schema, user).map_err(Into::into)
    }

    /// When the account got each app its licenses grant, in seconds since
    /// the Unix epoch. It expands the licenses through PICS, as a download
    /// does, once until the library loads again.
    pub async fn acquisitions(&self) -> Result<BTreeMap<u32, u32>, ClientError> {
        Ok(self.licensed_content().await?.acquired.clone())
    }
    #[cfg(test)]
    pub(crate) fn with_test_connection(cm: SteamConnection) -> Self {
        Self::from_connection(http_client().unwrap(), cm, String::new())
    }

    fn from_connection(http: reqwest::Client, cm: SteamConnection, refresh_token: String) -> Self {
        Self {
            http,
            friends: cm.friends_source(),
            chat: cm.chat_source(),
            own_game: cm.own_game_source(),
            invites: cm.invite_source(),
            typing: Mutex::new(HashMap::new()),
            connection_abort: cm.abort_handle(),
            cloud_authorized: std::sync::atomic::AtomicBool::new(true),
            cm,
            catalog: AsyncMutex::new(None),
            app_infos: Mutex::new(HashMap::new()),
            build_choice: BuildChoice::default(),
            downloads: Transfers::default(),
            refresh_token,
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
        // Only this UI session joins the friends chat and shows the account
        // Online; Cloud sessions log on without either.
        cm.log_on(account_name, steam_id, refresh_token, ChatMode::Enabled)
            .await
            .map_err(ClientConnectError::from)?;
        cm.set_online().await.map_err(ClientConnectError::from)?;
        Ok(Self::from_connection(http, cm, refresh_token.into()))
    }

    /// Cookies that sign Steam's sites in as the account, through a web
    /// access token this session asks for.
    pub async fn web_cookies(&self) -> Result<Vec<crate::auth::WebCookie>, ClientError> {
        let (steam_id, token) = self.cm.web_access_token(&self.refresh_token).await?;
        if crate::auth::steam_id_from_refresh_token(&token).ok() != Some(steam_id) {
            return Err(
                crate::cm::CmError::Protocol("backend.steam.cm.invalidSession".into()).into(),
            );
        }
        Ok(crate::auth::web_cookies(steam_id, &token))
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

    /// Shows the account Online again, for instance after a game's own Steam
    /// session changed its status.
    pub async fn set_online(&self) -> Result<(), ClientError> {
        self.cm.set_online().await.map_err(Into::into)
    }

    /// Chat events after `after_sequence`; waits until there are some or the
    /// session ends.
    pub async fn next_chat(&self, after_sequence: u64) -> chat::ChatEvents {
        self.chat.next(after_sequence).await
    }

    pub async fn send_message(&self, friend: u64, text: &str) -> Result<SentMessage, ClientError> {
        let sent = self.cm.send_friend_message(friend, text).await?;
        // The message ends the typing; the next keystroke may say so again.
        self.typing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&friend);
        Ok(sent)
    }

    /// Tells `friend` the account is typing, at most once per interval;
    /// calls in between do nothing.
    pub async fn send_typing(&self, friend: u64) -> Result<(), ClientError> {
        {
            let now = Instant::now();
            let mut typing = self.typing.lock().unwrap_or_else(PoisonError::into_inner);
            typing.retain(|_, sent| now.duration_since(*sent) < TYPING_INTERVAL);
            if typing.contains_key(&friend) {
                return Ok(());
            }
            typing.insert(friend, now);
        }
        let sent = self.cm.send_typing(friend).await;
        if sent.is_err() {
            self.typing
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&friend);
        }
        sent.map_err(Into::into)
    }

    /// Up to `count` (at most 50) messages before `before`, a (timestamp,
    /// ordinal) position, or the latest ones; oldest first.
    pub async fn recent_messages(
        &self,
        friend: u64,
        before: Option<(u32, u32)>,
        count: u32,
    ) -> Result<History, ClientError> {
        self.cm
            .recent_messages(friend, before, count)
            .await
            .map_err(Into::into)
    }

    /// Conversations with messages, with their unread counts.
    pub async fn active_sessions(&self) -> Result<Vec<MessageSession>, ClientError> {
        self.cm.active_sessions().await.map_err(Into::into)
    }

    pub async fn ack(&self, friend: u64, timestamp: u32) -> Result<(), ClientError> {
        self.cm.ack(friend, timestamp).await.map_err(Into::into)
    }

    /// The invitations friends sent, once they differ from `after_revision`
    /// or the session ends.
    pub async fn next_invites(&self, after_revision: u64) -> ReceivedInvites {
        self.invites.next(after_revision).await
    }

    pub async fn next_own_game(&self, after_revision: u64) -> OwnGame {
        self.own_game.next(after_revision).await
    }

    /// Invites `friend` into the account's current game, `app_id`, the way
    /// Steam's friend menu does, and only when that menu would offer it.
    pub async fn invite_to_game(
        &self,
        friend: u64,
        app_id: u32,
    ) -> Result<GameInviteKind, ClientError> {
        let game = self.own_game.snapshot();
        if !game.connected {
            return Err(crate::cm::CmError::Disconnected.into());
        }
        if app_id == 0 || game.app_id != app_id {
            return Err(crate::cm::SocialError::InviteUnavailable.into());
        }
        self.cm
            .invite(friend, app_id, &game.invite())
            .await
            .map_err(Into::into)
    }

    /// Invites `friend` into `invite`, which the account's running game of
    /// `app_id` named itself, such as the lobby its own Invite button asked
    /// Steam's overlay for. Steam may never show this session a private
    /// lobby, so only a persona already showing another game refuses it.
    pub async fn invite_into_game(
        &self,
        friend: u64,
        app_id: u32,
        invite: &GameInvite,
    ) -> Result<GameInviteKind, ClientError> {
        let game = self.own_game.snapshot();
        if !game.connected {
            return Err(crate::cm::CmError::Disconnected.into());
        }
        if app_id == 0 || !invite.sendable() || (game.app_id != 0 && game.app_id != app_id) {
            return Err(crate::cm::SocialError::InviteUnavailable.into());
        }
        self.cm
            .invite(friend, app_id, invite)
            .await
            .map_err(Into::into)
    }

    /// Works even while a download owns the CM query mutex. Observers wake
    /// before the worker is aborted, and the old socket cannot keep reconnecting.
    pub fn shutdown(&self) {
        self.cloud_authorized
            .store(false, std::sync::atomic::Ordering::Release);
        self.friends.disconnect(false);
        self.chat.disconnect();
        self.own_game.disconnect();
        self.invites.disconnect();
        self.connection_abort.abort();
        self.downloads.cancel();
    }

    /// Whether its CM connection ended. A request waiting on the connection
    /// reads the end only once this is true.
    pub fn ended(&self) -> bool {
        !self.cm.logged_on()
    }

    /// The account's name and avatar, as Steam pushes them once it is online.
    pub async fn profile(&self) -> Result<crate::cm::AccountProfile, ClientError> {
        match tokio::time::timeout(crate::cm::PROFILE_TIMEOUT, self.cm.profile()).await {
            Ok(profile) => profile.map_err(Into::into),
            Err(_) => Err(crate::cm::CmError::Timeout.into()),
        }
    }

    /// Most recently played first, then name. Steam's owned games leave out
    /// the free games the account never played, which its store and client
    /// still count, such as Counter-Strike 2 once its store page added it:
    /// the account's own licenses add them back, and the games a family
    /// member lends join them.
    pub async fn games(&self) -> Result<Vec<OwnedGame>, ClientError> {
        // Download rights then follow the licenses this load reads.
        *self.catalog.lock().await = None;
        self.app_infos
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        let (games, licensed) = tokio::join!(self.cm.owned_games(), self.readable_licenses());
        let mut games = games?;
        let unplayed = match licensed {
            Ok(licensed) => self.unplayed_games(&licensed, &games).await,
            Err(error) => Err(error),
        };
        // The owned games alone still make a library.
        match unplayed {
            Ok(unplayed) => {
                games.extend(unplayed);
                games.sort_by(crate::cm::library_order);
            }
            Err(error) => eprintln!("steam-kit: library leaves out never-played games: {error}"),
        }
        Ok(games)
    }

    /// The released games, demos and playtests the account's own licenses
    /// grant that `listed` leaves out, and those a family member lends, which
    /// Steam's owned games never list.
    async fn unplayed_games(
        &self,
        licensed: &ContentLicenses,
        listed: &[OwnedGame],
    ) -> Result<Vec<OwnedGame>, ClientError> {
        let listed: HashSet<_> = listed.iter().map(|game| game.app_id).collect();
        let missing: Vec<_> = licensed
            .apps
            .iter()
            .copied()
            .filter(|app_id| !listed.contains(app_id))
            .collect();
        // Steam withholds the metadata of some apps the licenses grant: those
        // stay out.
        let mut games = Vec::new();
        for app in self.cm.readable_app_infos(&missing).await? {
            // Steam's free license also grants apps the store does not sell,
            // such as Valve's Spacewar test app and Half-Life 2's old demo.
            if licensed.free_apps.contains(&app.app_id) && !app.on_store() {
                continue;
            }
            // A publisher can keep a game from the family's libraries.
            if !licensed.own_apps.contains(&app.app_id) && app.excluded_from_sharing() {
                continue;
            }
            // The DLC and tools a family library holds are not worth keeping.
            let Some(game) = app.unplayed_game() else {
                continue;
            };
            self.remember_app(&Arc::new(app));
            games.push(game);
        }
        Ok(games)
    }

    /// Serves PICS metadata from a short-lived cache.
    async fn app_info(&self, app_id: u32) -> Result<Arc<AppInfo>, ClientError> {
        if let Some(app) = self.cached_app(app_id) {
            return Ok(app);
        }
        let app = Arc::new(self.cm.app_info(app_id).await?);
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
        let mut missing = HashSet::new();
        for id in app_ids {
            if let Some(app) = self.cached_app(*id) {
                apps.insert(*id, app);
            } else {
                missing.insert(*id);
            }
        }
        if missing.is_empty() {
            return Ok(apps);
        }
        let requested: Vec<_> = app_ids
            .iter()
            .copied()
            .filter(|id| missing.contains(id))
            .collect();
        for app in self.cm.app_infos(&requested).await? {
            if missing.contains(&app.app_id) {
                let app = Arc::new(app);
                self.remember_app(&app);
                apps.insert(app.app_id, app);
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

    /// Every package the licenses grant, which downloads need.
    /// Player.GetOwnedGames is presentation data, not proof of depot/DLC rights.
    async fn licensed_content(&self) -> Result<Arc<ContentLicenses>, ClientError> {
        let owned = self.readable_licenses().await?;
        owned.complete()?;
        Ok(owned)
    }

    /// The licenses as far as Steam describes them: a package it withholds
    /// costs the library only its own games. One caller expands them; the
    /// others wait for its catalog, which only a complete expansion becomes,
    /// so rights missing a package ask Steam again.
    async fn readable_licenses(&self) -> Result<Arc<ContentLicenses>, ClientError> {
        let mut catalog = self.catalog.lock().await;
        if let Some(owned) = catalog.as_ref() {
            return Ok(Arc::clone(owned));
        }
        let owned = Arc::new(self.cm.content_licenses().await?);
        if owned.complete().is_ok() {
            *catalog = Some(Arc::clone(&owned));
        } else {
            eprintln!(
                "steam-kit: licenses leave out the packages Steam withholds: {:?} {:?}",
                owned.unknown_packages, owned.withheld_packages
            );
        }
        Ok(owned)
    }

    /// The account's licenses and the PICS metadata of `app_id`, which the
    /// account must own.
    async fn owned_app(
        &self,
        app_id: u32,
    ) -> Result<(Arc<ContentLicenses>, Arc<AppInfo>), ClientError> {
        let owned = self.licensed_content().await?;
        if !owned.apps.contains(&app_id) {
            return Err(ClientError::Failure(
                "backend.steam.client.gameNotOwned".into(),
            ));
        }
        let app = self.app_info(app_id).await?;
        Ok((owned, app))
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
                pics: apps.get(&app_id).map(|app| app.metadata()),
            })
            .collect())
    }

    pub async fn details(&self, app_id: u32, language: &str) -> Result<GameDetails, ClientError> {
        let owned = self.licensed_content().await?;
        let app = self.app_info(app_id).await?;
        let windows = Selection::of(&app, &owned, Platform::Windows, language);
        let macos = Selection::of(&app, &owned, Platform::Mac, language);
        let (windows_bytes, macos_bytes) = (windows.size_bytes(), macos.size_bytes());
        // Not `OwnedApp::builds`: a page also shows a game the account does
        // not own, and needs the depots for their sizes.
        let offered = Licensed {
            owned,
            app: Arc::clone(&app),
        }
        .offered(!macos.is_empty(), !windows.is_empty());
        // Only a download reads the Mac build's files: the page shows the Mac
        // build until one found that its game does not run natively.
        let game = OwnedApp {
            client: self,
            app_id,
            language,
        };
        let platform = self
            .build_choice
            .shown(&game, offered)
            .await
            .map(|(platform, _)| platform);
        Ok(GameDetails {
            app_id,
            name: app.name.clone(),
            platform,
            windows_bytes,
            macos_bytes,
        })
    }

    pub fn cancel_download(&self) {
        self.build_choice.cancel();
        self.downloads.cancel();
    }

    /// Resolve only a completed installation belonging to this account. PICS is
    /// fetched again so executable paths and launch arguments follow the publisher.
    pub async fn launch_plan(
        &self,
        app_id: u32,
        platform: Platform,
        directory: &Path,
    ) -> Result<crate::launch::LaunchPlan, ClientError> {
        if !directory.is_absolute() || !directory.is_dir() {
            return Err(ClientError::Failure(
                "backend.steam.launch.directoryMissing".into(),
            ));
        }
        // No Transfer of this installation can start until the plan returns.
        let installed = installation::open::<Installation>(directory)?;
        let receipt = installed
            .receipt
            .as_ref()
            .filter(|receipt| receipt.app_id == app_id && receipt.platform == platform)
            .ok_or_else(|| {
                ClientError::Failure("backend.steam.client.incompleteInstallation".into())
            })?;
        let owned = self.licensed_content().await?;
        if !owned.apps.contains(&receipt.app_id) {
            return Err(ClientError::Failure(
                "backend.steam.client.gameNotOwned".into(),
            ));
        }
        let app = Arc::new(self.cm.app_info(app_id).await?);
        self.remember_app(&app);
        crate::launch::plan(&app, platform, directory).map_err(Into::into)
    }

    /// The build a download of the game installs: of those the account's
    /// licenses grant, the Mac build when its game runs natively on Apple
    /// silicon, else the Windows build. It asks Steam for the licenses, then
    /// reads a few of the Mac build's files; `cancel_download` stops it.
    pub async fn platform(&self, app_id: u32, language: &str) -> Result<Platform, ClientError> {
        if !crate::language::is_supported(language) {
            return Err(ClientError::Failure(
                "backend.steam.client.unsupportedPlatformOrLanguage".into(),
            ));
        }
        self.build_choice
            .pick(async |pick| {
                pick.of(&OwnedApp {
                    client: self,
                    app_id,
                    language,
                })
                .await
            })
            .await
    }

    /// Installs the game's build for `platform` under `parent`.
    pub async fn download(
        &self,
        app_id: u32,
        platform: Platform,
        language: &str,
        parent: &Path,
    ) -> Result<Installation, ClientError> {
        if !crate::language::is_supported(language) {
            return Err(ClientError::Failure(
                "backend.steam.client.unsupportedPlatformOrLanguage".into(),
            ));
        }
        // The chosen folder is the only caller-controlled path. App IDs and depot
        // manifest validation determine every path beneath it.
        let directory = installation_directory_name(app_id, platform);
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

    /// The validated HTTPS CDN and SteamCache hosts allowed to serve the app.
    /// Every one is kept: Steam's first servers are often its busiest, and a
    /// depot spreads over the fastest ones it measures.
    async fn content_servers(&self, app_id: u32) -> Result<Vec<ContentServer>, ClientError> {
        let servers: Vec<ContentServer> = self
            .cm
            .content_servers()
            .await?
            .into_iter()
            .filter(|server| {
                server.allowed_app_ids.is_empty() || server.allowed_app_ids.contains(&app_id)
            })
            .collect();
        if servers.is_empty() {
            return Err(ClientError::Failure(
                "backend.steam.client.noContentServer".into(),
            ));
        }
        Ok(servers)
    }

    /// Whether Steam offers a newer build than the one installed in
    /// `directory`: the public manifests of the depots a download would pick
    /// differ from those it installed. An installation whose receipt is gone
    /// needs its Transfer too. It reads PICS only, never the game's files.
    pub async fn update_available(
        &self,
        app_id: u32,
        platform: Platform,
        language: &str,
        directory: &Path,
    ) -> Result<bool, ClientError> {
        let Some(installed) = installation::read_receipt::<Installation>(directory)?
            .filter(|receipt| receipt.app_id == app_id && receipt.platform == platform)
        else {
            return Ok(true);
        };
        let owned = self.licensed_content().await?;
        let app = self.app_info(app_id).await?;
        let build = self
            .build_of(Selection::of(&app, &owned, platform, language))
            .await?;
        Ok(build.is_update_of(&installed.manifests))
    }

    /// Installs every prepared depot under the installation lock, including
    /// waits for refreshed request codes.
    async fn install_depots(
        &self,
        app_id: u32,
        platform: Platform,
        language: &str,
        mut prepared: PreparedInstall,
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
        let mut scripts = Vec::new();
        let content = ContentClient::new(self.http.clone());
        for depot in &mut prepared.depots {
            cancel.check()?;
            let DepotInstallation {
                summary,
                install_scripts,
            } = self
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
            base.network += summary.downloaded_bytes;
            base.content_bytes += summary.content_bytes;
            manifests.push(InstalledDepot {
                depot_id: depot.depot_id,
                manifest_id: depot.manifest_id,
            });
            scripts.extend(install_scripts);
        }
        scripts.sort();
        obelisk_core::install::write_cached_file(
            destination,
            Path::new(crate::install_script::LIST),
            scripts.join("\n").as_bytes(),
        )
        .map_err(ContentError::from)?;
        Ok(InstalledFiles {
            receipt: Installation {
                app_id,
                name: prepared.name,
                directory: destination.to_string_lossy().into_owned(),
                platform,
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
        platform: Platform,
        language: &str,
        cancel: &CancelToken,
    ) -> Result<PreparedInstall, ClientError> {
        let (owned, app) = self.owned_app(app_id).await?;
        let selection = Selection::of(&app, &owned, platform, language);
        if selection.is_empty() {
            return Err(ClientError::Failure(
                "backend.steam.client.noCompatibleDepot".into(),
            ));
        }
        let servers = self.content_servers(app_id);
        let depots = async {
            cancel.check()?;
            let build = self.build_of(selection).await?;
            let mut depots = build.prepare(&self.cm, cancel).await?;
            // The manifests the build's pick read, kept under the build's
            // revision.
            if platform == Platform::Mac
                && let Some(mut picked) = self.build_choice.take(&build.revision())
            {
                for depot in &mut depots {
                    depot.picked = picked.remove(&(depot.depot_id, depot.manifest_id));
                }
            }
            Ok(depots)
        };
        let (servers, depots) = tokio::try_join!(servers, depots)?;
        Ok(PreparedInstall {
            name: app.name.clone(),
            servers,
            depots,
        })
    }

    /// Installs one depot from every server allowed to serve it. A server that
    /// refuses the depot is authorized over the CM.
    async fn install_depot(
        &self,
        content: &ContentClient,
        depot: &mut PreparedDepot,
        servers: &[ContentServer],
        destination: &Path,
        transfer: &TransferContext<TransferProgress>,
        base: ProgressBase,
    ) -> Result<DepotInstallation, ClientError> {
        let cancel = transfer.cancel_token();
        // The current manifest ID and depot key came from authorized Steam
        // metadata above. Keep one validated snapshot across all CDN requests.
        let cached_manifest = match content
            .cached_manifest(destination, depot.depot_id, depot.manifest_id, &depot.key)
            .await?
        {
            Some(cached) => Some(cached),
            None => depot.picked.take(),
        };
        cancel.check()?;
        // No request code is needed for an already validated cached archive. A
        // miss always gets a fresh code just before the CDN.
        let request_code = if cached_manifest.is_some() {
            0
        } else {
            depot.request_code(&self.cm).await?
        };
        let authorization = depot.authorization(&self.cm);
        let plan = DepotDownload {
            depot_id: depot.depot_id,
            manifest_id: depot.manifest_id,
            request_code,
            depot_key: depot.key,
            servers: servers.to_vec(),
            authorization: Some(&authorization),
            destination: destination.to_owned(),
            cached_manifest,
        };
        let depot_label =
            crate::message::format("backend.steam.client.depot", &[depot.depot_id.to_string()]);
        // Progress arrives from the content module's network and disk
        // threads, so the callback owns everything it touches.
        let transfer = transfer.clone();
        let size = depot.estimated_bytes;
        content
            .download_depot(&plan, cancel, move |progress: DownloadProgress| {
                transfer.update(|status| {
                    status.show(progress);
                    status.completed_bytes += base.completed;
                    status.total_bytes += base.total_estimate.saturating_sub(size);
                    status.network_bytes += base.network;
                    status.content_bytes += base.content_bytes;
                    status.message.clone_from(&depot_label);
                });
            })
            .await
            .map_err(Into::into)
    }
}

/// Asks Steam for the CDN authorization a server requires for one depot.
pub(crate) struct DepotAuthorization<'a> {
    pub(crate) cm: &'a SteamConnection,
    pub(crate) app_id: u32,
    pub(crate) depot_id: u32,
}

impl CdnAuthorization for DepotAuthorization<'_> {
    fn token<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            self.cm
                .cdn_auth(self.app_id, self.depot_id, host)
                .await
                .ok()
        })
    }
}

/// Lower layers keep typed errors; the client passes message keys to the host.
impl From<Cancelled> for ClientError {
    fn from(_: Cancelled) -> Self {
        ClientError::Cancelled("backend.steam.client.downloadStopped".into())
    }
}

impl From<Unavailable> for ClientError {
    fn from(unavailable: Unavailable) -> Self {
        ClientError::Failure(
            match unavailable {
                Unavailable::NoBuild => "backend.steam.client.noCompatibleDepot",
                Unavailable::NotNative => NOT_NATIVE,
            }
            .into(),
        )
    }
}

impl From<ContentError> for ClientError {
    fn from(error: ContentError) -> Self {
        if matches!(error, ContentError::Cancelled) {
            ClientError::Cancelled(error.to_string())
        } else {
            ClientError::Failure(error.to_string())
        }
    }
}

impl From<crate::cm::CmError> for ClientError {
    fn from(error: crate::cm::CmError) -> Self {
        ClientError::Failure(error.to_string())
    }
}

impl From<crate::cm::SocialError> for ClientError {
    fn from(error: crate::cm::SocialError) -> Self {
        ClientError::Failure(error.to_string())
    }
}

impl From<crate::launch::LaunchError> for ClientError {
    fn from(error: crate::launch::LaunchError) -> Self {
        ClientError::Failure(error.to_string())
    }
}

impl From<TransferBusy> for ClientError {
    fn from(_: TransferBusy) -> Self {
        ClientError::Failure("backend.steam.client.downloadInProgress".into())
    }
}

impl From<InstallFailure> for ClientError {
    fn from(failure: InstallFailure) -> Self {
        ClientError::Failure(
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
        matches!(self, Self::Cancelled(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cm::supports_platform;

    #[test]
    fn steam_spells_its_builds_as_pics_oslists_do() {
        let installation = Installation {
            app_id: 620,
            name: "Portal 2".into(),
            directory: "/Games/steam-620-macos".into(),
            platform: Platform::Mac,
            language: "english".into(),
            total_bytes: 1,
            manifests: Vec::new(),
        };
        let written = serde_json::to_value(&installation).unwrap();
        assert_eq!(written["platform"], "macos");
        let read: Installation = serde_json::from_value(written).unwrap();
        assert_eq!(read.platform, Platform::Mac);
        assert_eq!(
            installation_directory_name(620, Platform::Mac),
            "steam-620-macos"
        );
        assert_eq!(
            installation_directory_name(620, Platform::Windows),
            "steam-620-windows"
        );
        assert!(supports_platform(&["windows".into()], Platform::Windows));
        assert!(!supports_platform(&["windows".into()], Platform::Mac));
        assert!(supports_platform(&[], Platform::Mac));
    }

    #[test]
    fn a_game_without_an_installable_build_says_why() {
        let message = |unavailable| match ClientError::from(unavailable) {
            ClientError::Failure(message) => message,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            message(Unavailable::NoBuild),
            "backend.steam.client.noCompatibleDepot"
        );
        assert_eq!(
            message(Unavailable::NotNative),
            "backend.install.macBuildNotNative"
        );
    }
}
