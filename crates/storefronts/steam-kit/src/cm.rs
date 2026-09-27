//! Authenticated Steam CM content queries over TLS WebSocket.
//!
//! Requests on one connection run concurrently: its background worker routes
//! each response to the request that sent its job, and keeps heartbeats going
//! while callers download content over HTTP. Dropping it closes that session.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use rsa::rand_core::{OsRng, RngCore};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::tungstenite::{Message as WsMessage, protocol::WebSocketConfig};

use crate::{friends, kv, proto};
pub mod achievements;
mod cloud;
mod pics;
use pics::parse_app;
pub(crate) use pics::supports_platform;
pub use pics::{AppInfo, ContentLicenses, DepotInfo, LaunchConfig};
#[cfg(test)]
mod login_tests;
#[cfg(test)]
mod platforms_tests;
#[cfg(test)]
mod test_server;

const PROTOCOL: u32 = 65_581;
const PROTO_MASK: u32 = 0x8000_0000;
const MAX_PACKET: usize = 32 * 1024 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(45);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const PROFILE_TIMEOUT: Duration = Duration::from_secs(5);
/// Apps and packages PICS accepts in one access-token and product request.
pub(crate) const PICS_BATCH: usize = 50;
const MULTI: u32 = 1;
const SERVICE_RESPONSE: u32 = 147;
const SERVICE_REQUEST: u32 = 151;
// Content jobs begin at1; job0 belongs only to initial social hydration.
const FRIENDS_HYDRATION_JOB: u64 = 0;
const HEARTBEAT: u32 = 703;
const LOGON_RESPONSE: u32 = 751;
const LOGGED_OFF: u32 = 757;
const PERSONA_STATE: u32 = 766;
const LICENSE_LIST: u32 = 780;
const FRIEND_DATA_REQUEST: u32 = 815;
const PERSONA_PLAYER_NAME: u32 = 2;
const PERSONA_PRESENCE: u32 = 16;
const PERSONA_PROFILE: u32 = PERSONA_PLAYER_NAME | PERSONA_PRESENCE;
const DEPOT_KEY_REQUEST: u32 = 5438;
const DEPOT_KEY_RESPONSE: u32 = 5439;
const LOGON: u32 = 5514;
const PRODUCT_REQUEST: u32 = 8903;
const PRODUCT_RESPONSE: u32 = 8904;
const TOKEN_REQUEST: u32 = 8905;
const TOKEN_RESPONSE: u32 = 8906;
const HELLO: u32 = 9805;

