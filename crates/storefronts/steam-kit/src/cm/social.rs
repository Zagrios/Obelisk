//! Presence, friends chat and game invitations for the account's UI session.
//! Wire fields follow steammessages_friendmessages.steamclient.proto and the
//! clientserver friends, MMS and base protobufs; EMsgs follow SteamKit's emsg.steamd.
use rsa::rand_core::{OsRng, RngCore};

use super::{CmError, SERVICE_REQUEST, SteamConnection, chat, invites, own_game, proto};

const CHANGE_STATUS: u32 = 716;
const MMS_INVITE_TO_LOBBY: u32 = 6621;
/// EResult values a chat request reports to the person (SteamKit eresult.steamd).
const ACCESS_DENIED: i32 = 15;
const RATE_LIMIT_EXCEEDED: i32 = 84;
/// Steam's history pages; Valve's web chat loads the same amount.
pub(crate) const HISTORY_PAGE: u32 = 50;
/// The SteamID of every public individual account, without its account ID.
const INDIVIDUAL: u64 = 0x0110_0001_0000_0000;

/// Chat and invitation failures the person can act on, apart from transport.
#[derive(Debug, thiserror::Error)]
pub enum SocialError {
    #[error(transparent)]
    Cm(#[from] CmError),
    #[error("backend.steam.chat.rateLimited")]
    RateLimited,
    #[error("backend.steam.chat.notFriends")]
    NotFriends,
    #[error("backend.steam.chat.emptyMessage")]
    EmptyMessage,
    #[error("backend.steam.chat.invalidRecipient")]
    InvalidRecipient,
    #[error("backend.steam.invite.unavailable")]
    InviteUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentMessage {
    pub timestamp: u32,
    pub ordinal: u32,
    /// BBCode, as Steam stored it.
    pub message: String,
    pub plain: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryMessage {
    pub from_me: bool,
    pub timestamp: u32,
    pub ordinal: u32,
    /// BBCode.
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    /// Oldest first.
    pub messages: Vec<HistoryMessage>,
    pub more_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageSession {
    pub friend: u64,
    pub last_message: u32,
    pub last_view: u32,
    pub unread_message_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameInviteKind {
    Lobby,
    Connect,
}

impl SteamConnection {
    pub(crate) fn chat_source(&self) -> chat::ChatEventSource {
        self.social.chat.clone()
    }

    pub(crate) fn own_game_source(&self) -> own_game::OwnGameSource {
        self.social.own_game.clone()
    }

    pub(crate) fn invite_source(&self) -> invites::InviteSource {
        self.social.invites.clone()
    }

    /// Shows the account as `status` says. Steam keeps the status for the
    /// session, so a caller sends it again after anything else changed it,
    /// such as a game.
    ///
    /// Sent as Steam's own status menu sends it (steamui `FriendStore`
    /// `SetUserPersonaState` → `SendPersonaStateToServer`): chosen by the
    /// player, not idle. Steam's client sends an Away the player did not
    /// choose as idleness only. Steam's own friends list shows an Away
    /// friend as Online, whichever client set it.
    pub(crate) async fn set_status(
        &self,
        status: obelisk_core::OnlineStatus,
    ) -> Result<(), CmError> {
        self.send_unjobbed(
            CHANGE_STATUS,
            &proto::ChangeStatus {
                persona_state: Some(crate::friends::persona_state(status).into()),
                persona_set_by_user: Some(true),
                need_persona_response: Some(false),
                is_client_idle: Some(false),
            },
            None,
            None,
        )
        .await
    }

    /// Sends text as Valve's web chat does, with BBCode enabled; literal
    /// brackets are escaped so the text cannot become markup.
    pub(crate) async fn send_friend_message(
        &self,
        friend: u64,
        text: &str,
    ) -> Result<SentMessage, SocialError> {
        if text.trim().is_empty() {
            return Err(SocialError::EmptyMessage);
        }
        let message = text.replace('[', "\\[");
        let response: proto::SendMessageResponse = self
            .chat_service(
                "FriendMessages.SendMessage#1",
                &self.chat_entry(friend, chat::CHAT_MESSAGE, message.clone())?,
            )
            .await?;
        Ok(SentMessage {
            timestamp: response.server_timestamp,
            ordinal: response.ordinal,
            message: if response.modified_message.is_empty() {
                message
            } else {
                response.modified_message
            },
            plain: if response.message_without_bb_code.is_empty() {
                text.to_owned()
            } else {
                response.message_without_bb_code
            },
        })
    }

    pub(crate) async fn send_typing(&self, friend: u64) -> Result<(), SocialError> {
        let _: proto::SendMessageResponse = self
            .chat_service(
                "FriendMessages.SendMessage#1",
                &self.chat_entry(friend, chat::TYPING, String::new())?,
            )
            .await?;
        Ok(())
    }

    fn chat_entry(
        &self,
        friend: u64,
        entry_type: i32,
        message: String,
    ) -> Result<proto::SendMessageRequest, SocialError> {
        self.check_friend(friend)?;
        let mut id = [0; 8];
        OsRng
            .try_fill_bytes(&mut id)
            .map_err(|_| CmError::Protocol("backend.steam.cm.invalidSession".into()))?;
        Ok(proto::SendMessageRequest {
            steamid: friend,
            chat_entry_type: entry_type,
            message,
            contains_bbcode: true,
            echo_to_sender: false,
            client_message_id: crate::content::hex(&id),
        })
    }

    /// Messages before `before` (timestamp, ordinal), or the latest ones.
    pub(crate) async fn recent_messages(
        &self,
        friend: u64,
        before: Option<(u32, u32)>,
        count: u32,
    ) -> Result<History, SocialError> {
        self.check_friend(friend)?;
        // Valve's web chat bounds the latest page by the end of RTime32 time.
        let (time_last, ordinal_last) = before.unwrap_or((i32::MAX as u32, 0));
        let response: proto::RecentMessagesResponse = self
            .chat_service(
                "FriendMessages.GetRecentMessages#1",
                &proto::RecentMessagesRequest {
                    steamid1: self.steam_id,
                    steamid2: friend,
                    count: count.clamp(1, HISTORY_PAGE),
                    bbcode_format: true,
                    time_last,
                    ordinal_last,
                },
            )
            .await?;
        let me = self.steam_id as u32;
        let mut messages: Vec<_> = response
            .messages
            .into_iter()
            // The page may repeat its boundary message.
            .filter(|message| before != Some((message.timestamp, message.ordinal)))
            .map(|message| HistoryMessage {
                from_me: message.accountid == me,
                timestamp: message.timestamp,
                ordinal: message.ordinal,
                message: message.message,
            })
            .collect();
        messages.sort_by_key(|message| (message.timestamp, message.ordinal));
        Ok(History {
            messages,
            more_available: response.more_available,
        })
    }

    pub(crate) async fn active_sessions(&self) -> Result<Vec<MessageSession>, SocialError> {
        let response: proto::ActiveSessionsResponse = self
            .chat_service(
                "FriendMessages.GetActiveMessageSessions#1",
                &proto::ActiveSessionsRequest {
                    lastmessage_since: 0,
                    only_sessions_with_messages: true,
                },
            )
            .await?;
        Ok(response
            .message_sessions
            .into_iter()
            .filter(|session| session.accountid_friend != 0)
            .map(|session| MessageSession {
                friend: INDIVIDUAL | u64::from(session.accountid_friend),
                last_message: session.last_message,
                last_view: session.last_view,
                unread_message_count: session.unread_message_count,
            })
            .collect())
    }

    /// Marks the conversation read up to `timestamp`, for every session.
    pub(crate) async fn ack(&self, friend: u64, timestamp: u32) -> Result<(), SocialError> {
        self.check_friend(friend)?;
        self.send_unjobbed(
            SERVICE_REQUEST,
            &proto::AckMessage {
                steamid_partner: friend,
                timestamp,
            },
            Some("FriendMessages.AckMessage#1"),
            None,
        )
        .await
        .map_err(Into::into)
    }

    /// Steam answers neither invitation; the friend's client receives it.
    pub(crate) async fn invite(
        &self,
        friend: u64,
        app_id: u32,
        invite: &own_game::GameInvite,
    ) -> Result<GameInviteKind, SocialError> {
        self.check_friend(friend)?;
        match invite {
            own_game::GameInvite::Lobby { lobby_id } => {
                self.send_unjobbed(
                    MMS_INVITE_TO_LOBBY,
                    &proto::InviteToLobby {
                        steam_id_lobby: *lobby_id,
                        steam_id_user_invited: friend,
                    },
                    None,
                    Some(app_id),
                )
                .await?;
                Ok(GameInviteKind::Lobby)
            }
            own_game::GameInvite::Connect { connect } => {
                self.send_unjobbed(
                    invites::INVITE_TO_GAME,
                    &proto::InviteToGame {
                        steam_id_dest: friend,
                        steam_id_src: self.steam_id,
                        connect_string: connect.clone(),
                        remote_play: String::new(),
                    },
                    None,
                    Some(app_id),
                )
                .await?;
                Ok(GameInviteKind::Connect)
            }
            own_game::GameInvite::None => Err(SocialError::InviteUnavailable),
        }
    }

    fn check_friend(&self, friend: u64) -> Result<(), SocialError> {
        if crate::friends::individual(friend, self.steam_id) {
            Ok(())
        } else {
            Err(SocialError::InvalidRecipient)
        }
    }

    async fn chat_service<R: prost::Message + Default>(
        &self,
        name: &str,
        request: &impl prost::Message,
    ) -> Result<R, SocialError> {
        self.service_for(name, request, "backend.steam.cm.action.chat")
            .await
            .map_err(|error| match error {
                CmError::Rejected {
                    result: RATE_LIMIT_EXCEEDED,
                    ..
                } => SocialError::RateLimited,
                // Valve's web chat shows this result as "not friends".
                CmError::Rejected {
                    result: ACCESS_DENIED,
                    ..
                } => SocialError::NotFriends,
                error => error.into(),
            })
    }
}
