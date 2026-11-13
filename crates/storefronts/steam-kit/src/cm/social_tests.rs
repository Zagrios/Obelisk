use super::*;
use crate::client::{Client, GameInviteKind};
use obelisk_core::OnlineStatus;

const ACCOUNT: u64 = 76_561_198_000_000_000;
const FRIEND: u64 = ACCOUNT + 1;

/// A logged-on UI session whose worker knows its account, as after a logon.
async fn session() -> (Client, test_server::TestServer) {
    let (connection, server) = test_server::connection(ACCOUNT, true).await;
    connection
        .outgoing
        .send(Command::Session {
            steam_id: ACCOUNT,
            session_id: 1,
            heartbeat_seconds: 15,
        })
        .await
        .unwrap();
    (Client::with_test_connection(connection), server)
}

fn fixed64(tag: u8, value: u64) -> Vec<u8> {
    let mut bytes = vec![tag];
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes
}

fn own_persona(app_id: u32, lobby_id: u64, connect: Option<&str>) -> Vec<u8> {
    crate::own_game::fixture::persona(ACCOUNT, app_id, lobby_id, connect)
}

// Decodes only CMsgClientLogon.chat_mode (33), independently of proto::Logon.
#[derive(Clone, PartialEq, prost::Message)]
struct WireChatMode {
    #[prost(uint32, optional, tag = "33")]
    chat_mode: Option<u32>,
}

async fn logon_body(chat: ChatMode) -> Bytes {
    let (mut connection, mut server) = test_server::connection(0, false).await;
    let steam = async {
        let logon = server.request(LOGON).await;
        server
            .send(
                LOGON_RESPONSE,
                proto::Header {
                    steam_id: Some(ACCOUNT),
                    session_id: Some(7),
                    ..Default::default()
                },
                &proto::LogonResponse {
                    result: Some(1),
                    ..Default::default()
                }
                .encode_to_vec(),
            )
            .await;
        logon.body
    };
    let (logged_on, body) = tokio::join!(
        connection.log_on("fixture", ACCOUNT, "fixture-token", chat),
        steam
    );
    logged_on.unwrap();
    body
}

#[tokio::test]
async fn only_the_ui_session_asks_for_the_friends_chat() {
    let ui = logon_body(ChatMode::Enabled).await;
    // Tag 33, varint: key 0x88 0x02, value 2.
    assert!(ui.windows(3).any(|bytes| bytes == [0x88, 0x02, 0x02]));
    assert_eq!(WireChatMode::decode(&ui[..]).unwrap().chat_mode, Some(2));
    let cloud = logon_body(ChatMode::Disabled).await;
    assert_eq!(WireChatMode::decode(&cloud[..]).unwrap().chat_mode, None);
}

#[test]
fn header_routing_app_is_tag_3() {
    let header = proto::Header {
        routing_appid: Some(620),
        ..Default::default()
    };
    assert_eq!(header.encode_to_vec(), [0x18, 0xec, 0x04]);
    assert_eq!(
        proto::Header::decode(&[0x18, 0xec, 0x04][..])
            .unwrap()
            .routing_appid,
        Some(620)
    );
}

#[tokio::test]
async fn the_chosen_status_is_sent_without_a_job() {
    let (client, mut server) = session().await;
    for (status, persona) in [
        (OnlineStatus::Online, 1),
        (OnlineStatus::Away, 3),
        (OnlineStatus::Invisible, 7),
    ] {
        client.set_status(status).await.unwrap();
        let sent = server.request(716).await;
        // persona_state, then as Steam's menu: set by the user (5), no
        // response asked (7), not idle (8).
        assert_eq!(
            &sent.body[..],
            [0x08, persona, 0x28, 0x01, 0x38, 0x00, 0x40, 0x00]
        );
        assert_eq!(sent.header.job_source, None);
        assert_eq!(sent.header.steam_id, Some(ACCOUNT));
    }
}

