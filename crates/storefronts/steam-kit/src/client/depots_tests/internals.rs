//! What the interface of `Client` cannot stage or show: a preparation and a
//! read of a Mac build that start cancelled, the key a Mac build's verdict
//! is kept under, a Mac build that has none, and Steam describing a game
//! anew within the minute its PICS metadata is remembered. These reach
//! `prepare_depots`, `OwnedApp` and the `app_infos` field.
use std::sync::PoisonError;

use obelisk_core::build_choice::GameBuilds;

use super::*;
use crate::{client::OwnedApp, content::CancelToken};

#[tokio::test]
async fn a_cancelled_download_prepares_no_depot() {
    let (client, mut steam) = account().await;
    let cancel = CancelToken::new();
    cancel.cancel();
    let prepared = steam
        .during(client.prepare_depots(GAME, Platform::Windows, ENGLISH, &cancel))
        .await;
    let error = prepared.err().expect("a cancelled preparation fails");
    assert!(matches!(error, ClientError::Cancelled(_)), "{error:?}");
    assert_eq!(message(error), "backend.steam.client.downloadStopped");
    assert!(steam.depot_keys().is_empty());
}

#[tokio::test]
async fn the_mac_builds_key_is_its_game_and_the_public_manifests_of_its_selected_depots() {
    let (client, mut steam) = account().await;
    let game = OwnedApp {
        client: &client,
        app_id: SHARED_GAME,
        language: ENGLISH,
    };
    let offered = steam.during(game.builds()).await.unwrap();
    assert!(offered.windows.is_some());
    let mac = offered.mac.expect("the account can install the Mac build");
    let key = steam.during(game.key(&mac)).await.unwrap();
    // Depot 302 has its source app's manifest, in the app's order. A key
    // shows what it holds only as it prints it.
    assert_eq!(
        format!("{:?}", key.expect("a key")),
        format!(
            "Revision {{ app_id: {SHARED_GAME}, manifests: [(302, 3502), (303, 3003), (304, 3004)] }}"
        )
    );
    assert_eq!(steam.app_queries(), [vec![SHARED_GAME], vec![BASE]]);
    assert!(steam.depot_keys().is_empty());
}

#[tokio::test]
async fn a_mac_build_without_a_mac_program_has_no_key_and_asks_steam_nothing() {
    let (client, mut steam) = account().await;
    let game = OwnedApp {
        client: &client,
        app_id: NO_LAUNCH,
        language: ENGLISH,
    };
    let mac = steam.during(game.builds()).await.unwrap().mac.unwrap();
    steam.forget_requests();
    assert_eq!(steam.during(game.key(&mac)).await.unwrap(), None);
    let read = steam.during(game.inspect(&mac, &CancelToken::new())).await;
    assert!(read.unwrap().into_kept().is_none());
    assert!(steam.requests().is_empty());
}

#[tokio::test]
async fn a_cancelled_read_of_a_mac_build_with_no_manifest_to_read_fails_cancelled() {
    let (client, mut steam) = account().await;
    let game = OwnedApp {
        client: &client,
        app_id: SELF_SHARED,
        language: ENGLISH,
    };
    let mac = steam.during(game.builds()).await.unwrap().mac.unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let read = steam.during(game.inspect(&mac, &cancel)).await;
    assert!(matches!(read, Err(ClientError::Cancelled(_))));
    assert!(steam.depot_keys().is_empty());
}

#[tokio::test]
async fn a_manifest_that_moved_since_the_pick_hands_over_none_of_the_picks_manifests() {
    let [windows, mac, shared] = game_depots(ARM64);
    let cdn = holding(&[windows, mac, shared, Held::data(103, 1103, GAME)]);
    let (client, mut steam) = account_served_by(&cdn).await;
    let picked = steam.during(client.platform(GAME, ENGLISH, None)).await;
    assert_eq!(picked.unwrap(), Platform::Mac);
    // Depot 103 published a new manifest, and the download reads PICS again,
    // as it does once the minute it remembers a game for is over.
    steam.world.replace_app(
        game(GAME, "Game")
            .depot(windows_depot(101).manifest(1001).size(500))
            .depot(mac_depot(102).manifest(1002).size(300))
            .depot(depot(103).manifest(1103).size(100)),
    );
    client
        .app_infos
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    steam.forget_requests();
    let parent = tempfile::tempdir().unwrap();
    let download = client.download(GAME, Platform::Mac, ENGLISH, parent.path());
    let installation = steam.during(download).await.unwrap();
    assert_eq!(recorded(&installation), [(102, 1002), (103, 1103)]);
    // The pick is kept whole under the build it read: depot 102 did not move,
    // yet its manifest is not handed over either.
    assert_eq!(
        steam.manifest_codes(),
        [(102, GAME, 1002), (103, GAME, 1103)]
    );
    // The receipt of the build the pick read is out of date.
    let folder = installed(GAME, Platform::Mac, ENGLISH, &[(102, 1002), (103, 1003)]);
    let update = update_available(
        &client,
        &mut steam,
        GAME,
        Platform::Mac,
        ENGLISH,
        folder.path(),
    );
    assert!(update.await.unwrap());
}
