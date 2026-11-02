//! The same decisions, read where they are made: the depots a download
//! prepares and the manifests a Mac pick hands to its install. These reach
//! `prepare_depots`, `PreparedInstall`, `OwnedApp`, `Licensed` and the
//! `build_choice` field, which `client::build` names its builds for.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::PoisonError,
};

use obelisk_core::build_choice::{GameBuilds, Inspection, Offered};

use super::*;
use crate::client::{
    Licensed, OwnedApp, PreparedInstall,
    build::{Kept, Revision},
};
use crate::{
    cm::test_steam::UNUSED_CDN,
    content::{CachedManifest, CancelToken, CdnAuthorization},
};

/// A prepared depot as the download will install it.
#[derive(Debug, PartialEq)]
struct Prepared {
    depot: u32,
    manifest: u64,
    authorizing_app: u32,
    key: [u8; 32],
    estimated_bytes: u64,
    /// Whether the Mac pick handed over its manifest.
    picked: bool,
}

async fn prepare(
    client: &Client,
    steam: &mut Steam,
    app_id: u32,
    platform: Platform,
) -> Result<PreparedInstall, ClientError> {
    let cancel = CancelToken::new();
    steam
        .during(client.prepare_depots(app_id, platform, ENGLISH, &cancel))
        .await
}

/// A prepared depot: its manifest, the app that authorizes it, the app its key
/// was asked for under, the bytes PICS estimates for it and whether the Mac
/// pick handed over its manifest.
fn prepared_depot(
    depot: u32,
    manifest: u64,
    authorizing_app: u32,
    key_app: u32,
    estimated_bytes: u64,
    picked: bool,
) -> Prepared {
    Prepared {
        depot,
        manifest,
        authorizing_app,
        key: depot_key(depot, key_app),
        estimated_bytes,
        picked,
    }
}

/// Each prepared depot, as `prepared_depot` describes one.
fn depots(prepared: &PreparedInstall) -> Vec<Prepared> {
    prepared
        .depots
        .iter()
        .map(|depot| Prepared {
            depot: depot.depot_id,
            manifest: depot.manifest_id,
            authorizing_app: depot.authorizing_app,
            key: depot.key,
            estimated_bytes: depot.estimated_bytes,
            picked: depot.picked.is_some(),
        })
        .collect()
}

#[tokio::test]
async fn a_downloads_depots_are_those_selected_with_a_public_manifest_in_the_apps_order() {
    let (client, mut steam) = account().await;
    let mac = prepare(&client, &mut steam, SHARED_GAME, Platform::Mac)
        .await
        .unwrap();
    assert_eq!(mac.name, "Shared Game");
    assert_eq!(mac.servers.len(), 1);
    assert_eq!(mac.servers[0].host, NO_CDN_ADDRESS);
    assert_eq!(mac.servers[0].vhost, NO_CDN_ADDRESS);
    // The shared depots have the manifest of their source app, or their own.
    // Each is authorized through its source app, and keyed under the game.
    assert_eq!(
        depots(&mac),
        [
            prepared_depot(302, 3502, BASE, SHARED_GAME, 0, false),
            prepared_depot(303, 3003, BASE, SHARED_GAME, 40, false),
            prepared_depot(304, 3004, SHARED_GAME, SHARED_GAME, 60, false),
        ]
    );
    assert_eq!(steam.package_queries(), 1);
    assert_eq!(steam.app_queries(), [vec![SHARED_GAME], vec![BASE]]);
    assert_eq!(steam.content_server_queries(), 1);
    // Preparing asks for keys, never for manifests or CDN tokens.
    assert_eq!(
        steam.depot_keys(),
        [(302, SHARED_GAME), (303, SHARED_GAME), (304, SHARED_GAME)]
    );
    assert!(steam.manifest_codes().is_empty());
    assert!(steam.cdn_tokens().is_empty());
}

