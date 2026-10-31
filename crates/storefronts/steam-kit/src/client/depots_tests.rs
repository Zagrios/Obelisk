//! Which depots and manifests an account installs of a game, and how often
//! Steam is asked for them, through the interface of `Client`: the update
//! check, the game's page, the Mac pick and the download. Steam is scripted
//! (`cm::test_steam`); no CDN is reachable, so a download that needs one ends
//! at the CDN address the content directory names.
use std::path::Path;

use obelisk_core::platform::Platform;

use super::{Client, ClientError, Installation, InstalledDepot};
use crate::cm::test_steam::{
    AppSpec, DepotSpec, NO_CDN_ADDRESS, PackageSpec, Request, Steam, World, depot_key,
};

mod internals;

const ENGLISH: &str = "english";
const REDISTRIBUTABLES: u32 = 228_980;

/// A game with a Windows build, a Mac build, depots of other languages, a DLC
/// depot the account lacks and a depot with no public manifest.
const GAME: u32 = 100;
/// The DLC of one of its depots, which the account lacks.
const DLC: u32 = 199;
/// A game whose Mac depots come from another app, `BASE`.
const SHARED_GAME: u32 = 300;
const BASE: u32 = 350;
/// A Windows game naming the Common Redistributables' depots.
const REDIST_GAME: u32 = 400;
/// A free to download game, which owns no depot.
const FREE_GAME: u32 = 500;
/// A game the account does not own.
const UNOWNED: u32 = 700;
/// A game whose only depot has no public manifest.
const UNPUBLISHED: u32 = 800;
/// A game whose shared depot is missing from its source app.
const SOURCE_LACKS_DEPOT: u32 = 900;
/// A game whose shared depot names an app PICS does not know.
const SOURCE_UNKNOWN: u32 = 950;
/// A game with a Mac build but no Mac launch configuration.
const NO_LAUNCH: u32 = 1000;
/// A game the account owns without the rights to any of its depots.
const NO_RIGHTS: u32 = 1100;
/// A game whose Windows and Mac builds select the same depot.
const SAME_DEPOTS: u32 = 1200;
/// A game whose shared Mac depot has a manifest of its own.
const OWN_MANIFEST: u32 = 1300;
/// A game with three Mac depots.
const THREE_DEPOTS: u32 = 1400;
/// A game whose only Mac depot names the game itself and has no public manifest.
const SELF_SHARED: u32 = 1500;

fn depot(id: u32) -> DepotSpec {
    DepotSpec::new(id)
}

fn windows_depot(id: u32) -> DepotSpec {
    depot(id).platforms("windows")
}

fn mac_depot(id: u32) -> DepotSpec {
    depot(id).platforms("macos")
}

fn game(id: u32, name: &str) -> AppSpec {
    AppSpec::new(id, name)
        .platforms("windows,macos")
        .launching("game.exe", "windows")
        .launching("Game.app/Contents/MacOS/Game", "macos")
}

fn windows_game(id: u32, name: &str) -> AppSpec {
    AppSpec::new(id, name)
        .platforms("windows")
        .launching("game.exe", "windows")
}

