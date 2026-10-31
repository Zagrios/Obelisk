//! The Installation receipts an account keeps, seen through `Installations`,
//! the games folder and the two files on disk: the receipts shown, those
//! whose disk is not connected (kept in the file, never shown), and the
//! incomplete installations. It covers the flows of an account (Transfers,
//! uninstalls, discards, moves, starting again) and a disk that leaves or
//! returns while one of them runs; `receipt_book::tests` covers the book's
//! own rules at its interface.
//!
//! A disk that leaves and returns is a `Disk`, a folder that stands for it:
//! `games_folder::disconnected` tells it away while it is unplugged. A disk
//! that was never there stands for one that left, where its coming back does
//! not matter.
use super::*;
use crate::games_folder::disks::Disk;
use serde_json::json;
use std::slice::from_ref;

fn receipts_file(fixture: &Fixture) -> PathBuf {
    fixture.root.join("steam-installations-42.json")
}

fn incomplete_file(fixture: &Fixture) -> PathBuf {
    fixture.root.join("steam-installations-42.incomplete.json")
}

/// The receipts `file` keeps, in its order; none when it is not there.
fn kept(file: &Path) -> Vec<Receipt<Steam>> {
    load(file).unwrap_or_else(|error| panic!("{}: {error}", file.display()))
}

/// What `file` holds, as the JSON an older Obelisk reads.
fn written(file: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
}

/// Writes `receipts` as an earlier Obelisk left them.
fn keep(file: &Path, receipts: &[Receipt<Steam>]) {
    save(file, receipts).unwrap();
}

/// `receipts` by id, where a file's order is not the point.
fn by_id(mut receipts: Vec<Receipt<Steam>>) -> Vec<Receipt<Steam>> {
    receipts.sort_by_key(Receipt::id);
    receipts
}

fn title(app_id: u32) -> &'static str {
    match app_id {
        620 => "Portal 2",
        570 => "Dota 2",
        _ => "Another game",
    }
}

fn offer(app_id: u32) -> Offer<Steam> {
    Offer {
        product: product(app_id),
        name: title(app_id).into(),
    }
}

/// The receipt of `app_id`'s installation of `platform`, kept in `directory`.
fn receipt_in(app_id: u32, platform: Platform, directory: &Path) -> Receipt<Steam> {
    Receipt {
        product: product(app_id),
        name: title(app_id).into(),
        directory: directory.to_string_lossy().into_owned(),
        platform,
        total_bytes: 100,
        needs_verification: false,
    }
}

/// The folder `app_id`'s installation of `platform` has in Obelisk's library.
fn library_folder(fixture: &Fixture, app_id: u32, platform: Platform) -> PathBuf {
    fixture
        .root
        .join("Games/Steam")
        .join(steam_kit::client::installation_directory_name(
            app_id, platform,
        ))
}

/// An installation in Obelisk's library: its folder is there.
fn installed(fixture: &Fixture, app_id: u32, platform: Platform) -> Receipt<Steam> {
    let folder = library_folder(fixture, app_id, platform);
    std::fs::create_dir_all(&folder).unwrap();
    receipt_in(app_id, platform, &folder)
}

/// The receipt of an installation on an external disk that is not connected.
fn on_missing_disk(app_id: u32, platform: Platform) -> Receipt<Steam> {
    let folder = Path::new("/Volumes/Obelisk Pin Disk That Is Not There/Obelisk Games/Steam").join(
        steam_kit::client::installation_directory_name(app_id, platform),
    );
    receipt_in(app_id, platform, &folder)
}

/// The same account started from its files, as Obelisk starts again. The
/// fixture's own installations, started before the files were written, hold
/// no receipts when the fixture was built without any.
fn start(fixture: &Fixture) -> Installations<Steam> {
    fixture.reload_through(receipts_file(fixture), fixture.fake.clone())
}

/// The installation of `app_id`'s `platform` on `disk`, folder made.
fn on_disk(disk: &Disk, app_id: u32, platform: Platform) -> Receipt<Steam> {
    let folder = disk.mount().join("Obelisk Games/Steam").join(
        steam_kit::client::installation_directory_name(app_id, platform),
    );
    std::fs::create_dir_all(&folder).unwrap();
    receipt_in(app_id, platform, &folder)
}

// MARK: Transfers

#[tokio::test]
async fn a_finished_transfer_shows_its_receipt_and_the_file_keeps_it_for_the_next_start() {
    let fixture = Fixture::new(false).await;
    fixture.installations.install(offer(620));
    idle(&fixture.installations).await;

    let state = fixture.installations.state();
    let folder = library_folder(&fixture, 620, Platform::Mac);
    assert_eq!(ids(&state.receipts), ["620-macos"]);
    assert_eq!(state.receipts[0].directory, folder.to_string_lossy());
    assert!(state.incomplete.is_empty());
    assert_eq!(
        written(&receipts_file(&fixture)),
        json!([{
            "appId": 620,
            "language": "french",
            "name": "Portal 2",
            "directory": folder.to_string_lossy(),
            "platform": "macos",
            "totalBytes": 100
        }])
    );
    assert!(kept(&incomplete_file(&fixture)).is_empty());

    let restarted = fixture.reload(receipts_file(&fixture));
    assert_eq!(restarted.state().receipts, state.receipts);
    assert!(restarted.state().incomplete.is_empty());
}

