//! Authenticated Steam CM content queries over TLS WebSocket.
//!
//! One connection serializes queries; its background worker continues heartbeats
//! while callers download content over HTTP. Dropping it closes that session.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
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
#[cfg(test)]
mod login_tests;
#[cfg(test)]
mod platforms_tests;

const PROTOCOL: u32 = 65_581;
const PROTO_MASK: u32 = 0x8000_0000;
const MAX_PACKET: usize = 32 * 1024 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(45);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const PROFILE_TIMEOUT: Duration = Duration::from_secs(5);
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

#[derive(Clone, Debug)]
pub struct AppInfo {
    pub app_id: u32,
    pub name: String,
    /// Declared application platforms. Empty means Steam supplied no declaration.
    pub os_list: Vec<String>,
    pub free_to_download: bool,
    pub depots: Vec<DepotInfo>,
    pub launch_configs: Vec<LaunchConfig>,
    /// Complete Steam UFS configuration, including unknown future fields.
    pub cloud: Option<crate::cloud::CloudMetadata>,
}

/// App ownership and depot entitlement are distinct package fields.
#[derive(Clone, Debug, Default)]
pub struct ContentLicenses {
    pub apps: BTreeSet<u32>,
    pub depots: BTreeSet<u32>,
}

impl ContentLicenses {
    fn extend_package(&mut self, root: &kv::Kv) {
        for (field, ids) in [("appids", &mut self.apps), ("depotids", &mut self.depots)] {
            if let Some(entries) = root.get(field) {
                ids.extend(entries.children().iter().filter_map(|(_, value)| {
                    value.text()?.parse::<u32>().ok().filter(|id| *id > 0)
                }));
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct DepotInfo {
    pub depot_id: u32,
    pub shared_from_app: Option<u32>,
    pub dlc_app_id: Option<u32>,
    pub os_list: Vec<String>,
    pub os_arch: Option<String>,
    pub language: Option<String>,
    pub public_manifest: Option<u64>,
    pub size_bytes: Option<u64>,
}

/// Steam `oslist` semantics: an empty list means no platform restriction.
pub(crate) fn supports_platform(os_list: &[String], platform: &str) -> bool {
    os_list.is_empty() || os_list.iter().any(|os| os == platform)
}

#[derive(Clone, Debug)]
pub struct LaunchConfig {
    pub executable: String,
    pub arguments: String,
    pub working_directory: String,
    pub launch_type: String,
    pub os_list: Vec<String>,
    pub os_arch: Option<String>,
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
    Account(u64),
    Session {
        steam_id: u64,
        session_id: i32,
        heartbeat_seconds: u64,
    },
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
    /// the whole request, including any wait for the CM lock.
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
    incoming: mpsc::Receiver<Result<Packet, CmError>>,
    profile: watch::Receiver<ProfileState>,
    friends: friends::FriendsEventSource,
    worker: JoinHandle<()>,
    steam_id: u64,
    session_id: i32,
    cell_id: u32,
    next_job: u64,
    logged_on: bool,
    licenses: Option<Vec<proto::License>>,
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
            let (outgoing, commands) = mpsc::channel(32);
            let (incoming_tx, incoming) = mpsc::channel(128);
            let (profile_tx, profile) = watch::channel(ProfileState::default());
            let friends = friends::FriendsEventSource::new(0);
            let worker_friends = friends.clone();
            let worker = tokio::spawn(async move {
                let result =
                    connection_worker(socket, commands, &incoming_tx, &profile_tx, &worker_friends)
                        .await;
                profile_tx.send_replace(ProfileState::Disconnected);
                worker_friends.disconnect(false);
                if let Err(error) = result {
                    let _ = incoming_tx.try_send(Err(error));
                }
            });
            let mut connection = Self {
                outgoing,
                incoming,
                profile,
                friends,
                worker,
                steam_id: 0,
                session_id: 0,
                cell_id: 0,
                next_job: 1,
                logged_on: false,
                licenses: None,
            };
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

    /// Uses a Steam client refresh token obtained by the authentication module.
    /// Secrets are sent only to the TLS CM endpoint and never included in errors.
    pub async fn log_on(
        &mut self,
        account_name: &str,
        steam_id: u64,
        refresh_token: &str,
    ) -> Result<(), CmError> {
        if self.logged_on || account_name.is_empty() || steam_id == 0 || refresh_token.is_empty() {
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
        self.send(
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
                machine_name: "Forge".into(),
                supports_rate_limit: true,
                access_token: refresh_token.into(),
            },
            None,
        )
        .await?;
        let packet = self.wait_for(LOGON_RESPONSE, None).await?;
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
        self.logged_on = true;
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
    pub(crate) async fn request_profile(&mut self) -> Result<ProfileRequest, CmError> {
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
    pub async fn owned_games(&mut self) -> Result<Vec<OwnedGameInfo>, CmError> {
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
    pub async fn owned_app_ids(&mut self) -> Result<Vec<u32>, CmError> {
        Ok(self.content_licenses().await?.apps.into_iter().collect())
    }

    /// Preserve both namespaces when expanding active PICS packages.
    pub async fn content_licenses(&mut self) -> Result<ContentLicenses, CmError> {
        self.ensure_logged_on()?;
        if self.licenses.is_none() {
            let packet = self.wait_for(LICENSE_LIST, None).await?;
            self.accept_licenses(&packet)?;
        }
        // Expired, cancelled, pending, or never activated licenses grant no access.
        const INACTIVE: u32 = 0x04 | 0x08 | 0x10 | 0x20 | 0x400 | 0x800 | 0x40000;
        let packages: Vec<_> = self
            .licenses
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|license| license.package_id != 0 && license.flags & INACTIVE == 0)
            .map(|license| proto::ProductId {
                id: license.package_id,
                access_token: license.access_token,
            })
            .collect();
        let mut rights = ContentLicenses::default();
        for batch in packages.chunks(50) {
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

    pub async fn app_info(&mut self, app_id: u32) -> Result<AppInfo, CmError> {
        self.app_infos(&[app_id])
            .await?
            .into_iter()
            .find(|app| app.app_id == app_id)
            .ok_or(CmError::MissingApp(app_id))
    }

    /// Unknown or removed apps are omitted. Malformed metadata fails the request.
    pub async fn app_infos(&mut self, app_ids: &[u32]) -> Result<Vec<AppInfo>, CmError> {
        self.ensure_logged_on()?;
        let mut apps = Vec::new();
        for batch in app_ids.chunks(50) {
            let token_job = self
                .send(
                    TOKEN_REQUEST,
                    &proto::AccessTokenRequest {
                        package_ids: Vec::new(),
                        app_ids: batch.to_vec(),
                    },
                    None,
                )
                .await?;
            let token_packet = self.wait_for(TOKEN_RESPONSE, Some(token_job)).await?;
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

    pub async fn depot_key(&mut self, depot_id: u32, app_id: u32) -> Result<Vec<u8>, CmError> {
        self.ensure_logged_on()?;
        let job = self
            .send(
                DEPOT_KEY_REQUEST,
                &proto::DepotKeyRequest { depot_id, app_id },
                None,
            )
            .await?;
        let packet = self.wait_for(DEPOT_KEY_RESPONSE, Some(job)).await?;
        let response = proto::DepotKeyResponse::decode(&packet.body[..])?;
        check_result(response.result, "backend.steam.cm.action.depot")?;
        if response.depot_id != depot_id || response.key.len() != 32 {
            return Err(CmError::Protocol("backend.steam.cm.invalidDepotKey".into()));
        }
        Ok(response.key)
    }

    /// Public branch only. Password protected beta branches are not requested.
    pub async fn manifest_request_code(
        &mut self,
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

    pub async fn content_servers(&mut self) -> Result<Vec<ContentServer>, CmError> {
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
        &mut self,
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
        &mut self,
        apps: &[proto::ProductId],
        packages: &[proto::ProductId],
    ) -> Result<proto::ProductResponse, CmError> {
        let job = self
            .send(
                PRODUCT_REQUEST,
                &proto::ProductRequest {
                    apps: apps.to_vec(),
                    packages: packages.to_vec(),
                    metadata_only: false,
                },
                None,
            )
            .await?;
        let mut combined = proto::ProductResponse::default();
        let mut total_bytes = 0_usize;
        for _ in 0..512 {
            let packet = self.wait_for(PRODUCT_RESPONSE, Some(job)).await?;
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
        &mut self,
        name: &str,
        request: &impl Message,
    ) -> Result<R, CmError> {
        self.ensure_logged_on()?;
        let job = self.send(SERVICE_REQUEST, request, Some(name)).await?;
        let packet = self.wait_for(SERVICE_RESPONSE, Some(job)).await?;
        check_result(
            packet.header.result,
            "backend.steam.cm.action.contentRequest",
        )?;
        R::decode(&packet.body[..]).map_err(Into::into)
    }

    fn ensure_logged_on(&self) -> Result<(), CmError> {
        if self.logged_on {
            Ok(())
        } else {
            Err(CmError::Disconnected)
        }
    }

    async fn send(
        &mut self,
        kind: u32,
        message: &impl Message,
        name: Option<&str>,
    ) -> Result<u64, CmError> {
        let job = self.next_job;
        self.next_job = self
            .next_job
            .checked_add(1)
            .ok_or_else(|| CmError::Protocol("backend.steam.cm.requestIdsExhausted".into()))?;
        let header = proto::Header {
            steam_id: Some(self.steam_id),
            session_id: Some(self.session_id),
            job_source: Some(job),
            target_name: name.map(str::to_owned),
            ..Default::default()
        };
        self.outgoing
            .send(Command::Send(encode(kind, &header, message)))
            .await
            .map_err(|_| CmError::Disconnected)?;
        Ok(job)
    }

    async fn wait_for(&mut self, kind: u32, job: Option<u64>) -> Result<Packet, CmError> {
        let deadline = Instant::now() + QUERY_TIMEOUT;
        loop {
            let packet = timeout_at(deadline, self.incoming.recv())
                .await
                .map_err(|_| CmError::Timeout)?
                .ok_or(CmError::Disconnected)??;
            if packet.kind == LOGGED_OFF {
                self.logged_on = false;
                return Err(CmError::Disconnected);
            }
            if packet.kind == LICENSE_LIST {
                self.accept_licenses(&packet)?;
            }
            if packet.kind == kind && job.is_none_or(|id| packet.header.job_target == Some(id)) {
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
            // Calls are serialized; other job IDs belong to a timed-out request.
        }
    }

    fn accept_licenses(&mut self, packet: &Packet) -> Result<(), CmError> {
        let response = proto::LicenseList::decode(&packet.body[..])?;
        check_result(response.result, "backend.steam.cm.action.readLicenses")?;
        self.licenses = Some(response.licenses);
        Ok(())
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
    incoming: &mpsc::Sender<Result<Packet, CmError>>,
    profile: &watch::Sender<ProfileState>,
    friends: &friends::FriendsEventSource,
) -> Result<(), CmError> {
    let mut heartbeat_header = None;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pending_hydration = BTreeSet::new();
    let mut persona_push_pending = false;
    let mut persona_push_in_flight = false;
    let mut hydration_deadline = None;
    let mut last_received = Instant::now();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Send(bytes)) => {
                    send_message(&mut socket, WsMessage::Binary(bytes.into())).await?;
                }
                Some(Command::Account(steam_id)) => friends.set_account(steam_id),
                Some(Command::Session { steam_id, session_id, heartbeat_seconds }) => {
                    heartbeat_header = Some(proto::Header { steam_id: Some(steam_id), session_id: Some(session_id), ..Default::default() });
                    heartbeat = tokio::time::interval(Duration::from_secs(heartbeat_seconds.clamp(1, 60)));
                    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    hydration_deadline = Some(Instant::now() + Duration::from_secs(20));
                    last_received = Instant::now();
                }
                None => return Ok(()),
            },
            _ = heartbeat.tick() => {
                if let Some(header) = &heartbeat_header {
                    send_message(&mut socket, WsMessage::Binary(encode(HEARTBEAT, header, &proto::Heartbeat { send_reply: true }).into())).await?;
                }
            },
            _ = async { tokio::time::sleep_until(hydration_deadline.expect("guarded")).await }, if hydration_deadline.is_some() => {
                friends.loading_timed_out();
                hydration_deadline = None;
            },
            _ = tokio::time::sleep_until(last_received + Duration::from_secs(90)), if heartbeat_header.is_some() => return Err(CmError::Disconnected),
            _ = std::future::ready(()), if persona_push_pending && !persona_push_in_flight && heartbeat_header.is_some() && friends.has_full_list() => {
                // Read-only push request after a full list or newly accepted
                // friend. Coalesce relationship bursts without changing the
                // account's own presence or periodically refreshing statuses.
                let mut header = heartbeat_header.clone().expect("authenticated hydration");
                header.job_source = Some(FRIENDS_HYDRATION_JOB);
                header.target_name = Some("Chat.RequestFriendPersonaStates#1".into());
                send_message(&mut socket, WsMessage::Binary(encode(SERVICE_REQUEST, &header, &proto::RequestFriendPersonaStates {}).into())).await?;
                persona_push_pending = false;
                persona_push_in_flight = true;
            },
            _ = std::future::ready(()), if !pending_hydration.is_empty() && heartbeat_header.is_some() => {
                let batch: Vec<_> = (0..friends::REQUEST_BATCH).filter_map(|_| pending_hydration.pop_first()).filter(|id| friends.retains(*id)).collect();
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
                            persona_push_in_flight = false;
                            // This optional reply cannot consume a library or
                            // download response or fill their incoming channel.
                            continue;
                        }
                        let hydrate = friends.apply_packet_deferred(packet.kind, &packet.body);
                        persona_push_pending |= !hydrate.is_empty();
                        pending_hydration.extend(hydrate);
                        if packet.kind == friends::FRIENDS_LIST {
                            friends.retain_hydration(&mut pending_hydration);
                        }
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
                            profile.send_replace(ProfileState::Disconnected);
                            let authentication_required = proto::LoggedOff::decode(&packet.body[..]).ok().and_then(|logged_off| logged_off.result).is_some_and(friends::authentication_required);
                            friends.disconnect(authentication_required);
                            // Close this physical connection before a replacement
                            // starts; queued content calls retain disconnection.
                            let _ = incoming.try_send(Ok(packet));
                            return Err(CmError::Disconnected);
                        }
                        if matches!(packet.kind, SERVICE_RESPONSE | LOGON_RESPONSE | LOGGED_OFF | LICENSE_LIST | DEPOT_KEY_RESPONSE | PRODUCT_RESPONSE | TOKEN_RESPONSE) {
                            incoming.try_send(Ok(packet)).map_err(|_| CmError::Protocol("backend.steam.cm.tooManyPendingResponses".into()))?;
                        }
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
    let header = header.encode_to_vec();
    let mut bytes = Vec::with_capacity(8 + header.len() + message.encoded_len());
    bytes.extend_from_slice(&(kind | PROTO_MASK).to_le_bytes());
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&message.encode_to_vec());
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

fn string_at(tree: &kv::Kv, path: &[&str]) -> String {
    tree.at(path)
        .and_then(kv::Kv::text)
        .unwrap_or_default()
        .to_owned()
}

fn optional_at(tree: &kv::Kv, path: &[&str]) -> Option<String> {
    tree.at(path)
        .and_then(kv::Kv::text)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn number_at<T: std::str::FromStr>(tree: &kv::Kv, path: &[&str]) -> Option<T> {
    tree.at(path)
        .and_then(kv::Kv::text)
        .and_then(|value| value.parse().ok())
}

fn os_list(tree: &kv::Kv, path: &[&str]) -> Vec<String> {
    string_at(tree, path)
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn parse_app(app_id: u32, bytes: &[u8]) -> Result<AppInfo, CmError> {
    let tree = kv::text(bytes)?;
    let root = tree.get("appinfo").unwrap_or(&tree);
    if number_at::<u32>(root, &["appid"]).is_some_and(|id| id != app_id) {
        return Err(CmError::Protocol("backend.steam.cm.unexpectedAppId".into()));
    }
    let mut depots = Vec::new();
    if let Some(entries) = root.get("depots") {
        for (id, depot) in entries.children() {
            let Ok(depot_id) = id.parse::<u32>() else {
                continue;
            };
            depots.push(DepotInfo {
                depot_id,
                shared_from_app: number_at(depot, &["depotfromapp"]),
                dlc_app_id: number_at(depot, &["dlcappid"]),
                os_list: os_list(depot, &["config", "oslist"]),
                os_arch: optional_at(depot, &["config", "osarch"]),
                language: optional_at(depot, &["config", "language"]),
                public_manifest: number_at(depot, &["manifests", "public", "gid"])
                    .or_else(|| number_at(depot, &["manifests", "public"])),
                size_bytes: number_at(depot, &["manifests", "public", "size"]),
            });
        }
    }
    let mut launch_configs = Vec::new();
    if let Some(entries) = root.at(&["config", "launch"]) {
        for (_, launch) in entries.children() {
            let executable = string_at(launch, &["executable"]);
            if executable.is_empty() {
                continue;
            }
            launch_configs.push(LaunchConfig {
                executable,
                arguments: string_at(launch, &["arguments"]),
                working_directory: string_at(launch, &["workingdir"]),
                launch_type: string_at(launch, &["type"]),
                os_list: os_list(launch, &["config", "oslist"]),
                os_arch: optional_at(launch, &["config", "osarch"]),
            });
        }
    }
    let mut declared_os = os_list(root, &["common", "oslist"]);
    if declared_os.is_empty() {
        declared_os = launch_configs
            .iter()
            .flat_map(|launch| launch.os_list.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    Ok(AppInfo {
        app_id,
        name: string_at(root, &["common", "name"]),
        os_list: declared_os,
        free_to_download: matches!(
            string_at(root, &["common", "freetodownload"]).as_str(),
            "1" | "true"
        ),
        depots,
        launch_configs,
        cloud: root.get("ufs").map(crate::cloud::CloudMetadata::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn app_info_retains_generic_cloud_rules_and_unknown_fields() {
        let app = parse_app(42, br#""appinfo" {
            "appid" "42" "ufs" {
                "quota" "100000" "maxnumfiles" "32"
                "savefiles" { "0" { "root" "MacDocuments" "path" "Game/{64BitSteamID}" "pattern" "*.sav" } }
                "rootoverrides" { "0" { "root" "WinMyDocuments" "useinstead" "MacDocuments" } }
                "future-field" { "nested" "retained" }
                "duplicate" "a" "duplicate" "b"
            }
        }"#).unwrap();
        let cloud = app.cloud.unwrap();
        assert_eq!(cloud.field("quota").and_then(|v| v.text()), Some("100000"));
        assert_eq!(
            cloud
                .field("savefiles")
                .unwrap()
                .field("0")
                .unwrap()
                .field("path")
                .unwrap()
                .text(),
            Some("Game/{64BitSteamID}")
        );
        assert!(cloud.field("rootoverrides").is_some());
        assert_eq!(
            cloud
                .field("future-field")
                .unwrap()
                .field("nested")
                .unwrap()
                .text(),
            Some("retained")
        );
        assert!(cloud.field("duplicate").is_none());
        assert_eq!(
            cloud
                .entries()
                .unwrap()
                .iter()
                .filter(|(key, _)| key == "duplicate")
                .count(),
            2
        );
        let absent = parse_app(42, br#""appinfo" { "appid" "42" }"#).unwrap();
        assert!(absent.cloud.is_none());
        let malformed =
            parse_app(42, br#""appinfo" { "appid" "42" "ufs" "invalid shape" }"#).unwrap();
        assert!(malformed.cloud.unwrap().entries().is_none());
    }

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

    #[tokio::test]
    async fn social_worker_keeps_library_download_replies_and_coalesces_persona_floods() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (client, server) = tokio::join!(tokio_tungstenite::connect_async(endpoint), async {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        });
        let (outgoing, commands) = mpsc::channel(32);
        let (incoming_tx, incoming) = mpsc::channel(128);
        let (profile_tx, profile) = watch::channel(ProfileState::default());
        let friends = friends::FriendsEventSource::new(TEST_STEAM_ID);
        let worker_friends = friends.clone();
        let worker = tokio::spawn(async move {
            connection_worker(
                client.unwrap().0,
                commands,
                &incoming_tx,
                &profile_tx,
                &worker_friends,
            )
            .await
            .unwrap();
        });
        let mut connection = SteamConnection {
            outgoing,
            incoming,
            profile,
            friends,
            worker,
            steam_id: TEST_STEAM_ID,
            session_id: 42,
            cell_id: 0,
            next_job: 1,
            logged_on: true,
            licenses: None,
        };
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
        // Request awaiting does not hold &mut SteamConnection or drain incoming.
        let content_job = connection
            .send(SERVICE_REQUEST, &proto::Empty {}, Some("Test.Content#1"))
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
        connection
            .wait_for(SERVICE_RESPONSE, Some(content_job))
            .await
            .unwrap();
        assert_eq!(connection.licenses.as_ref().unwrap()[0].package_id, 99);
        let depot = connection
            .wait_for(DEPOT_KEY_RESPONSE, Some(content_job + 1))
            .await
            .unwrap();
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
        let (incoming, _responses) = mpsc::channel(128);
        let (profile, _profile_updates) = watch::channel(ProfileState::default());
        let social = friends::FriendsEventSource::new(TEST_STEAM_ID);
        let writer = social.clone();
        let worker = tokio::spawn(async move {
            let result =
                connection_worker(client.unwrap().0, commands, &incoming, &profile, &writer).await;
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
        connection
            .send(
                LOGON,
                &proto::Logon {
                    protocol_version: PROTOCOL,
                    language: "english".into(),
                    os_type: (-102_i32) as u32,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let packet = connection.wait_for(LOGON_RESPONSE, None).await.unwrap();
        let response = proto::LogonResponse::decode(&packet.body[..]).unwrap();
        check_result(response.result, "the anonymous test sign-in").unwrap();
        connection.steam_id = packet.header.steam_id.unwrap();
        connection.session_id = packet.header.session_id.unwrap();
        connection.cell_id = response.cell_id;
        connection.logged_on = true;
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
        let mut verified = None;
        let mut last_error = None;
        for server in servers
            .iter()
            .filter(|server| {
                server.allowed_app_ids.is_empty() || server.allowed_app_ids.contains(&app.app_id)
            })
            .take(5)
        {
            let http = crate::client::http_client().unwrap();
            let content = crate::content::ContentClient::new(http);
            let mut plan = crate::content::DepotDownload {
                depot_id: depot.depot_id,
                manifest_id: depot.public_manifest.unwrap(),
                request_code: code,
                depot_key: key.as_slice().try_into().unwrap(),
                server: server.vhost.clone(),
                cdn_token: None,
                destination: std::path::PathBuf::new(), // Helper never writes files.
                cached_manifest: None,
            };
            let mut result = content.verify_live_sample(&plan).await;
            if matches!(result, Err(crate::content::ContentError::Http(401 | 403))) {
                plan.cdn_token = Some(
                    connection
                        .cdn_auth(app.app_id, depot.depot_id, &server.host)
                        .await
                        .unwrap(),
                );
                result = content.verify_live_sample(&plan).await;
            }
            match result {
                Ok(sample) => {
                    verified = Some(sample);
                    break;
                }
                Err(error @ crate::content::ContentError::InvalidData(_)) => {
                    panic!("Steam content format: {error}")
                }
                Err(error) => last_error = Some(error),
            }
        }
        let (bytes_verified, chunks_verified) =
            verified.unwrap_or_else(|| panic!("No CDN verified the public sample: {last_error:?}"));
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
    fn package_entitlements_preserve_app_and_depot_namespaces() {
        let package = kv::text(br#""appids" { "0" "588650" } "depotids" { "0" "588651" "1" "588652" "2" "588653" "3" "588654" }"#).unwrap();
        let mut rights = ContentLicenses::default();
        rights.extend_package(&package);
        assert_eq!(rights.apps, BTreeSet::from([588650]));
        assert_eq!(
            rights.depots,
            BTreeSet::from([588651, 588652, 588653, 588654])
        );
        assert!(!rights.depots.contains(&1046443));
        rights.extend_package(
            &kv::text(br#""appids" { "0" "99" } "depotids" { "0" "1046443" "1" "0" "2" "bad" }"#)
                .unwrap(),
        );
        assert!(rights.depots.contains(&1046443));
        assert!(rights.apps.contains(&99));
        assert!(!rights.apps.contains(&1046443));
        assert!(!rights.depots.contains(&99));
        assert!(!rights.depots.contains(&0));
    }

    #[test]
    fn parses_download_filters_shared_depots_and_launches() {
        let app = parse_app(42, br#""appinfo" {
            "appid" "42" "common" { "name" "Test" "type" "Game" "FreeToDownload" "1" "oslist" "windows" }
            "config" { "installdir" "Test" "launch" { "0" { "executable" "game.exe" "workingdir" "bin" "type" "default" "config" { "oslist" "windows" } } } }
            "depots" {
                "43" { "config" { "oslist" "windows,macos" "osarch" "64" "language" "french" }
                    "manifests" { "public" { "gid" "123456789012345" "size" "1000" "download" "500" } } }
                "44" { "depotfromapp" "99" "dlcappid" "45" }
                "branches" { "public" { "buildid" "123" } }
            }
        }"#).unwrap();
        assert_eq!(app.name, "Test");
        assert_eq!(app.os_list, ["windows"]);
        assert!(app.free_to_download);
        assert_eq!(app.depots.len(), 2);
        assert_eq!(app.depots[0].public_manifest, Some(123456789012345));
        assert_eq!(app.depots[0].os_list, ["windows", "macos"]);
        assert_eq!(app.depots[0].size_bytes, Some(1000));
        assert_eq!(app.depots[1].shared_from_app, Some(99));
        assert_eq!(app.depots[1].dlc_app_id, Some(45));
        assert_eq!(app.launch_configs[0].executable, "game.exe");
        assert_eq!(app.launch_configs[0].working_directory, "bin");
        assert_eq!(app.launch_configs[0].launch_type, "default");
    }

    #[test]
    fn missing_app_platforms_fall_back_to_explicit_launch_platforms_only() {
        let unknown = parse_app(
            1,
            br#""appinfo" { "depots" { "2" { "manifests" { "public" "3" } } } }"#,
        )
        .unwrap();
        assert!(unknown.os_list.is_empty());
        let with_launch = parse_app(1, br#""appinfo" { "config" { "launch" { "0" { "executable" "game.exe" "config" { "oslist" "Windows" } } } } }"#).unwrap();
        assert_eq!(with_launch.os_list, ["windows"]);
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