/// Every app and license package of the account.
fn world() -> World {
    let apps = vec![
        game(GAME, "Game")
            .depot(windows_depot(101).manifest(1001).size(500))
            .depot(mac_depot(102).manifest(1002).size(300))
            .depot(depot(103).manifest(1003).size(100))
            .depot(
                windows_depot(104)
                    .language("french")
                    .manifest(1004)
                    .size(50),
            )
            .depot(mac_depot(105).language("french").manifest(1005).size(30))
            .depot(windows_depot(106).dlc(DLC).manifest(1006).size(7))
            .depot(windows_depot(107)),
        game(SHARED_GAME, "Shared Game")
            .depot(windows_depot(301).manifest(3001).size(10))
            .depot(mac_depot(302).shared_from(BASE))
            .depot(mac_depot(303).shared_from(BASE).manifest(3003).size(40))
            .depot(depot(304).manifest(3004).size(60))
            .depot(windows_depot(305).shared_from(SHARED_GAME)),
        game(BASE, "Base")
            .depot(mac_depot(302).manifest(3502))
            .depot(windows_depot(502).manifest(5502)),
        windows_game(REDIST_GAME, "Redistributables Game")
            .depot(windows_depot(401).manifest(4001).size(50))
            .depot(windows_depot(228_990).shared_from(REDISTRIBUTABLES))
            .depot(
                windows_depot(228_989)
                    .shared_from(REDISTRIBUTABLES)
                    .manifest(2_289_891),
            )
            .depot(
                windows_depot(229_000)
                    .shared_from(REDISTRIBUTABLES)
                    .manifest(2_290_001),
            )
            .depot(
                windows_depot(229_012)
                    .shared_from(REDISTRIBUTABLES)
                    .manifest(2_290_121),
            )
            .depot(
                windows_depot(229_013)
                    .shared_from(REDISTRIBUTABLES)
                    .manifest(2_290_131),
            ),
        AppSpec::new(REDISTRIBUTABLES, "Steamworks Common Redistributables")
            .depot(windows_depot(228_990).manifest(9990)),
        windows_game(FREE_GAME, "Free Game")
            .free_to_download()
            .depot(windows_depot(501).manifest(5001).size(20))
            .depot(windows_depot(502).shared_from(BASE))
            .depot(windows_depot(228_990).shared_from(REDISTRIBUTABLES)),
        windows_game(UNOWNED, "Unowned").depot(windows_depot(701).manifest(7001).size(5)),
        windows_game(UNPUBLISHED, "Unpublished").depot(windows_depot(801).shared_from(UNPUBLISHED)),
        windows_game(SOURCE_LACKS_DEPOT, "Source Lacks Depot")
            .depot(windows_depot(901).shared_from(BASE)),
        windows_game(SOURCE_UNKNOWN, "Source Unknown").depot(windows_depot(951).shared_from(960)),
        AppSpec::new(NO_LAUNCH, "No Launch")
            .platforms("windows,macos")
            .depot(windows_depot(1001).manifest(10_001))
            .depot(mac_depot(1002).manifest(10_002)),
        windows_game(NO_RIGHTS, "No Rights").depot(windows_depot(1101).manifest(11_001)),
        game(SAME_DEPOTS, "Same Depots").depot(depot(1201).manifest(12_001).size(9)),
        game(OWN_MANIFEST, "Own Manifest")
            .depot(mac_depot(1301).shared_from(BASE).manifest(13_001).size(11)),
        game(THREE_DEPOTS, "Three Depots")
            .depot(mac_depot(1401).manifest(14_001).size(1000))
            .depot(mac_depot(1402).manifest(14_002).size(2000))
            .depot(mac_depot(1403).manifest(14_003).size(4000)),
        game(SELF_SHARED, "Self Shared").depot(mac_depot(1501).shared_from(SELF_SHARED)),
    ];
    let packages = vec![
        PackageSpec::new(1, &[GAME], &[101, 102, 103, 104, 105, 106, 107]),
        PackageSpec::new(3, &[SHARED_GAME], &[301, 302, 303, 304, 305]),
        PackageSpec::new(4, &[REDIST_GAME], &[401]),
        PackageSpec::new(5, &[FREE_GAME], &[]),
        PackageSpec::new(8, &[UNPUBLISHED], &[801]),
        PackageSpec::new(9, &[SOURCE_LACKS_DEPOT], &[901]),
        PackageSpec::new(10, &[SOURCE_UNKNOWN], &[951]),
        PackageSpec::new(11, &[NO_LAUNCH], &[1001, 1002]),
        PackageSpec::new(12, &[NO_RIGHTS], &[]),
        PackageSpec::new(13, &[THREE_DEPOTS], &[1401, 1402, 1403]),
        PackageSpec::new(14, &[SAME_DEPOTS], &[1201]),
        PackageSpec::new(15, &[OWN_MANIFEST], &[1301]),
        PackageSpec::new(16, &[SELF_SHARED], &[1501]),
    ];
    World::new(apps, packages)
}

async fn account() -> (Client, Steam) {
    Steam::account(world()).await
}