#[tokio::test]
async fn a_new_installation_is_kept_as_incomplete_in_its_own_file_until_its_transfer_finishes() {
    let fixture = Fixture::new(false).await;
    fixture.fake.hold();
    fixture.installations.install(offer(620));
    eventually(|| fixture.fake.calls().len() == 1).await;

    let folder = library_folder(&fixture, 620, Platform::Mac);
    assert_eq!(
        written(&incomplete_file(&fixture)),
        json!([{
            "appId": 620,
            "language": "french",
            "name": "Portal 2",
            "directory": folder.to_string_lossy(),
            "platform": "macos",
            "totalBytes": 0,
            "needsVerification": true
        }])
    );
    assert!(
        kept(&receipts_file(&fixture)).is_empty(),
        "an installed game's file never names it"
    );
    assert_eq!(
        ids(&fixture.installations.state().incomplete),
        ["620-macos"]
    );

    fixture.fake.release();
    idle(&fixture.installations).await;
    assert!(fixture.installations.state().incomplete.is_empty());
    assert!(kept(&incomplete_file(&fixture)).is_empty());
    assert_eq!(ids(&kept(&receipts_file(&fixture))), ["620-macos"]);
}

#[tokio::test]
async fn a_transfer_that_finishes_over_an_installed_game_replaces_its_receipt_which_then_comes_last()
 {
    let fixture = Fixture::with_both_builds().await;
    assert_eq!(
        ids(&fixture.installations.state().receipts),
        ["620-macos", "620-windows"]
    );

    fixture.installations.update("620-macos");
    idle(&fixture.installations).await;

    let state = fixture.installations.state();
    assert_eq!(ids(&state.receipts), ["620-windows", "620-macos"]);
    assert_eq!(kept(&receipts_file(&fixture)), state.receipts);
}

#[tokio::test]
async fn a_finished_transfer_saves_the_shown_receipts_then_those_of_a_disk_that_is_away() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let away = on_missing_disk(620, Platform::Windows);
    keep(&receipts_file(&fixture), &[away.clone(), mac.clone()]);
    let installations = start(&fixture);
    assert_eq!(installations.state().receipts, from_ref(&mac));
    assert_eq!(
        kept(&receipts_file(&fixture)),
        [away.clone(), mac.clone()],
        "starting rewrites nothing"
    );

    installations.install(offer(570));
    idle(&installations).await;

    let shown = installations.state().receipts;
    assert_eq!(ids(&shown), ["620-macos", "570-macos"]);
    let mut saved = shown;
    saved.push(away);
    assert_eq!(kept(&receipts_file(&fixture)), saved);
}

#[tokio::test]
async fn a_finished_transfer_forgets_its_incomplete_record_but_not_those_of_a_disk_that_is_away() {
    let fixture = Fixture::new(false).await;
    let mut half = on_missing_disk(620, Platform::Windows);
    half.needs_verification = true;
    keep(&incomplete_file(&fixture), &[half.clone()]);
    let installations = start(&fixture);
    assert!(installations.state().incomplete.is_empty());

    fixture.fake.hold();
    installations.install(offer(620));
    eventually(|| fixture.fake.calls().len() == 1).await;
    let recorded = installations.state().incomplete;
    assert_eq!(ids(&recorded), ["620-macos"]);
    let mut saved = recorded;
    saved.push(half.clone());
    assert_eq!(kept(&incomplete_file(&fixture)), saved);

    fixture.fake.release();
    idle(&installations).await;
    assert!(installations.state().incomplete.is_empty());
    assert_eq!(kept(&incomplete_file(&fixture)), [half]);
    assert_eq!(ids(&installations.state().receipts), ["620-macos"]);
}

#[tokio::test]
async fn a_new_installation_is_recorded_beside_the_same_game_incomplete_on_a_disk_that_is_away() {
    let fixture = Fixture::new(false).await;
    let mut waiting = on_missing_disk(620, Platform::Mac);
    waiting.needs_verification = true;
    keep(&incomplete_file(&fixture), from_ref(&waiting));
    let installations = start(&fixture);
    assert!(installations.state().incomplete.is_empty());

    fixture.fake.hold();
    installations.install(offer(620));
    eventually(|| fixture.fake.calls().len() == 1).await;
    let recorded = installations.state().incomplete;
    assert_eq!(ids(&recorded), ["620-macos"]);
    assert_ne!(recorded[0], waiting);
    assert_eq!(
        kept(&incomplete_file(&fixture)),
        [recorded[0].clone(), waiting.clone()],
        "the download Obelisk quits during still resumes"
    );

    fixture.fake.release();
    idle(&installations).await;
    assert!(installations.state().incomplete.is_empty());
    assert_eq!(kept(&incomplete_file(&fixture)), [waiting]);
}

#[tokio::test]
async fn a_finished_transfer_whose_receipts_cannot_be_saved_shows_its_receipt_and_warns() {
    let fixture = Fixture::new(false).await;
    std::fs::create_dir_all(receipts_file(&fixture)).unwrap();

    fixture.installations.install(offer(620));
    idle(&fixture.installations).await;

    let state = fixture.installations.state();
    assert_eq!(ids(&state.receipts), ["620-macos"]);
    assert_eq!(
        state.warning.map(|(_, key)| key).as_deref(),
        Some(<Steam as Storefront>::RECEIPTS_NOT_SAVED)
    );
}