#[tokio::test]
async fn messages_are_sent_as_valves_web_chat_sends_them() {
    let (client, mut server) = session().await;
    let sending = tokio::spawn(async move {
        let sent = client.send_message(FRIEND, "a[b").await;
        (client, sent)
    });
    let request = server.request(SERVICE_REQUEST).await;
    assert_eq!(
        request.header.target_name.as_deref(),
        Some("FriendMessages.SendMessage#1")
    );
    assert!(request.header.job_source.is_some());
    // steamid 1, chat_entry_type 2 = 1, message 3 escaped, contains_bbcode 4,
    // no echo_to_sender 5, client_message_id 8 of 16 hex characters.
    let mut expected = fixed64(0x09, FRIEND);
    expected.extend_from_slice(&[0x10, 0x01, 0x1a, 0x04, b'a', b'\\', b'[', b'b', 0x20, 0x01]);
    expected.extend_from_slice(&[0x42, 0x10]);
    assert_eq!(&request.body[..expected.len()], expected);
    let id = &request.body[expected.len()..];
    assert_eq!(id.len(), 16);
    assert!(id.iter().all(u8::is_ascii_hexdigit));
    server
        .reply(
            &request,
            SERVICE_RESPONSE,
            &proto::SendMessageResponse {
                modified_message: "a\\[b".into(),
                server_timestamp: 1_700_000_000,
                ordinal: 2,
                message_without_bb_code: "a[b".into(),
            },
        )
        .await;
    let (client, sent) = sending.await.unwrap();
    assert_eq!(
        sent.unwrap(),
        SentMessage {
            timestamp: 1_700_000_000,
            ordinal: 2,
            message: "a\\[b".into(),
            plain: "a[b".into(),
        }
    );
    assert_eq!(
        client
            .send_message(FRIEND, "  ")
            .await
            .unwrap_err()
            .to_string(),
        "backend.steam.chat.emptyMessage"
    );
    assert_eq!(
        client
            .send_message(ACCOUNT, "me")
            .await
            .unwrap_err()
            .to_string(),
        "backend.steam.chat.invalidRecipient"
    );
}

#[tokio::test]
async fn rate_limits_and_strangers_are_distinct_errors() {
    for (result, key) in [
        (84, "backend.steam.chat.rateLimited"),
        (15, "backend.steam.chat.notFriends"),
    ] {
        let (client, mut server) = session().await;
        let sending = tokio::spawn(async move { client.send_message(FRIEND, "hi").await });
        let request = server.request(SERVICE_REQUEST).await;
        server
            .send(
                SERVICE_RESPONSE,
                proto::Header {
                    job_target: request.header.job_source,
                    result: Some(result),
                    ..Default::default()
                },
                &[],
            )
            .await;
        assert_eq!(sending.await.unwrap().unwrap_err().to_string(), key);
    }
}

#[tokio::test]
async fn typing_is_throttled_per_friend() {
    let (client, mut server) = session().await;
    let client = Arc::new(client);
    let typing = |friend| {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.send_typing(friend).await })
    };
    for friend in [FRIEND, FRIEND + 1] {
        let sending = typing(friend);
        let request = server.request(SERVICE_REQUEST).await;
        let body = proto::SendMessageRequest::decode(&request.body[..]).unwrap();
        assert_eq!(
            (body.steamid, body.chat_entry_type, body.message.as_str()),
            (friend, 2, "")
        );
        server
            .reply(
                &request,
                SERVICE_RESPONSE,
                &proto::SendMessageResponse::default(),
            )
            .await;
        sending.await.unwrap().unwrap();
    }
    // Within the interval, typing again tells the friend nothing new.
    typing(FRIEND).await.unwrap().unwrap();
    assert!(server.quiet().await);
}

