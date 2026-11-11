//! An account's Cloud saves between Game sessions through their interface,
//! once for every storefront, on the Cloud sync's harnesses: the uninstall
//! archive, the removal, Cloud maintenance, pending discovery and the
//! indicators.
use super::*;
use crate::cloud::Choice;
use crate::cloud::sync::tests::{
    Harness, Of, RecordOf, archive, idle_guard_released, indicator, listed, maintain,
    next_question, pending, played_offline, player_sees, record_removal, synced, uninstall,
    uploading, vault, work,
};
use crate::cloud_status::SavedState;
use crate::supervisor::client::no_other_session;
use cloud_vault::{EpicScope, Manifest, Scope};
use std::{path::PathBuf, time::Duration};

/// Beside `scope`: another game of the same account, the same game of another
/// account, and a game of the other storefront.
fn neighbours(scope: &StoreScope) -> [StoreScope; 3] {
    let steam = |steam_id, app_id| {
        StoreScope::from(Scope {
            steam_id,
            app_id,
            cloud_app_id: app_id,
        })
    };
    match scope {
        StoreScope::Steam(scope) => [
            steam(scope.steam_id, 7),
            steam(scope.steam_id + 1, scope.app_id),
            EpicScope {
                account_id: "0123456789abcdef0123456789abcdef".into(),
                namespace: "namespace".into(),
                app_name: "game".into(),
            }
            .into(),
        ],
        StoreScope::Epic(scope) => [
            EpicScope {
                app_name: "other-game".into(),
                ..scope.clone()
            }
            .into(),
            EpicScope {
                account_id: "abcdef0123456789abcdef0123456789".into(),
                ..scope.clone()
            }
            .into(),
            steam(0x0110_0001_0000_0001, 10),
        ],
    }
}

/// The account `scope` belongs to.
fn account(scope: &StoreScope) -> AccountIdentity {
    match scope {
        StoreScope::Steam(Scope { steam_id, .. }) => (*steam_id).into(),
        StoreScope::Epic(scope) => scope.account_id.as_str().into(),
    }
}

/// The games of `account` whose Cloud indicator shows: those `requested`,
/// and those pending discovery finds.
fn listing<H: Harness>(
    game: &H,
    account: &AccountIdentity,
    requested: &[StoreScope],
) -> Vec<StoreScope> {
    discovered::<RecordOf<H>>(&game.root(), account, requested)
        .unwrap()
        .into_iter()
        .map(|(scope, _)| scope)
        .collect()
}

pub(crate) async fn pending_discovery_includes_uploads_the_vault_still_holds<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut vault = vault(&game);
    vault.set_preferences(cloud_vault::Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let files = Manifest([(game.slot().into(), vault.stage(b"exit", 2).unwrap())].into());
    let authorization = vault.authorize(game.account()).unwrap();
    let before = vault.baseline().unwrap().clone();
    vault
        .queue_upload(&files, &before, "fixture", authorization)
        .unwrap();
    drop(vault);

    assert_eq!(pending(&game), None);
    assert!(listed(&game));
}

pub(crate) async fn indicators_follow_installed_and_uninstalled_games_without_locking<
    H: Harness,
>() {
    let game = H::game();
    assert_eq!(
        indicator(&game, true).unwrap().state,
        SavedState::NeverSynced
    );
    synced(&game, b"old").await;
    let shown = indicator(&game, true).unwrap();
    assert_eq!(shown.state, SavedState::Synced);
    assert!(shown.synced_at.is_some());

    played_offline(&game, b"played offline").await;
    let held = game.open();
    let shown = indicator(&game, true).unwrap();
    assert_eq!(shown.state, SavedState::Pending);
    assert!(shown.pending_upload);
    drop(held);

    uninstall(&game).await;
    let shown = indicator(&game, false).expect("uninstalled games with pending work are listed");
    assert!(shown.installation_removed);
    assert!(shown.pending_upload);
}

pub(crate) async fn a_windows_game_whose_engine_moved_its_prefix_keeps_its_cloud_saves<
    H: Harness,
>() {
    let mut game = H::game();
    let rosetta = game.root().with_file_name("Prefixes/game");
    game.run_in(&rosetta);
    synced(&game, b"progress").await;
    let (old, uploads) = (game.save(), game.uploads());

    // The Linux engine runs the game in a prefix of its own, still empty.
    let linux = game.root().with_file_name("Prefixes/game-linux/prefix");
    game.run_in(&linux);
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();

    assert_eq!(fs::read(game.save()).unwrap(), b"progress");
    sync.started().unwrap();
    sync.finished(true).await;
    assert_eq!(game.cloud().as_deref(), Some(&b"progress"[..]));
    assert_eq!(game.uploads(), uploads);
    assert_eq!(pending(&game), None);
    assert_eq!(fs::read(old).unwrap(), b"progress");
}