// MARK: Uninstalls and discards

#[tokio::test]
async fn an_uninstall_forgets_its_receipt_in_the_list_and_the_file_but_keeps_those_of_a_disk_that_is_away()
 {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let windows = installed(&fixture, 620, Platform::Windows);
    let away = on_missing_disk(570, Platform::Mac);
    keep(
        &receipts_file(&fixture),
        &[mac.clone(), away.clone(), windows.clone()],
    );
    let installations = start(&fixture);
    fixture.fake.with(|script| script.newer.push(620));
    installations.check_updates();
    eventually(|| installations.state().updates.len() == 2).await;

    installations.uninstall("620-macos");
    assert_eq!(installations.state().uninstall_game, Some(620));
    eventually(|| installations.state().uninstall_game.is_none()).await;

    let state = installations.state();
    assert_eq!(state.receipts, from_ref(&windows));
    assert_eq!(state.updates, ["620-windows"]);
    assert_eq!(state.report, None);
    assert!(!Path::new(&mac.directory).exists());
    assert!(Path::new(&windows.directory).is_dir());
    assert_eq!(
        kept(&receipts_file(&fixture)),
        [windows.clone(), away.clone()]
    );
    assert_eq!(fixture.fake.calls(), ["archive:620", "removed:620"]);

    let restarted = fixture.reload(receipts_file(&fixture));
    assert_eq!(restarted.state().receipts, from_ref(&windows));

    // A later save still names what is not shown.
    installations.update("620-windows");
    idle(&installations).await;
    assert_eq!(kept(&receipts_file(&fixture)), [windows, away]);
}

#[tokio::test]
async fn the_receipt_stays_shown_and_saved_while_the_cloud_archive_takes_the_saves() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let away = on_missing_disk(570, Platform::Mac);
    keep(&receipts_file(&fixture), &[mac.clone(), away.clone()]);
    let installations = start(&fixture);
    fixture.fake.hold_archive();

    installations.uninstall("620-macos");
    eventually(|| fixture.fake.calls() == ["archive:620"]).await;
    let state = installations.state();
    assert_eq!(state.uninstall_game, Some(620));
    assert_eq!(state.receipts, from_ref(&mac));
    assert_eq!(kept(&receipts_file(&fixture)), [mac.clone(), away.clone()]);
    assert!(Path::new(&mac.directory).is_dir());

    fixture.fake.release_archive();
    eventually(|| installations.state().uninstall_game.is_none()).await;
    assert!(installations.state().receipts.is_empty());
    assert_eq!(kept(&receipts_file(&fixture)), [away]);
}

#[tokio::test]
async fn an_uninstall_that_sign_out_overtakes_changes_neither_the_list_nor_the_file() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let away = on_missing_disk(570, Platform::Mac);
    keep(&receipts_file(&fixture), &[mac.clone(), away.clone()]);
    let installations = start(&fixture);
    fixture.fake.hold_archive();

    installations.uninstall("620-macos");
    eventually(|| fixture.fake.calls() == ["archive:620"]).await;
    installations.signed_out();
    fixture.fake.release_archive();
    eventually(|| installations.state().uninstall_game.is_none()).await;

    let state = installations.state();
    assert_eq!(state.receipts, from_ref(&mac));
    assert_eq!(state.report, None);
    assert!(Path::new(&mac.directory).is_dir());
    assert_eq!(kept(&receipts_file(&fixture)), [mac, away]);
    assert_eq!(fixture.fake.calls(), ["archive:620"]);
}

#[tokio::test]
async fn a_failed_uninstall_leaves_the_list_and_the_file_as_they_were() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let windows = installed(&fixture, 620, Platform::Windows);
    let away = on_missing_disk(570, Platform::Mac);
    let before = [mac.clone(), windows.clone(), away];
    keep(&receipts_file(&fixture), &before);
    let installations = start(&fixture);
    fixture.fake.with(|script| {
        script.archive = Some(Failure::Failed("backend.steam.cloud.network".into()))
    });

    installations.uninstall("620-macos");
    eventually(|| installations.state().uninstall_game.is_none()).await;

    let state = installations.state();
    assert_eq!(
        state.report,
        Some(Report::Uninstall("backend.steam.cloud.network".into()))
    );
    assert_eq!(state.receipts, [mac.clone(), windows]);
    assert!(Path::new(&mac.directory).is_dir());
    assert_eq!(kept(&receipts_file(&fixture)), before);
}

#[tokio::test]
async fn a_cancelled_uninstall_leaves_the_list_and_the_file_as_they_were_without_a_report() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let away = on_missing_disk(570, Platform::Mac);
    let before = [mac.clone(), away];
    keep(&receipts_file(&fixture), &before);
    let installations = start(&fixture);
    fixture
        .fake
        .with(|script| script.archive = Some(Failure::Cancelled));

    installations.uninstall("620-macos");
    eventually(|| installations.state().uninstall_game.is_none()).await;

    let state = installations.state();
    assert_eq!(state.report, None);
    assert_eq!(state.receipts, from_ref(&mac));
    assert!(Path::new(&mac.directory).is_dir());
    assert_eq!(kept(&receipts_file(&fixture)), before);
}

