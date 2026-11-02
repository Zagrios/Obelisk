//! Social records contain no credentials or protobuf types. Revisioned full
//! snapshots coalesce bursts without dropping the final state.
use std::sync::Arc;

use game_runtime::steam::account_session::{Phase, State};
use obelisk_core::transfer::CancelToken;
use steam_kit::friends;

use crate::{
    LibraryCache, SteamCredentials, SteamError, SteamSession, steam_chat::SteamJoinTarget,
};

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
    /// The CM connection dropped, and the session connects again.
    Disconnected,
    AuthenticationRequired,
    Error,
    /// Every attempt to connect again failed; a retry or the Mac's wake
    /// tries again.
    Unreachable,
    /// The session was shut down.
    Stopped,
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
    /// The friend's game and how Steam's "Join Game" joins it, while it can.
    #[uniffi(default = None)]
    pub join: Option<SteamJoinTarget>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamFriendsSnapshot {
    /// The session's revision, which grows across its CM connections.
    pub revision: u64,
    pub account_id: u64,
    pub connection: SteamFriendsConnection,
    /// The session's CM connection: 0 for the first, one more for each that
    /// replaced a dropped one.
    pub transport: u64,
    pub friends: Vec<SteamFriend>,
}

/// The session's state, as the friends list shows it: the connection its
/// friends tell while it is up, else where the session stands.
impl From<State> for SteamFriendsSnapshot {
    fn from(state: State) -> Self {
        let snapshot = state.friends;
        let connection = match state.phase {
            Phase::Connected => snapshot.connection.into(),
            Phase::Reconnecting => SteamFriendsConnection::Disconnected,
            Phase::Unreachable => SteamFriendsConnection::Unreachable,
            Phase::AuthenticationRequired => SteamFriendsConnection::AuthenticationRequired,
            Phase::Closed => SteamFriendsConnection::Stopped,
        };
        Self {
            revision: state.revision,
            account_id: snapshot.account_id,
            connection,
            transport: state.transport,
            friends: snapshot
                .friends
                .into_iter()
                .map(|friend| SteamFriend {
                    steam_id: friend.steam_id,
                    display_name: friend.display_name,
                    avatar_url: friend.avatar_url,
                    presence: friend.presence.into(),
                    join: friend.game.as_ref().and_then(|game| {
                        let invite = game.invite();
                        let app_id = game.app_id?;
                        invite.sendable().then(|| SteamJoinTarget {
                            app_id,
                            invite: invite.into(),
                        })
                    }),
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

    /// Signs in, showing the account to its friends as `status` says.
    pub async fn connect(
        &self,
        credentials: SteamCredentials,
        library: Option<Arc<LibraryCache>>,
        status: crate::OnlineStatus,
    ) -> Result<Arc<SteamSession>, SteamError> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(SteamError::Cancelled),
            result = SteamSession::connect(credentials, library, status) => result,
        }
    }
}

/// Debug-only controlled Account session. Its friends go through the
/// production reducer and its states through the production conversion,
/// letting macOS tests exercise raw packets and reconnections across UniFFI.
#[cfg(debug_assertions)]
#[derive(uniffi::Object)]
pub struct SteamFriendsFixture {
    account_id: u64,
    /// The friends of the CM connection of the moment.
    source: std::sync::Mutex<friends::FriendsEventSource>,
    state: tokio::sync::watch::Sender<State>,
}

#[cfg(debug_assertions)]
impl SteamFriendsFixture {
    /// Publishes the source's friends, the session standing as `phase` with
    /// `transport`, when anything changed.
    fn publish(&self, source: &friends::FriendsEventSource, phase: Phase, transport: u64) {
        let friends = source.snapshot();
        self.state.send_if_modified(|state| {
            if (state.phase, state.transport, &state.friends) == (phase, transport, &friends) {
                return false;
            }
            *state = State {
                revision: state.revision + 1,
                phase,
                transport,
                friends,
            };
            true
        });
    }

    fn source(&self) -> std::sync::MutexGuard<'_, friends::FriendsEventSource> {
        self.source
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Publishes the same friends, the session standing as `phase`.
    fn stand(&self, phase: Phase) {
        let transport = self.state.borrow().transport;
        self.publish(&self.source(), phase, transport);
    }
}

#[cfg(debug_assertions)]
#[uniffi::export(async_runtime = "tokio")]
impl SteamFriendsFixture {
    #[uniffi::constructor]
    pub fn new(account_id: u64) -> Arc<Self> {
        let source = friends::FriendsEventSource::new(account_id);
        let (state, _) = tokio::sync::watch::channel(State {
            revision: 1,
            phase: Phase::Connected,
            transport: 0,
            friends: source.snapshot(),
        });
        Arc::new(Self {
            account_id,
            source: std::sync::Mutex::new(source),
            state,
        })
    }

    /// The CM connection of the moment receives a packet.
    pub fn receive(&self, kind: u32, body: Vec<u8>) {
        let source = self.source();
        source.apply_packet(kind, &body);
        let (phase, transport) = {
            let state = self.state.borrow();
            (state.phase, state.transport)
        };
        self.publish(&source, phase, transport);
    }

    /// Steam pushes `friend_steam_id` playing `app_id` (0 for none), in a
    /// lobby and with a rich presence `connect` string when given.
    pub fn friend_playing(
        &self,
        friend_steam_id: u64,
        app_id: u32,
        lobby_id: u64,
        connect: Option<String>,
    ) {
        // ClientPersonaState.
        self.receive(
            766,
            steam_kit::own_game::fixture::persona(
                friend_steam_id,
                app_id,
                lobby_id,
                connect.as_deref(),
            ),
        );
    }

    /// The CM connection drops and the session connects again, or Steam
    /// asks the account to sign in again.
    pub fn disconnect(&self, authentication_required: bool) {
        let source = self.source();
        source.disconnect(authentication_required);
        let transport = self.state.borrow().transport;
        let phase = if authentication_required {
            Phase::AuthenticationRequired
        } else {
            Phase::Reconnecting
        };
        self.publish(&source, phase, transport);
    }

    /// A new CM connection replaces the one that dropped, its friends still
    /// loading.
    pub fn reconnect(&self) {
        let mut source = self.source();
        *source = friends::FriendsEventSource::new(self.account_id);
        let transport = self.state.borrow().transport + 1;
        self.publish(&source, Phase::Connected, transport);
    }

    /// Every attempt to connect again failed.
    pub fn unreachable(&self) {
        self.stand(Phase::Unreachable);
    }

    /// The player's retry, or the Mac's wake: the session connects again,
    /// unless its account must sign in again or it closed.
    pub fn retry(&self) {
        if self.state.borrow().retries() {
            self.stand(Phase::Reconnecting);
        }
    }

    /// The session is shut down.
    pub fn close(&self) {
        self.stand(Phase::Closed);
    }

    pub async fn next_friends(&self, after_revision: u64) -> SteamFriendsSnapshot {
        let ready = obelisk_core::watch::next_matching(self.state.subscribe(), |state| {
            state.follows(after_revision)
        })
        .await;
        match ready {
            Ok(state) | Err(state) => state.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(phase: Phase, connection: friends::FriendsConnection) -> State {
        let mut snapshot = friends::FriendsEventSource::new(7).snapshot();
        snapshot.connection = connection;
        State {
            revision: 12,
            phase,
            transport: 2,
            friends: snapshot,
        }
    }

    #[test]
    fn a_friend_crosses_with_how_to_join_their_game_while_steam_shows_one() {
        const LOBBY: u64 = 109_775_244_421_126_053;
        let friend = |game: Option<friends::FriendGame>| friends::Friend {
            steam_id: 76_561_197_960_265_729,
            display_name: None,
            avatar_url: None,
            presence: friends::FriendPresence::Online,
            game,
        };
        let mut state = state(Phase::Connected, friends::FriendsConnection::Ready);
        state.friends.friends = vec![
            friend(Some(friends::FriendGame {
                app_id: Some(620),
                lobby_id: LOBBY,
                ..Default::default()
            })),
            friend(Some(friends::FriendGame {
                app_id: Some(620),
                ..Default::default()
            })),
            friend(None),
        ];
        let crossed = SteamFriendsSnapshot::from(state);
        assert_eq!(
            crossed.friends[0].join,
            Some(SteamJoinTarget {
                app_id: 620,
                invite: crate::SteamGameInvite::Lobby { lobby_id: LOBBY }
            })
        );
        assert_eq!(crossed.friends[1].join, None, "no lobby or connect string");
        assert_eq!(crossed.friends[2].join, None);
    }

    #[test]
    fn each_session_phase_crosses_as_the_friends_connection_swift_shows() {
        use friends::FriendsConnection as Friends;
        let crossed =
            |phase, connection| SteamFriendsSnapshot::from(state(phase, connection)).connection;

        assert_eq!(
            crossed(Phase::Connected, Friends::Loading),
            SteamFriendsConnection::Loading
        );
        assert_eq!(
            crossed(Phase::Connected, Friends::Ready),
            SteamFriendsConnection::Ready
        );
        assert_eq!(
            crossed(Phase::Connected, Friends::Error),
            SteamFriendsConnection::Error
        );
        assert_eq!(
            crossed(Phase::Reconnecting, Friends::Disconnected),
            SteamFriendsConnection::Disconnected
        );
        assert_eq!(
            crossed(Phase::Unreachable, Friends::Disconnected),
            SteamFriendsConnection::Unreachable
        );
        assert_eq!(
            crossed(Phase::AuthenticationRequired, Friends::Ready),
            SteamFriendsConnection::AuthenticationRequired
        );
        assert_eq!(
            crossed(Phase::Closed, Friends::Ready),
            SteamFriendsConnection::Stopped
        );

        let snapshot = SteamFriendsSnapshot::from(state(Phase::Connected, Friends::Ready));
        assert_eq!(
            (snapshot.revision, snapshot.account_id, snapshot.transport),
            (12, 7, 2),
            "the session's revision and connection, the friends' account"
        );
    }
}