pub(crate) async fn an_uninstall_after_the_engine_moved_the_prefix_keeps_the_archived_saves<
    H: Harness,
>() {
    let mut game = H::game();
    let rosetta = game.root().with_file_name("Prefixes/game");
    game.run_in(&rosetta);
    synced(&game, b"progress").await;
    // The Linux engine readied a prefix of its own, still empty, and the
    // player uninstalls the game before it ever runs there.
    let linux = game.root().with_file_name("Prefixes/game-linux/prefix");
    game.run_in(&linux);

    uninstall(&game).await;

    let vault = vault(&game);
    let archived = vault.local().unwrap();
    assert_eq!(
        vault.read_content(&archived.0[game.slot()]).unwrap(),
        b"progress"
    );
    let work = work(&game).unwrap();
    assert!(work.installation_removed);
    assert_eq!(work.pending, None);
}

pub(crate) async fn an_uninstall_archives_the_saves_of_a_game_never_synced<H: Harness>() {
    let game = H::game();
    fs::write(game.save(), b"progress").unwrap();
    assert_eq!(work(&game), None);

    uninstall(&game).await;

    let vault = vault(&game);
    let archived = vault.local().unwrap();
    assert_eq!(
        vault.read_content(&archived.0[game.slot()]).unwrap(),
        b"progress"
    );
    assert!(vault.pending().is_none());
    let work = work(&game).unwrap();
    assert!(work.installation_removed);
    assert_eq!(work.pending, None, "an uninstall creates no work");
    assert!(!listed(&game));
    assert!(!game.install().exists());
}

pub(crate) async fn an_uninstall_keeps_the_exit_save_and_the_upload_still_waiting<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"exit save").await;
    let exit = uploading(pending(&game)).unwrap();
    // The exit save's upload began, and its outcome is unknown.
    let mut held = vault(&game);
    held.set_preferences(cloud_vault::Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let before = held.baseline().unwrap().clone();
    let authorization = held.authorize(game.account()).unwrap();
    let upload = held
        .queue_upload(&exit, &before, "fixture", authorization)
        .unwrap();
    held.begin_upload(&upload, authorization, &before).unwrap();
    drop(held);
    fs::write(game.save(), b"edited before uninstall").unwrap();

    archive(&game).await.unwrap();
    archive(&game).await.unwrap();
    fs::remove_dir_all(game.install()).unwrap();
    record_removal(&game).await.unwrap();
    record_removal(&game).await.unwrap();

    let work = work(&game).unwrap();
    assert!(work.installation_removed);
    assert_eq!(uploading(work.pending), Some(exit.clone()));
    let vault = vault(&game);
    let waiting = vault.pending().unwrap();
    assert_eq!(waiting.id, upload);
    assert_eq!(waiting.desired, exit);
    assert!(waiting.may_have_committed);
    let archived = vault.local().unwrap();
    assert_eq!(
        vault.read_content(&archived.0[game.slot()]).unwrap(),
        b"edited before uninstall"
    );
    assert_eq!(
        vault.read_content(&exit.0[game.slot()]).unwrap(),
        b"exit save"
    );
    assert!(listed(&game));
}

pub(crate) async fn an_interrupted_uninstall_keeps_the_saves_it_already_removed<H: Harness>() {
    let game = H::game();
    let other = game.save().with_file_name("other.sav");
    let other_slot = game.slot().replace("slot.sav", "other.sav");
    fs::write(game.save(), b"original slot").unwrap();
    fs::write(&other, b"original other").unwrap();
    archive(&game).await.unwrap();
    // The removal deleted one save, then failed with the installation and
    // another save, edited meanwhile, still there.
    fs::remove_file(game.save()).unwrap();
    fs::write(&other, b"edited survivor").unwrap();

    let refused = record_removal(&game).await.unwrap_err();
    archive(&game).await.unwrap();

    assert_eq!(refused, player_sees::<H>(Of::<H>::recovery()));
    let work = work(&game).unwrap();
    assert!(work.installation_removed);
    assert_eq!(work.pending, None);
    assert!(!listed(&game));
    let vault = vault(&game);
    assert!(vault.pending().is_none());
    let archived = vault.local().unwrap();
    assert_eq!(archived.0.len(), 2);
    assert_eq!(
        vault.read_content(&archived.0[game.slot()]).unwrap(),
        b"original slot"
    );
    assert_eq!(
        vault
            .read_content(&archived.0[other_slot.as_str()])
            .unwrap(),
        b"edited survivor"
    );
    assert!(!game.save().exists());
    assert_eq!(fs::read(&other).unwrap(), b"edited survivor");
}

