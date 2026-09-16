//! Social records contain no credentials or protobuf types. Revisioned full
//! snapshots coalesce bursts without dropping the final state.
use std::sync::Arc;

use forge_core::transfer::CancelToken;
use steam_kit::friends;

use crate::{SteamCredentials, SteamError, SteamSession};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamFriendPresence {
    Unknown,
    Offline,
    Online,
    Busy,
    Away,
    Snooze,
    LookingToTrade,
    LookingToPlay,
    Invisible,
}

impl From<friends::FriendPresence> for SteamFriendPresence {
    fn from(value: friends::FriendPresence) -> Self {
        match value {
            friends::FriendPresence::Unknown => Self::Unknown,
            friends::FriendPresence::Offline => Self::Offline,
            friends::FriendPresence::Online => Self::Online,
            friends::FriendPresence::Busy => Self::Busy,
            friends::FriendPresence::Away => Self::Away,
            friends::FriendPresence::Snooze => Self::Snooze,
            friends::FriendPresence::LookingToTrade => Self::LookingToTrade,
            friends::FriendPresence::LookingToPlay => Self::LookingToPlay,
            friends::FriendPresence::Invisible => Self::Invisible,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamFriendsConnection {
    Loading,
    Ready,
    Disconnected,
    AuthenticationRequired,
    Error,
}

impl From<friends::FriendsConnection> for SteamFriendsConnection {
    fn from(value: friends::FriendsConnection) -> Self {
        match value {
            friends::FriendsConnection::Loading => Self::Loading,
            friends::FriendsConnection::Ready => Self::Ready,
            friends::FriendsConnection::Disconnected => Self::Disconnected,
            friends::FriendsConnection::AuthenticationRequired => Self::AuthenticationRequired,
            friends::FriendsConnection::Error => Self::Error,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamFriendGame {
    pub app_id: Option<u32>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamFriend {
    pub steam_id: u64,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub presence: SteamFriendPresence,
    pub game: Option<SteamFriendGame>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamFriendsSnapshot {
    pub revision: u64,
    pub account_id: u64,
    pub connection: SteamFriendsConnection,
    pub friends: Vec<SteamFriend>,
}

impl From<friends::FriendsSnapshot> for SteamFriendsSnapshot {
    fn from(snapshot: friends::FriendsSnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            account_id: snapshot.account_id,
            connection: snapshot.connection.into(),
            friends: snapshot
                .friends
                .into_iter()
                .map(|friend| SteamFriend {
                    steam_id: friend.steam_id,
                    display_name: friend.display_name,
                    avatar_url: friend.avatar_url,
                    presence: friend.presence.into(),
                    game: friend.game.map(|game| SteamFriendGame {
                        app_id: game.app_id,
                        name: game.name,
                    }),
                })
                .collect(),
        }
    }
}

/// A single-use connection attempt with explicit cancellation across UniFFI.
#[derive(uniffi::Object)]
pub struct SteamConnectionAttempt {
    cancel: CancelToken,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamConnectionAttempt {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancel: CancelToken::new(),
        })
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub async fn connect(
        &self,
        credentials: SteamCredentials,
    ) -> Result<Arc<SteamSession>, SteamError> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(SteamError::Cancelled),
            result = SteamSession::connect(credentials) => result,
        }
    }
}

/// Debug-only controlled CM event source. It uses the production reducer and
/// watch subscription, letting macOS tests exercise raw packets across UniFFI.
#[cfg(debug_assertions)]
#[derive(uniffi::Object)]
pub struct SteamFriendsFixture {
    source: friends::FriendsEventSource,
}

#[cfg(debug_assertions)]
#[uniffi::export(async_runtime = "tokio")]
impl SteamFriendsFixture {
    #[uniffi::constructor]
    pub fn new(account_id: u64) -> Arc<Self> {
        Arc::new(Self {
            source: friends::FriendsEventSource::new(account_id),
        })
    }

    pub fn receive(&self, kind: u32, body: Vec<u8>) {
        self.source.apply_packet(kind, &body);
    }

    pub fn disconnect(&self, authentication_required: bool) {
        self.source.disconnect(authentication_required);
    }

    pub async fn next_friends(&self, after_revision: u64) -> SteamFriendsSnapshot {
        self.source.next(after_revision).await.into()
    }
}