#[tokio::test]
async fn a_failed_uninstall_stops_showing_a_receipt_whose_folder_went_meanwhile() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let windows = installed(&fixture, 620, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    let installations = start(&fixture);
    fixture.fake.with(|script| {
        script.archive = Some(Failure::Failed("backend.steam.cloud.network".into()))
    });
    std::fs::remove_dir_all(&windows.directory).unwrap();

    installations.uninstall("620-macos");
    eventually(|| installations.state().uninstall_game.is_none()).await;

    assert_eq!(installations.state().receipts, [mac]);
}

#[tokio::test]
async fn an_uninstall_that_sign_out_overtakes_after_the_files_went_leaves_the_list_and_the_file_agreeing()
 {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let away = on_missing_disk(570, Platform::Mac);
    keep(&receipts_file(&fixture), &[mac.clone(), away.clone()]);
    let installations = start(&fixture);
    fixture.fake.hold_removed();

    installations.uninstall("620-macos");
    eventually(|| fixture.fake.calls() == ["archive:620", "removed:620"]).await;
    let state = installations.state();
    assert_eq!(state.uninstall_game, Some(620));
    assert!(
        state.receipts.is_empty(),
        "the receipt goes with the files, before the Cloud record and the prefix"
    );
    assert!(!Path::new(&mac.directory).exists());
    assert_eq!(kept(&receipts_file(&fixture)), from_ref(&away));

    installations.signed_out();
    fixture.fake.release_removed();
    eventually(|| installations.state().uninstall_game.is_none()).await;

    let state = installations.state();
    assert!(state.receipts.is_empty());
    assert_eq!(state.report, None);
    assert_eq!(kept(&receipts_file(&fixture)), from_ref(&away));
    assert_eq!(start(&fixture).state().receipts, []);
}

#[tokio::test]
async fn discarding_an_incomplete_installation_keeps_those_of_a_disk_that_is_away_in_the_file() {
    let fixture = Fixture::new(false).await;
    let folder = stopped_install(&fixture).await;
    let mut half = on_missing_disk(620, Platform::Windows);
    half.needs_verification = true;
    let mut saved = kept(&incomplete_file(&fixture));
    saved.push(half.clone());
    keep(&incomplete_file(&fixture), &saved);
    let installations = start(&fixture);
    assert_eq!(ids(&installations.state().incomplete), ["620-macos"]);

    installations.discard("620-macos");
    eventually(|| installations.state().incomplete.is_empty()).await;

    assert!(!folder.exists());
    assert_eq!(kept(&incomplete_file(&fixture)), [half.clone()]);
    assert!(kept(&receipts_file(&fixture)).is_empty());

    let restarted = fixture.reload(receipts_file(&fixture));
    assert!(restarted.state().incomplete.is_empty());
    assert_eq!(kept(&incomplete_file(&fixture)), [half]);
}

#[tokio::test]
async fn discarding_an_incomplete_installation_keeps_the_same_game_incomplete_on_a_disk_that_is_away()
 {
    let fixture = Fixture::new(false).await;
    let disk = Disk::plugged();
    let waiting = half(on_disk(&disk, 620, Platform::Mac));
    let here = half(installed(&fixture, 620, Platform::Mac));
    keep(&incomplete_file(&fixture), &[waiting.clone(), here.clone()]);
    disk.unplug();
    let installations = start(&fixture);
    assert_eq!(installations.state().incomplete, from_ref(&here));

    installations.discard("620-macos");
    eventually(|| installations.state().incomplete.is_empty()).await;

    assert!(!Path::new(&here.directory).exists());
    assert_eq!(kept(&incomplete_file(&fixture)), from_ref(&waiting));
    assert!(start(&fixture).state().incomplete.is_empty());

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().incomplete, from_ref(&waiting));
    assert_eq!(kept(&incomplete_file(&fixture)), from_ref(&waiting));
    assert!(Path::new(&waiting.directory).is_dir());
}

#[tokio::test]
async fn discarding_one_of_two_incomplete_installations_of_a_game_keeps_the_other() {
    let fixture = Fixture::new(false).await;
    let disk = Disk::plugged();
    let here = half(installed(&fixture, 620, Platform::Mac));
    let other = half(on_disk(&disk, 620, Platform::Mac));
    keep(&incomplete_file(&fixture), &[here.clone(), other.clone()]);
    let installations = start(&fixture);
    assert_eq!(
        installations.state().incomplete,
        [here.clone(), other.clone()]
    );

    installations.discard("620-macos");
    eventually(|| installations.state().incomplete.len() == 1).await;

    assert!(!Path::new(&here.directory).exists());
    assert!(Path::new(&other.directory).is_dir());
    assert_eq!(installations.state().incomplete, from_ref(&other));
    assert_eq!(kept(&incomplete_file(&fixture)), from_ref(&other));
    assert_eq!(start(&fixture).state().incomplete, from_ref(&other));
}

// MARK: Starting again