/// The folder of an installation of `app_id`'s `platform` build whose receipt
/// lists `manifests`.
fn installed(
    app_id: u32,
    platform: Platform,
    language: &str,
    manifests: &[(u32, u64)],
) -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    let receipt = Installation {
        app_id,
        name: "Game".into(),
        directory: folder.path().to_string_lossy().into_owned(),
        platform,
        language: language.into(),
        total_bytes: 1,
        manifests: manifests
            .iter()
            .map(|&(depot_id, manifest_id)| InstalledDepot {
                depot_id,
                manifest_id,
            })
            .collect(),
    };
    std::fs::write(
        folder.path().join(obelisk_core::install::RECEIPT_FILE),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    folder
}

async fn update_available(
    client: &Client,
    steam: &mut Steam,
    app_id: u32,
    platform: Platform,
    language: &str,
    folder: &Path,
) -> Result<bool, ClientError> {
    steam
        .during(client.update_available(app_id, platform, language, folder))
        .await
}

fn message(error: ClientError) -> String {
    match error {
        ClientError::Failure(message) | ClientError::Cancelled(message) => message,
    }
}

/// What a download that needs a CDN ends as when the content directory names
/// no usable CDN address.
fn no_cdn_address() -> String {
    crate::message::format(
        "backend.steam.content.invalid",
        &["backend.steam.content.invalidCdnAddress".into()],
    )
}

mod update_check {
    use super::*;

    #[tokio::test]
    async fn an_installation_without_a_receipt_of_this_build_needs_its_transfer_and_asks_steam_nothing()
     {
        let (client, mut steam) = account().await;
        let empty = tempfile::tempdir().unwrap();
        let other_platform = installed(GAME, Platform::Mac, ENGLISH, &[(102, 1002), (103, 1003)]);
        let other_game = installed(SHARED_GAME, Platform::Windows, ENGLISH, &[(301, 3001)]);
        for folder in [&empty, &other_platform, &other_game] {
            let update = update_available(
                &client,
                &mut steam,
                GAME,
                Platform::Windows,
                ENGLISH,
                folder.path(),
            );
            assert!(update.await.unwrap());
        }
        assert!(steam.requests().is_empty());
    }

