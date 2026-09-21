use super::*;
use crate::client::Client;

type Responses = mpsc::Sender<Result<Packet, CmError>>;

fn fixture() -> (
    Client,
    mpsc::Receiver<Command>,
    Responses,
    friends::FriendsEventSource,
) {
    let (outgoing, commands) = mpsc::channel(8);
    let (responses, incoming) = mpsc::channel(8);
    let (_, profile) = watch::channel(ProfileState::default());
    let connection = SteamConnection {
        outgoing,
        incoming,
        profile,
        friends: friends::FriendsEventSource::new(76_561_198_000_000_000),
        worker: tokio::spawn(std::future::pending()),
        steam_id: 76_561_198_000_000_000,
        session_id: 1,
        cell_id: 0,
        next_job: 1,
        logged_on: true,
        licenses: None,
    };
    let friends = connection.friends_source();
    (
        Client::with_test_connection(connection),
        commands,
        responses,
        friends,
    )
}

fn befriend(source: &friends::FriendsEventSource) {
    source.apply_packet(
        friends::FRIENDS_LIST,
        &proto::FriendsList {
            incremental: false,
            friends: vec![proto::FriendRelationship {
                steam_id: source.account_id() + 1,
                relationship: 3,
            }],
        }
        .encode_to_vec(),
    );
}

/// App 0 means the friend left the game.
fn playing(source: &friends::FriendsEventSource, app_id: u32) {
    source.apply_packet(
        friends::PERSONA_STATE,
        &proto::PersonaState {
            status_flags: friends::REQUESTED | friends::STATUS,
            friends: vec![proto::Persona {
                friend_id: source.account_id() + 1,
                player_name: Some("Friend".into()),
                persona_state: Some(1),
                game_played_app_id: Some(app_id),
                ..Default::default()
            }],
        }
        .encode_to_vec(),
    );
}

fn title(snapshot: &friends::FriendsSnapshot) -> Option<&str> {
    snapshot.friends[0]
        .game
        .as_ref()
        .expect("friend in a game")
        .name
        .as_deref()
}

