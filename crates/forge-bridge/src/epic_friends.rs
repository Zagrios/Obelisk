//! Epic protocol values are converted before entering the shared Swift presentation.
use epic_kit::friends;
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum EpicFriendPresence {
    Unknown,
    Offline,
    Online,
    Away,
    Busy,
    ExtendedAway,
    Invisible,
}
#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum EpicFriendsConnection {
    Loading,
    Ready,
    Interrupted,
    Failed,
    Forbidden,
    AuthenticationRequired,
    Stopped,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicFriendActivity {
    pub id: String,
    pub name: Option<String>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicFriend {
    pub account_id: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub presence: EpicFriendPresence,
    pub activity: Option<EpicFriendActivity>,
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicFriendsSnapshot {
    pub account_id: String,
    pub revision: u64,
    pub connection: EpicFriendsConnection,
    pub friends: Vec<EpicFriend>,
    pub message: Option<String>,
}
impl From<friends::Snapshot> for EpicFriendsSnapshot {
    fn from(s: friends::Snapshot) -> Self {
        Self {
            account_id: s.account_id,
            revision: s.revision,
            connection: match s.connection {
                friends::Connection::Loading => EpicFriendsConnection::Loading,
                friends::Connection::Ready => EpicFriendsConnection::Ready,
                friends::Connection::Interrupted => EpicFriendsConnection::Interrupted,
                friends::Connection::Failed => EpicFriendsConnection::Failed,
                friends::Connection::Forbidden => EpicFriendsConnection::Forbidden,
                friends::Connection::AuthenticationRequired => {
                    EpicFriendsConnection::AuthenticationRequired
                }
                friends::Connection::Stopped => EpicFriendsConnection::Stopped,
            },
            message: s.message,
            friends: s
                .friends
                .into_iter()
                .map(|f| EpicFriend {
                    account_id: f.account_id,
                    display_name: f.display_name,
                    avatar_url: f.avatar_url,
                    presence: match f.presence {
                        friends::Presence::Unknown => EpicFriendPresence::Unknown,
                        friends::Presence::Offline => EpicFriendPresence::Offline,
                        friends::Presence::Online => EpicFriendPresence::Online,
                        friends::Presence::Away => EpicFriendPresence::Away,
                        friends::Presence::Busy => EpicFriendPresence::Busy,
                        friends::Presence::ExtendedAway => EpicFriendPresence::ExtendedAway,
                        friends::Presence::Invisible => EpicFriendPresence::Invisible,
                    },
                    activity: f.activity.map(|a| EpicFriendActivity {
                        id: a.id,
                        name: a.name,
                    }),
                })
                .collect(),
        }
    }
}