#[tokio::test]
async fn history_pages_backwards_oldest_first() {
    let (client, mut server) = session().await;
    let loading =
        tokio::spawn(async move { client.recent_messages(FRIEND, Some((100, 2)), 500).await });
    let request = server.request(SERVICE_REQUEST).await;
    assert_eq!(
        request.header.target_name.as_deref(),
        Some("FriendMessages.GetRecentMessages#1")
    );
    // steamid1 1 = self, steamid2 2 = friend, count 3 = 50, bbcode_format 6,
    // time_last 8, ordinal_last 9.
    let mut expected = fixed64(0x09, ACCOUNT);
    expected.extend(fixed64(0x11, FRIEND));
    expected.extend_from_slice(&[0x18, 50, 0x30, 0x01, 0x40, 100, 0x48, 0x02]);
    assert_eq!(&request.body[..], expected);
    let message = |accountid: u64, timestamp, ordinal| proto::FriendMessage {
        accountid: accountid as u32,
        timestamp,
        message: format!("{timestamp}.{ordinal}"),
        ordinal,
    };
    server
        .reply(
            &request,
            SERVICE_RESPONSE,
            &proto::RecentMessagesResponse {
                // Newest first, repeating the boundary.
                messages: vec![
                    message(ACCOUNT, 100, 2),
                    message(FRIEND, 100, 1),
                    message(ACCOUNT, 90, 0),
                ],
                more_available: true,
            },
        )
        .await;
    let history = loading.await.unwrap().unwrap();
    assert!(history.more_available);
    assert_eq!(
        history.messages,
        vec![
            HistoryMessage {
                from_me: true,
                timestamp: 90,
                ordinal: 0,
                message: "90.0".into()
            },
            HistoryMessage {
                from_me: false,
                timestamp: 100,
                ordinal: 1,
                message: "100.1".into()
            },
        ]
    );
}

#[tokio::test]
async fn the_latest_page_is_bounded_by_the_end_of_time() {
    let (client, mut server) = session().await;
    let loading = tokio::spawn(async move { client.recent_messages(FRIEND, None, 20).await });
    let request = server.request(SERVICE_REQUEST).await;
    // time_last 8 = 2^31 - 1, ordinal_last 9 omitted.
    let mut expected = fixed64(0x09, ACCOUNT);
    expected.extend(fixed64(0x11, FRIEND));
    expected.extend_from_slice(&[0x18, 20, 0x30, 0x01, 0x40, 0xff, 0xff, 0xff, 0xff, 0x07]);
    assert_eq!(&request.body[..], expected);
    server
        .reply(
            &request,
            SERVICE_RESPONSE,
            &proto::RecentMessagesResponse {
                messages: vec![],
                more_available: false,
            },
        )
        .await;
    let history = loading.await.unwrap().unwrap();
    assert!(history.messages.is_empty());
}

#[tokio::test]
async fn active_sessions_name_friends_by_steam_id() {
    let (client, mut server) = session().await;
    let loading = tokio::spawn(async move { client.active_sessions().await });
    let request = server.request(SERVICE_REQUEST).await;
    assert_eq!(&request.body[..], [0x10, 0x01]);
    server
        .reply(
            &request,
            SERVICE_RESPONSE,
            &proto::ActiveSessionsResponse {
                message_sessions: vec![proto::MessageSession {
                    accountid_friend: FRIEND as u32,
                    last_message: 10,
                    last_view: 5,
                    unread_message_count: 3,
                }],
            },
        )
        .await;
    assert_eq!(
        loading.await.unwrap().unwrap(),
        vec![MessageSession {
            friend: FRIEND,
            last_message: 10,
            last_view: 5,
            unread_message_count: 3
        }]
    );
}

#[tokio::test]
async fn reading_is_acknowledged_as_a_notification_without_a_job() {
    let (client, mut server) = session().await;
    client.ack(FRIEND, 300).await.unwrap();
    let ack = server.request(SERVICE_REQUEST).await;
    assert_eq!(ack.header.job_source, None);
    assert_eq!(
        ack.header.target_name.as_deref(),
        Some("FriendMessages.AckMessage#1")
    );
    let mut expected = fixed64(0x09, FRIEND);
    expected.extend_from_slice(&[0x10, 0xac, 0x02]);
    assert_eq!(&ack.body[..], expected);
}

#[tokio::test]
async fn notifications_reach_the_chat_stream_until_the_session_ends() {
    let (client, mut server) = session().await;
    let client = Arc::new(client);
    let waiting = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.next_chat(0).await }
    });
    server
        .send(
            146,
            proto::Header {
                target_name: Some(crate::chat::INCOMING_MESSAGE.into()),
                ..Default::default()
            },
            &crate::chat::fixture::incoming_message(FRIEND, 1, "hello", 1_700_000_000, 4, false),
        )
        .await;
    let events = waiting.await.unwrap();
    assert_eq!(events.last_sequence, 1);
    assert!(matches!(
        &events.events[0].event,
        crate::chat::ChatEvent::Message(message)
            if message.friend == FRIEND && message.message == "hello" && !message.from_me
    ));
    let chat = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.next_chat(1).await }
    });
    let game = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.next_own_game(1).await }
    });
    server
        .send(
            LOGGED_OFF,
            proto::Header::default(),
            &proto::LoggedOff { result: Some(6) }.encode_to_vec(),
        )
        .await;
    assert!(!chat.await.unwrap().connected);
    assert!(!game.await.unwrap().connected);
}