    #[tokio::test]
    async fn the_manifests_installed_are_the_public_ones_of_the_depots_a_download_would_pick() {
        let (client, mut steam) = account().await;
        let windows = installed(
            GAME,
            Platform::Windows,
            ENGLISH,
            &[(101, 1001), (103, 1003)],
        );
        let current = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Windows,
            ENGLISH,
            windows.path(),
        );
        assert!(!current.await.unwrap());
        // The receipt's order does not matter.
        let reversed = installed(
            GAME,
            Platform::Windows,
            ENGLISH,
            &[(103, 1003), (101, 1001)],
        );
        let current = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Windows,
            ENGLISH,
            reversed.path(),
        );
        assert!(!current.await.unwrap());
        // It reads PICS only: the licenses and the game, once.
        assert_eq!(steam.package_queries(), 1);
        assert_eq!(steam.app_queries(), [[GAME]]);
        assert!(steam.depot_keys().is_empty());
        assert!(steam.manifest_codes().is_empty());
        assert_eq!(steam.content_server_queries(), 0);
        assert!(steam.cdn_tokens().is_empty());
    }

    #[tokio::test]
    async fn a_new_manifest_a_missing_depot_or_an_extra_one_is_an_update() {
        let (client, mut steam) = account().await;
        for manifests in [
            // A depot published a new manifest.
            &[(101, 1001), (103, 9999)][..],
            // A depot the download would install is not installed.
            &[(101, 1001)],
            // An installed depot is no longer one the download picks.
            &[(101, 1001), (103, 1003), (104, 1004)],
            &[(101, 1001), (103, 1003), (102, 1002)],
        ] {
            let folder = installed(GAME, Platform::Windows, ENGLISH, manifests);
            let update = update_available(
                &client,
                &mut steam,
                GAME,
                Platform::Windows,
                ENGLISH,
                folder.path(),
            );
            assert!(update.await.unwrap(), "{manifests:?}");
        }
    }

    #[tokio::test]
    async fn the_depots_that_count_follow_the_platform_and_the_language() {
        let (client, mut steam) = account().await;
        let mac = installed(GAME, Platform::Mac, ENGLISH, &[(102, 1002), (103, 1003)]);
        let update = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Mac,
            ENGLISH,
            mac.path(),
        );
        assert!(!update.await.unwrap());
        let french_mac = installed(
            GAME,
            Platform::Mac,
            "french",
            &[(102, 1002), (103, 1003), (105, 1005)],
        );
        let update = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Mac,
            "french",
            french_mac.path(),
        );
        assert!(!update.await.unwrap());
        // The English receipt lacks the French depot the French download adds.
        let update = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Mac,
            "french",
            mac.path(),
        );
        assert!(update.await.unwrap());
        let french_windows = installed(
            GAME,
            Platform::Windows,
            "french",
            &[(101, 1001), (103, 1003), (104, 1004)],
        );
        let update = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Windows,
            "french",
            french_windows.path(),
        );
        assert!(!update.await.unwrap());
    }

    #[tokio::test]
    async fn depots_the_account_does_not_hold_do_not_count() {
        // Depot 106 needs a DLC the account lacks, 107 has no public manifest.
        let (client, mut steam) = account().await;
        let folder = installed(
            GAME,
            Platform::Windows,
            ENGLISH,
            &[(101, 1001), (103, 1003)],
        );
        let update = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Windows,
            ENGLISH,
            folder.path(),
        );
        assert!(!update.await.unwrap());
        let with_dlc = installed(
            GAME,
            Platform::Windows,
            ENGLISH,
            &[(101, 1001), (103, 1003), (106, 1006)],
        );
        let update = update_available(
            &client,
            &mut steam,
            GAME,
            Platform::Windows,
            ENGLISH,
            with_dlc.path(),
        );
        assert!(update.await.unwrap());
    }

    #[tokio::test]
    async fn a_build_steam_would_not_install_is_no_update() {
        let (client, mut steam) = account().await;
        // The game has no Mac depot.
        let mac = installed(REDIST_GAME, Platform::Mac, ENGLISH, &[(401, 4001)]);
        let update = update_available(
            &client,
            &mut steam,
            REDIST_GAME,
            Platform::Mac,
            ENGLISH,
            mac.path(),
        );
        assert!(!update.await.unwrap());
        // The account holds no license of the game.
        let unowned = installed(UNOWNED, Platform::Windows, ENGLISH, &[(701, 7001)]);
        let update = update_available(
            &client,
            &mut steam,
            UNOWNED,
            Platform::Windows,
            ENGLISH,
            unowned.path(),
        );
        assert!(!update.await.unwrap());
        // Its only selected depot is its own, without a manifest of its own.
        let unpublished = installed(UNPUBLISHED, Platform::Windows, ENGLISH, &[(801, 1)]);
        let update = update_available(
            &client,
            &mut steam,
            UNPUBLISHED,
            Platform::Windows,
            ENGLISH,
            unpublished.path(),
        );
        assert!(!update.await.unwrap());
    }

    #[tokio::test]
    async fn a_shared_depot_takes_its_manifest_from_its_source_app_in_one_more_query() {
        let (client, mut steam) = account().await;
        // Depot 302 has no manifest of its own: BASE's depot 302 gives it.
        // Depot 303 has one, so its source is not read for it; depot 305
        // names the game itself and has none.
        let mac = installed(
            SHARED_GAME,
            Platform::Mac,
            ENGLISH,
            &[(302, 3502), (303, 3003), (304, 3004)],
        );
        let update = update_available(
            &client,
            &mut steam,
            SHARED_GAME,
            Platform::Mac,
            ENGLISH,
            mac.path(),
        );
        assert!(!update.await.unwrap());
        assert_eq!(steam.app_queries(), [vec![SHARED_GAME], vec![BASE]]);
        steam.forget_requests();
        let windows = installed(
            SHARED_GAME,
            Platform::Windows,
            ENGLISH,
            &[(301, 3001), (304, 3004)],
        );
        let update = update_available(
            &client,
            &mut steam,
            SHARED_GAME,
            Platform::Windows,
            ENGLISH,
            windows.path(),
        );
        assert!(!update.await.unwrap());
        // No selected depot of the Windows build needs another app, and the
        // game's PICS metadata is still known.
        assert!(steam.requests().is_empty());
        let stale = installed(
            SHARED_GAME,
            Platform::Mac,
            ENGLISH,
            &[(302, 3501), (303, 3003), (304, 3004)],
        );
        let update = update_available(
            &client,
            &mut steam,
            SHARED_GAME,
            Platform::Mac,
            ENGLISH,
            stale.path(),
        );
        assert!(update.await.unwrap());
    }

    #[tokio::test]
    async fn a_shared_depot_with_a_manifest_of_its_own_reads_no_source_app() {
        let (client, mut steam) = account().await;
        let mac = installed(OWN_MANIFEST, Platform::Mac, ENGLISH, &[(1301, 13_001)]);
        let update = update_available(
            &client,
            &mut steam,
            OWN_MANIFEST,
            Platform::Mac,
            ENGLISH,
            mac.path(),
        );
        assert!(!update.await.unwrap());
        assert_eq!(steam.app_queries(), [[OWN_MANIFEST]]);
    }

    #[tokio::test]
    async fn the_common_redistributables_a_game_names_count_without_being_owned_except_the_net_ones()
     {
        // Depot 228990 has no manifest of its own: the redistributables' app
        // gives it. 229000 and 229012 are .NET and XNA, which a download
        // leaves out; 229013 is not.
        let (client, mut steam) = account().await;
        let current = installed(
            REDIST_GAME,
            Platform::Windows,
            ENGLISH,
            &[
                (401, 4001),
                (228_990, 9990),
                (228_989, 2_289_891),
                (229_013, 2_290_131),
            ],
        );
        let update = update_available(
            &client,
            &mut steam,
            REDIST_GAME,
            Platform::Windows,
            ENGLISH,
            current.path(),
        );
        assert!(!update.await.unwrap());
        assert_eq!(
            steam.app_queries(),
            [vec![REDIST_GAME], vec![REDISTRIBUTABLES]]
        );
        let with_net = installed(
            REDIST_GAME,
            Platform::Windows,
            ENGLISH,
            &[
                (401, 4001),
                (228_990, 9990),
                (228_989, 2_289_891),
                (229_000, 2_290_001),
                (229_013, 2_290_131),
            ],
        );
        let update = update_available(
            &client,
            &mut steam,
            REDIST_GAME,
            Platform::Windows,
            ENGLISH,
            with_net.path(),
        );
        assert!(update.await.unwrap());
    }

    #[tokio::test]
    async fn a_checks_licenses_and_app_info_are_remembered_for_the_next() {
        let (client, mut steam) = account().await;
        let folder = installed(
            GAME,
            Platform::Windows,
            ENGLISH,
            &[(101, 1001), (103, 1003)],
        );
        for _ in 0..3 {
            let update = update_available(
                &client,
                &mut steam,
                GAME,
                Platform::Windows,
                ENGLISH,
                folder.path(),
            );
            assert!(!update.await.unwrap());
        }
        assert_eq!(steam.package_queries(), 1);
        assert_eq!(steam.app_queries(), [[GAME]]);
    }

    #[tokio::test]
    async fn a_shared_depot_whose_source_lacks_it_or_is_unknown_fails_the_check() {
        let (client, mut steam) = account().await;
        let lacking = installed(SOURCE_LACKS_DEPOT, Platform::Windows, ENGLISH, &[(901, 1)]);
        let error = update_available(
            &client,
            &mut steam,
            SOURCE_LACKS_DEPOT,
            Platform::Windows,
            ENGLISH,
            lacking.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            message(error),
            crate::message::format(
                "backend.steam.client.sharedManifestMissing",
                &["901".into()]
            )
        );
        let unknown = installed(SOURCE_UNKNOWN, Platform::Windows, ENGLISH, &[(951, 1)]);
        let error = update_available(
            &client,
            &mut steam,
            SOURCE_UNKNOWN,
            Platform::Windows,
            ENGLISH,
            unknown.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            message(error),
            crate::message::format("backend.steam.cm.missingApp", &["960".into()])
        );
    }
}