#[tokio::test]
async fn a_depot_that_names_its_own_game_and_has_no_manifest_is_selected_but_not_prepared() {
    let (client, mut steam) = account().await;
    let windows = prepare(&client, &mut steam, SHARED_GAME, Platform::Windows)
        .await
        .unwrap();
    // Depot 305 is selected: no key is asked for it.
    assert_eq!(
        depots(&windows),
        [
            prepared_depot(301, 3001, SHARED_GAME, SHARED_GAME, 10, false),
            prepared_depot(304, 3004, SHARED_GAME, SHARED_GAME, 60, false),
        ]
    );
    assert_eq!(steam.depot_keys(), [(301, SHARED_GAME), (304, SHARED_GAME)]);
    // Nothing the Windows build selects needs another app.
    assert_eq!(steam.app_queries(), [[SHARED_GAME]]);
}

#[tokio::test]
async fn the_common_redistributables_get_their_key_from_their_own_app_except_the_net_ones() {
    let (client, mut steam) = account().await;
    let windows = prepare(&client, &mut steam, REDIST_GAME, Platform::Windows)
        .await
        .unwrap();
    assert_eq!(
        depots(&windows),
        [
            prepared_depot(401, 4001, REDIST_GAME, REDIST_GAME, 50, false),
            prepared_depot(228_990, 9990, REDISTRIBUTABLES, REDISTRIBUTABLES, 0, false),
            prepared_depot(
                228_989,
                2_289_891,
                REDISTRIBUTABLES,
                REDISTRIBUTABLES,
                0,
                false
            ),
            prepared_depot(
                229_013,
                2_290_131,
                REDISTRIBUTABLES,
                REDISTRIBUTABLES,
                0,
                false
            ),
        ]
    );
    // 229000 and 229012 are left out: no key is asked for them.
    assert_eq!(
        steam.depot_keys(),
        [
            (401, REDIST_GAME),
            (228_989, REDISTRIBUTABLES),
            (228_990, REDISTRIBUTABLES),
            (229_013, REDISTRIBUTABLES),
        ]
    );
    assert_eq!(
        steam.app_queries(),
        [vec![REDIST_GAME], vec![REDISTRIBUTABLES]]
    );
}

#[tokio::test]
async fn a_free_to_download_game_authorizes_every_depot_through_itself() {
    let (client, mut steam) = account().await;
    let windows = prepare(&client, &mut steam, FREE_GAME, Platform::Windows)
        .await
        .unwrap();
    // It owns no depot, so the depots come from the game being free; a
    // shared depot is keyed under the game, a redistributable under its app.
    assert_eq!(
        depots(&windows),
        [
            prepared_depot(501, 5001, FREE_GAME, FREE_GAME, 20, false),
            prepared_depot(502, 5502, FREE_GAME, FREE_GAME, 0, false),
            prepared_depot(228_990, 9990, FREE_GAME, REDISTRIBUTABLES, 0, false),
        ]
    );
    assert_eq!(
        steam.app_queries(),
        [vec![FREE_GAME], vec![BASE, REDISTRIBUTABLES]]
    );
}

#[tokio::test]
async fn a_download_gets_the_servers_steam_allows_for_the_game_in_its_order() {
    let (client, mut steam) = account().await;
    steam.world.cdn_host = UNUSED_CDN.into();
    steam.world.restricted_hosts = vec![
        ("for-others.invalid".into(), vec![GAME]),
        ("for-the-game.invalid".into(), vec![GAME, SHARED_GAME]),
        ("for-no-one.invalid".into(), vec![SHARED_GAME + 1]),
    ];
    let prepared = prepare(&client, &mut steam, SHARED_GAME, Platform::Windows)
        .await
        .unwrap();
    let hosts: Vec<_> = prepared.servers.iter().map(|server| &server.host).collect();
    assert_eq!(hosts, [UNUSED_CDN, "for-the-game.invalid"]);
    assert_eq!(steam.content_server_queries(), 1);
}