#[tokio::test]
async fn starting_again_restores_the_shown_offline_and_incomplete_receipts_and_rewrites_nothing() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let windows = installed(&fixture, 620, Platform::Windows);
    let away = on_missing_disk(440, Platform::Windows);
    let mut half = installed(&fixture, 570, Platform::Mac);
    half.needs_verification = true;
    let mut half_away = on_missing_disk(570, Platform::Windows);
    half_away.needs_verification = true;
    // Pretty-printed: any rewrite shows in the bytes.
    let receipts =
        serde_json::to_vec_pretty(&[away.clone(), mac.clone(), windows.clone()]).unwrap();
    let incomplete = serde_json::to_vec_pretty(&[half_away.clone(), half.clone()]).unwrap();
    std::fs::write(receipts_file(&fixture), &receipts).unwrap();
    std::fs::write(incomplete_file(&fixture), &incomplete).unwrap();

    let installations = start(&fixture);
    let state = installations.state();
    assert_eq!(state.receipts, [mac.clone(), windows.clone()]);
    assert_eq!(state.incomplete, [half.clone()]);
    assert_eq!(std::fs::read(receipts_file(&fixture)).unwrap(), receipts);
    assert_eq!(
        std::fs::read(incomplete_file(&fixture)).unwrap(),
        incomplete
    );

    // The next save keeps what is not shown.
    installations.update("620-macos");
    idle(&installations).await;
    assert_eq!(
        ids(&installations.state().receipts),
        ["620-windows", "620-macos"]
    );
    assert_eq!(
        kept(&receipts_file(&fixture)),
        [windows.clone(), mac.clone(), away.clone()]
    );
    assert_eq!(
        by_id(kept(&incomplete_file(&fixture))),
        by_id(vec![half_away, half.clone()])
    );

    let restarted = fixture.reload(receipts_file(&fixture));
    assert_eq!(restarted.state().receipts, [windows, mac]);
    assert_eq!(restarted.state().incomplete, [half]);
}

#[tokio::test]
async fn a_disk_change_forgets_a_receipt_whose_folder_went_outside_obelisk_and_saves_the_rest() {
    let fixture = Fixture::with_both_builds().await;
    let state = fixture.installations.state();
    std::fs::remove_dir_all(&state.receipts[1].directory).unwrap();
    assert_eq!(state.receipts.len(), 2, "nothing told Obelisk yet");

    fixture.folder.volumes_changed();

    let after = fixture.installations.state();
    assert_eq!(ids(&after.receipts), ["620-macos"]);
    assert!(after.revision > state.revision);
    assert_eq!(kept(&receipts_file(&fixture)), after.receipts);
}

#[tokio::test]
async fn a_disk_change_that_changes_nothing_publishes_nothing() {
    let fixture = Fixture::new(true).await;
    let away = on_missing_disk(570, Platform::Mac);
    let mut saved = kept(&receipts_file(&fixture));
    saved.push(away);
    keep(&receipts_file(&fixture), &saved);
    let installations = start(&fixture);
    let before = installations.state();

    fixture.folder.volumes_changed();

    assert_eq!(installations.state(), before);
    assert_eq!(kept(&receipts_file(&fixture)), saved);
}

#[tokio::test]
async fn a_disk_that_leaves_while_idle_hides_its_receipts_and_the_file_keeps_them_until_it_returns()
{
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 620, Platform::Windows);
    keep(&receipts_file(&fixture), &[windows.clone(), mac.clone()]);
    let installations = start(&fixture);
    assert_eq!(
        installations.state().receipts,
        [windows.clone(), mac.clone()]
    );

    disk.unplug();
    assert_eq!(
        installations.state().receipts.len(),
        2,
        "nothing told Obelisk yet"
    );
    let before = installations.state().revision;
    fixture.folder.volumes_changed();
    let state = installations.state();
    assert_eq!(state.receipts, from_ref(&mac));
    assert!(state.revision > before);
    assert_eq!(
        by_id(kept(&receipts_file(&fixture))),
        by_id(vec![mac.clone(), windows.clone()])
    );

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(
        installations.state().receipts,
        [mac.clone(), windows.clone()],
        "a returning receipt comes after those shown"
    );
    assert_eq!(
        by_id(kept(&receipts_file(&fixture))),
        by_id(vec![mac, windows])
    );
}

#[tokio::test]
async fn an_account_started_while_a_disk_is_away_shows_its_receipts_once_it_returns() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 620, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    disk.unplug();

    let installations = start(&fixture);
    assert_eq!(installations.state().receipts, from_ref(&mac));
    assert_eq!(
        kept(&receipts_file(&fixture)),
        [mac.clone(), windows.clone()],
        "starting rewrites nothing"
    );

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, [mac, windows]);
}

#[tokio::test]
async fn a_save_while_a_disk_is_away_keeps_its_receipts_which_come_back_after_the_others() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 620, Platform::Windows);
    keep(&receipts_file(&fixture), &[windows.clone(), mac.clone()]);
    let installations = start(&fixture);
    disk.unplug();
    fixture.folder.volumes_changed();

    installations.install(offer(570));
    idle(&installations).await;
    let shown = installations.state().receipts;
    assert_eq!(ids(&shown), ["620-macos", "570-macos"]);
    let mut saved = shown.clone();
    saved.push(windows.clone());
    assert_eq!(kept(&receipts_file(&fixture)), saved);

    disk.plug();
    fixture.folder.volumes_changed();
    let mut back = shown;
    back.push(windows);
    assert_eq!(installations.state().receipts, back);
    assert_eq!(kept(&receipts_file(&fixture)), back);
}