mod game_page {
    use super::*;

    #[tokio::test]
    async fn the_page_shows_the_mac_build_and_sums_each_builds_selected_depots_sizes() {
        let (client, mut steam) = account().await;
        let details = steam.during(client.details(GAME, ENGLISH)).await.unwrap();
        assert_eq!(details.name, "Game");
        assert_eq!(details.platform, Some(Platform::Mac));
        assert_eq!(details.windows_bytes, 500 + 100);
        assert_eq!(details.macos_bytes, 300 + 100);
        let french = steam.during(client.details(GAME, "french")).await.unwrap();
        assert_eq!(french.windows_bytes, 500 + 100 + 50);
        assert_eq!(french.macos_bytes, 300 + 100 + 30);
        assert_eq!(steam.package_queries(), 1);
        assert_eq!(steam.app_queries(), [[GAME]]);
    }

    #[tokio::test]
    async fn the_page_of_a_game_with_both_builds_reads_the_apps_its_mac_depots_share() {
        // The Mac build's key names the manifests of its shared depots.
        let (client, mut steam) = account().await;
        let details = steam
            .during(client.details(SHARED_GAME, ENGLISH))
            .await
            .unwrap();
        assert_eq!(details.platform, Some(Platform::Mac));
        assert_eq!(details.windows_bytes, 10 + 60);
        assert_eq!(details.macos_bytes, 40 + 60);
        assert_eq!(steam.app_queries(), [vec![SHARED_GAME], vec![BASE]]);
        assert!(steam.depot_keys().is_empty());
        assert!(steam.manifest_codes().is_empty());
        assert_eq!(steam.content_server_queries(), 0);
    }