#[tokio::test]
async fn a_download_with_no_server_allowed_for_the_game_fails() {
    let (client, mut steam) = account().await;
    steam.world.cdn_host = String::new();
    steam.world.restricted_hosts = vec![("for-others.invalid".into(), vec![GAME])];
    let prepared = prepare(&client, &mut steam, SHARED_GAME, Platform::Windows).await;
    let error = prepared.err().expect("no server to download from");
    assert_eq!(message(error), "backend.steam.client.noContentServer");
}

/// The `(depot, app)` of each CDN token Steam is asked for when a CDN refuses
/// `prepared`'s depots.
async fn cdn_tokens_asked_for(
    client: &Client,
    steam: &mut Steam,
    prepared: &PreparedInstall,
) -> Vec<(u32, u32)> {
    steam.forget_requests();
    let refused = async {
        for depot in &prepared.depots {
            depot.authorization(&client.cm).token("cdn.invalid").await;
        }
    };
    steam.during(refused).await;
    steam.cdn_tokens()
}

#[tokio::test]
async fn a_cdn_that_refuses_a_depot_gets_its_token_through_the_app_that_authorizes_it() {
    let (client, mut steam) = account().await;
    let shared = prepare(&client, &mut steam, SHARED_GAME, Platform::Mac)
        .await
        .unwrap();
    assert_eq!(
        cdn_tokens_asked_for(&client, &mut steam, &shared).await,
        [(302, BASE), (303, BASE), (304, SHARED_GAME)]
    );
    let redistributables = prepare(&client, &mut steam, REDIST_GAME, Platform::Windows)
        .await
        .unwrap();
    assert_eq!(
        cdn_tokens_asked_for(&client, &mut steam, &redistributables).await,
        [
            (401, REDIST_GAME),
            (228_989, REDISTRIBUTABLES),
            (228_990, REDISTRIBUTABLES),
            (229_013, REDISTRIBUTABLES),
        ]
    );
    let free = prepare(&client, &mut steam, FREE_GAME, Platform::Windows)
        .await
        .unwrap();
    assert_eq!(
        cdn_tokens_asked_for(&client, &mut steam, &free).await,
        [(501, FREE_GAME), (502, FREE_GAME), (228_990, FREE_GAME)]
    );
}

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
    // Depot 302 has its source app's manifest, in the app's order.
    assert_eq!(
        key,
        Some(Revision::fabricated(
            SHARED_GAME,
            &[(302, 3502), (303, 3003), (304, 3004)]
        ))
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
    assert!(matches!(read.unwrap(), Inspection::NotNative));
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

const PROGRAM: &[u8] = b"arm64 program";
const DATA: &[u8] = b"shared data";

/// Stands for `OwnedApp`, whose pick reads a Mac build's files from a CDN,
/// with the manifests that read keeps for the download.
struct ReadByThePick<'a> {
    game: OwnedApp<'a>,
    kept: Kept,
}

impl GameBuilds for ReadByThePick<'_> {
    type Build = Licensed;
    type Key = Revision;
    type Kept = Kept;
    type Error = ClientError;

    async fn builds(&self) -> Result<Offered<Licensed>, ClientError> {
        self.game.builds().await
    }

    async fn key(&self, mac: &Licensed) -> Result<Option<Revision>, ClientError> {
        self.game.key(mac).await
    }

    async fn inspect(
        &self,
        _: &Licensed,
        _: &CancelToken,
    ) -> Result<Inspection<Self::Kept>, ClientError> {
        Ok(Inspection::Native(self.kept.clone()))
    }
}

/// What a pick of a Mac build keeps: for each of the depots named, its manifest
/// and the one file it lists.
fn kept(depots: &[(u32, u64, &str, &[u8])]) -> Kept {
    depots
        .iter()
        .map(|&(depot, manifest, name, bytes)| {
            let manifest_read = CachedManifest::fabricated(depot, manifest, &[(name, bytes)]);
            ((depot, manifest), manifest_read)
        })
        .collect()
}