#[derive(Debug, thiserror::Error)]
pub enum CmError {
    #[error("backend.steam.cm.disconnected")]
    Disconnected,
    #[error("backend.steam.cm.timeout")]
    Timeout,
    #[error("{}", crate::message::format("backend.steam.cm.rejected", &[.action.to_string(), .result.to_string()]))]
    Rejected { action: &'static str, result: i32 },
    #[error("{}", crate::message::format("backend.steam.cm.protocol", std::slice::from_ref(.0)))]
    Protocol(String),
    #[error("{}", crate::message::format("backend.steam.cm.missingApp", &[.0.to_string()]))]
    MissingApp(u32),
    #[error("backend.steam.cm.noContentServer")]
    NoContentServer,
    #[error("{}", crate::message::format("backend.steam.cm.directory", &[.0.to_string()]))]
    Directory(#[from] reqwest::Error),
}

impl CmError {
    /// A rejection whose EResult means the session itself is no longer valid,
    /// as opposed to a transient service or transport failure.
    pub fn authentication_required(&self) -> bool {
        matches!(self, Self::Rejected { result, .. } if friends::authentication_required(*result))
    }
}

impl From<prost::DecodeError> for CmError {
    fn from(_: prost::DecodeError) -> Self {
        Self::Protocol("backend.steam.cm.unreadableProtobuf".into())
    }
}

impl From<kv::KvError> for CmError {
    fn from(error: kv::KvError) -> Self {
        Self::Protocol(error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct OwnedGameInfo {
    pub app_id: u32,
    pub name: String,
    pub icon_hash: String,
    pub last_played: u64,
    pub playtime_minutes: u32,
}

/// An HTTPS CDN or SteamCache server. `vhost` is the download host and always
/// falls back to `host`; both are validated before the server is returned.
#[derive(Clone, Debug)]
pub struct ContentServer {
    pub host: String,
    pub vhost: String,
    pub allowed_app_ids: Vec<u32>,
}

struct Packet {
    kind: u32,
    header: proto::Header,
    /// A slice of the received frame (or of its gzip expansion); never copied.
    body: Bytes,
}

enum Command {
    Send(Vec<u8>),
    /// Sends a request whose responses `route` reaches.
    Request {
        bytes: Vec<u8>,
        route: Route,
        replies: mpsc::UnboundedSender<Packet>,
    },
    Account(u64),
    Session {
        steam_id: u64,
        session_id: i32,
        heartbeat_seconds: u64,
    },
}

/// Which responses reach a request: those answering its job or, for the
/// logon reply, the next one of a kind.
#[derive(Clone, Copy)]
enum Route {
    Job(u64),
    Kind(u32),
}

/// Responses Steam addresses to one request's job.
const JOB_RESPONSES: [u32; 4] = [
    SERVICE_RESPONSE,
    DEPOT_KEY_RESPONSE,
    PRODUCT_RESPONSE,
    TOKEN_RESPONSE,
];

/// Requests waiting for their responses. Each response reaches only its own
/// request, so requests never queue behind each other; a response to a
/// request that gave up is dropped.
#[derive(Default)]
struct Routes(Vec<(Route, mpsc::UnboundedSender<Packet>)>);

impl Routes {
    fn wait(&mut self, route: Route, replies: mpsc::UnboundedSender<Packet>) {
        self.0.retain(|(_, replies)| !replies.is_closed());
        self.0.push((route, replies));
    }

    fn deliver(&mut self, packet: Packet) {
        let waiting = self.0.iter().find(|(route, replies)| {
            !replies.is_closed()
                && match *route {
                    Route::Job(job) => {
                        JOB_RESPONSES.contains(&packet.kind)
                            && packet.header.job_target == Some(job)
                    }
                    Route::Kind(kind) => packet.kind == kind,
                }
        });
        if let Some((_, replies)) = waiting {
            let _ = replies.send(packet);
        }
    }
}

/// The responses to one request, each within the query deadline.
struct Replies {
    packets: mpsc::UnboundedReceiver<Packet>,
    kind: u32,
}

impl Replies {
    async fn next(&mut self) -> Result<Packet, CmError> {
        let deadline = Instant::now() + QUERY_TIMEOUT;
        loop {
            let packet = timeout_at(deadline, self.packets.recv())
                .await
                .map_err(|_| CmError::Timeout)?
                .ok_or(CmError::Disconnected)?;
            if packet.kind != self.kind {
                continue;
            }
            if packet
                .header
                .transport_error
                .is_some_and(|error| error != 0 && error != 1)
            {
                return Err(CmError::Rejected {
                    action: "backend.steam.cm.action.requestTransport",
                    result: packet.header.transport_error.unwrap_or(2),
                });
            }
            return Ok(packet);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountProfile {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Default)]
struct ProfileSnapshot {
    value: AccountProfile,
    name_revision: u64,
    avatar_revision: u64,
}

#[derive(Clone)]
enum ProfileState {
    Ready(ProfileSnapshot),
    Disconnected,
}

impl Default for ProfileState {
    fn default() -> Self {
        Self::Ready(ProfileSnapshot::default())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ProfileUpdate {
    // Outer None means Steam did not include this field group in the update.
    display_name: Option<Option<String>>,
    avatar_url: Option<Option<String>>,
}

impl ProfileSnapshot {
    fn apply(&mut self, update: ProfileUpdate) {
        if let Some(name) = update.display_name {
            self.value.display_name = name;
            self.name_revision = self.name_revision.wrapping_add(1);
        }
        if let Some(url) = update.avatar_url {
            self.value.avatar_url = url;
            self.avatar_revision = self.avatar_revision.wrapping_add(1);
        }
    }
}

pub(crate) struct ProfileRequest {
    updates: watch::Receiver<ProfileState>,
    name_revision: u64,
    avatar_revision: u64,
}

impl ProfileRequest {
    fn new(mut updates: watch::Receiver<ProfileState>) -> Result<Self, CmError> {
        let state = updates.borrow_and_update().clone();
        let ProfileState::Ready(snapshot) = state else {
            return Err(CmError::Disconnected);
        };
        Ok(Self {
            updates,
            name_revision: snapshot.name_revision,
            avatar_revision: snapshot.avatar_revision,
        })
    }

    /// Waits without a deadline: `Client::profile` applies `PROFILE_TIMEOUT` to
    /// the whole request.
    pub(crate) async fn receive(mut self) -> Result<AccountProfile, CmError> {
        loop {
            self.updates
                .changed()
                .await
                .map_err(|_| CmError::Disconnected)?;
            match self.updates.borrow_and_update().clone() {
                ProfileState::Ready(snapshot)
                    if snapshot.name_revision != self.name_revision
                        && snapshot.avatar_revision != self.avatar_revision =>
                {
                    return Ok(snapshot.value);
                }
                ProfileState::Disconnected => return Err(CmError::Disconnected),
                _ => {}
            }
        }
    }
}

pub struct SteamConnection {
    outgoing: mpsc::Sender<Command>,
    /// The last license list Steam pushed, as received.
    licenses: watch::Receiver<Option<Bytes>>,
    profile: watch::Receiver<ProfileState>,
    friends: friends::FriendsEventSource,
    worker: JoinHandle<()>,
    steam_id: u64,
    session_id: i32,
    cell_id: u32,
    next_job: AtomicU64,
    /// Cleared by the worker when Steam logs the session off.
    logged_on: Arc<AtomicBool>,
}

impl Drop for SteamConnection {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

#[derive(Deserialize)]
struct DirectoryEnvelope {
    response: DirectoryResponse,
}

#[derive(Deserialize)]
struct DirectoryResponse {
    success: bool,
    serverlist: Vec<DirectoryServer>,
}

#[derive(Deserialize)]
struct DirectoryServer {
    endpoint: String,
    #[serde(rename = "type")]
    kind: String,
}

impl SteamConnection {
    pub async fn connect(client: reqwest::Client) -> Result<Self, CmError> {
        let directory = client
            .get("https://api.steampowered.com/ISteamDirectory/GetCMListForConnect/v1/")
            .query(&[("cellid", "0"), ("cmtype", "websockets"), ("maxcount", "8")])
            .timeout(IO_TIMEOUT)
            .send()
            .await?
            .error_for_status()?
            .json::<DirectoryEnvelope>()
            .await?;
        if !directory.response.success {
            return Err(CmError::Protocol(
                "backend.steam.cm.directoryUnavailable".into(),
            ));
        }
        for server in directory
            .response
            .serverlist
            .into_iter()
            .filter(|server| server.kind == "websockets")
            .take(8)
        {
            let Some(url) = cm_url(&server.endpoint) else {
                continue;
            };
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_PACKET))
                .max_frame_size(Some(MAX_PACKET));
            let connected = timeout(
                IO_TIMEOUT,
                tokio_tungstenite::connect_async_with_config(url, Some(config), true),
            )
            .await;
            let Ok(Ok((socket, _))) = connected else {
                continue;
            };
            let connection = Self::start(socket, 0, false);
            connection
                .send(
                    HELLO,
                    &proto::Hello {
                        protocol_version: PROTOCOL,
                    },
                    None,
                )
                .await?;
            return Ok(connection);
        }
        Err(CmError::Disconnected)
    }

    /// Runs the worker on an open socket. Responses reach their requests from here.
    fn start(socket: Socket, steam_id: u64, logged_on: bool) -> Self {
        let (outgoing, commands) = mpsc::channel(32);
        let (licenses_tx, licenses) = watch::channel(None);
        let (profile_tx, profile) = watch::channel(ProfileState::default());
        let friends = friends::FriendsEventSource::new(steam_id);
        let worker_friends = friends.clone();
        let logged_on = Arc::new(AtomicBool::new(logged_on));
        let session = Arc::clone(&logged_on);
        let worker = tokio::spawn(async move {
            // Ending drops every waiting request, which then reads a disconnection.
            let _ = connection_worker(
                socket,
                commands,
                &licenses_tx,
                &profile_tx,
                &worker_friends,
                &session,
            )
            .await;
            session.store(false, Ordering::Release);
            profile_tx.send_replace(ProfileState::Disconnected);
            worker_friends.disconnect(false);
        });
        Self {
            outgoing,
            licenses,
            profile,
            friends,
            worker,
            steam_id,
            session_id: 0,
            cell_id: 0,
            next_job: AtomicU64::new(1),
            logged_on,
        }
    }

    /// Uses a Steam client refresh token obtained by the authentication module.
    /// Secrets are sent only to the TLS CM endpoint and never included in errors.
    pub async fn log_on(
        &mut self,
        account_name: &str,
        steam_id: u64,
        refresh_token: &str,
    ) -> Result<(), CmError> {
        if self.logged_on.load(Ordering::Acquire)
            || account_name.is_empty()
            || steam_id == 0
            || refresh_token.is_empty()
        {
            return Err(CmError::Protocol("backend.steam.cm.invalidSession".into()));
        }
        // Steam replaces a same-account login from the same public IP when its
        // login ID is reused. UI, supervisor and maintenance connections must
        // coexist, including across processes. This value is not a credential.
        let mut random = [0; 4];
        OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| CmError::Protocol("backend.steam.cm.invalidSession".into()))?;
        let login_id = u32::from_le_bytes(random).max(1);
        self.steam_id = steam_id;
        self.outgoing
            .send(Command::Account(steam_id))
            .await
            .map_err(|_| CmError::Disconnected)?;
        let mut replies = self
            .request_route(
                LOGON,
                &proto::Logon {
                    protocol_version: PROTOCOL,
                    deprecated_obfuscated_private_ip: login_id,
                    obfuscated_private_ip: Some(proto::LoginAddress { v4: login_id }),
                    package_version: 1771,
                    language: "english".into(),
                    os_type: (-102_i32) as u32, // EOSType::MacOSUnknown, encoded as uint32.
                    remember_password: true,
                    account_name: account_name.into(),
                    machine_name: "SailOver".into(),
                    supports_rate_limit: true,
                    access_token: refresh_token.into(),
                },
                None,
                |_| Route::Kind(LOGON_RESPONSE),
                LOGON_RESPONSE,
            )
            .await?;
        let packet = replies.next().await?;
        let response = proto::LogonResponse::decode(&packet.body[..])?;
        check_result(response.result, "backend.steam.cm.action.signIn")?;
        let assigned_id = packet
            .header
            .steam_id
            .ok_or_else(|| CmError::Protocol("backend.steam.cm.missingSteamId".into()))?;
        if assigned_id != steam_id {
            return Err(CmError::Protocol(
                "backend.steam.cm.unexpectedSteamId".into(),
            ));
        }
        self.session_id = packet
            .header
            .session_id
            .ok_or_else(|| CmError::Protocol("backend.steam.cm.missingSessionId".into()))?;
        self.cell_id = response.cell_id;
        self.logged_on.store(true, Ordering::Release);
        let seconds = if response.out_of_game_heartbeat > 0 {
            response.out_of_game_heartbeat
        } else {
            response.heartbeat
        };
        self.outgoing
            .send(Command::Session {
                steam_id,
                session_id: self.session_id,
                heartbeat_seconds: if seconds > 0 { seconds as u64 } else { 15 },
            })
            .await
            .map_err(|_| CmError::Disconnected)?;
        Ok(())
    }

    pub(crate) fn friends_source(&self) -> friends::FriendsEventSource {
        self.friends.clone()
    }

    pub(crate) fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.worker.abort_handle()
    }

    /// Requests only the connected account's profile. Persona updates have their
    /// own channel so this optional query never drains content or license replies.
    pub(crate) async fn request_profile(&self) -> Result<ProfileRequest, CmError> {
        self.ensure_logged_on()?;
        let request = ProfileRequest::new(self.profile.clone())?;
        self.send(
            FRIEND_DATA_REQUEST,
            &proto::FriendDataRequest {
                persona_state_requested: PERSONA_PROFILE,
                friends: vec![self.steam_id],
            },
            None,
        )
        .await?;
        Ok(request)
    }

    /// Returns the user's own visible game library, including played free games.
    pub async fn owned_games(&self) -> Result<Vec<OwnedGameInfo>, CmError> {
        let response: proto::OwnedGamesResponse = self
            .service(
                "Player.GetOwnedGames#1",
                &proto::OwnedGamesRequest {
                    steam_id: self.steam_id,
                    include_app_info: true,
                    include_played_free_games: true,
                    include_free_sub: true,
                    skip_unvetted_apps: Some(false),
                    language: "english".into(),
                },
            )
            .await?;
        if response.game_count > 0 && response.games.is_empty() {
            return Err(CmError::Protocol(
                "backend.steam.cm.incompleteGameList".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        let mut games: Vec<_> = response
            .games
            .into_iter()
            .filter(|game| game.app_id > 0 && seen.insert(game.app_id))
            .map(|game| OwnedGameInfo {
                app_id: game.app_id as u32,
                name: game.name,
                icon_hash: game.icon_hash,
                last_played: u64::from(game.last_played),
                playtime_minutes: game.playtime_forever.max(0) as u32,
            })
            .collect();
        games.sort_by(|left, right| {
            right
                .last_played
                .cmp(&left.last_played)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.app_id.cmp(&right.app_id))
        });
        Ok(games)
    }

    /// Resolves licensed packages to app IDs (including DLC) for download checks.
    /// Steam still authorizes every depot key and manifest request individually.
    pub async fn owned_app_ids(&self) -> Result<Vec<u32>, CmError> {
        Ok(self.content_licenses().await?.apps.into_iter().collect())
    }

    /// Preserve both namespaces when expanding active PICS packages.
    pub async fn content_licenses(&self) -> Result<ContentLicenses, CmError> {
        self.ensure_logged_on()?;
        // Steam pushes the license list after logon, and again when it changes.
        let mut pushed = self.licenses.clone();
        let body = timeout(QUERY_TIMEOUT, pushed.wait_for(Option::is_some))
            .await
            .map_err(|_| CmError::Timeout)?
            .map_err(|_| CmError::Disconnected)?
            .clone()
            .unwrap_or_default();
        let list = proto::LicenseList::decode(&body[..])?;
        check_result(list.result, "backend.steam.cm.action.readLicenses")?;
        // Expired, cancelled, pending, or never activated licenses grant no access.
        const INACTIVE: u32 = 0x04 | 0x08 | 0x10 | 0x20 | 0x400 | 0x800 | 0x40000;
        let packages: Vec<_> = list
            .licenses
            .iter()
            .filter(|license| license.package_id != 0 && license.flags & INACTIVE == 0)
            .map(|license| proto::ProductId {
                id: license.package_id,
                access_token: license.access_token,
            })
            .collect();
        let mut rights = ContentLicenses::default();
        for batch in packages.chunks(PICS_BATCH) {
            let response = self.products(&[], batch).await?;
            if !response.unknown_packages.is_empty() {
                return Err(CmError::Protocol(
                    "backend.steam.cm.packagesUnavailable".into(),
                ));
            }
            for package in response.packages {
                if package.missing_token || package.buffer.is_empty() {
                    return Err(CmError::Protocol(
                        "backend.steam.cm.incompletePackageAccess".into(),
                    ));
                }
                let tree = kv::package(&package.buffer)?;
                let name = package.id.to_string();
                let root = tree.get(&name).unwrap_or(&tree);
                rights.extend_package(root);
            }
        }
        Ok(rights)
    }

    pub async fn app_info(&self, app_id: u32) -> Result<AppInfo, CmError> {
        self.app_infos(&[app_id])
            .await?
            .into_iter()
            .find(|app| app.app_id == app_id)
            .ok_or(CmError::MissingApp(app_id))
    }

    /// Unknown or removed apps are omitted. Malformed metadata fails the request.
    pub async fn app_infos(&self, app_ids: &[u32]) -> Result<Vec<AppInfo>, CmError> {
        self.ensure_logged_on()?;
        let mut apps = Vec::new();
        for batch in app_ids.chunks(PICS_BATCH) {
            let token_packet = self
                .request(
                    TOKEN_REQUEST,
                    &proto::AccessTokenRequest {
                        package_ids: Vec::new(),
                        app_ids: batch.to_vec(),
                    },
                    None,
                    TOKEN_RESPONSE,
                )
                .await?
                .next()
                .await?;
            let tokens = proto::AccessTokenResponse::decode(&token_packet.body[..])?;
            let token_map: HashMap<_, _> = tokens
                .apps
                .into_iter()
                .map(|token| (token.id, token.token))
                .collect();
            let requested: Vec<_> = batch
                .iter()
                .map(|id| proto::ProductId {
                    id: *id,
                    access_token: token_map.get(id).copied().unwrap_or(0),
                })
                .collect();
            let response = self.products(&requested, &[]).await?;
            for info in response.apps {
                if info.missing_token {
                    return Err(CmError::Rejected {
                        action: "backend.steam.cm.action.metadata",
                        result: 15,
                    });
                }
                if !info.buffer.is_empty() {
                    apps.push(parse_app(info.id, &info.buffer)?);
                }
            }
        }
        Ok(apps)
    }

    pub async fn depot_key(&self, depot_id: u32, app_id: u32) -> Result<Vec<u8>, CmError> {
        self.ensure_logged_on()?;
        let packet = self
            .request(
                DEPOT_KEY_REQUEST,
                &proto::DepotKeyRequest { depot_id, app_id },
                None,
                DEPOT_KEY_RESPONSE,
            )
            .await?
            .next()
            .await?;
        let response = proto::DepotKeyResponse::decode(&packet.body[..])?;
        check_result(response.result, "backend.steam.cm.action.depot")?;
        if response.depot_id != depot_id || response.key.len() != 32 {
            return Err(CmError::Protocol("backend.steam.cm.invalidDepotKey".into()));
        }
        Ok(response.key)
    }

    /// Public branch only. Password protected beta branches are not requested.
    pub async fn manifest_request_code(
        &self,
        depot_id: u32,
        app_id: u32,
        manifest_id: u64,
    ) -> Result<u64, CmError> {
        let response: proto::ManifestCodeResponse = self
            .service(
                "ContentServerDirectory.GetManifestRequestCode#1",
                &proto::ManifestCodeRequest {
                    app_id,
                    depot_id,
                    manifest_id,
                },
            )
            .await?;
        if response.code == 0 {
            return Err(CmError::Rejected {
                action: "backend.steam.cm.action.manifest",
                result: 15,
            });
        }
        Ok(response.code)
    }

    pub async fn content_servers(&self) -> Result<Vec<ContentServer>, CmError> {
        let mut response: proto::ServerResponse = self
            .service(
                "ContentServerDirectory.GetServersForSteamPipe#1",
                &proto::ServerRequest {
                    cell_id: self.cell_id,
                    max_servers: 30,
                },
            )
            .await?;
        response
            .servers
            .sort_by(|left, right| left.weighted_load.total_cmp(&right.weighted_load));
        let servers: Vec<_> = response
            .servers
            .into_iter()
            .filter(|server| {
                !server.use_as_proxy
                    && matches!(server.server_type.as_str(), "CDN" | "SteamCache")
                    && matches!(server.https_support.as_str(), "mandatory" | "optional")
                    && valid_host(&server.host)
                    && (server.vhost.is_empty() || valid_host(&server.vhost))
            })
            .map(|server| ContentServer {
                vhost: if server.vhost.is_empty() {
                    server.host.clone()
                } else {
                    server.vhost
                },
                host: server.host,
                allowed_app_ids: server.allowed_app_ids,
            })
            .collect();
        if servers.is_empty() {
            Err(CmError::NoContentServer)
        } else {
            Ok(servers)
        }
    }

    pub async fn cdn_auth(
        &self,
        app_id: u32,
        depot_id: u32,
        host: &str,
    ) -> Result<String, CmError> {
        if !valid_host(host) {
            return Err(CmError::Protocol("backend.steam.cm.invalidCdnHost".into()));
        }
        let response: proto::CdnAuthResponse = self
            .service(
                "ContentServerDirectory.GetCDNAuthToken#1",
                &proto::CdnAuthRequest {
                    app_id,
                    depot_id,
                    host: host.into(),
                },
            )
            .await?;
        Ok(response.token)
    }

    async fn products(
        &self,
        apps: &[proto::ProductId],
        packages: &[proto::ProductId],
    ) -> Result<proto::ProductResponse, CmError> {
        let mut replies = self
            .request(
                PRODUCT_REQUEST,
                &proto::ProductRequest {
                    apps: apps.to_vec(),
                    packages: packages.to_vec(),
                    metadata_only: false,
                },
                None,
                PRODUCT_RESPONSE,
            )
            .await?;
        let mut combined = proto::ProductResponse::default();
        let mut total_bytes = 0_usize;
        for _ in 0..512 {
            let packet = replies.next().await?;
            total_bytes += packet.body.len();
            if total_bytes > MAX_PACKET {
                return Err(CmError::Protocol(
                    "backend.steam.cm.picsBatchTooLarge".into(),
                ));
            }
            let response = proto::ProductResponse::decode(packet.body)?;
            combined.apps.extend(response.apps);
            combined.packages.extend(response.packages);
            combined.unknown_apps.extend(response.unknown_apps);
            combined.unknown_packages.extend(response.unknown_packages);
            if !response.response_pending {
                return Ok(combined);
            }
        }
        Err(CmError::Protocol(
            "backend.steam.cm.tooManyPicsFragments".into(),
        ))
    }

    async fn service<R: Message + Default>(
        &self,
        name: &str,
        request: &impl Message,
    ) -> Result<R, CmError> {
        self.ensure_logged_on()?;
        let packet = self
            .request(SERVICE_REQUEST, request, Some(name), SERVICE_RESPONSE)
            .await?
            .next()
            .await?;
        check_result(
            packet.header.result,
            "backend.steam.cm.action.contentRequest",
        )?;
        R::decode(&packet.body[..]).map_err(Into::into)
    }

    fn ensure_logged_on(&self) -> Result<(), CmError> {
        if self.logged_on.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(CmError::Disconnected)
        }
    }

    fn job(&self) -> Result<u64, CmError> {
        let job = self.next_job.fetch_add(1, Ordering::Relaxed);
        if job == u64::MAX {
            return Err(CmError::Protocol(
                "backend.steam.cm.requestIdsExhausted".into(),
            ));
        }
        Ok(job)
    }

    fn message(&self, kind: u32, message: &impl Message, name: Option<&str>, job: u64) -> Vec<u8> {
        let header = proto::Header {
            steam_id: Some(self.steam_id),
            session_id: Some(self.session_id),
            job_source: Some(job),
            target_name: name.map(str::to_owned),
            ..Default::default()
        };
        encode(kind, &header, message)
    }

    /// Sends a message nobody answers by job.
    async fn send(
        &self,
        kind: u32,
        message: &impl Message,
        name: Option<&str>,
    ) -> Result<u64, CmError> {
        let job = self.job()?;
        self.outgoing
            .send(Command::Send(self.message(kind, message, name, job)))
            .await
            .map_err(|_| CmError::Disconnected)?;
        Ok(job)
    }

    /// Sends a request; its `reply` responses reach only the returned replies.
    async fn request(
        &self,
        kind: u32,
        message: &impl Message,
        name: Option<&str>,
        reply: u32,
    ) -> Result<Replies, CmError> {
        self.request_route(kind, message, name, Route::Job, reply)
            .await
    }

    async fn request_route(
        &self,
        kind: u32,
        message: &impl Message,
        name: Option<&str>,
        route: impl FnOnce(u64) -> Route,
        reply: u32,
    ) -> Result<Replies, CmError> {
        let job = self.job()?;
        let (replies, packets) = mpsc::unbounded_channel();
        self.outgoing
            .send(Command::Request {
                bytes: self.message(kind, message, name, job),
                route: route(job),
                replies,
            })
            .await
            .map_err(|_| CmError::Disconnected)?;
        Ok(Replies {
            packets,
            kind: reply,
        })
    }
}

fn check_result(result: Option<i32>, action: &'static str) -> Result<(), CmError> {
    let result = result.unwrap_or(2);
    if result == 1 {
        Ok(())
    } else {
        Err(CmError::Rejected { action, result })
    }
}

fn cm_url(endpoint: &str) -> Option<String> {
    let url = reqwest::Url::parse(&format!("wss://{endpoint}/cmsocket/")).ok()?;
    if url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/cmsocket/"
        && url
            .host_str()
            .is_some_and(|host| host.ends_with(".steamserver.net"))
    {
        Some(url.into())
    } else {
        None
    }
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'.' || ch == b'-')
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// One bounded frame write; a stalled socket times out and a closed one disconnects.
async fn send_message(socket: &mut Socket, message: WsMessage) -> Result<(), CmError> {
    timeout(IO_TIMEOUT, socket.send(message))
        .await
        .map_err(|_| CmError::Timeout)?
        .map_err(|_| CmError::Disconnected)
}

async fn connection_worker(
    mut socket: Socket,
    mut commands: mpsc::Receiver<Command>,
    licenses: &watch::Sender<Option<Bytes>>,
    profile: &watch::Sender<ProfileState>,
    friends: &friends::FriendsEventSource,
    logged_on: &AtomicBool,
) -> Result<(), CmError> {
    let mut routes = Routes::default();
    let mut heartbeat_header = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut hydration = friends::Hydration::default();
    let mut last_received = Instant::now();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Send(bytes)) => {
                    send_message(&mut socket, WsMessage::Binary(bytes.into())).await?;
                }
                Some(Command::Request { bytes, route, replies }) => {
                    // Waiting before sending, so no response can precede its route.
                    routes.wait(route, replies);
                    send_message(&mut socket, WsMessage::Binary(bytes.into())).await?;
                }
                Some(Command::Account(steam_id)) => friends.set_account(steam_id),
                Some(Command::Session { steam_id, session_id, heartbeat_seconds }) => {
                    heartbeat_header = Some(proto::Header { steam_id: Some(steam_id), session_id: Some(session_id), ..Default::default() });
                    heartbeat = tokio::time::interval(Duration::from_secs(heartbeat_seconds.clamp(1, 60)));
                    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    hydration.started();
                    last_received = Instant::now();
                }
                None => return Ok(()),
            },
            _ = heartbeat.tick() => {
                if let Some(header) = &heartbeat_header {
                    send_message(&mut socket, WsMessage::Binary(encode(HEARTBEAT, header, &proto::Heartbeat { send_reply: true }).into())).await?;
                }
            },
            _ = async { tokio::time::sleep_until(hydration.deadline().expect("guarded")).await }, if hydration.deadline().is_some() => {
                hydration.timed_out(friends);
            },
            _ = tokio::time::sleep_until(last_received + Duration::from_secs(90)), if heartbeat_header.is_some() => return Err(CmError::Disconnected),
            _ = std::future::ready(()), if heartbeat_header.is_some() && hydration.push_due(friends) => {
                // Read-only push request after a full list or newly accepted
                // friend. Coalesce relationship bursts without changing the
                // account's own presence or periodically refreshing statuses.
                let mut header = heartbeat_header.clone().expect("authenticated hydration");
                header.job_source = Some(FRIENDS_HYDRATION_JOB);
                header.target_name = Some("Chat.RequestFriendPersonaStates#1".into());
                send_message(&mut socket, WsMessage::Binary(encode(SERVICE_REQUEST, &header, &proto::RequestFriendPersonaStates {}).into())).await?;
                hydration.push_sent();
            },
            _ = std::future::ready(()), if heartbeat_header.is_some() && hydration.has_batch() => {
                let batch = hydration.next_batch(friends);
                if !batch.is_empty() {
                    let request = proto::FriendDataRequest { persona_state_requested: friends::REQUESTED, friends: batch };
                    let header = heartbeat_header.as_ref().expect("authenticated hydration");
                    send_message(&mut socket, WsMessage::Binary(encode(FRIEND_DATA_REQUEST, header, &request).into())).await?;
                }
            },
            frame = socket.next() => match frame {
                Some(Ok(WsMessage::Binary(bytes))) => {
                    last_received = Instant::now();
                    let mut packets = Vec::new();
                    let mut budget = MAX_PACKET;
                    decode_packets(bytes, 0, &mut budget, &mut packets)?;
                    for packet in packets {
                        if packet.kind == SERVICE_RESPONSE && packet.header.job_target == Some(FRIENDS_HYDRATION_JOB) {
                            hydration.push_answered();
                            // This optional reply never reaches a library or
                            // download request.
                            continue;
                        }
                        hydration.apply(friends, packet.kind, &packet.body);
                        if packet.kind == PERSONA_STATE
                            && let Some(steam_id) = heartbeat_header.as_ref().and_then(|header| header.steam_id)
                            && let Some(update) = persona_profile(&packet.body, steam_id)
                        {
                            profile.send_modify(|state| {
                                if let ProfileState::Ready(snapshot) = state {
                                    snapshot.apply(update);
                                }
                            });
                        }
                        if packet.kind == LOGGED_OFF {
                            logged_on.store(false, Ordering::Release);
                            profile.send_replace(ProfileState::Disconnected);
                            let authentication_required = proto::LoggedOff::decode(&packet.body[..]).ok().and_then(|logged_off| logged_off.result).is_some_and(friends::authentication_required);
                            friends.disconnect(authentication_required);
                            // Close this physical connection before a replacement
                            // starts; waiting requests read the disconnection.
                            return Err(CmError::Disconnected);
                        }
                        if packet.kind == LICENSE_LIST {
                            licenses.send_replace(Some(packet.body.clone()));
                        }
                        routes.deliver(packet);
                    }
                    friends.publish();
                }
                Some(Ok(WsMessage::Ping(data))) => {
                    last_received = Instant::now();
                    send_message(&mut socket, WsMessage::Pong(data)).await?;
                }
                Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => return Err(CmError::Disconnected),
                _ => {},
            }
        }
    }
}

fn persona_profile(body: &[u8], steam_id: u64) -> Option<ProfileUpdate> {
    // Malformed or unrelated optional persona updates must not fail downloads.
    let state = proto::PersonaState::decode(body).ok()?;
    if state.status_flags & PERSONA_PROFILE == 0 {
        return None;
    }
    let persona = state
        .friends
        .iter()
        .find(|friend| friend.friend_id == steam_id)?;
    Some(ProfileUpdate {
        display_name: (state.status_flags & PERSONA_PLAYER_NAME != 0).then(|| {
            let name = persona.player_name.as_deref().unwrap_or_default().trim();
            (!name.is_empty()).then(|| name.to_owned())
        }),
        avatar_url: (state.status_flags & PERSONA_PRESENCE != 0)
            .then(|| avatar_url_from_hash(persona.avatar_hash.as_deref().unwrap_or_default())),
    })
}

pub(crate) fn avatar_url_from_hash(hash: &[u8]) -> Option<String> {
    if hash.len() != 20 || hash.iter().all(|byte| *byte == 0) {
        return None;
    }
    let hash = crate::content::hex(hash);
    Some(format!("https://avatars.steamstatic.com/{hash}_full.jpg"))
}

fn encode(kind: u32, header: &proto::Header, message: &impl Message) -> Vec<u8> {
    encode_body(kind, header, &message.encode_to_vec())
}

fn encode_body(kind: u32, header: &proto::Header, body: &[u8]) -> Vec<u8> {
    let header = header.encode_to_vec();
    let mut bytes = Vec::with_capacity(8 + header.len() + body.len());
    bytes.extend_from_slice(&(kind | PROTO_MASK).to_le_bytes());
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(body);
    bytes
}

fn decode_packets(
    bytes: Bytes,
    depth: usize,
    budget: &mut usize,
    output: &mut Vec<Packet>,
) -> Result<(), CmError> {
    if depth > 8 || bytes.len() > *budget || output.len() >= 2048 {
        return Err(CmError::Protocol("backend.steam.cm.invalidEnvelope".into()));
    }
    let Some((wire_kind, rest)) = bytes.split_first_chunk::<4>() else {
        return Err(CmError::Protocol("backend.steam.cm.missingType".into()));
    };
    let Some((header_size, _)) = rest.split_first_chunk::<4>() else {
        return Err(CmError::Protocol("backend.steam.cm.missingHeader".into()));
    };
    *budget -= bytes.len();
    let wire_kind = u32::from_le_bytes(*wire_kind);
    if wire_kind & PROTO_MASK == 0 {
        return decode_legacy_packet(wire_kind, &bytes, output);
    }
    let header_size = u32::from_le_bytes(*header_size) as usize;
    let Some(header_end) = header_size.checked_add(8).filter(|end| *end <= bytes.len()) else {
        return Err(CmError::Protocol("backend.steam.cm.truncatedHeader".into()));
    };
    let header = proto::Header::decode(&bytes[8..header_end])?;
    let kind = wire_kind & !PROTO_MASK;
    let body = bytes.slice(header_end..);
    if kind != MULTI {
        output.push(Packet { kind, header, body });
        return Ok(());
    }
    let multi = proto::Multi::decode(body)?;
    let body = if multi.size_unzipped == 0 {
        multi.body
    } else {
        let size = multi.size_unzipped as usize;
        if size > *budget {
            return Err(CmError::Protocol("backend.steam.cm.gzipTooLarge".into()));
        }
        let mut decompressed = Vec::with_capacity(size);
        flate2::read::GzDecoder::new(&multi.body[..])
            .take(size as u64 + 1)
            .read_to_end(&mut decompressed)
            .map_err(|_| CmError::Protocol("backend.steam.cm.unreadableGzip".into()))?;
        if decompressed.len() != size {
            return Err(CmError::Protocol("backend.steam.cm.invalidGzipSize".into()));
        }
        Bytes::from(decompressed)
    };
    let mut offset = 0;
    while offset < body.len() {
        let Some((length, _)) = body[offset..].split_first_chunk::<4>() else {
            return Err(CmError::Protocol(
                "backend.steam.cm.truncatedSubmessage".into(),
            ));
        };
        let start = offset + 4;
        let Some(end) = start
            .checked_add(u32::from_le_bytes(*length) as usize)
            .filter(|end| *end <= body.len())
        else {
            return Err(CmError::Protocol(
                "backend.steam.cm.truncatedSubmessage".into(),
            ));
        };
        decode_packets(body.slice(start..end), depth + 1, budget, output)?;
        offset = end;
    }
    Ok(())
}

/// Account sessions include legacy notifications such as ClientVACBanStatus.
/// They are unrelated to content queries and may precede a protobuf response
/// inside the same Multi. Validate the header and leave their bodies untouched.
fn decode_legacy_packet(kind: u32, bytes: &[u8], output: &mut Vec<Packet>) -> Result<(), CmError> {
    // The three channel-encryption messages use MsgHdr (20 bytes), not the
    // extended client header. TLS WebSocket connections do not use their bodies.
    if matches!(kind, 1303..=1305) {
        return if bytes.len() >= 20 {
            Ok(())
        } else {
            Err(CmError::Protocol(crate::message::format(
                "backend.steam.cm.truncatedLegacyHeader",
                &[kind.to_string()],
            )))
        };
    }
    // ExtendedClientMsgHdr layout: EMsg4, HeaderSize1, HeaderVersion2,
    // TargetJobID8, SourceJobID8, Canary1, SteamID8, SessionID4.
    if bytes.len() < 36 || bytes[4] != 36 || bytes[5..7] != 2_u16.to_le_bytes() || bytes[23] != 239
    {
        return Err(CmError::Protocol(crate::message::format(
            "backend.steam.cm.invalidLegacyHeader",
            &[kind.to_string()],
        )));
    }
    if kind == LOGON_RESPONSE {
        let result = bytes
            .get(36..40)
            .and_then(|bytes| bytes.try_into().ok())
            .map(i32::from_le_bytes)
            .ok_or_else(|| CmError::Protocol("backend.steam.cm.truncatedLegacyLogon".into()))?;
        // Steam sends this legacy form when rejecting an incomplete logon.
        check_result(Some(result), "backend.steam.cm.action.signIn")?;
        return Err(CmError::Protocol(
            "backend.steam.cm.unsupportedLegacyLogon".into(),
        ));
    }
    if kind == LOGGED_OFF {
        let result = bytes
            .get(36..40)
            .and_then(|body| body.try_into().ok())
            .map(i32::from_le_bytes);
        output.push(Packet {
            kind,
            header: proto::Header::default(),
            body: proto::LoggedOff { result }.encode_to_vec().into(),
        });
        return Ok(());
    }
    if matches!(
        kind,
        MULTI
            | SERVICE_RESPONSE
            | LICENSE_LIST
            | DEPOT_KEY_RESPONSE
            | PRODUCT_RESPONSE
            | TOKEN_RESPONSE
    ) {
        return Err(CmError::Protocol(crate::message::format(
            "backend.steam.cm.expectedProtobuf",
            &[kind.to_string()],
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const TEST_STEAM_ID: u64 = 76561198000000000;

    fn persona_body(steam_id: u64, flags: u32, hash: &[u8]) -> Vec<u8> {
        proto::PersonaState {
            status_flags: flags,
            friends: vec![proto::Persona {
                friend_id: steam_id,
                player_name: Some("Zagrios".into()),
                avatar_hash: Some(hash.to_vec()),
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[test]
    fn profile_wire_includes_public_name_and_avatar() {
        let request = proto::FriendDataRequest {
            persona_state_requested: PERSONA_PROFILE,
            friends: vec![TEST_STEAM_ID],
        };
        let mut expected = vec![0x08, 0x12, 0x11];
        expected.extend_from_slice(&TEST_STEAM_ID.to_le_bytes());
        assert_eq!(request.encode_to_vec(), expected);

        // Independent fixture: friendid tag 1, player_name tag 15, avatar_hash tag 31.
        let mut friend = vec![0x09];
        friend.extend_from_slice(&TEST_STEAM_ID.to_le_bytes());
        friend.extend_from_slice(&[0x7a, 7]);
        friend.extend_from_slice(b"Zagrios");
        friend.extend_from_slice(&[0xfa, 0x01, 20]);
        friend.extend_from_slice(&[0xab; 20]);
        let mut response = vec![0x08, 0x12, 0x12, friend.len() as u8];
        response.extend_from_slice(&friend);
        assert_eq!(
            persona_profile(&response, TEST_STEAM_ID),
            Some(ProfileUpdate {
                display_name: Some(Some("Zagrios".into())),
                avatar_url: Some(Some(format!(
                    "https://avatars.steamstatic.com/{}_full.jpg",
                    "ab".repeat(20)
                ))),
            })
        );
    }

    #[test]
    fn profile_ignores_unrelated_updates_and_handles_missing_avatar() {
        assert_eq!(
            persona_profile(
                &persona_body(TEST_STEAM_ID + 1, PERSONA_PRESENCE, &[0xab; 20]),
                TEST_STEAM_ID
            ),
            None
        );
        assert_eq!(
            persona_profile(&persona_body(TEST_STEAM_ID, 4, &[0xab; 20]), TEST_STEAM_ID),
            None
        );
        assert_eq!(persona_profile(&[0xff], TEST_STEAM_ID), None);
        for hash in [vec![], vec![0; 20], vec![1; 19], vec![1; 21]] {
            assert_eq!(
                persona_profile(
                    &persona_body(TEST_STEAM_ID, PERSONA_PRESENCE, &hash),
                    TEST_STEAM_ID
                ),
                Some(ProfileUpdate {
                    display_name: None,
                    avatar_url: Some(None)
                })
            );
        }
    }

    fn depot_reply(depot_id: u32) -> proto::DepotKeyResponse {
        proto::DepotKeyResponse {
            result: Some(1),
            depot_id,
            key: vec![0; 32],
        }
    }

    #[tokio::test]
    async fn requests_in_flight_each_receive_only_their_own_responses() {
        let (connection, mut server) = test_server::connection(TEST_STEAM_ID, true).await;
        let depot = |depot_id| proto::DepotKeyRequest {
            depot_id,
            app_id: 620,
        };
        let abandoned = connection
            .request(DEPOT_KEY_REQUEST, &depot(1), None, DEPOT_KEY_RESPONSE)
            .await
            .unwrap();
        let mut waiting = connection
            .request(DEPOT_KEY_REQUEST, &depot(2), None, DEPOT_KEY_RESPONSE)
            .await
            .unwrap();
        let first = server.request(DEPOT_KEY_REQUEST).await;
        let second = server.request(DEPOT_KEY_REQUEST).await;
        // The first request gave up; its late reply must not reach the second.
        drop(abandoned);
        server
            .reply(&first, DEPOT_KEY_RESPONSE, &depot_reply(1))
            .await;
        server
            .reply(&second, DEPOT_KEY_RESPONSE, &depot_reply(2))
            .await;
        let reply = waiting.next().await.unwrap();
        assert_eq!(
            proto::DepotKeyResponse::decode(&reply.body[..])
                .unwrap()
                .depot_id,
            2
        );
        // Both finished requests left the worker nothing to route.
        assert!(server.quiet().await);
    }

    #[tokio::test]
    async fn a_logoff_ends_every_waiting_request() {
        let (connection, mut server) = test_server::connection(TEST_STEAM_ID, true).await;
        let mut waiting = connection
            .request(
                DEPOT_KEY_REQUEST,
                &proto::DepotKeyRequest {
                    depot_id: 1,
                    app_id: 620,
                },
                None,
                DEPOT_KEY_RESPONSE,
            )
            .await
            .unwrap();
        server.request(DEPOT_KEY_REQUEST).await;
        server
            .send(
                LOGGED_OFF,
                proto::Header::default(),
                &proto::LoggedOff { result: Some(6) }.encode_to_vec(),
            )
            .await;
        assert!(matches!(waiting.next().await, Err(CmError::Disconnected)));
        assert!(matches!(
            connection.ensure_logged_on(),
            Err(CmError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn social_worker_keeps_library_download_replies_and_coalesces_persona_floods() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (client, server) = tokio::join!(tokio_tungstenite::connect_async(endpoint), async {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        });
        let mut connection = SteamConnection::start(client.unwrap().0, TEST_STEAM_ID, true);
        connection.session_id = 42;
        connection
            .outgoing
            .send(Command::Session {
                steam_id: TEST_STEAM_ID,
                session_id: 42,
                heartbeat_seconds: 15,
            })
            .await
            .unwrap();
        let request = connection.request_profile().await.unwrap();
        // Waiting requests run concurrently; each reads only its own responses.
        let content_job = connection.next_job.load(Ordering::Relaxed);
        let mut content = connection
            .request(
                SERVICE_REQUEST,
                &proto::Empty {},
                Some("Test.Content#1"),
                SERVICE_RESPONSE,
            )
            .await
            .unwrap();
        let mut depot = connection
            .request(
                DEPOT_KEY_REQUEST,
                &proto::DepotKeyRequest {
                    depot_id: 22,
                    app_id: 620,
                },
                None,
                DEPOT_KEY_RESPONSE,
            )
            .await
            .unwrap();
        let server_task = tokio::spawn(async move {
            let mut server = server;
            let mut saw_avatar = false;
            let mut saw_content = false;
            while !saw_avatar || !saw_content {
                let frame = server.next().await.unwrap().unwrap();
                let WsMessage::Binary(bytes) = frame else {
                    continue;
                };
                let mut packets = Vec::new();
                decode_packets(bytes.clone(), 0, &mut { MAX_PACKET }, &mut packets).unwrap();
                for packet in packets {
                    if packet.kind == FRIEND_DATA_REQUEST {
                        let body = proto::FriendDataRequest::decode(&packet.body[..]).unwrap();
                        assert_eq!(body.friends, vec![TEST_STEAM_ID]);
                        assert_eq!(body.persona_state_requested, PERSONA_PROFILE);
                        assert_eq!(packet.header.steam_id, Some(TEST_STEAM_ID));
                        saw_avatar = true;
                    }
                    if packet.kind == SERVICE_REQUEST
                        && packet.header.job_source != Some(FRIENDS_HYDRATION_JOB)
                    {
                        saw_content = true;
                    }
                }
            }
            let mut body = Vec::new();
            let mut append = |packet: Vec<u8>| {
                body.extend_from_slice(&(packet.len() as u32).to_le_bytes());
                body.extend_from_slice(&packet);
            };
            // Independent full-friends wire fixture: message767, zero-length
            // header, incremental=false, fixed64 ID field1, relationship3 field2.
            let mut relationship = vec![
                0xff, 0x02, 0x00, 0x80, 0, 0, 0, 0, 0x08, 0, 0x12, 0x0b, 0x09,
            ];
            relationship.extend_from_slice(&(TEST_STEAM_ID + 1).to_le_bytes());
            relationship.extend_from_slice(&[0x10, 3]);
            append(relationship);
            append(encode(
                SERVICE_RESPONSE,
                &proto::Header {
                    job_target: Some(FRIENDS_HYDRATION_JOB),
                    result: Some(1),
                    ..Default::default()
                },
                &proto::Empty {},
            ));
            // Optional malformed persona body with a valid transport envelope.
            append(vec![0xfe, 0x02, 0x00, 0x80, 0, 0, 0, 0, 0xff]);
            // More than the content channel's capacity must still be harmless.
            for index in 0..140 {
                append(encode(
                    PERSONA_STATE,
                    &proto::Header::default(),
                    &proto::PersonaState {
                        status_flags: friends::REQUESTED | friends::STATUS,
                        friends: vec![proto::Persona {
                            friend_id: TEST_STEAM_ID + 1,
                            persona_state: Some(index % 5),
                            player_name: Some("Other account".into()),
                            avatar_hash: Some(vec![0x12; 20]),
                            ..Default::default()
                        }],
                    },
                ));
            }
            append(encode(
                LICENSE_LIST,
                &proto::Header::default(),
                &proto::LicenseList {
                    result: Some(1),
                    licenses: vec![proto::License {
                        package_id: 99,
                        ..Default::default()
                    }],
                },
            ));
            append(encode(
                SERVICE_RESPONSE,
                &proto::Header {
                    job_target: Some(content_job),
                    result: Some(1),
                    ..Default::default()
                },
                &proto::Empty {},
            ));
            append(encode(
                DEPOT_KEY_RESPONSE,
                &proto::Header {
                    job_target: Some(content_job + 1),
                    ..Default::default()
                },
                &proto::DepotKeyResponse {
                    result: Some(1),
                    depot_id: 22,
                    key: vec![0; 32],
                },
            ));
            append(encode(
                PERSONA_STATE,
                &proto::Header::default(),
                &proto::PersonaState {
                    status_flags: PERSONA_PRESENCE,
                    friends: vec![proto::Persona {
                        friend_id: TEST_STEAM_ID,
                        player_name: Some(String::new()),
                        avatar_hash: Some(vec![0xab; 20]),
                        ..Default::default()
                    }],
                },
            ));
            // Steam can deliver the requested name independently of Presence.
            append(encode(
                PERSONA_STATE,
                &proto::Header::default(),
                &proto::PersonaState {
                    status_flags: PERSONA_PLAYER_NAME,
                    friends: vec![proto::Persona {
                        friend_id: TEST_STEAM_ID,
                        player_name: Some("Zagrios".into()),
                        avatar_hash: Some(Vec::new()),
                        ..Default::default()
                    }],
                },
            ));
            server
                .send(WsMessage::Binary(
                    encode(
                        MULTI,
                        &proto::Header::default(),
                        &proto::Multi {
                            size_unzipped: 0,
                            body: body.into(),
                        },
                    )
                    .into(),
                ))
                .await
                .unwrap();
            server
        });
        let profile = request.receive().await.unwrap();
        assert_eq!(profile.display_name.as_deref(), Some("Zagrios"));
        assert_eq!(
            profile.avatar_url,
            Some(format!(
                "https://avatars.steamstatic.com/{}_full.jpg",
                "ab".repeat(20)
            ))
        );
        // The depot reply arrives first; it still reaches only its own request.
        let depot = depot.next().await.unwrap();
        content.next().await.unwrap();
        let licenses = connection.licenses.borrow().clone().unwrap();
        assert_eq!(
            proto::LicenseList::decode(&licenses[..]).unwrap().licenses[0].package_id,
            99
        );
        assert_eq!(
            proto::DepotKeyResponse::decode(&depot.body[..])
                .unwrap()
                .depot_id,
            22
        );
        let social = connection.friends.snapshot();
        assert_eq!(social.connection, friends::FriendsConnection::Ready);
        assert_eq!(social.friends.len(), 1);
        assert_eq!(social.friends[0].presence, friends::FriendPresence::Snooze);
        assert_eq!(
            social.friends[0].display_name.as_deref(),
            Some("Other account")
        );
        // One publication for the entire Multi, not one snapshot per delta.
        assert_eq!(social.revision, 2);
        let _server = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn social_worker_hydrates_offline_friends_in_bounded_batches_and_observes_logoff() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (client, mut server) =
            tokio::join!(tokio_tungstenite::connect_async(endpoint), async {
                let (stream, _) = listener.accept().await.unwrap();
                tokio_tungstenite::accept_async(stream).await.unwrap()
            });
        let (outgoing, commands) = mpsc::channel(32);
        let (licenses, _licenses) = watch::channel(None);
        let (profile, _profile_updates) = watch::channel(ProfileState::default());
        let social = friends::FriendsEventSource::new(TEST_STEAM_ID);
        let writer = social.clone();
        let worker = tokio::spawn(async move {
            let result = connection_worker(
                client.unwrap().0,
                commands,
                &licenses,
                &profile,
                &writer,
                &AtomicBool::new(true),
            )
            .await;
            writer.disconnect(false);
            result
        });
        outgoing
            .send(Command::Session {
                steam_id: TEST_STEAM_ID,
                session_id: 42,
                heartbeat_seconds: 15,
            })
            .await
            .unwrap();
        let ids: Vec<_> = (1..=205).map(|offset| TEST_STEAM_ID + offset).collect();
        server
            .send(WsMessage::Binary(
                encode(
                    friends::FRIENDS_LIST,
                    &proto::Header::default(),
                    &proto::FriendsList {
                        incremental: false,
                        friends: ids
                            .iter()
                            .map(|steam_id| proto::FriendRelationship {
                                steam_id: *steam_id,
                                relationship: 3,
                            })
                            .collect(),
                    },
                )
                .into(),
            ))
            .await
            .unwrap();
        let mut received = BTreeSet::new();
        let mut batches = 0;
        let mut snapshot_requests = 0;
        while received.len() < ids.len() || snapshot_requests == 0 {
            let WsMessage::Binary(bytes) = server.next().await.unwrap().unwrap() else {
                continue;
            };
            let mut packets = vec![];
            decode_packets(bytes, 0, &mut { MAX_PACKET }, &mut packets).unwrap();
            for packet in packets {
                if packet.kind == SERVICE_REQUEST
                    && packet.header.job_source == Some(FRIENDS_HYDRATION_JOB)
                {
                    assert_eq!(
                        packet.header.target_name.as_deref(),
                        Some("Chat.RequestFriendPersonaStates#1")
                    );
                    assert!(packet.body.is_empty());
                    assert!(social.has_full_list());
                    snapshot_requests += 1;
                    server
                        .send(WsMessage::Binary(
                            encode(
                                SERVICE_RESPONSE,
                                &proto::Header {
                                    job_target: Some(FRIENDS_HYDRATION_JOB),
                                    result: Some(1),
                                    ..Default::default()
                                },
                                &proto::Empty {},
                            )
                            .into(),
                        ))
                        .await
                        .unwrap();
                    server
                        .send(WsMessage::Binary(
                            encode(
                                PERSONA_STATE,
                                &proto::Header::default(),
                                &proto::PersonaState {
                                    status_flags: friends::STATUS,
                                    friends: ids
                                        .iter()
                                        .map(|id| proto::Persona {
                                            friend_id: *id,
                                            // Offline0 is deliberately omitted, matching Valve's
                                            // Status group defaults instead of a synthetic Some0.
                                            persona_state: (*id == TEST_STEAM_ID + 1).then_some(3),
                                            game_id: (*id == TEST_STEAM_ID + 1).then_some(570),
                                            game_name: (*id == TEST_STEAM_ID + 1)
                                                .then(|| "Simulated game".into()),
                                            ..Default::default()
                                        })
                                        .collect(),
                                },
                            )
                            .into(),
                        ))
                        .await
                        .unwrap();
                    continue;
                }
                if packet.kind != FRIEND_DATA_REQUEST {
                    continue;
                }
                let request = proto::FriendDataRequest::decode(&packet.body[..]).unwrap();
                assert!(request.friends.len() <= 100);
                assert_eq!(request.persona_state_requested, 18);
                batches += 1;
                for id in &request.friends {
                    assert!(received.insert(*id));
                }
                server
                    .send(WsMessage::Binary(
                        encode(
                            PERSONA_STATE,
                            &proto::Header::default(),
                            &proto::PersonaState {
                                status_flags: friends::REQUESTED,
                                friends: request
                                    .friends
                                    .into_iter()
                                    .map(|friend_id| proto::Persona {
                                        friend_id,
                                        player_name: Some(format!("Friend {friend_id}")),
                                        avatar_hash: Some(vec![]),
                                        ..Default::default()
                                    })
                                    .collect(),
                            },
                        )
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
        }
        assert_eq!(batches, 3);
        assert_eq!(snapshot_requests, 1);
        let mut snapshot = social.snapshot();
        while snapshot.connection != friends::FriendsConnection::Ready {
            snapshot = social.next(snapshot.revision).await;
        }
        assert_eq!(snapshot.friends.len(), 205);
        assert_eq!(snapshot.friends[0].presence, friends::FriendPresence::Away);
        assert_eq!(
            snapshot.friends[0]
                .game
                .as_ref()
                .and_then(|game| game.name.as_deref()),
            Some("Simulated game")
        );
        assert_eq!(
            snapshot.friends[1].presence,
            friends::FriendPresence::Offline
        );
        let before = snapshot.friends.clone();
        // Protobuf logoff result26 is revoked credentials, not a network outage.
        server
            .send(WsMessage::Binary(
                encode(
                    LOGGED_OFF,
                    &proto::Header::default(),
                    &proto::LoggedOff { result: Some(26) },
                )
                .into(),
            ))
            .await
            .unwrap();
        assert!(matches!(worker.await.unwrap(), Err(CmError::Disconnected)));
        let stopped = social.next(snapshot.revision).await;
        assert_eq!(
            stopped.connection,
            friends::FriendsConnection::AuthenticationRequired
        );
        assert_eq!(stopped.friends, before);
    }

    #[tokio::test]
    async fn avatar_request_observes_disconnection() {
        let (sender, receiver) = watch::channel(ProfileState::default());
        let request = ProfileRequest::new(receiver).unwrap();
        sender.send_replace(ProfileState::Disconnected);
        assert!(matches!(
            request.receive().await,
            Err(CmError::Disconnected)
        ));
        let (sender, receiver) = watch::channel(ProfileState::default());
        drop(sender);
        assert!(matches!(
            ProfileRequest::new(receiver).unwrap().receive().await,
            Err(CmError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn profile_request_waits_for_fresh_name_and_avatar_in_either_order() {
        for name_first in [true, false] {
            let (sender, receiver) = watch::channel(ProfileState::Ready(ProfileSnapshot {
                value: AccountProfile {
                    display_name: Some("Old public name".into()),
                    avatar_url: Some("old-avatar".into()),
                },
                name_revision: 1,
                avatar_revision: 1,
            }));
            let request = ProfileRequest::new(receiver).unwrap();
            let mut result = Box::pin(request.receive());
            let flags = if name_first {
                [PERSONA_PLAYER_NAME, PERSONA_PRESENCE]
            } else {
                [PERSONA_PRESENCE, PERSONA_PLAYER_NAME]
            };
            for (index, flag) in flags.into_iter().enumerate() {
                let update = persona_profile(
                    &persona_body(TEST_STEAM_ID, flag, &[0xab; 20]),
                    TEST_STEAM_ID,
                )
                .unwrap();
                sender.send_modify(|state| {
                    if let ProfileState::Ready(snapshot) = state {
                        snapshot.apply(update);
                    }
                });
                if index == 0 {
                    assert!(
                        timeout(Duration::from_millis(10), &mut result)
                            .await
                            .is_err()
                    );
                }
            }
            let profile = result.await.unwrap();
            assert_eq!(profile.display_name.as_deref(), Some("Zagrios"));
            assert_eq!(
                profile.avatar_url,
                Some(format!(
                    "https://avatars.steamstatic.com/{}_full.jpg",
                    "ab".repeat(20)
                ))
            );
        }
    }

    #[test]
    fn absent_public_name_does_not_erase_avatar() {
        let update = proto::PersonaState {
            status_flags: PERSONA_PLAYER_NAME,
            friends: vec![proto::Persona {
                friend_id: TEST_STEAM_ID,
                player_name: Some("  ".into()),
                avatar_hash: Some(vec![]),
                ..Default::default()
            }],
        }
        .encode_to_vec();
        let mut snapshot = ProfileSnapshot {
            value: AccountProfile {
                display_name: Some("Old public name".into()),
                avatar_url: Some("existing-avatar".into()),
            },
            ..Default::default()
        };
        snapshot.apply(persona_profile(&update, TEST_STEAM_ID).unwrap());
        assert_eq!(snapshot.value.display_name, None);
        assert_eq!(
            snapshot.value.avatar_url.as_deref(),
            Some("existing-avatar")
        );
    }

    fn legacy_message(kind: u32, payload: &[u8]) -> Vec<u8> {
        // ExtendedClientMsgHdr: msg, size, version, target/source jobs,
        // header canary, steamid and sessionid.
        let mut legacy = kind.to_le_bytes().to_vec();
        legacy.push(36);
        legacy.extend_from_slice(&2_u16.to_le_bytes());
        legacy.extend_from_slice(&u64::MAX.to_le_bytes());
        legacy.extend_from_slice(&u64::MAX.to_le_bytes());
        legacy.push(239);
        legacy.extend_from_slice(&0_u64.to_le_bytes());
        legacy.extend_from_slice(&0_i32.to_le_bytes());
        legacy.extend_from_slice(payload);
        legacy
    }

    #[test]
    fn unrelated_legacy_message_does_not_drop_protobuf_logon_in_multi() {
        let legacy = legacy_message(782, &0_u32.to_le_bytes()); // No VAC ban entries.
        let logon = encode(
            LOGON_RESPONSE,
            &proto::Header {
                session_id: Some(42),
                ..Default::default()
            },
            &proto::LogonResponse {
                result: Some(1),
                ..Default::default()
            },
        );
        let mut body = Vec::new();
        for packet in [&legacy, &logon] {
            body.extend_from_slice(&(packet.len() as u32).to_le_bytes());
            body.extend_from_slice(packet);
        }
        let multi = encode(
            MULTI,
            &proto::Header::default(),
            &proto::Multi {
                size_unzipped: 0,
                body: body.into(),
            },
        );
        let mut packets = Vec::new();
        decode_packets(
            Bytes::from(multi.clone()),
            0,
            &mut { MAX_PACKET },
            &mut packets,
        )
        .unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].kind, LOGON_RESPONSE);
        assert_eq!(packets[0].header.session_id, Some(42));
        assert_eq!(
            proto::LogonResponse::decode(&packets[0].body[..])
                .unwrap()
                .result,
            Some(1)
        );
    }

    #[test]
    fn legacy_validation_and_login_failures_are_not_silenced() {
        let legacy = legacy_message(782, &0_u32.to_le_bytes());
        assert!(
            decode_packets(
                Bytes::copy_from_slice(&legacy[..35]),
                0,
                &mut { MAX_PACKET },
                &mut Vec::new()
            )
            .is_err()
        );
        let mut wrong_canary = legacy;
        wrong_canary[23] = 0;
        assert!(
            decode_packets(
                Bytes::from(wrong_canary.clone()),
                0,
                &mut { MAX_PACKET },
                &mut Vec::new()
            )
            .is_err()
        );
        let rejected = legacy_message(LOGON_RESPONSE, &5_i32.to_le_bytes());
        assert!(matches!(
            decode_packets(
                Bytes::from(rejected.clone()),
                0,
                &mut { MAX_PACKET },
                &mut Vec::new()
            ),
            Err(CmError::Rejected { result: 5, .. })
        ));
        let logoff = legacy_message(LOGGED_OFF, &1_i32.to_le_bytes());
        let mut packets = Vec::new();
        decode_packets(
            Bytes::from(logoff.clone()),
            0,
            &mut { MAX_PACKET },
            &mut packets,
        )
        .unwrap();
        assert_eq!(packets[0].kind, LOGGED_OFF);
        let bad_multi = legacy_message(MULTI, &[]);
        assert!(
            decode_packets(
                Bytes::from(bad_multi.clone()),
                0,
                &mut { MAX_PACKET },
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[tokio::test]
    #[ignore = "contacts Steam anonymously; verifies one public file up to 8 MiB in memory"]
    async fn live_anonymous_cm_public_content_metadata() {
        let mut connection = SteamConnection::connect(crate::client::http_client().unwrap())
            .await
            .unwrap();
        connection.steam_id = (1_u64 << 56) | (10_u64 << 52);
        let packet = connection
            .request_route(
                LOGON,
                &proto::Logon {
                    protocol_version: PROTOCOL,
                    language: "english".into(),
                    os_type: (-102_i32) as u32,
                    ..Default::default()
                },
                None,
                |_| Route::Kind(LOGON_RESPONSE),
                LOGON_RESPONSE,
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap();
        let response = proto::LogonResponse::decode(&packet.body[..]).unwrap();
        check_result(response.result, "the anonymous test sign-in").unwrap();
        connection.steam_id = packet.header.steam_id.unwrap();
        connection.session_id = packet.header.session_id.unwrap();
        connection.cell_id = response.cell_id;
        connection.logged_on.store(true, Ordering::Release);
        connection
            .outgoing
            .send(Command::Session {
                steam_id: connection.steam_id,
                session_id: connection.session_id,
                heartbeat_seconds: response.out_of_game_heartbeat.max(1) as u64,
            })
            .await
            .unwrap();

        let app = connection.app_info(1007).await.unwrap();
        assert_eq!(app.app_id, 1007);
        assert!(!app.name.is_empty());
        assert!(!app.depots.is_empty());
        let packages = connection
            .products(
                &[],
                &[proto::ProductId {
                    id: 17906,
                    access_token: 0,
                }],
            )
            .await
            .unwrap();
        let package = packages
            .packages
            .iter()
            .find(|package| package.id == 17906)
            .unwrap();
        let package_tree = kv::package(&package.buffer).unwrap();
        assert!(
            !package_tree
                .at(&["17906", "appids"])
                .unwrap()
                .children()
                .is_empty()
        );
        let servers = connection.content_servers().await.unwrap();
        assert!(!servers.is_empty());
        let depot = app
            .depots
            .iter()
            .find(|depot| depot.public_manifest.is_some())
            .unwrap();
        let key = connection
            .depot_key(depot.depot_id, app.app_id)
            .await
            .unwrap();
        assert_eq!(key.len(), 32);
        let code = connection
            .manifest_request_code(depot.depot_id, app.app_id, depot.public_manifest.unwrap())
            .await
            .unwrap();
        assert_ne!(code, 0);
        let http = crate::client::http_client().unwrap();
        let content = crate::content::ContentClient::new(http);
        let authorization = crate::client::DepotAuthorization {
            cm: &connection,
            app_id: app.app_id,
            depot_id: depot.depot_id,
        };
        let plan = crate::content::DepotDownload {
            depot_id: depot.depot_id,
            manifest_id: depot.public_manifest.unwrap(),
            request_code: code,
            depot_key: key.as_slice().try_into().unwrap(),
            servers: servers
                .iter()
                .filter(|server| {
                    server.allowed_app_ids.is_empty()
                        || server.allowed_app_ids.contains(&app.app_id)
                })
                .cloned()
                .collect(),
            authorization: Some(&authorization),
            destination: std::path::PathBuf::new(), // Helper never writes files.
            cached_manifest: None,
        };
        let verified = match content.verify_live_sample(&plan).await {
            Err(error @ crate::content::ContentError::InvalidData(_)) => {
                panic!("Steam content format: {error}")
            }
            result => result,
        };
        let (bytes_verified, chunks_verified) =
            verified.unwrap_or_else(|error| panic!("No CDN verified the public sample: {error:?}"));
        assert!(bytes_verified > 0 && bytes_verified <= 8 * 1024 * 1024);
        assert!(chunks_verified > 0);
        eprintln!(
            "CM live: {} ({}), {} depots, {} HTTPS content servers, depot {} authorized; full public file verified: {} bytes in {} chunks",
            app.name,
            app.app_id,
            app.depots.len(),
            servers.len(),
            depot.depot_id,
            bytes_verified,
            chunks_verified,
        );
    }

    #[test]
    fn protobuf_envelope_preserves_job_and_session_identity() {
        let header = proto::Header {
            steam_id: Some(76561198000000000),
            session_id: Some(42),
            job_source: Some(7),
            job_target: Some(9),
            ..Default::default()
        };
        let bytes = encode(
            HELLO,
            &header,
            &proto::Hello {
                protocol_version: PROTOCOL,
            },
        );
        let mut output = Vec::new();
        decode_packets(
            Bytes::from(bytes.clone()),
            0,
            &mut { MAX_PACKET },
            &mut output,
        )
        .unwrap();
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].kind, HELLO);
        assert_eq!(output[0].header, header);
        assert_eq!(
            proto::Hello::decode(&output[0].body[..])
                .unwrap()
                .protocol_version,
            PROTOCOL
        );
    }

    #[test]
    fn multi_gzip_is_bounded_and_validates_lengths() {
        let packet = encode(
            HELLO,
            &proto::Header::default(),
            &proto::Hello {
                protocol_version: PROTOCOL,
            },
        );
        let mut body = (packet.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(&packet);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&body).unwrap();
        let compressed = gzip.finish().unwrap();
        let message = encode(
            MULTI,
            &proto::Header::default(),
            &proto::Multi {
                size_unzipped: body.len() as u32,
                body: compressed.clone().into(),
            },
        );
        let mut output = Vec::new();
        decode_packets(
            Bytes::from(message.clone()),
            0,
            &mut { MAX_PACKET },
            &mut output,
        )
        .unwrap();
        assert_eq!(output.len(), 1);
        let bad = encode(
            MULTI,
            &proto::Header::default(),
            &proto::Multi {
                size_unzipped: 2,
                body: compressed.into(),
            },
        );
        assert!(
            decode_packets(
                Bytes::from(bad.clone()),
                0,
                &mut { MAX_PACKET },
                &mut Vec::new()
            )
            .is_err()
        );
        for end in 0..8 {
            assert!(
                decode_packets(
                    Bytes::copy_from_slice(&message[..end]),
                    0,
                    &mut { MAX_PACKET },
                    &mut Vec::new()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn cm_endpoints_cannot_inject_paths_credentials_or_other_hosts() {
        assert_eq!(
            cm_url("ext2-par1.steamserver.net:27024").as_deref(),
            Some("wss://ext2-par1.steamserver.net:27024/cmsocket/")
        );
        for endpoint in [
            "attacker.example",
            "a.steamserver.net/a",
            "user@a.steamserver.net",
            "a.steamserver.net?x=y",
        ] {
            assert!(cm_url(endpoint).is_none());
        }
    }
}
