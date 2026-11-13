use super::*;
use crate::client::Client;

async fn fixture() -> (Client, test_server::TestServer, friends::FriendsEventSource) {
    let (connection, server) = test_server::connection(76_561_198_000_000_000, true).await;
    let friends = connection.friends_source();
    (Client::with_test_connection(connection), server, friends)
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
    let (client, mut server, source) = fixture().await;
    befriend(&source);
    playing(&source, 620);
    let initial = client.next_friends(0).await;
    assert_eq!(title(&initial), None);
    assert!(
        server.quiet().await,
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
        respond_batch(&mut server, &[620], result)
    );
    assert_eq!(title(&resolved), Some("Portal 2"));
    playing(&source, 0);
    let closed = client.next_friends(resolved.revision).await;
    assert!(closed.friends[0].game.is_none());
    playing(&source, 620);
    let restarted = client.next_friends(closed.revision).await;
    assert_eq!(title(&restarted), Some("Portal 2"));
    assert!(
        server.quiet().await,
        "A restarted game must reuse the cached title"
    );
}

#[tokio::test]
async fn friend_game_exit_and_disconnect_interrupt_pending_title_lookup() {
    let (client, mut server, source) = fixture().await;
    befriend(&source);
    playing(&source, 620);
    let revision = source.snapshot().revision;
    let stop_game = async {
        server.request(TOKEN_REQUEST).await;
        playing(&source, 0);
    };
    let (closed, ()) = tokio::join!(client.next_friends(revision), stop_game);
    assert!(closed.friends[0].game.is_none());

    playing(&source, 570);
    let revision = source.snapshot().revision;
    let disconnect = async {
        server.request(TOKEN_REQUEST).await;
        client.shutdown();
    };
    let (stopped, ()) = tokio::join!(client.next_friends(revision), disconnect);
    assert_eq!(stopped.connection, friends::FriendsConnection::Disconnected);
}

async fn respond_batch(
    server: &mut test_server::TestServer,
    expected_ids: &[u32],
    result: proto::ProductResponse,
) {
    respond_batches(server, &[(expected_ids, result)]).await;
}

/// Answers PICS batches sent together: every token request, then every product
/// request, each matched to its batch by the apps it names.
async fn respond_batches(
    server: &mut test_server::TestServer,
    batches: &[(&[u32], proto::ProductResponse)],
) {
    let mut token_requests = Vec::new();
    for _ in batches {
        token_requests.push(server.request(TOKEN_REQUEST).await);
    }
    for token_request in &token_requests {
        let tokens = proto::AccessTokenRequest::decode(token_request.body.clone()).unwrap();
        assert!(batches.iter().any(|(ids, _)| tokens.app_ids == *ids));
        assert!(tokens.package_ids.is_empty());
        server
            .reply(
                token_request,
                TOKEN_RESPONSE,
                &proto::AccessTokenResponse::default(),
            )
            .await;
    }
    for _ in batches {
        let product_request = server.request(PRODUCT_REQUEST).await;
        let products = proto::ProductRequest::decode(product_request.body.clone()).unwrap();
        let ids: Vec<_> = products.apps.iter().map(|app| app.id).collect();
        let (_, result) = batches
            .iter()
            .find(|(expected, _)| *expected == ids)
            .expect("a product request for a sent batch");
        assert!(products.packages.is_empty());
        server
            .reply(&product_request, PRODUCT_RESPONSE, result)
            .await;
    }
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
    let (client, mut server, _) = fixture().await;
    let mut requested = vec![0, 1];
    requested.extend(1..=51);
    requested.extend([51, 0]);
    let responder = async {
        let first: Vec<_> = (1..=50).collect();
        // Both batches are in flight at once.
        respond_batches(
            &mut server,
            &[(&first, metadata(&first)), (&[51], metadata(&[51]))],
        )
        .await;
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
    assert!(server.quiet().await);
    assert!(client.platforms(&[0, 0]).await.unwrap().is_empty());
}

#[tokio::test]
async fn library_platforms_failure_is_not_cached_as_unknown() {
    let (client, mut server, _) = fixture().await;
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
        respond_batch(&mut server, &[42], malformed)
    );
    assert!(failed.is_err());
    let (retry, ()) = tokio::join!(
        client.platforms(&[42]),
        respond_batch(&mut server, &[42], metadata(&[42]))
    );
    assert_eq!(retry.unwrap()[0].platforms, ["windows", "macos"]);
}