#[tokio::test]
async fn invitations_reach_the_invite_stream_from_every_channel() {
    const LOBBY: u64 = 109_775_244_421_126_053;
    let (client, mut server) = session().await;
    let client = Arc::new(client);
    let waiting = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.next_invites(1).await }
    });
    server
        .send(
            crate::invites::CHAT_INVITE,
            proto::Header::default(),
            &crate::invites::fixture::lobby_invite(FRIEND, LOBBY, 3_527_290),
        )
        .await;
    let invites = waiting.await.unwrap();
    assert_eq!(invites.invites.len(), 1);
    assert_eq!(invites.invites[0].app_id, 3_527_290);
    // Steam's chat copy of the same invitation adds nothing.
    server
        .send(
            146,
            proto::Header {
                target_name: Some(crate::chat::INCOMING_MESSAGE.into()),
                ..Default::default()
            },
            &crate::chat::fixture::incoming_message(
                FRIEND,
                1,
                &format!("[lobbyinvite appid=\"3527290\" lobbyid=\"{LOBBY}\"][/lobbyinvite]"),
                1_700_000_000,
                5,
                false,
            ),
        )
        .await;
    let waiting = tokio::spawn({
        let client = Arc::clone(&client);
        let revision = invites.revision;
        async move { client.next_invites(revision).await }
    });
    // A connect invitation names its game in the header only.
    server
        .send(
            crate::invites::INVITE_TO_GAME,
            proto::Header {
                routing_appid: Some(620),
                ..Default::default()
            },
            &crate::invites::fixture::connect_invite(FRIEND + 1, "+connect 10.0.0.1"),
        )
        .await;
    let invites = waiting.await.unwrap();
    assert_eq!(invites.invites.len(), 2);
    assert_eq!(invites.invites[0].friend, FRIEND + 1);
    assert_eq!(invites.invites[0].app_id, 620);
}

/// Invites the friend into app 620. The caller keeps the session alive, since
/// dropping it would abort the worker before the invitation is written.
fn invite(client: &Arc<Client>) -> tokio::task::JoinHandle<Result<GameInviteKind, String>> {
    let client = Arc::clone(client);
    tokio::spawn(async move {
        client
            .invite_to_game(FRIEND, 620)
            .await
            .map_err(|error| error.to_string())
    })
}

/// The account's own game, as Steam pushes it: an invitation never asks,
/// since asking makes the game drop out for every friend.
async fn push_own_game(client: &Client, server: &mut test_server::TestServer, persona: Vec<u8>) {
    server
        .send(PERSONA_STATE, proto::Header::default(), &persona)
        .await;
    client.next_own_game(1).await;
}

#[tokio::test]
async fn a_lobby_invitation_is_routed_to_the_game() {
    let (client, mut server) = session().await;
    let client = Arc::new(client);
    const LOBBY: u64 = 0x0186_0000_0000_0042;
    push_own_game(
        &client,
        &mut server,
        own_persona(620, LOBBY, Some("+connect")),
    )
    .await;
    let inviting = invite(&client);
    let invite = server.request(6621).await;
    assert_eq!(invite.header.routing_appid, Some(620));
    assert_eq!(invite.header.job_source, None);
    // app_id 1 stays empty; steam_id_lobby 2, steam_id_user_invited 3.
    let mut expected = fixed64(0x11, LOBBY);
    expected.extend(fixed64(0x19, FRIEND));
    assert_eq!(&invite.body[..], expected);
    assert_eq!(inviting.await.unwrap().unwrap(), GameInviteKind::Lobby);
}

#[tokio::test]
async fn a_connect_invitation_names_both_accounts() {
    let (client, mut server) = session().await;
    let client = Arc::new(client);
    push_own_game(&client, &mut server, own_persona(620, 0, Some("+join 7"))).await;
    let inviting = invite(&client);
    let invite = server.request(7005).await;
    assert_eq!(invite.header.routing_appid, Some(620));
    assert_eq!(invite.header.job_source, None);
    let mut expected = fixed64(0x09, FRIEND);
    expected.extend(fixed64(0x11, ACCOUNT));
    expected.extend_from_slice(&[0x1a, 0x07]);
    expected.extend_from_slice(b"+join 7");
    assert_eq!(&invite.body[..], expected);
    assert_eq!(inviting.await.unwrap().unwrap(), GameInviteKind::Connect);
}