    #[tokio::test]
    async fn the_page_of_a_game_with_one_build_or_no_mac_program_reads_no_shared_app() {
        let (client, mut steam) = account().await;
        let windows = steam
            .during(client.details(REDIST_GAME, ENGLISH))
            .await
            .unwrap();
        assert_eq!(windows.platform, Some(Platform::Windows));
        assert_eq!(windows.macos_bytes, 0);
        assert_eq!(windows.windows_bytes, 50);
        let no_program = steam
            .during(client.details(NO_LAUNCH, ENGLISH))
            .await
            .unwrap();
        assert_eq!(no_program.platform, Some(Platform::Mac));
        // Neither needed another app: only their own metadata was read.
        assert_eq!(steam.app_queries(), [vec![REDIST_GAME], vec![NO_LAUNCH]]);
    }

    #[tokio::test]
    async fn the_page_of_a_game_with_no_installable_depot_offers_no_build() {
        let (client, mut steam) = account().await;
        let details = steam
            .during(client.details(UNOWNED, ENGLISH))
            .await
            .unwrap();
        assert_eq!(details.platform, None);
        assert_eq!((details.windows_bytes, details.macos_bytes), (0, 0));
    }
}

mod mac_pick {
    use super::*;

    #[tokio::test]
    async fn a_game_without_a_mac_program_is_installed_for_windows_with_no_depot_read() {
        let (client, mut steam) = account().await;
        let platform = steam.during(client.platform(NO_LAUNCH, ENGLISH)).await;
        assert_eq!(platform.unwrap(), Platform::Windows);
        assert_eq!(steam.app_queries(), [[NO_LAUNCH]]);
        assert!(steam.depot_keys().is_empty());
        assert!(steam.manifest_codes().is_empty());
        assert_eq!(steam.content_server_queries(), 0);
    }

    #[tokio::test]
    async fn a_game_with_only_a_windows_build_is_installed_for_windows_with_no_depot_read() {
        let (client, mut steam) = account().await;
        let platform = steam.during(client.platform(REDIST_GAME, ENGLISH)).await;
        assert_eq!(platform.unwrap(), Platform::Windows);
        assert_eq!(steam.app_queries(), [[REDIST_GAME]]);
        assert!(steam.depot_keys().is_empty());
        assert_eq!(steam.content_server_queries(), 0);
    }