#[tokio::test]
async fn library_platforms_carry_the_whole_pics_app_info() {
    let (client, mut server, _) = fixture().await;
    let result = proto::ProductResponse {
        apps: vec![proto::ProductInfo {
            id: 620,
            buffer: Bytes::from_static(
                br#""appinfo" { "appid" "620" "common" { "name" "Portal 2" "oslist" "windows,macos"
                    "associations" { "0" { "type" "developer" "name" "Valve" } } "review_score" "9" }
                    "extended" { "homepage" "https://www.thinkwithportals.com" } }"#,
            ),
            ..Default::default()
        }],
        ..Default::default()
    };
    let (games, ()) = tokio::join!(
        client.platforms(&[620]),
        respond_batch(&mut server, &[620], result)
    );
    let pics = games
        .unwrap()
        .remove(0)
        .pics
        .expect("PICS answered for the game");
    assert_eq!(pics["common"]["name"], "Portal 2");
    assert_eq!(pics["common"]["associations"]["0"]["name"], "Valve");
    assert_eq!(pics["common"]["review_score"], "9");
    assert_eq!(
        pics["extended"]["homepage"],
        "https://www.thinkwithportals.com"
    );
}

/// The account's licenses, as Steam pushes them after logon.
async fn push_licenses(server: &mut test_server::TestServer, licenses: Vec<proto::License>) {
    let list = proto::LicenseList {
        result: Some(1),
        licenses,
    };
    server
        .send(
            LICENSE_LIST,
            proto::Header::default(),
            &list.encode_to_vec(),
        )
        .await;
}

/// A package as PICS sends it, in binary KeyValues, granting `apps`.
fn package(id: u32, apps: &[u32]) -> proto::ProductInfo {
    let mut buffer = 1_u32.to_le_bytes().to_vec();
    buffer.push(0);
    buffer.extend(format!("{id}\0").bytes());
    buffer.push(0);
    buffer.extend(b"appids\0");
    for (index, app) in apps.iter().enumerate() {
        buffer.push(2);
        buffer.extend(format!("{index}\0").bytes());
        buffer.extend(app.to_le_bytes());
    }
    buffer.extend([8, 8, 8]);
    proto::ProductInfo {
        id,
        buffer: buffer.into(),
        ..Default::default()
    }
}