#[tokio::test]
async fn an_uninstall_while_a_disk_is_away_leaves_its_receipts_to_show_when_it_returns() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    let installations = start(&fixture);
    disk.unplug();
    fixture.folder.volumes_changed();

    installations.uninstall("620-macos");
    eventually(|| installations.state().uninstall_game.is_none()).await;
    assert!(installations.state().receipts.is_empty());
    assert_eq!(kept(&receipts_file(&fixture)), from_ref(&windows));

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, from_ref(&windows));
    assert_eq!(kept(&receipts_file(&fixture)), [windows]);
}

#[tokio::test]
async fn the_incomplete_installations_of_a_disk_that_leaves_wait_in_their_file_until_it_returns() {
    let fixture = Fixture::new(false).await;
    let disk = Disk::plugged();
    let mut half = on_disk(&disk, 570, Platform::Mac);
    half.needs_verification = true;
    keep(&incomplete_file(&fixture), &[half.clone()]);
    let installations = start(&fixture);
    assert_eq!(installations.state().incomplete, [half.clone()]);

    disk.unplug();
    fixture.folder.volumes_changed();
    assert!(installations.state().incomplete.is_empty());
    assert_eq!(kept(&incomplete_file(&fixture)), [half.clone()]);

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().incomplete, [half.clone()]);
    assert_eq!(kept(&incomplete_file(&fixture)), [half]);
    assert!(kept(&receipts_file(&fixture)).is_empty());
}

#[tokio::test]
async fn choosing_a_folder_counts_only_the_installations_whose_disk_is_connected() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 620, Platform::Windows);
    let away = on_missing_disk(570, Platform::Mac);
    keep(&receipts_file(&fixture), &[mac, windows, away]);
    let _installations = start(&fixture);
    let (chosen, _) = fixture.elsewhere();

    assert_eq!(fixture.folder.inspect(&chosen).unwrap().games, 2);

    disk.unplug();
    fixture.folder.volumes_changed();
    assert_eq!(fixture.folder.inspect(&chosen).unwrap().games, 1);

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(fixture.folder.inspect(&chosen).unwrap().games, 2);
}

#[tokio::test]
async fn a_disk_that_leaves_during_a_transfer_hides_its_receipts_and_the_transfer_still_saves_them()
{
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    keep(&receipts_file(&fixture), &[windows.clone(), mac.clone()]);
    let installations = start(&fixture);
    fixture.fake.hold();
    installations.update("620-macos");
    eventually(|| fixture.fake.calls().len() == 1).await;

    disk.unplug();
    fixture.folder.volumes_changed();
    assert_eq!(ids(&installations.state().receipts), ["620-macos"]);
    fixture.fake.release();
    idle(&installations).await;

    assert_eq!(installations.state().receipts, from_ref(&mac));
    assert_eq!(
        kept(&receipts_file(&fixture)),
        [mac.clone(), windows.clone()]
    );
    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, [mac, windows]);
}

#[tokio::test]
async fn a_disk_that_returns_during_a_transfer_shows_its_receipts_and_the_transfer_keeps_them() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    disk.unplug();
    let installations = start(&fixture);
    assert_eq!(installations.state().receipts, from_ref(&mac));
    fixture.fake.hold();
    installations.update("620-macos");
    eventually(|| fixture.fake.calls().len() == 1).await;

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(
        ids(&installations.state().receipts),
        ["620-macos", "570-windows"]
    );
    fixture.fake.release();
    idle(&installations).await;

    let state = installations.state();
    assert_eq!(state.receipts, [windows.clone(), mac.clone()]);
    assert_eq!(kept(&receipts_file(&fixture)), [windows, mac]);
}

// MARK: Moves

#[tokio::test]
async fn a_move_updates_the_directory_of_the_receipt_where_it_stands_in_the_list_and_in_the_file() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let (chosen, moved_mac) = fixture.elsewhere();
    // Already in the games folder chosen: only the Mac build moves.
    let inside = moved_mac.with_file_name("steam-620-windows");
    std::fs::create_dir_all(&inside).unwrap();
    let windows = receipt_in(620, Platform::Windows, &inside);
    let away = on_missing_disk(570, Platform::Mac);
    keep(
        &receipts_file(&fixture),
        &[mac.clone(), away.clone(), windows.clone()],
    );
    let installations = start(&fixture);

    fixture.folder.choose(&chosen).unwrap();
    eventually(|| installations.state().receipts[0].directory == moved_mac.to_string_lossy()).await;
    eventually(|| fixture.moves().is_empty()).await;

    let moved = Receipt {
        directory: moved_mac.to_string_lossy().into_owned(),
        ..mac
    };
    let state = installations.state();
    assert_eq!(state.receipts, [moved.clone(), windows.clone()]);
    assert_eq!(state.report, None);
    assert_eq!(kept(&receipts_file(&fixture)), [moved, windows, away]);
}

