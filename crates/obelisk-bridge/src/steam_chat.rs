//! Friends chat, Online presence and game invitations of the account's UI
//! session. Chat arrives as sequenced events rather than snapshots; a `gap`
//! tells Swift it fell behind and must reload the history it shows.
use std::sync::Arc;

use steam_kit::{chat, invites, own_game};

use crate::{SteamError, SteamSession, game_session::SteamReceivedInvite};

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamChatMessage {
    pub friend_steam_id: u64,
    /// Sent by this account, from Obelisk or another of its sessions.
    pub from_me: bool,
    pub entry_type: i32,
    /// BBCode, as Steam stores it.
    pub message: String,
    pub plain_message: String,
    pub timestamp: u32,
    pub ordinal: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamChatEvent {
    Message {
        message: SteamChatMessage,
    },
    Typing {
        friend_steam_id: u64,
    },
    /// The account read the conversation up to `timestamp` in another session.
    Read {
        friend_steam_id: u64,
        timestamp: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamChatEventEntry {
    pub sequence: u64,
    pub event: SteamChatEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamChatEvents {
    pub events: Vec<SteamChatEventEntry>,
    /// The cursor to pass to the next call.
    pub last_sequence: u64,
    pub gap: bool,
    pub connected: bool,
}

impl From<chat::ChatEvents> for SteamChatEvents {
    fn from(events: chat::ChatEvents) -> Self {
        Self {
            events: events
                .events
                .into_iter()
                .map(|entry| SteamChatEventEntry {
                    sequence: entry.sequence,
                    event: match entry.event {
                        chat::ChatEvent::Message(message) => SteamChatEvent::Message {
                            message: SteamChatMessage {
                                friend_steam_id: message.friend,
                                from_me: message.from_me,
                                entry_type: message.entry_type,
                                message: message.message,
                                plain_message: message.plain,
                                timestamp: message.timestamp,
                                ordinal: message.ordinal,
                            },
                        },
                        chat::ChatEvent::Typing { friend } => SteamChatEvent::Typing {
                            friend_steam_id: friend,
                        },
                        chat::ChatEvent::Read { friend, timestamp } => SteamChatEvent::Read {
                            friend_steam_id: friend,
                            timestamp,
                        },
                    },
                })
                .collect(),
            last_sequence: events.last_sequence,
            gap: events.gap,
            connected: events.connected,
        }
    }
}

/// A message's place in its conversation; Steam orders messages by both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamChatPosition {
    pub timestamp: u32,
    pub ordinal: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamChatHistoryMessage {
    pub from_me: bool,
    pub timestamp: u32,
    pub ordinal: u32,
    /// BBCode.
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamChatHistory {
    /// Oldest first.
    pub messages: Vec<SteamChatHistoryMessage>,
    pub more_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamChatSession {
    pub friend_steam_id: u64,
    pub last_message: u32,
    pub last_view: u32,
    pub unread_message_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamSentMessage {
    pub timestamp: u32,
    pub ordinal: u32,
    /// BBCode, as Steam stored it.
    pub message: String,
    pub plain_message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamGameInvite {
    None,
    Lobby { lobby_id: u64 },
    Connect { connect: String },
}

/// A game and how to join it: a friend's lobby or rich presence `connect`
/// string, as an invitation or Steam's "Join Game" names it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamJoinTarget {
    pub app_id: u32,
    /// Never `None`.
    pub invite: SteamGameInvite,
}

/// The invitations friends sent the account's session, newest first, at most
/// one per friend.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamReceivedInvites {
    pub revision: u64,
    pub connected: bool,
    pub invites: Vec<SteamReceivedInvite>,
}

impl From<invites::ReceivedInvites> for SteamReceivedInvites {
    fn from(received: invites::ReceivedInvites) -> Self {
        Self {
            revision: received.revision,
            connected: received.connected,
            invites: received
                .invites
                .into_iter()
                .map(|invite| SteamReceivedInvite {
                    friend_steam_id: invite.friend,
                    app_id: invite.app_id,
                    invite: invite.invite.into(),
                    received_at: invite.received_at,
                })
                .collect(),
        }
    }
}

/// The invitation a friends chat message carries, read as Steam's chat reads
/// its `[lobbyinvite]` and `[gameinvite]` tags; none for Remote Play.
#[uniffi::export]
pub fn steam_chat_invite(bbcode: String) -> Option<SteamJoinTarget> {
    invites::chat_invite(&bbcode).map(|(app_id, invite)| SteamJoinTarget {
        app_id,
        invite: invite.into(),
    })
}

/// The account's own game, and the invitation it can send right now.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SteamOwnGame {
    pub revision: u64,
    pub connected: bool,
    /// 0 when the account plays nothing.
    pub app_id: u32,
    pub invite: SteamGameInvite,
}

impl From<SteamGameInvite> for own_game::GameInvite {
    fn from(invite: SteamGameInvite) -> Self {
        match invite {
            SteamGameInvite::None => Self::None,
            SteamGameInvite::Lobby { lobby_id } => Self::Lobby { lobby_id },
            SteamGameInvite::Connect { connect } => Self::Connect { connect },
        }
    }
}

impl From<own_game::GameInvite> for SteamGameInvite {
    fn from(invite: own_game::GameInvite) -> Self {
        match invite {
            own_game::GameInvite::None => Self::None,
            own_game::GameInvite::Lobby { lobby_id } => Self::Lobby { lobby_id },
            own_game::GameInvite::Connect { connect } => Self::Connect { connect },
        }
    }
}

impl From<game_runtime::protocol::InviteTarget> for SteamGameInvite {
    fn from(target: game_runtime::protocol::InviteTarget) -> Self {
        match target {
            game_runtime::protocol::InviteTarget::Lobby { lobby_id } => Self::Lobby { lobby_id },
            game_runtime::protocol::InviteTarget::Connect { connect } => Self::Connect { connect },
        }
    }
}

/// `None` names nothing to join.
impl TryFrom<SteamGameInvite> for game_runtime::protocol::InviteTarget {
    type Error = ();

    fn try_from(invite: SteamGameInvite) -> Result<Self, ()> {
        match invite {
            SteamGameInvite::None => Err(()),
            SteamGameInvite::Lobby { lobby_id } => Ok(Self::Lobby { lobby_id }),
            SteamGameInvite::Connect { connect } => Ok(Self::Connect { connect }),
        }
    }
}

impl From<own_game::OwnGame> for SteamOwnGame {
    fn from(game: own_game::OwnGame) -> Self {
        Self {
            revision: game.revision,
            connected: game.connected,
            app_id: game.app_id,
            invite: game.invite().into(),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamSession {
    /// Shows the account Online again, for instance after a game ended.
    /// Shows the account as `status` says, through this connection and
    /// every later one; also again once a game's own Steam session changed
    /// it.
    pub async fn set_status(&self, status: crate::OnlineStatus) -> Result<(), SteamError> {
        Ok(self.account.set_status(status.into()).await?)
    }

    /// Suspends until chat events follow `after_sequence` or the session ends.
    pub async fn next_chat(&self, after_sequence: u64) -> SteamChatEvents {
        self.client().next_chat(after_sequence).await.into()
    }

    pub async fn send_message(
        &self,
        friend_steam_id: u64,
        text: String,
    ) -> Result<SteamSentMessage, SteamError> {
        let sent = self.client().send_message(friend_steam_id, &text).await?;
        Ok(SteamSentMessage {
            timestamp: sent.timestamp,
            ordinal: sent.ordinal,
            message: sent.message,
            plain_message: sent.plain,
        })
    }

    /// Throttled per friend in Rust; calling on every keystroke is fine.
    pub async fn send_typing(&self, friend_steam_id: u64) -> Result<(), SteamError> {
        Ok(self.client().send_typing(friend_steam_id).await?)
    }

    /// Up to `count` (at most 50) messages before `before`, or the latest ones.
    pub async fn recent_messages(
        &self,
        friend_steam_id: u64,
        before: Option<SteamChatPosition>,
        count: u32,
    ) -> Result<SteamChatHistory, SteamError> {
        let history = self
            .client()
            .recent_messages(
                friend_steam_id,
                before.map(|position| (position.timestamp, position.ordinal)),
                count,
            )
            .await?;
        Ok(SteamChatHistory {
            messages: history
                .messages
                .into_iter()
                .map(|message| SteamChatHistoryMessage {
                    from_me: message.from_me,
                    timestamp: message.timestamp,
                    ordinal: message.ordinal,
                    message: message.message,
                })
                .collect(),
            more_available: history.more_available,
        })
    }

    pub async fn active_sessions(&self) -> Result<Vec<SteamChatSession>, SteamError> {
        Ok(self
            .client()
            .active_sessions()
            .await?
            .into_iter()
            .map(|session| SteamChatSession {
                friend_steam_id: session.friend,
                last_message: session.last_message,
                last_view: session.last_view,
                unread_message_count: session.unread_message_count,
            })
            .collect())
    }

    /// Marks the conversation read up to `timestamp` in every session.
    pub async fn ack(&self, friend_steam_id: u64, timestamp: u32) -> Result<(), SteamError> {
        Ok(self.client().ack(friend_steam_id, timestamp).await?)
    }

    /// Suspends until the own game's revision differs or the session ends.
    pub async fn next_own_game(&self, after_revision: u64) -> SteamOwnGame {
        self.client().next_own_game(after_revision).await.into()
    }

    /// Suspends until the invitations friends sent change or the session ends.
    pub async fn next_invites(&self, after_revision: u64) -> SteamReceivedInvites {
        self.client().next_invites(after_revision).await.into()
    }

    /// Fails with `backend.steam.invite.unavailable` unless the account plays
    /// `app_id` and that game can take an invitation now.
    pub async fn invite_to_game(
        &self,
        friend_steam_id: u64,
        app_id: u32,
    ) -> Result<(), SteamError> {
        self.client()
            .invite_to_game(friend_steam_id, app_id)
            .await?;
        Ok(())
    }

    /// Invites into `invite`, which the account's running game of `app_id`
    /// reported itself. Fails with `backend.steam.invite.unavailable` for an
    /// invitation Steam cannot carry or while the account plays another game.
    pub async fn invite_into_game(
        &self,
        friend_steam_id: u64,
        app_id: u32,
        invite: SteamGameInvite,
    ) -> Result<(), SteamError> {
        self.client()
            .invite_into_game(friend_steam_id, app_id, &invite.into())
            .await?;
        Ok(())
    }
}

/// Debug-only controlled chat and own-game sources. They use the production
/// decoders and observation, letting macOS tests drive both streams.
#[cfg(debug_assertions)]
#[derive(uniffi::Object)]
pub struct SteamChatFixture {
    account_id: u64,
    chat: chat::ChatEventSource,
    own_game: own_game::OwnGameSource,
    invites: invites::InviteSource,
}

#[cfg(debug_assertions)]
#[uniffi::export(async_runtime = "tokio")]
impl SteamChatFixture {
    #[uniffi::constructor]
    pub fn new(account_id: u64) -> Arc<Self> {
        Arc::new(Self {
            account_id,
            chat: chat::ChatEventSource::new(),
            own_game: own_game::OwnGameSource::new(),
            invites: invites::InviteSource::new(),
        })
    }

    /// A friend's chat message, as Steam writes it for an invitation too.
    pub fn incoming_invitation(
        &self,
        friend_steam_id: u64,
        message: String,
        timestamp: u32,
        ordinal: u32,
    ) {
        let body = chat::fixture::incoming_message(
            friend_steam_id,
            chat::CHAT_MESSAGE,
            &message,
            timestamp,
            ordinal,
            false,
        );
        self.chat.receive(chat::INCOMING_MESSAGE, &body);
        self.invites
            .receive_notification(chat::INCOMING_MESSAGE, &body);
    }

    /// A lobby invitation, as Steam sends it apart from the chat.
    pub fn lobby_invite(&self, friend_steam_id: u64, lobby_id: u64, app_id: u32) {
        self.invites.receive_packet(
            invites::CHAT_INVITE,
            None,
            &invites::fixture::lobby_invite(friend_steam_id, lobby_id, app_id),
        );
    }

    /// A connect invitation into `app_id`, its header's routing AppID.
    pub fn connect_invite(&self, friend_steam_id: u64, app_id: u32, connect: String) {
        self.invites.receive_packet(
            invites::INVITE_TO_GAME,
            Some(app_id),
            &invites::fixture::connect_invite(friend_steam_id, &connect),
        );
    }

    pub fn incoming_message(
        &self,
        friend_steam_id: u64,
        message: String,
        timestamp: u32,
        ordinal: u32,
        from_me: bool,
    ) {
        self.chat.receive(
            chat::INCOMING_MESSAGE,
            &chat::fixture::incoming_message(
                friend_steam_id,
                chat::CHAT_MESSAGE,
                &message,
                timestamp,
                ordinal,
                from_me,
            ),
        );
    }

    pub fn typing(&self, friend_steam_id: u64) {
        self.chat.receive(
            chat::INCOMING_MESSAGE,
            &chat::fixture::incoming_message(friend_steam_id, chat::TYPING, "", 0, 0, false),
        );
    }

    pub fn read_elsewhere(&self, friend_steam_id: u64, timestamp: u32) {
        self.chat.receive(
            chat::ACK_ECHO,
            &chat::fixture::ack_echo(friend_steam_id, timestamp),
        );
    }

    /// The account playing `app_id` (0 for none), with an optional lobby and
    /// rich presence `connect` string.
    pub fn own_game(&self, app_id: u32, lobby_id: u64, connect: Option<String>) {
        self.own_game.receive(
            self.account_id,
            &own_game::fixture::persona(self.account_id, app_id, lobby_id, connect.as_deref()),
        );
    }

    pub fn disconnect(&self) {
        self.chat.disconnect();
        self.own_game.disconnect();
        self.invites.disconnect();
    }

    pub async fn next_invites(&self, after_revision: u64) -> SteamReceivedInvites {
        self.invites.next(after_revision).await.into()
    }

    pub async fn next_chat(&self, after_sequence: u64) -> SteamChatEvents {
        self.chat.next(after_sequence).await.into()
    }

    pub async fn next_own_game(&self, after_revision: u64) -> SteamOwnGame {
        self.own_game.next(after_revision).await.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: u64 = 76_561_197_960_265_730;
    const FRIEND: u64 = ACCOUNT - 1;

    #[tokio::test]
    async fn fixture_drives_chat_and_own_game_across_the_bridge() {
        let fixture = SteamChatFixture::new(ACCOUNT);
        fixture.incoming_message(FRIEND, "hi".into(), 10, 1, false);
        fixture.typing(FRIEND);
        fixture.read_elsewhere(FRIEND, 10);
        let events = fixture.next_chat(0).await;
        assert_eq!(events.last_sequence, 3);
        assert!(!events.gap && events.connected);
        assert_eq!(
            events.events[0].event,
            SteamChatEvent::Message {
                message: SteamChatMessage {
                    friend_steam_id: FRIEND,
                    from_me: false,
                    entry_type: 1,
                    message: "hi".into(),
                    plain_message: "hi".into(),
                    timestamp: 10,
                    ordinal: 1,
                }
            }
        );
        assert_eq!(
            events.events[2].event,
            SteamChatEvent::Read {
                friend_steam_id: FRIEND,
                timestamp: 10
            }
        );

        fixture.own_game(620, 0, Some("+join".into()));
        let game = fixture.next_own_game(1).await;
        assert_eq!(game.app_id, 620);
        assert_eq!(
            game.invite,
            SteamGameInvite::Connect {
                connect: "+join".into()
            }
        );
        fixture.disconnect();
        assert!(!fixture.next_chat(3).await.connected);
        assert!(!fixture.next_own_game(game.revision).await.connected);
    }

    #[tokio::test]
    async fn invitations_cross_the_bridge_once_whatever_their_channel() {
        const LOBBY: u64 = 109_775_244_421_126_053;
        let tag = format!("[lobbyinvite appid=\"3527290\" lobbyid=\"{LOBBY}\"][/lobbyinvite]");
        let target = SteamJoinTarget {
            app_id: 3_527_290,
            invite: SteamGameInvite::Lobby { lobby_id: LOBBY },
        };
        assert_eq!(steam_chat_invite(tag.clone()), Some(target.clone()));
        assert_eq!(steam_chat_invite("hi".into()), None);

        let fixture = SteamChatFixture::new(ACCOUNT);
        fixture.incoming_invitation(FRIEND, tag, 10, 1);
        fixture.lobby_invite(FRIEND, LOBBY, 3_527_290);
        let invites = fixture.next_invites(1).await;
        assert_eq!(invites.revision, 2);
        assert_eq!(invites.invites.len(), 1);
        assert_eq!(invites.invites[0].friend_steam_id, FRIEND);
        assert_eq!(invites.invites[0].app_id, target.app_id);
        assert_eq!(invites.invites[0].invite, target.invite);

        fixture.connect_invite(ACCOUNT + 1, 620, "+connect 1".into());
        let invites = fixture.next_invites(2).await;
        assert_eq!(invites.invites[0].app_id, 620);
        fixture.disconnect();
        assert!(!fixture.next_invites(invites.revision).await.connected);
    }
}