/// An app as PICS sends it, in text KeyValues, with `common` fields.
fn app(id: u32, common: &str) -> proto::ProductInfo {
    proto::ProductInfo {
        id,
        buffer: format!(r#""appinfo" {{ "appid" "{id}" "common" {{ {common} }} }}"#)
            .into_bytes()
            .into(),
        ..Default::default()
    }
}

/// A product Steam withholds from the account.
fn withheld(id: u32) -> proto::ProductInfo {
    proto::ProductInfo {
        id,
        missing_token: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn library_adds_the_free_and_family_lent_games_owned_games_leave_out() {
    const ACCOUNT: u32 = 76_561_198_000_000_000_u64 as u32;
    const BORROWED: u32 = 0x4000;
    let (client, mut server, _) = fixture().await;
    push_licenses(
        &mut server,
        vec![
            proto::License {
                package_id: 1,
                owner_id: ACCOUNT,
                time_created: 1_700_000_000,
                ..Default::default()
            },
            // A family member lends apps 400 and 401.
            proto::License {
                package_id: 2,
                owner_id: ACCOUNT + 1,
                flags: BORROWED,
                ..Default::default()
            },
        ],
    )
    .await;
    let answer = async {
        let owned = server.request(SERVICE_REQUEST).await;
        let response = proto::OwnedGamesResponse {
            game_count: 1,
            games: vec![proto::OwnedGame {
                app_id: 620,
                name: "Portal 2".into(),
                last_played: 1_789_430_000,
                ..Default::default()
            }],
        };
        server.reply(&owned, SERVICE_RESPONSE, &response).await;
        let packages = server.request(PRODUCT_REQUEST).await;
        let response = proto::ProductResponse {
            packages: vec![
                package(1, &[620, 730, 731, 732, 733]),
                package(2, &[400, 401]),
            ],
            ..Default::default()
        };
        server.reply(&packages, PRODUCT_RESPONSE, &response).await;
        let apps = proto::ProductResponse {
            apps: vec![
                app(
                    730,
                    r#""name" "Counter-Strike 2" "type" "Game" "icon" "cs2""#,
                ),
                app(731, r#""name" "Soundtrack" "type" "Music""#),
                app(
                    732,
                    r#""name" "Upcoming" "type" "Game" "releasestate" "prerelease""#,
                ),
                // Steam withholds this one's metadata from the account.
                withheld(733),
                app(400, r#""name" "Stardew Valley" "type" "Game""#),
                // Its publisher keeps it from the family's libraries.
                app(401, r#""name" "Kept Home" "type" "Game" "exfgls" "1""#),
            ],
            ..Default::default()
        };
        respond_batch(&mut server, &[400, 401, 730, 731, 732, 733], apps).await;
    };
    let (games, ()) = tokio::join!(client.games(), answer);
    let games = games.unwrap();
    let names: Vec<_> = games.iter().map(|game| game.name.as_str()).collect();
    assert_eq!(names, ["Portal 2", "Counter-Strike 2", "Stardew Valley"]);
    assert_eq!(games[1].icon_hash, "cs2");
    // The licenses this load read date the games without asking Steam again.
    assert_eq!(
        client.acquisitions().await.unwrap().get(&730),
        Some(&1_700_000_000)
    );
    assert!(server.quiet().await);
}

#[tokio::test]
async fn library_adds_the_free_license_games_the_store_sells() {
    let (client, mut server, _) = fixture().await;
    push_licenses(
        &mut server,
        vec![proto::License {
            package_id: 0,
            ..Default::default()
        }],
    )
    .await;
    let answer = async {
        let owned = server.request(SERVICE_REQUEST).await;
        let response = proto::OwnedGamesResponse::default();
        server.reply(&owned, SERVICE_RESPONSE, &response).await;
        let packages = server.request(PRODUCT_REQUEST).await;
        let request = proto::ProductRequest::decode(packages.body.clone()).unwrap();
        assert_eq!(request.packages[0].id, 0);
        let response = proto::ProductResponse {
            packages: vec![package(0, &[440, 480])],
            ..Default::default()
        };
        server.reply(&packages, PRODUCT_RESPONSE, &response).await;
        // Spacewar, Valve's test app, has no store page.
        let apps = proto::ProductResponse {
            apps: vec![
                app(
                    440,
                    r#""name" "Team Fortress 2" "type" "game" "store_asset_mtime" "1745368576""#,
                ),
                app(480, r#""name" "Spacewar" "type" "Game""#),
            ],
            ..Default::default()
        };
        respond_batch(&mut server, &[440, 480], apps).await;
    };
    let (games, ()) = tokio::join!(client.games(), answer);
    let names: Vec<_> = games.unwrap().into_iter().map(|game| game.name).collect();
    assert_eq!(names, ["Team Fortress 2"]);
    assert!(server.quiet().await);
}

#[tokio::test]
async fn a_withheld_free_license_keeps_the_other_licenses_games() {
    let (client, mut server, _) = fixture().await;
    push_licenses(
        &mut server,
        vec![
            proto::License {
                package_id: 0,
                ..Default::default()
            },
            proto::License {
                package_id: 1,
                ..Default::default()
            },
        ],
    )
    .await;
    let answer = async {
        let owned = server.request(SERVICE_REQUEST).await;
        let response = proto::OwnedGamesResponse::default();
        server.reply(&owned, SERVICE_RESPONSE, &response).await;
        let packages = server.request(PRODUCT_REQUEST).await;
        let response = proto::ProductResponse {
            packages: vec![withheld(0), package(1, &[730])],
            ..Default::default()
        };
        server.reply(&packages, PRODUCT_RESPONSE, &response).await;
        let apps = proto::ProductResponse {
            apps: vec![app(730, r#""name" "Counter-Strike 2" "type" "Game""#)],
            ..Default::default()
        };
        respond_batch(&mut server, &[730], apps).await;
    };
    let (games, ()) = tokio::join!(client.games(), answer);
    let names: Vec<_> = games.unwrap().into_iter().map(|game| game.name).collect();
    assert_eq!(names, ["Counter-Strike 2"]);
    assert!(server.quiet().await);
}

#[tokio::test]
async fn a_withheld_package_keeps_the_other_licenses_games() {
    let (client, mut server, _) = fixture().await;
    push_licenses(
        &mut server,
        (1..=3)
            .map(|package_id| proto::License {
                package_id,
                ..Default::default()
            })
            .collect(),
    )
    .await;
    let answer = async {
        let owned = server.request(SERVICE_REQUEST).await;
        let response = proto::OwnedGamesResponse::default();
        server.reply(&owned, SERVICE_RESPONSE, &response).await;
        let packages = server.request(PRODUCT_REQUEST).await;
        // Steam does not describe package 2 and withholds package 3.
        let response = proto::ProductResponse {
            packages: vec![package(1, &[730]), withheld(3)],
            unknown_packages: vec![2],
            ..Default::default()
        };
        server.reply(&packages, PRODUCT_RESPONSE, &response).await;
        let apps = proto::ProductResponse {
            apps: vec![app(730, r#""name" "Counter-Strike 2" "type" "Game""#)],
            ..Default::default()
        };
        respond_batch(&mut server, &[730], apps).await;
    };
    let (games, ()) = tokio::join!(client.games(), answer);
    let names: Vec<_> = games.unwrap().into_iter().map(|game| game.name).collect();
    assert_eq!(names, ["Counter-Strike 2"]);
    assert!(server.quiet().await);
    // Download rights still read every package, so they ask Steam again and
    // fail on the withheld ones.
    let answer = async {
        let packages = server.request(PRODUCT_REQUEST).await;
        let response = proto::ProductResponse {
            packages: vec![package(1, &[730])],
            unknown_packages: vec![2, 3],
            ..Default::default()
        };
        server.reply(&packages, PRODUCT_RESPONSE, &response).await;
    };
    let (acquired, ()) = tokio::join!(client.acquisitions(), answer);
    let error = acquired.unwrap_err().to_string();
    assert!(
        error.contains("backend.steam.cm.packagesUnavailable"),
        "{error}"
    );
}

#[tokio::test]
async fn library_asks_steam_for_every_owned_game_field() {
    let (client, mut server, _) = fixture().await;
    push_licenses(&mut server, Vec::new()).await;
    let answer = async {
        let packet = server.request(SERVICE_REQUEST).await;
        assert_eq!(
            packet.header.target_name.as_deref(),
            Some("Player.GetOwnedGames#1")
        );
        let request = proto::OwnedGamesRequest::decode(packet.body.clone()).unwrap();
        assert!(
            request.include_extended_app_info,
            "Steam only sends the extended fields on request"
        );
        let response = proto::OwnedGamesResponse {
            game_count: 1,
            games: vec![proto::OwnedGame {
                app_id: 620,
                name: "Portal 2".into(),
                playtime_forever: 824,
                playtime_mac_forever: 120,
                icon_hash: "icon".into(),
                last_played: 1_789_430_000,
                capsule_filename: "capsule.jpg".into(),
                sort_as: "portal 2".into(),
                has_dlc: true,
                content_descriptor_ids: vec![2, 5],
                ..Default::default()
            }],
        };
        server.reply(&packet, SERVICE_RESPONSE, &response).await;
    };
    let (games, ()) = tokio::join!(client.games(), answer);
    let game = games.unwrap().remove(0);
    assert_eq!(game.playtime_minutes, 824);
    assert_eq!(game.record["appid"], 620);
    assert_eq!(game.record["img_icon_url"], "icon");
    assert_eq!(game.record["rtime_last_played"], 1_789_430_000);
    assert_eq!(game.record["playtime_mac_forever"], 120);
    assert_eq!(game.record["capsule_filename"], "capsule.jpg");
    assert_eq!(game.record["has_dlc"], true);
    assert_eq!(
        game.record["content_descriptorids"],
        serde_json::json!([2, 5])
    );
}