#[tokio::test]
async fn no_invitation_leaves_when_the_game_cannot_take_one() {
    for persona in [
        own_persona(730, 9, None),
        own_persona(620, 0, None),
        own_persona(0, 9, Some("+join")),
    ] {
        let (client, mut server) = session().await;
        let client = Arc::new(client);
        push_own_game(&client, &mut server, persona).await;
        let inviting = invite(&client);
        assert_eq!(
            inviting.await.unwrap().unwrap_err(),
            "backend.steam.invite.unavailable"
        );
        assert!(server.quiet().await);
    }
}

/// A lobby the game reported itself: no refresh, no wait, no persona needed.
#[tokio::test]
async fn a_lobby_the_game_reported_is_invited_into_without_a_persona() {
    let (client, mut server) = session().await;
    const LOBBY: u64 = 109_775_244_421_126_053;
    let kind = client
        .invite_into_game(
            FRIEND,
            3_527_290,
            &own_game::GameInvite::Lobby { lobby_id: LOBBY },
        )
        .await
        .unwrap();
    assert_eq!(kind, GameInviteKind::Lobby);
    let invite = server.request(6621).await;
    assert_eq!(invite.header.routing_appid, Some(3_527_290));
    assert_eq!(invite.header.job_source, None);
    let mut expected = fixed64(0x11, LOBBY);
    expected.extend(fixed64(0x19, FRIEND));
    assert_eq!(&invite.body[..], expected);

    client
        .invite_into_game(
            FRIEND,
            3_527_290,
            &own_game::GameInvite::Connect {
                connect: "+connect_lobby 1".into(),
            },
        )
        .await
        .unwrap();
    let invite = server.request(7005).await;
    assert_eq!(invite.header.routing_appid, Some(3_527_290));
    let mut expected = fixed64(0x09, FRIEND);
    expected.extend(fixed64(0x11, ACCOUNT));
    expected.extend_from_slice(&[0x1a, 0x10]);
    expected.extend_from_slice(b"+connect_lobby 1");
    assert_eq!(&invite.body[..], expected);
}

#[tokio::test]
async fn a_reported_invitation_is_refused_for_another_game_or_target() {
    let (client, mut server) = session().await;
    const LOBBY: u64 = 109_775_244_421_126_053;
    let refused = |app_id, invite| {
        let client = &client;
        async move {
            client
                .invite_into_game(FRIEND, app_id, &invite)
                .await
                .unwrap_err()
                .to_string()
        }
    };
    let unavailable = "backend.steam.invite.unavailable";
    assert_eq!(
        refused(0, own_game::GameInvite::Lobby { lobby_id: LOBBY }).await,
        unavailable
    );
    assert_eq!(refused(620, own_game::GameInvite::None).await, unavailable);
    // An account is no lobby.
    assert_eq!(
        refused(620, own_game::GameInvite::Lobby { lobby_id: FRIEND }).await,
        unavailable
    );
    assert_eq!(
        refused(
            620,
            own_game::GameInvite::Connect {
                connect: "x".repeat(256)
            }
        )
        .await,
        unavailable
    );
    // The persona shows the account in another game.
    server
        .send(
            PERSONA_STATE,
            proto::Header::default(),
            &own_persona(730, 0, None),
        )
        .await;
    let game = client.next_own_game(1).await;
    assert_eq!(game.app_id, 730);
    assert_eq!(
        refused(620, own_game::GameInvite::Lobby { lobby_id: LOBBY }).await,
        unavailable
    );
    // To a stranger's account type, nothing leaves.
    assert_eq!(
        client
            .invite_into_game(
                0x0170_0001_0000_0001,
                730,
                &own_game::GameInvite::Lobby { lobby_id: LOBBY }
            )
            .await
            .unwrap_err()
            .to_string(),
        "backend.steam.chat.invalidRecipient"
    );
    assert!(server.quiet().await);
}