#[tokio::test]
async fn friend_title_resolves_from_pics_and_reuses_name_after_game_restart() {
    let (client, mut commands, responses, source) = fixture();
    befriend(&source);
    playing(&source, 620);
    let initial = client.next_friends(0).await;
    assert_eq!(title(&initial), None);
    assert!(
        commands.try_recv().is_err(),
        "Initial presence must not wait for metadata"
    );
    let result = proto::ProductResponse {
        apps: vec![proto::ProductInfo {
            id: 620,
            buffer: Bytes::from_static(br#""appinfo" { "common" { "name" "Portal 2" } }"#),
            ..Default::default()
        }],
        ..Default::default()
    };
    let (resolved, ()) = tokio::join!(
        client.next_friends(initial.revision),
        respond_batch(&mut commands, &responses, &[620], result)
    );
    assert_eq!(title(&resolved), Some("Portal 2"));
    playing(&source, 0);
    let closed = client.next_friends(resolved.revision).await;
    assert!(closed.friends[0].game.is_none());
    playing(&source, 620);
    let restarted = client.next_friends(closed.revision).await;
    assert_eq!(title(&restarted), Some("Portal 2"));
    assert!(
        commands.try_recv().is_err(),
        "A restarted game must reuse the cached title"
    );
}

#[tokio::test]
async fn friend_game_exit_and_disconnect_interrupt_pending_title_lookup() {
    let (client, mut commands, _responses, source) = fixture();
    befriend(&source);
    playing(&source, 620);
    let revision = source.snapshot().revision;
    let stop_game = async {
        request(&mut commands, TOKEN_REQUEST).await;
        playing(&source, 0);
    };
    let (closed, ()) = tokio::join!(client.next_friends(revision), stop_game);
    assert!(closed.friends[0].game.is_none());

    playing(&source, 570);
    let revision = source.snapshot().revision;
    let disconnect = async {
        request(&mut commands, TOKEN_REQUEST).await;
        client.shutdown();
    };
    let (stopped, ()) = tokio::join!(client.next_friends(revision), disconnect);
    assert_eq!(stopped.connection, friends::FriendsConnection::Disconnected);
}

async fn request(commands: &mut mpsc::Receiver<Command>, kind: u32) -> Packet {
    let command = timeout(Duration::from_secs(1), commands.recv())
        .await
        .unwrap()
        .unwrap();
    let Command::Send(bytes) = command else {
        panic!("platform metadata unexpectedly changed session state");
    };
    let mut packets = Vec::new();
    let mut budget = MAX_PACKET;
    decode_packets(bytes.into(), 0, &mut budget, &mut packets).unwrap();
    assert_eq!(packets.len(), 1);
    let packet = packets.remove(0);
    assert_eq!(packet.kind, kind);
    packet
}

async fn reply(responses: &Responses, request: &Packet, kind: u32, body: &impl Message) {
    assert!(
        responses
            .send(Ok(Packet {
                kind,
                header: proto::Header {
                    job_target: request.header.job_source,
                    ..Default::default()
                },
                body: body.encode_to_vec().into(),
            }))
            .await
            .is_ok()
    );
}

async fn respond_batch(
    commands: &mut mpsc::Receiver<Command>,
    responses: &Responses,
    expected_ids: &[u32],
    result: proto::ProductResponse,
) {
    let token_request = request(commands, TOKEN_REQUEST).await;
    let tokens = proto::AccessTokenRequest::decode(token_request.body.clone()).unwrap();
    assert_eq!(tokens.app_ids, expected_ids);
    assert!(tokens.package_ids.is_empty());
    reply(
        responses,
        &token_request,
        TOKEN_RESPONSE,
        &proto::AccessTokenResponse::default(),
    )
    .await;

    let product_request = request(commands, PRODUCT_REQUEST).await;
    let products = proto::ProductRequest::decode(product_request.body.clone()).unwrap();
    assert_eq!(
        products.apps.iter().map(|app| app.id).collect::<Vec<_>>(),
        expected_ids
    );
    assert!(products.packages.is_empty());
    reply(responses, &product_request, PRODUCT_RESPONSE, &result).await;
}

fn metadata(ids: &[u32]) -> proto::ProductResponse {
    proto::ProductResponse {
        apps: ids
            .iter()
            .filter(|id| **id != 25)
            .map(|id| proto::ProductInfo {
                id: *id,
                // 51 has valid metadata with no platform declaration.
                buffer: format!(
                    "\"appinfo\" {{ \"appid\" \"{id}\" \"common\" {{ \"oslist\" \"{}\" }} }}",
                    if *id == 51 { "" } else { "windows,macos" }
                )
                .into_bytes()
                .into(),
                ..Default::default()
            })
            .collect(),
        unknown_apps: ids.iter().copied().filter(|id| *id == 25).collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn library_platforms_batches_51_ids_preserves_unknown_and_reuses_metadata() {
    let (client, mut commands, responses, _) = fixture();
    let mut requested = vec![0, 1];
    requested.extend(1..=51);
    requested.extend([51, 0]);
    let responder = async {
        let first: Vec<_> = (1..=50).collect();
        respond_batch(&mut commands, &responses, &first, metadata(&first)).await;
        respond_batch(&mut commands, &responses, &[51], metadata(&[51])).await;
    };
    let (result, ()) = tokio::join!(client.platforms(&requested), responder);
    let result = result.unwrap();
    assert_eq!(result.len(), 51);
    assert_eq!(
        result.iter().map(|game| game.app_id).collect::<Vec<_>>(),
        (1..=51).collect::<Vec<_>>()
    );
    assert_eq!(result[0].platforms, ["windows", "macos"]);
    assert!(result[24].platforms.is_empty());
    assert!(result[50].platforms.is_empty());

    // Known metadata, including a valid empty declaration, needs no new CM job.
    let cached = timeout(Duration::from_secs(1), client.platforms(&[51, 1, 51, 0]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cached, vec![result[50].clone(), result[0].clone()]);
    assert!(commands.try_recv().is_err());
    assert!(client.platforms(&[0, 0]).await.unwrap().is_empty());
}

#[tokio::test]
async fn library_platforms_failure_is_not_cached_as_unknown() {
    let (client, mut commands, responses, _) = fixture();
    let malformed = proto::ProductResponse {
        apps: vec![proto::ProductInfo {
            id: 42,
            buffer: Bytes::from_static(b"\"appinfo\" {"),
            ..Default::default()
        }],
        ..Default::default()
    };
    let (failed, ()) = tokio::join!(
        client.platforms(&[42]),
        respond_batch(&mut commands, &responses, &[42], malformed)
    );
    assert!(failed.is_err());
    let (retry, ()) = tokio::join!(
        client.platforms(&[42]),
        respond_batch(&mut commands, &responses, &[42], metadata(&[42]))
    );
    assert_eq!(retry.unwrap()[0].platforms, ["windows", "macos"]);
}