#[tokio::test]
async fn a_move_updates_the_directory_of_the_receipt_in_place_and_the_file_keeps_the_receipts_of_a_disk_that_is_away()
 {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let windows = installed(&fixture, 620, Platform::Windows);
    let away = on_missing_disk(570, Platform::Mac);
    keep(
        &receipts_file(&fixture),
        &[mac.clone(), away.clone(), windows.clone()],
    );
    let installations = start(&fixture);
    let (chosen, moved_mac) = fixture.elsewhere();
    let moved_windows = moved_mac.with_file_name("steam-620-windows");

    fixture.folder.choose(&chosen).unwrap();
    eventually(|| {
        let directories: Vec<_> = installations
            .state()
            .receipts
            .iter()
            .map(|receipt| PathBuf::from(&receipt.directory))
            .collect();
        directories == [moved_mac.clone(), moved_windows.clone()]
    })
    .await;
    eventually(|| fixture.moves().is_empty()).await;

    let state = installations.state();
    assert_eq!(ids(&state.receipts), ["620-macos", "620-windows"]);
    assert_eq!(state.report, None);
    assert!(moved_mac.is_dir() && moved_windows.is_dir());
    assert!(!Path::new(&mac.directory).exists());
    let mut saved = state.receipts.clone();
    saved.push(away);
    assert_eq!(kept(&receipts_file(&fixture)), saved);

    let restarted = fixture.reload(receipts_file(&fixture));
    assert_eq!(restarted.state().receipts, state.receipts);
}

#[tokio::test]
async fn a_move_that_waits_saves_the_receipts_of_a_disk_that_returned_meanwhile() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    disk.unplug();
    let installations = start(&fixture);
    let (chosen, moved) = fixture.elsewhere();
    let admission = fixture.sessions.admit(620, 42).unwrap();
    fixture.folder.choose(&chosen).unwrap();
    assert_eq!(fixture.moves().len(), 1, "only the connected installation");

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(
        installations.state().receipts,
        [mac.clone(), windows.clone()]
    );
    drop(admission);
    eventually(|| installations.state().receipts[0].directory == moved.to_string_lossy()).await;
    eventually(|| fixture.moves().is_empty()).await;

    let moved = Receipt {
        directory: moved.to_string_lossy().into_owned(),
        ..mac
    };
    assert_eq!(
        installations.state().receipts,
        [moved.clone(), windows.clone()]
    );
    assert_eq!(kept(&receipts_file(&fixture)), [moved, windows]);
}

// MARK: Disks during an action

/// The installation of `app_id`'s `platform` once moved into the games folder
/// that choosing `disk` gives.
fn on_chosen_disk(disk: &Disk, app_id: u32, platform: Platform) -> PathBuf {
    disk.mount()
        .join(games_folder::CHOSEN_DIRECTORY)
        .join("Steam")
        .join(steam_kit::client::installation_directory_name(
            app_id, platform,
        ))
}

#[tokio::test]
async fn a_disk_that_returns_while_an_uninstall_waits_on_the_archive_keeps_its_receipts_shown_and_saved()
 {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    disk.unplug();
    let installations = start(&fixture);
    assert_eq!(installations.state().receipts, from_ref(&mac));
    fixture.fake.hold_archive();

    installations.uninstall("620-macos");
    eventually(|| fixture.fake.calls() == ["archive:620"]).await;
    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(
        installations.state().receipts,
        [mac.clone(), windows.clone()]
    );

    fixture.fake.release_archive();
    eventually(|| installations.state().uninstall_game.is_none()).await;
    assert_eq!(installations.state().receipts, from_ref(&windows));
    assert_eq!(kept(&receipts_file(&fixture)), from_ref(&windows));

    // The next save does not lose what came back meanwhile.
    installations.install(offer(440));
    idle(&installations).await;
    let shown = installations.state().receipts;
    assert_eq!(ids(&shown), ["570-windows", "440-macos"]);
    assert_eq!(kept(&receipts_file(&fixture)), shown);
    assert_eq!(start(&fixture).state().receipts, shown);
}

#[tokio::test]
async fn a_disk_that_leaves_while_an_uninstall_waits_on_the_archive_is_hidden_and_kept_once() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    let installations = start(&fixture);
    assert_eq!(
        installations.state().receipts,
        [mac.clone(), windows.clone()]
    );
    fixture.fake.hold_archive();

    installations.uninstall("620-macos");
    eventually(|| fixture.fake.calls() == ["archive:620"]).await;
    disk.unplug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, from_ref(&mac));

    fixture.fake.release_archive();
    eventually(|| installations.state().uninstall_game.is_none()).await;
    assert!(installations.state().receipts.is_empty());
    assert_eq!(kept(&receipts_file(&fixture)), from_ref(&windows));

    // The next save names it once.
    installations.install(offer(440));
    idle(&installations).await;
    let shown = installations.state().receipts;
    assert_eq!(ids(&shown), ["440-macos"]);
    let mut saved = shown.clone();
    saved.push(windows.clone());
    assert_eq!(kept(&receipts_file(&fixture)), saved);
    assert_eq!(start(&fixture).state().receipts, shown);

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, saved);
    assert_eq!(kept(&receipts_file(&fixture)), saved);
}

#[tokio::test]
async fn a_failed_uninstall_while_a_disk_returns_shows_every_receipt_and_changes_no_file() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    let before = [mac.clone(), windows.clone()];
    keep(&receipts_file(&fixture), &before);
    disk.unplug();
    let installations = start(&fixture);
    fixture.fake.hold_archive();
    fixture.fake.with(|script| {
        script.archive = Some(Failure::Failed("backend.steam.cloud.network".into()))
    });

    installations.uninstall("620-macos");
    eventually(|| fixture.fake.calls() == ["archive:620"]).await;
    disk.plug();
    fixture.folder.volumes_changed();
    fixture.fake.release_archive();
    eventually(|| installations.state().uninstall_game.is_none()).await;

    let state = installations.state();
    assert_eq!(
        state.report,
        Some(Report::Uninstall("backend.steam.cloud.network".into()))
    );
    assert_eq!(state.receipts, before);
    assert_eq!(kept(&receipts_file(&fixture)), before);
    assert_eq!(start(&fixture).state().receipts, before);
    assert_eq!(kept(&receipts_file(&fixture)), before);
}