    #[tokio::test]
    async fn a_game_the_account_cannot_install_has_no_pick() {
        let (client, mut steam) = account().await;
        let unowned = steam.during(client.platform(UNOWNED, ENGLISH)).await;
        assert_eq!(
            message(unowned.unwrap_err()),
            "backend.steam.client.gameNotOwned"
        );
        // Owned, but no depot the account holds.
        let no_rights = steam.during(client.platform(NO_RIGHTS, ENGLISH)).await;
        assert_eq!(
            message(no_rights.unwrap_err()),
            "backend.steam.client.noCompatibleDepot"
        );
        let language = steam.during(client.platform(GAME, "klingon")).await;
        assert_eq!(
            message(language.unwrap_err()),
            "backend.steam.client.unsupportedPlatformOrLanguage"
        );
    }

    #[tokio::test]
    async fn reading_a_mac_build_asks_for_each_depots_key_and_manifest_code_and_its_servers_once() {
        // The files are read from the CDN, which the content directory gives
        // no address: everything before that is on record.
        let (client, mut steam) = account().await;
        let platform = steam.during(client.platform(SHARED_GAME, ENGLISH)).await;
        assert_eq!(message(platform.unwrap_err()), no_cdn_address());
        // The game, then the app its shared depots come from, once for the
        // whole pick.
        assert_eq!(steam.package_queries(), 1);
        assert_eq!(steam.app_queries(), [vec![SHARED_GAME], vec![BASE]]);
        assert_eq!(steam.content_server_queries(), 1);
        // A shared depot's key is asked for under the game, not its source.
        assert_eq!(
            steam.depot_keys(),
            [(302, SHARED_GAME), (303, SHARED_GAME), (304, SHARED_GAME)]
        );
        // Its manifest code is asked for under the app that authorizes it.
        assert_eq!(
            steam.manifest_codes(),
            [
                (302, BASE, 3502),
                (303, BASE, 3003),
                (304, SHARED_GAME, 3004)
            ]
        );
        assert!(steam.cdn_tokens().is_empty());
    }