/// `GAME`'s two Mac depots, each with the file its manifest lists.
const GAME_FILES: [(u32, u64, &str, &[u8]); 2] = [
    (102, 1002, "Game.app/Contents/MacOS/Game", PROGRAM),
    (103, 1003, "data/shared.bin", DATA),
];

/// Picks the Mac build of `app_id` as if its read found it native and kept
/// `kept`.
async fn pick_mac_build(client: &Client, steam: &mut Steam, app_id: u32, kept: Kept) {
    let game = ReadByThePick {
        game: OwnedApp {
            client,
            app_id,
            language: ENGLISH,
        },
        kept,
    };
    let picked = steam
        .during(client.build_choice.pick(async |pick| pick.of(&game).await))
        .await;
    assert_eq!(picked.unwrap(), Platform::Mac);
}

/// The installation folder `directory` under `parent`, holding `files`, which
/// an install then finds complete.
fn folder_holding(parent: &Path, directory: &str, files: &[(u32, u64, &str, &[u8])]) -> PathBuf {
    let folder = parent.canonicalize().unwrap().join(directory);
    for (_, _, name, bytes) in files {
        let path = folder.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    folder
}

fn mac_folder_with_the_picked_files(parent: &Path) -> PathBuf {
    folder_holding(parent, "steam-100-macos", &GAME_FILES)
}

#[tokio::test]
async fn after_a_mac_pick_the_download_hands_the_picked_manifests_to_the_install() {
    let (client, mut steam) = account().await;
    let parent = tempfile::tempdir().unwrap();
    mac_folder_with_the_picked_files(parent.path());
    pick_mac_build(&client, &mut steam, GAME, kept(&GAME_FILES)).await;
    let prepared = prepare(&client, &mut steam, GAME, Platform::Mac)
        .await
        .unwrap();
    // The manifests are those of the depots prepared; both are handed over.
    assert_eq!(
        depots(&prepared),
        [
            prepared_depot(102, 1002, GAME, GAME, 300, true),
            prepared_depot(103, 1003, GAME, GAME, 100, true),
        ]
    );
    // A second preparation of the same pick finds nothing left.
    let again = prepare(&client, &mut steam, GAME, Platform::Mac)
        .await
        .unwrap();
    assert!(depots(&again).iter().all(|depot| !depot.picked));
    // The pick read the game once; preparing twice only asked for keys again.
    assert_eq!(steam.package_queries(), 1);
    assert_eq!(steam.app_queries(), [[GAME]]);
}

#[tokio::test]
async fn a_windows_preparation_leaves_the_mac_picks_manifests_for_the_mac_download() {
    let (client, mut steam) = account().await;
    pick_mac_build(&client, &mut steam, GAME, kept(&GAME_FILES)).await;
    let windows = prepare(&client, &mut steam, GAME, Platform::Windows)
        .await
        .unwrap();
    assert!(depots(&windows).iter().all(|depot| !depot.picked));
    let mac = prepare(&client, &mut steam, GAME, Platform::Mac)
        .await
        .unwrap();
    assert!(depots(&mac).iter().all(|depot| depot.picked));
}

#[tokio::test]
async fn cancelling_the_download_forgets_what_the_pick_read() {
    let (client, mut steam) = account().await;
    pick_mac_build(&client, &mut steam, GAME, kept(&GAME_FILES)).await;
    client.cancel_download();
    let mac = prepare(&client, &mut steam, GAME, Platform::Mac)
        .await
        .unwrap();
    assert!(depots(&mac).iter().all(|depot| !depot.picked));
}

#[tokio::test]
async fn a_download_after_a_mac_pick_asks_steam_for_no_manifest_and_records_the_depots_it_installed()
 {
    let (client, mut steam) = account().await;
    steam.world.cdn_host = UNUSED_CDN.into();
    let parent = tempfile::tempdir().unwrap();
    let folder = mac_folder_with_the_picked_files(parent.path());
    pick_mac_build(&client, &mut steam, GAME, kept(&GAME_FILES)).await;
    let installation = steam
        .during(client.download(GAME, Platform::Mac, ENGLISH, parent.path()))
        .await
        .unwrap();
    assert_eq!(installation.app_id, GAME);
    assert_eq!(installation.name, "Game");
    assert_eq!(installation.platform, Platform::Mac);
    assert_eq!(installation.language, ENGLISH);
    assert_eq!(installation.directory, folder.to_string_lossy());
    // What the files hold, not the sizes PICS estimates.
    assert_eq!(
        installation.total_bytes,
        (PROGRAM.len() + DATA.len()) as u64
    );
    let recorded: Vec<_> = installation
        .manifests
        .iter()
        .map(|depot| (depot.depot_id, depot.manifest_id))
        .collect();
    assert_eq!(recorded, [(102, 1002), (103, 1003)]);
    // The pick's read served the install: no manifest code, no CDN token.
    assert!(steam.manifest_codes().is_empty());
    assert!(steam.cdn_tokens().is_empty());
    assert_eq!(steam.depot_keys(), [(102, GAME), (103, GAME)]);
    assert_eq!(steam.content_server_queries(), 1);
    // The game and its licenses were read once, by the pick.
    assert_eq!(steam.package_queries(), 1);
    assert_eq!(steam.app_queries(), [[GAME]]);
    // The receipt on disk is the one returned, and is current.
    let receipt = obelisk_core::installation::read_receipt::<Installation>(&folder)
        .unwrap()
        .expect("a receipt");
    assert_eq!(receipt.manifests.len(), 2);
    let update = update_available(&client, &mut steam, GAME, Platform::Mac, ENGLISH, &folder);
    assert!(!update.await.unwrap());
}

#[tokio::test]
async fn a_download_without_a_pick_asks_steam_for_the_manifest_of_the_first_depot() {
    let (client, mut steam) = account().await;
    let parent = tempfile::tempdir().unwrap();
    mac_folder_with_the_picked_files(parent.path());
    let error = steam
        .during(client.download(GAME, Platform::Mac, ENGLISH, parent.path()))
        .await
        .unwrap_err();
    assert_eq!(message(error), no_cdn_address());
    assert_eq!(steam.manifest_codes(), [(102, GAME, 1002)]);
}

#[tokio::test]
async fn the_picked_manifests_serve_one_download_only() {
    let (client, mut steam) = account().await;
    steam.world.cdn_host = UNUSED_CDN.into();
    let parent = tempfile::tempdir().unwrap();
    mac_folder_with_the_picked_files(parent.path());
    pick_mac_build(&client, &mut steam, GAME, kept(&GAME_FILES)).await;
    let download = client.download(GAME, Platform::Mac, ENGLISH, parent.path());
    steam.during(download).await.unwrap();
    assert!(steam.manifest_codes().is_empty());
    // The same installation again has the files but not the manifests: they
    // were not kept on disk, so Steam is asked for them.
    steam.world.cdn_host = NO_CDN_ADDRESS.into();
    let download = client.download(GAME, Platform::Mac, ENGLISH, parent.path());
    let error = steam.during(download).await.unwrap_err();
    assert_eq!(message(error), no_cdn_address());
    assert_eq!(steam.manifest_codes(), [(102, GAME, 1002)]);
}

#[tokio::test]
async fn a_windows_download_does_not_use_or_use_up_the_mac_picks_manifests() {
    let (client, mut steam) = account().await;
    let parent = tempfile::tempdir().unwrap();
    mac_folder_with_the_picked_files(parent.path());
    pick_mac_build(&client, &mut steam, GAME, kept(&GAME_FILES)).await;
    let windows = client.download(GAME, Platform::Windows, ENGLISH, parent.path());
    let error = steam.during(windows).await.unwrap_err();
    assert_eq!(message(error), no_cdn_address());
    assert_eq!(steam.manifest_codes(), [(101, GAME, 1001)]);
    steam.world.cdn_host = UNUSED_CDN.into();
    let mac = client.download(GAME, Platform::Mac, ENGLISH, parent.path());
    steam.during(mac).await.unwrap();
    // The Mac download asked for no manifest of its own.
    assert_eq!(steam.manifest_codes(), [(101, GAME, 1001)]);
}

#[tokio::test]
async fn a_windows_preparation_of_a_game_whose_builds_share_every_depot_still_leaves_the_mac_picks_manifests()
 {
    let (client, mut steam) = account().await;
    let files = [(1201, 12_001, "shared.bin", DATA)];
    pick_mac_build(&client, &mut steam, SAME_DEPOTS, kept(&files)).await;
    // The Windows build has the Mac build's key, but is not a Mac build.
    let windows = prepare(&client, &mut steam, SAME_DEPOTS, Platform::Windows)
        .await
        .unwrap();
    assert_eq!(
        depots(&windows),
        [prepared_depot(
            1201,
            12_001,
            SAME_DEPOTS,
            SAME_DEPOTS,
            9,
            false
        )]
    );
    let mac = prepare(&client, &mut steam, SAME_DEPOTS, Platform::Mac)
        .await
        .unwrap();
    assert_eq!(
        depots(&mac),
        [prepared_depot(
            1201,
            12_001,
            SAME_DEPOTS,
            SAME_DEPOTS,
            9,
            true
        )]
    );
}

#[tokio::test]
async fn a_manifest_that_moved_since_the_pick_hands_over_none_of_the_picks_manifests() {
    let (client, mut steam) = account().await;
    pick_mac_build(&client, &mut steam, GAME, kept(&GAME_FILES)).await;
    // Depot 103 published a new manifest, and the download reads PICS again.
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
    // The pick is kept whole under the build it read: depot 102 did not move,
    // yet its manifest is not handed over either.
    let moved = prepare(&client, &mut steam, GAME, Platform::Mac)
        .await
        .unwrap();
    assert_eq!(
        depots(&moved),
        [
            prepared_depot(102, 1002, GAME, GAME, 300, false),
            prepared_depot(103, 1103, GAME, GAME, 100, false),
        ]
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

#[tokio::test]
async fn the_download_shows_the_depots_installed_so_far_and_estimates_the_others_from_pics() {
    let (client, mut steam) = account().await;
    steam.world.cdn_host = UNUSED_CDN.into();
    // Steam refuses the third depot's manifest, which ends the install there.
    steam.world.refused_manifests = vec![1403];
    let files = [
        (1401, 14_001, "a.bin", b"abc".as_slice()),
        (1402, 14_002, "b.bin", b"defgh".as_slice()),
    ];
    let parent = tempfile::tempdir().unwrap();
    folder_holding(parent.path(), "steam-1400-macos", &files);
    pick_mac_build(&client, &mut steam, THREE_DEPOTS, kept(&files)).await;
    let download = client.download(THREE_DEPOTS, Platform::Mac, ENGLISH, parent.path());
    let error = steam.during(download).await.unwrap_err();
    assert_eq!(
        message(error),
        crate::message::format(
            "backend.steam.cm.rejected",
            &["backend.steam.cm.action.manifest".into(), "15".into()]
        )
    );
    // The first two depots came from the pick; only the third asked for its
    // manifest.
    assert_eq!(steam.manifest_codes(), [(1403, THREE_DEPOTS, 14_003)]);
    // The last depot installed showed what it installed (5 bytes) and the
    // first depot's (3), with the third depot's size as PICS estimates it.
    let progress = client.progress().expect("the download's progress");
    assert_eq!(progress.total_bytes, 3 + 5 + 4000);
    assert_eq!(progress.completed_bytes, 3 + 5);
    assert_eq!(progress.network_bytes, 0);
    assert_eq!(progress.content_bytes, 0);
}