pub(crate) async fn a_failed_uninstall_archive_leaves_the_record_as_it_was<H: Harness>() {
    let mut game = H::game();
    let rosetta = game.root().with_file_name("Prefixes/game");
    game.run_in(&rosetta);
    synced(&game, b"progress").await;
    let record = game
        .scope()
        .directory(&game.root())
        .unwrap()
        .join(RecordOf::<H>::FILE);
    let before = fs::read(&record).unwrap();
    // A Game session that crashed still holds the saves, and the engine has
    // moved the game to a prefix of its own since.
    vault(&game)
        .game_started(&uuid::Uuid::new_v4().to_string())
        .unwrap();
    let linux = game.root().with_file_name("Prefixes/game-linux/prefix");
    game.run_in(&linux);

    let refused = archive(&game).await.unwrap_err();

    assert_eq!(refused, player_sees::<H>(cloud_vault::Error::Busy.into()));
    assert_eq!(fs::read(&record).unwrap(), before);
}

pub(crate) async fn an_uninstall_needs_only_its_storefronts_lock<H: Harness>() {
    let game = H::game();
    synced(&game, b"progress").await;
    let [same_storefront, _, other_storefront] = neighbours(&game.scope());
    let held = Vault::open(&game.root(), other_storefront).unwrap();

    archive(&game).await.unwrap();
    fs::remove_dir_all(game.install()).unwrap();
    record_removal(&game).await.unwrap();

    assert!(work(&game).unwrap().installation_removed);
    let busy = player_sees::<H>(cloud_vault::Error::Busy.into());
    let other_game = Vault::open(&game.root(), same_storefront).unwrap();
    assert_eq!(archive(&game).await.unwrap_err(), busy);
    assert_eq!(record_removal(&game).await.unwrap_err(), busy);
    drop((held, other_game));
}

pub(crate) async fn an_uninstall_goes_on_when_the_storefront_keeps_no_saves_of_this_build<
    H: Harness,
>() {
    // Without Cloud rules, then with rules for the game's other build only.
    for other_build in [false, true] {
        let game = H::game();
        fs::write(game.save(), b"progress").unwrap();
        if other_build {
            game.set_rules_of_another_build();
        } else {
            game.set_rules(false);
        }

        uninstall(&game).await;

        assert!(!game.install().exists());
        assert_eq!(work(&game), None, "nothing was archived: no record");
        assert!(vault(&game).local().is_none());
        assert!(!listed(&game));
    }
}

/// The Wine prefix a Windows build of the game runs in without a choice of
/// engine: the one named after its folder.
fn wine_prefix<H: Harness>(game: &H) -> PathBuf {
    let prefixes = game.root().with_file_name("Prefixes");
    prefixes.join(game.install().file_name().unwrap())
}

pub(crate) async fn an_uninstall_archives_the_saves_where_its_receipts_build_keeps_them<
    H: Harness,
>() {
    // A Mac build, then a Windows build, whose saves live in its Wine prefix.
    for windows in [false, true] {
        let mut game = H::game();
        if windows {
            game.run_in(&wine_prefix(&game));
        }
        fs::write(game.save(), b"progress").unwrap();
        let cloud = of(game.saves());

        cloud.archive(&game.receipt()).await.unwrap();

        let vault = vault(&game);
        let archived = vault.local().unwrap();
        assert_eq!(
            vault.read_content(&archived.0[game.slot()]).unwrap(),
            b"progress"
        );
        assert!(work(&game).unwrap().installation_removed);
    }
}

pub(crate) async fn a_removal_is_recorded_once_the_folder_its_receipt_names_is_gone<H: Harness>() {
    let game = H::game();
    synced(&game, b"progress").await;
    let (cloud, receipt) = (of(game.saves()), game.receipt());

    // The folder is still there: nothing was removed.
    let refused = cloud.removed(&receipt).await.unwrap_err();

    assert_eq!(refused, player_sees::<H>(Of::<H>::recovery()));
    assert!(!work(&game).unwrap().installation_removed);
    fs::remove_dir_all(game.install()).unwrap();
    cloud.removed(&receipt).await.unwrap();
    assert!(work(&game).unwrap().installation_removed);
}

pub(crate) async fn maintenance_syncs_the_saves_where_its_installed_receipts_build_keeps_them<
    H: Harness,
>() {
    let mut game = H::game();
    game.run_in(&wine_prefix(&game));
    synced(&game, b"old").await;
    game.publish(b"from another device");
    idle_guard_released(game.saves().sessions().0);
    let (library_game, receipt) = (game.library_game(), game.receipt());
    let cloud = of(game.saves());

    let maintenance = cloud.maintain(&library_game, Some(&receipt), Arc::clone(game.control()));
    tokio::time::timeout(Duration::from_secs(20), maintenance)
        .await
        .expect("maintenance finished")
        .unwrap();

    assert_eq!(fs::read(game.save()).unwrap(), b"from another device");
}