    #[tokio::test]
    async fn a_mac_build_is_read_one_depot_at_a_time_after_its_sources_and_servers() {
        let (client, mut steam) = account().await;
        let platform = steam.during(client.platform(SHARED_GAME, ENGLISH)).await;
        assert_eq!(message(platform.unwrap_err()), no_cdn_address());
        // The sorted counts above cannot tell the order Steam is asked in: the
        // licenses, the game, the source app of the shared depots, the servers,
        // then each depot's key and manifest code before the next depot's.
        let requests = steam.requests();
        assert!(matches!(requests[0], Request::Packages(_)), "{requests:?}");
        assert_eq!(
            requests[1..],
            [
                Request::Apps(vec![SHARED_GAME]),
                Request::Apps(vec![BASE]),
                Request::ContentServers,
                Request::DepotKey {
                    depot: 302,
                    app: SHARED_GAME
                },
                Request::ManifestCode {
                    depot: 302,
                    app: BASE,
                    manifest: 3502
                },
                Request::DepotKey {
                    depot: 303,
                    app: SHARED_GAME
                },
                Request::ManifestCode {
                    depot: 303,
                    app: BASE,
                    manifest: 3003
                },
                Request::DepotKey {
                    depot: 304,
                    app: SHARED_GAME
                },
                Request::ManifestCode {
                    depot: 304,
                    app: SHARED_GAME,
                    manifest: 3004
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_mac_build_that_cannot_be_read_keeps_no_verdict_and_is_read_again() {
        let (client, mut steam) = account().await;
        for _ in 0..2 {
            steam.forget_requests();
            let platform = steam.during(client.platform(GAME, ENGLISH)).await;
            assert_eq!(message(platform.unwrap_err()), no_cdn_address());
            assert_eq!(steam.content_server_queries(), 1);
            assert_eq!(steam.depot_keys(), [(102, GAME), (103, GAME)]);
        }
        // The licenses and the game were known the second time.
        assert!(steam.app_queries().is_empty());
        assert_eq!(steam.package_queries(), 0);
    }
}

mod download {
    use super::*;

    async fn download(
        client: &Client,
        steam: &mut Steam,
        app_id: u32,
        platform: Platform,
        parent: &Path,
    ) -> Result<Installation, ClientError> {
        steam
            .during(client.download(app_id, platform, ENGLISH, parent))
            .await
    }

    #[tokio::test]
    async fn a_game_the_account_does_not_own_is_refused_before_its_app_is_read() {
        let (client, mut steam) = account().await;
        let parent = tempfile::tempdir().unwrap();
        let error = download(
            &client,
            &mut steam,
            UNOWNED,
            Platform::Windows,
            parent.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(message(error), "backend.steam.client.gameNotOwned");
        assert_eq!(steam.package_queries(), 1);
        assert!(steam.app_queries().is_empty());
    }

    #[tokio::test]
    async fn a_build_with_no_selected_depot_is_refused_before_servers_or_keys_are_asked() {
        let (client, mut steam) = account().await;
        let parent = tempfile::tempdir().unwrap();
        let error = download(
            &client,
            &mut steam,
            REDIST_GAME,
            Platform::Mac,
            parent.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(message(error), "backend.steam.client.noCompatibleDepot");
        assert_eq!(steam.app_queries(), [[REDIST_GAME]]);
        assert_eq!(steam.content_server_queries(), 0);
        assert!(steam.depot_keys().is_empty());
    }

    #[tokio::test]
    async fn selected_depots_without_a_public_manifest_ask_for_no_key() {
        let (client, mut steam) = account().await;
        let parent = tempfile::tempdir().unwrap();
        let error = download(
            &client,
            &mut steam,
            UNPUBLISHED,
            Platform::Windows,
            parent.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(message(error), "backend.steam.client.noPublicManifest");
        assert!(steam.depot_keys().is_empty());
        assert!(steam.manifest_codes().is_empty());
    }

    #[tokio::test]
    async fn a_download_asks_the_first_depot_for_its_manifest_code_and_stops_where_the_cdn_fails() {
        let (client, mut steam) = account().await;
        let parent = tempfile::tempdir().unwrap();
        let error = download(&client, &mut steam, GAME, Platform::Windows, parent.path())
            .await
            .unwrap_err();
        assert_eq!(message(error), no_cdn_address());
        assert_eq!(steam.package_queries(), 1);
        assert_eq!(steam.app_queries(), [[GAME]]);
        assert_eq!(steam.content_server_queries(), 1);
        // Every selected depot has its key before the first file.
        assert_eq!(steam.depot_keys(), [(101, GAME), (103, GAME)]);
        // Only the first depot's manifest was asked for, as that depot failed.
        assert_eq!(steam.manifest_codes(), [(101, GAME, 1001)]);
        assert!(steam.cdn_tokens().is_empty());
        // The failed Transfer leaves no receipt.
        let folder = parent.path().join("steam-100-windows");
        assert!(!folder.join(obelisk_core::install::RECEIPT_FILE).exists());
    }

    #[tokio::test]
    async fn a_download_whose_shared_depot_is_missing_from_its_source_fails_as_the_update_check_does()
     {
        let (client, mut steam) = account().await;
        let parent = tempfile::tempdir().unwrap();
        let lacking = download(
            &client,
            &mut steam,
            SOURCE_LACKS_DEPOT,
            Platform::Windows,
            parent.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            message(lacking),
            crate::message::format(
                "backend.steam.client.sharedManifestMissing",
                &["901".into()]
            )
        );
        let unknown = download(
            &client,
            &mut steam,
            SOURCE_UNKNOWN,
            Platform::Windows,
            parent.path(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            message(unknown),
            crate::message::format("backend.steam.cm.missingApp", &["960".into()])
        );
    }

    #[tokio::test]
    async fn a_download_of_a_language_picks_that_languages_depots() {
        let (client, mut steam) = account().await;
        let parent = tempfile::tempdir().unwrap();
        let error = steam
            .during(client.download(GAME, Platform::Windows, "french", parent.path()))
            .await
            .unwrap_err();
        assert_eq!(message(error), no_cdn_address());
        assert_eq!(steam.depot_keys(), [(101, GAME), (103, GAME), (104, GAME)]);
    }
}
