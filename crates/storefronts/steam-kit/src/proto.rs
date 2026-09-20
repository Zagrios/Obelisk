//! Minimal Steam wire messages used by the content client.
//!
//! Field numbers are protocol facts checked against SteamKit's generated schemas.
//! These Rust declarations are intentionally separate from application models.

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Header {
    #[prost(fixed64, optional, tag = "1")]
    pub steam_id: Option<u64>,
    #[prost(int32, optional, tag = "2")]
    pub session_id: Option<i32>,
    #[prost(fixed64, optional, tag = "10")]
    pub job_source: Option<u64>,
    #[prost(fixed64, optional, tag = "11")]
    pub job_target: Option<u64>,
    #[prost(string, optional, tag = "12")]
    pub target_name: Option<String>,
    #[prost(int32, optional, tag = "13")]
    pub result: Option<i32>,
    #[prost(int32, optional, tag = "17")]
    pub transport_error: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Multi {
    #[prost(uint32, tag = "1")]
    pub size_unzipped: u32,
    /// Borrowed from the WebSocket frame when decoded from `Bytes`; no copy.
    #[prost(bytes = "bytes", tag = "2")]
    pub body: bytes::Bytes,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Hello {
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
#[cfg(test)]
pub(crate) struct Empty {}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Heartbeat {
    #[prost(bool, tag = "1")]
    pub send_reply: bool,
}

/// steam/steammessages_chat.steamclient.proto, Chat.RequestFriendPersonaStates.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct RequestFriendPersonaStates {}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Logon {
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    #[prost(uint32, tag = "2")]
    pub deprecated_obfuscated_private_ip: u32,
    #[prost(uint32, tag = "5")]
    pub package_version: u32,
    #[prost(string, tag = "6")]
    pub language: String,
    #[prost(uint32, tag = "7")]
    pub os_type: u32,
    #[prost(bool, tag = "8")]
    pub remember_password: bool,
    #[prost(message, optional, tag = "11")]
    pub obfuscated_private_ip: Option<LoginAddress>,
    #[prost(string, tag = "50")]
    pub account_name: String,
    #[prost(string, tag = "96")]
    pub machine_name: String,
    #[prost(bool, tag = "102")]
    pub supports_rate_limit: bool,
    #[prost(string, tag = "108")]
    pub access_token: String,
}

/// CMsgIPAddress's IPv4 form, used as a non-secret client login identifier.
#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LoginAddress {
    #[prost(fixed32, tag = "1")]
    pub v4: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LogonResponse {
    #[prost(int32, optional, tag = "1")]
    pub result: Option<i32>,
    #[prost(int32, tag = "2")]
    pub out_of_game_heartbeat: i32,
    #[prost(int32, tag = "3")]
    pub heartbeat: i32,
    #[prost(uint32, tag = "7")]
    pub cell_id: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FriendDataRequest {
    #[prost(uint32, tag = "1")]
    pub persona_state_requested: u32,
    #[prost(fixed64, repeated, packed = "false", tag = "2")]
    pub friends: Vec<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FriendsList {
    #[prost(bool, tag = "1")]
    pub incremental: bool,
    #[prost(message, repeated, tag = "2")]
    pub friends: Vec<FriendRelationship>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct FriendRelationship {
    #[prost(fixed64, tag = "1")]
    pub steam_id: u64,
    #[prost(uint32, tag = "2")]
    pub relationship: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LoggedOff {
    #[prost(int32, optional, tag = "1")]
    pub result: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct PersonaState {
    #[prost(uint32, tag = "1")]
    pub status_flags: u32,
    #[prost(message, repeated, tag = "2")]
    pub friends: Vec<Persona>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Persona {
    #[prost(fixed64, tag = "1")]
    pub friend_id: u64,
    #[prost(uint32, optional, tag = "2")]
    pub persona_state: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub game_played_app_id: Option<u32>,
    #[prost(string, optional, tag = "15")]
    pub player_name: Option<String>,
    #[prost(bytes = "vec", optional, tag = "31")]
    pub avatar_hash: Option<Vec<u8>>,
    #[prost(string, optional, tag = "55")]
    pub game_name: Option<String>,
    #[prost(fixed64, optional, tag = "56")]
    pub game_id: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LicenseList {
    #[prost(int32, optional, tag = "1")]
    pub result: Option<i32>,
    #[prost(message, repeated, tag = "2")]
    pub licenses: Vec<License>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct License {
    #[prost(uint32, tag = "1")]
    pub package_id: u32,
    #[prost(uint32, tag = "7")]
    pub flags: u32,
    #[prost(uint64, tag = "17")]
    pub access_token: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AccessTokenRequest {
    #[prost(uint32, repeated, packed = "false", tag = "1")]
    pub package_ids: Vec<u32>,
    #[prost(uint32, repeated, packed = "false", tag = "2")]
    pub app_ids: Vec<u32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AccessTokenResponse {
    #[prost(message, repeated, tag = "1")]
    pub packages: Vec<AccessToken>,
    #[prost(uint32, repeated, packed = "false", tag = "2")]
    pub denied_packages: Vec<u32>,
    #[prost(message, repeated, tag = "3")]
    pub apps: Vec<AccessToken>,
    #[prost(uint32, repeated, packed = "false", tag = "4")]
    pub denied_apps: Vec<u32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AccessToken {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(uint64, tag = "2")]
    pub token: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ProductRequest {
    #[prost(message, repeated, tag = "1")]
    pub packages: Vec<ProductId>,
    #[prost(message, repeated, tag = "2")]
    pub apps: Vec<ProductId>,
    #[prost(bool, tag = "3")]
    pub metadata_only: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ProductId {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(uint64, tag = "2")]
    pub access_token: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ProductResponse {
    #[prost(message, repeated, tag = "1")]
    pub apps: Vec<ProductInfo>,
    #[prost(uint32, repeated, packed = "false", tag = "2")]
    pub unknown_apps: Vec<u32>,
    #[prost(message, repeated, tag = "3")]
    pub packages: Vec<ProductInfo>,
    #[prost(uint32, repeated, packed = "false", tag = "4")]
    pub unknown_packages: Vec<u32>,
    #[prost(bool, tag = "6")]
    pub response_pending: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ProductInfo {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(bool, tag = "3")]
    pub missing_token: bool,
    /// Multi-megabyte app-info VDF, sliced from the packet rather than copied.
    #[prost(bytes = "bytes", tag = "5")]
    pub buffer: bytes::Bytes,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DepotKeyRequest {
    #[prost(uint32, tag = "1")]
    pub depot_id: u32,
    #[prost(uint32, tag = "2")]
    pub app_id: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct DepotKeyResponse {
    #[prost(int32, optional, tag = "1")]
    pub result: Option<i32>,
    #[prost(uint32, tag = "2")]
    pub depot_id: u32,
    #[prost(bytes = "vec", tag = "3")]
    pub key: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ServerRequest {
    #[prost(uint32, tag = "1")]
    pub cell_id: u32,
    #[prost(uint32, tag = "2")]
    pub max_servers: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ServerResponse {
    #[prost(message, repeated, tag = "1")]
    pub servers: Vec<ServerInfo>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ServerInfo {
    #[prost(string, tag = "1")]
    pub server_type: String,
    #[prost(float, tag = "5")]
    pub weighted_load: f32,
    #[prost(string, tag = "8")]
    pub host: String,
    #[prost(string, tag = "9")]
    pub vhost: String,
    #[prost(bool, tag = "10")]
    pub use_as_proxy: bool,
    #[prost(string, tag = "12")]
    pub https_support: String,
    #[prost(uint32, repeated, packed = "false", tag = "13")]
    pub allowed_app_ids: Vec<u32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ManifestCodeRequest {
    #[prost(uint32, tag = "1")]
    pub app_id: u32,
    #[prost(uint32, tag = "2")]
    pub depot_id: u32,
    #[prost(uint64, tag = "3")]
    pub manifest_id: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ManifestCodeResponse {
    #[prost(uint64, tag = "1")]
    pub code: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CdnAuthRequest {
    #[prost(uint32, tag = "1")]
    pub depot_id: u32,
    #[prost(string, tag = "2")]
    pub host: String,
    #[prost(uint32, tag = "3")]
    pub app_id: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct CdnAuthResponse {
    #[prost(string, tag = "1")]
    pub token: String,
    #[prost(uint32, tag = "2")]
    pub expiration: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct OwnedGamesRequest {
    #[prost(uint64, tag = "1")]
    pub steam_id: u64,
    #[prost(bool, tag = "2")]
    pub include_app_info: bool,
    #[prost(bool, tag = "3")]
    pub include_played_free_games: bool,
    #[prost(bool, tag = "5")]
    pub include_free_sub: bool,
    // The protocol defaults this field to true; explicitly encode false.
    #[prost(bool, optional, tag = "6")]
    pub skip_unvetted_apps: Option<bool>,
    #[prost(string, tag = "7")]
    pub language: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct OwnedGamesResponse {
    #[prost(uint32, tag = "1")]
    pub game_count: u32,
    #[prost(message, repeated, tag = "2")]
    pub games: Vec<OwnedGame>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct OwnedGame {
    #[prost(int32, tag = "1")]
    pub app_id: i32,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(int32, tag = "4")]
    pub playtime_forever: i32,
    #[prost(string, tag = "5")]
    pub icon_hash: String,
    #[prost(uint32, tag = "11")]
    pub last_played: u32,
}