pub(crate) async fn maintenance_keeps_every_launch_of_its_storefront_out_until_it_ends<
    H: Harness,
>() {
    let game = H::game();
    synced(&game, b"old").await;
    fs::write(game.save(), b"played here").unwrap();
    game.publish(b"played elsewhere");
    let (sessions, helper) = game.saves().sessions();
    let (sessions, helper) = (sessions.to_owned(), helper.to_owned());
    let launch_could_start = move || {
        supervisor::Client::<<H::Saves as Storefront>::Of>::idle_guard(&sessions, &helper).is_ok()
    };
    let control = Arc::clone(game.control());
    let asked = control.status().decision_id;
    let answering = std::thread::spawn({
        let launch_could_start = launch_could_start.clone();
        move || {
            // The sync waits on its question: it still holds the idle guard.
            let question = next_question(&control, asked);
            let could_start = launch_could_start();
            control.answer(question.decision_id, Choice::Local).unwrap();
            could_start
        }
    });

    maintain(&game).await.unwrap();

    assert!(
        !answering.join().unwrap(),
        "maintenance holds its storefront's idle guard"
    );
    idle_guard_released(game.saves().sessions().0);
    assert!(launch_could_start(), "the guard goes with the maintenance");
}

pub(crate) async fn an_unreadable_record_neither_hides_nor_fakes_another_games_pending_work<
    H: Harness,
>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"exit save").await;
    let [other_game, _, _] = neighbours(&game.scope());
    let directory = other_game.directory(&game.root()).unwrap();
    fs::create_dir_all(&directory).unwrap();
    let broken = directory.join(RecordOf::<H>::FILE);
    fs::write(&broken, b"not valid JSON").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let found = listing(&game, &game.account(), &[]);

    assert_eq!(found, [game.scope()]);
    assert_eq!(
        listing(&game, &game.account(), &[other_game]),
        [game.scope()],
        "an unreadable record shows no indicator"
    );
}

pub(crate) async fn a_record_copied_into_another_account_or_game_is_never_read_as_its_own<
    H: Harness,
>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"exit save").await;
    let own = game
        .scope()
        .directory(&game.root())
        .unwrap()
        .join(RecordOf::<H>::FILE);
    let [other_game, other_account, _] = neighbours(&game.scope());

    for copy in [&other_game, &other_account] {
        let directory = copy.directory(&game.root()).unwrap();
        fs::create_dir_all(&directory).unwrap();
        fs::copy(&own, directory.join(RecordOf::<H>::FILE)).unwrap();
        assert!(saved::<RecordOf<H>>(&game.root(), copy).is_err());
    }

    assert_eq!(listing(&game, &game.account(), &[]), [game.scope()]);
    assert!(listing(&game, &account(&other_account), &[]).is_empty());
}

/// Instantiates every scenario of the Cloud saves between Game sessions for
/// one storefront's harness.
macro_rules! saves_suite {
    ($harness:ty) => {
        $crate::cloud::saves::tests::saves_suite!(
            $harness;
            pending_discovery_includes_uploads_the_vault_still_holds,
            indicators_follow_installed_and_uninstalled_games_without_locking,
            a_windows_game_whose_engine_moved_its_prefix_keeps_its_cloud_saves,
            an_uninstall_after_the_engine_moved_the_prefix_keeps_the_archived_saves,
            an_uninstall_archives_the_saves_of_a_game_never_synced,
            an_uninstall_keeps_the_exit_save_and_the_upload_still_waiting,
            an_interrupted_uninstall_keeps_the_saves_it_already_removed,
            a_failed_uninstall_archive_leaves_the_record_as_it_was,
            an_uninstall_needs_only_its_storefronts_lock,
            an_uninstall_goes_on_when_the_storefront_keeps_no_saves_of_this_build,
            an_uninstall_archives_the_saves_where_its_receipts_build_keeps_them,
            a_removal_is_recorded_once_the_folder_its_receipt_names_is_gone,
            maintenance_syncs_the_saves_where_its_installed_receipts_build_keeps_them,
            maintenance_keeps_every_launch_of_its_storefront_out_until_it_ends,
            an_unreadable_record_neither_hides_nor_fakes_another_games_pending_work,
            a_record_copied_into_another_account_or_game_is_never_read_as_its_own,
        );
    };
    ($harness:ty; $($scenario:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $scenario() {
                $crate::cloud::saves::tests::$scenario::<$harness>().await
            }
        )*
    };
}
pub(crate) use saves_suite;