#[tokio::test]
async fn a_failed_uninstall_hides_a_receipt_whose_disk_left_unnoticed_and_keeps_it_in_the_file() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    let before = [mac.clone(), windows.clone()];
    keep(&receipts_file(&fixture), &before);
    let installations = start(&fixture);
    assert_eq!(installations.state().receipts, before);
    fixture.fake.hold_archive();
    fixture.fake.with(|script| {
        script.archive = Some(Failure::Failed("backend.steam.cloud.network".into()))
    });

    installations.uninstall("620-macos");
    eventually(|| fixture.fake.calls() == ["archive:620"]).await;
    disk.unplug();
    fixture.fake.release_archive();
    eventually(|| installations.state().uninstall_game.is_none()).await;

    assert_eq!(installations.state().receipts, from_ref(&mac));
    assert_eq!(kept(&receipts_file(&fixture)), before);

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, before);
    assert_eq!(start(&fixture).state().receipts, before);
    assert_eq!(kept(&receipts_file(&fixture)), before);
}

#[tokio::test]
async fn a_transfer_that_finishes_over_a_game_whose_build_waits_on_an_absent_disk_replaces_it() {
    let fixture = Fixture::new(false).await;
    let disk = Disk::plugged();
    let away = on_disk(&disk, 620, Platform::Windows);
    keep(&receipts_file(&fixture), from_ref(&away));
    disk.unplug();
    let installations = start(&fixture);
    assert!(installations.state().receipts.is_empty());
    fixture
        .fake
        .with(|script| script.picked = Some(Platform::Windows));

    installations.install(offer(620));
    idle(&installations).await;

    let shown = installations.state().receipts;
    assert_eq!(ids(&shown), ["620-windows"]);
    assert_ne!(shown[0].directory, away.directory);
    assert_eq!(kept(&receipts_file(&fixture)), shown);
    assert_eq!(start(&fixture).state().receipts, shown);

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, shown);
    assert_eq!(kept(&receipts_file(&fixture)), shown);
}

#[tokio::test]
async fn the_destination_disk_leaving_as_a_move_ends_keeps_the_receipt_waiting_and_saved() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    keep(&receipts_file(&fixture), from_ref(&mac));
    let installations = start(&fixture);
    let disk = Disk::plugged();
    let moved = Receipt {
        directory: on_chosen_disk(&disk, 620, Platform::Mac)
            .to_string_lossy()
            .into_owned(),
        ..mac
    };
    fixture.fake.hold_relocation();

    fixture.folder.choose(disk.mount()).unwrap();
    eventually(|| {
        fixture
            .fake
            .calls()
            .iter()
            .any(|call| call.starts_with("relocated:"))
    })
    .await;
    assert!(Path::new(&moved.directory).is_dir(), "the files moved");
    disk.unplug();
    fixture.folder.volumes_changed();
    assert!(installations.state().receipts.is_empty());

    fixture.fake.release_relocation();
    eventually(|| fixture.moves().is_empty()).await;
    assert!(installations.state().receipts.is_empty());
    assert_eq!(installations.state().report, None);
    assert_eq!(kept(&receipts_file(&fixture)), from_ref(&moved));
    assert!(start(&fixture).state().receipts.is_empty());

    disk.plug();
    fixture.folder.volumes_changed();
    assert_eq!(installations.state().receipts, from_ref(&moved));
    assert_eq!(kept(&receipts_file(&fixture)), from_ref(&moved));
    assert_eq!(start(&fixture).state().receipts, from_ref(&moved));
}

#[tokio::test]
async fn a_disk_that_returns_while_a_move_waits_on_its_records_keeps_the_other_receipts_once() {
    let fixture = Fixture::new(false).await;
    let mac = installed(&fixture, 620, Platform::Mac);
    let disk = Disk::plugged();
    let windows = on_disk(&disk, 570, Platform::Windows);
    keep(&receipts_file(&fixture), &[mac.clone(), windows.clone()]);
    disk.unplug();
    let installations = start(&fixture);
    let (chosen, moved) = fixture.elsewhere();
    fixture.fake.hold_relocation();

    fixture.folder.choose(&chosen).unwrap();
    eventually(|| {
        fixture
            .fake
            .calls()
            .iter()
            .any(|call| call.starts_with("relocated:"))
    })
    .await;
    disk.plug();
    fixture.folder.volumes_changed();
    fixture.fake.release_relocation();
    eventually(|| fixture.moves().is_empty()).await;

    let moved = Receipt {
        directory: moved.to_string_lossy().into_owned(),
        ..mac
    };
    let expected = [moved, windows];
    assert_eq!(installations.state().receipts, expected);
    assert_eq!(kept(&receipts_file(&fixture)), expected);
    assert_eq!(start(&fixture).state().receipts, expected);
}
