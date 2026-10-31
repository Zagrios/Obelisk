//! The Receipt book through its interface and its two files, for Steam, which
//! stands in for every storefront. A disk that left is a folder under a
//! /Volumes disk nobody mounts, or a `Disk` unplugged; a receipt whose folder
//! turns up in the games folder is the same installation come back to it.
//! `installations::tests::receipt_book` covers the flows of an account.
use super::*;
use crate::games_folder::disks::Disk;
use crate::installations::tests::{half, ids, product};
use crate::steam::Steam;
use obelisk_core::Platform;
use serde_json::json;
use std::slice::from_ref;

/// An account's library directory, games folder and files.
struct Home {
    _directory: tempfile::TempDir,
    root: PathBuf,
    folder: Arc<GamesFolder>,
}

impl Home {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("Games/Steam")).unwrap();
        let folder = GamesFolder::new(&root);
        Self {
            _directory: directory,
            root,
            folder,
        }
    }

    fn file(&self) -> PathBuf {
        self.root.join("steam-installations-42.json")
    }

    fn incomplete_file(&self) -> PathBuf {
        self.root.join("steam-installations-42.incomplete.json")
    }

    fn open(&self) -> ReceiptBook<Steam> {
        ReceiptBook::open(self.file(), self.folder.clone())
    }

    /// The installation of `app_id`'s `platform`, its folder in the games
    /// folder.
    fn here(&self, app_id: u32, platform: Platform) -> Receipt<Steam> {
        let directory = self.root.join("Games/Steam").join(name(app_id, platform));
        std::fs::create_dir_all(&directory).unwrap();
        receipt_in(app_id, platform, &directory)
    }

    /// The installation of `app_id`'s `platform` on a disk that is not
    /// connected.
    fn away(&self, app_id: u32, platform: Platform) -> Receipt<Steam> {
        let directory =
            Path::new("/Volumes/Obelisk Book Disk That Is Not There/Obelisk Games/Steam")
                .join(name(app_id, platform));
        receipt_in(app_id, platform, &directory)
    }

    /// The installation of `app_id`'s `platform` as it was somewhere else,
    /// whose folder is gone.
    fn gone(&self, app_id: u32, platform: Platform) -> Receipt<Steam> {
        let directory = self.root.join("Elsewhere").join(name(app_id, platform));
        receipt_in(app_id, platform, &directory)
    }

    /// The folder of `receipt`'s installation turns up in the games folder.
    fn returns(&self, receipt: &Receipt<Steam>) -> Receipt<Steam> {
        let directory = self
            .root
            .join("Games/Steam")
            .join(Path::new(&receipt.directory).file_name().unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        Receipt {
            directory: directory.to_string_lossy().into_owned(),
            ..receipt.clone()
        }
    }

    /// The account started again from its files: they are as they were, and
    /// the book shows what `book` shows.
    fn restarted(&self, book: &ReceiptBook<Steam>) -> ReceiptBook<Steam> {
        let files = (
            std::fs::read(self.file()).ok(),
            std::fs::read(self.incomplete_file()).ok(),
        );
        let restarted = self.open();
        assert_eq!(restarted.shown(), book.shown());
        assert_eq!(restarted.incomplete(), book.incomplete());
        assert_eq!(
            (
                std::fs::read(self.file()).ok(),
                std::fs::read(self.incomplete_file()).ok()
            ),
            files,
            "starting rewrites nothing"
        );
        restarted
    }
}

fn name(app_id: u32, platform: Platform) -> String {
    steam_kit::client::installation_directory_name(app_id, platform)
}

fn receipt_in(app_id: u32, platform: Platform, directory: &Path) -> Receipt<Steam> {
    Receipt {
        product: product(app_id),
        name: format!("Game {app_id}"),
        directory: directory.to_string_lossy().into_owned(),
        platform,
        total_bytes: 100,
        needs_verification: false,
    }
}

/// The installation of `app_id`'s `platform` on `disk`, folder made.
fn on_disk(disk: &Disk, app_id: u32, platform: Platform) -> Receipt<Steam> {
    let directory = disk
        .mount()
        .join("Obelisk Games/Steam")
        .join(name(app_id, platform));
    std::fs::create_dir_all(&directory).unwrap();
    receipt_in(app_id, platform, &directory)
}

fn transfer(app_id: u32, platform: Platform) -> Transfer<Steam> {
    Transfer {
        product: product(app_id),
        name: format!("Game {app_id}"),
        platform: Some(platform),
        directory: PathBuf::from("/library"),
    }
}

/// The receipts `file` keeps, in its order.
fn kept(file: &Path) -> Vec<Receipt<Steam>> {
    load(file).unwrap()
}

/// Writes `receipts` pretty-printed, as no write of the book does: a file the
/// book rewrote shows in its bytes.
fn pretty(file: &Path, receipts: &[Receipt<Steam>]) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(receipts).unwrap();
    std::fs::write(file, &bytes).unwrap();
    bytes
}

/// Makes `file` a folder, so that writing it fails.
fn unwritable(file: &Path) {
    let _ = std::fs::remove_file(file);
    std::fs::create_dir(file).unwrap();
}

const NOT_SAVED: &str = <Steam as Storefront>::RECEIPTS_NOT_SAVED;
const UNAVAILABLE: &str = <Steam as Storefront>::RECEIPTS_UNAVAILABLE;

// MARK: Starting

#[test]
fn opening_shows_the_receipts_whose_folder_is_there_and_rewrites_nothing() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    let away = home.away(570, Platform::Mac);
    let (half_here, half_away) = (
        half(home.here(440, Platform::Mac)),
        half(home.away(440, Platform::Windows)),
    );
    let receipts = pretty(&home.file(), &[away.clone(), mac.clone(), windows.clone()]);
    let incomplete = pretty(&home.incomplete_file(), &[half_away, half_here.clone()]);

    let mut book = home.open();

    assert_eq!(book.shown(), [mac.clone(), windows.clone()]);
    assert_eq!(book.incomplete(), [half_here]);
    assert_eq!(book.receipt("620-windows"), Some(&windows));
    assert_eq!(book.receipt("570-macos"), None, "its disk is away");
    assert!(book.take_warnings().is_empty());
    assert_eq!(std::fs::read(home.file()).unwrap(), receipts);
    assert_eq!(std::fs::read(home.incomplete_file()).unwrap(), incomplete);
}

#[test]
fn opening_drops_a_receipt_whose_folder_went_and_writes_the_file_shown_first_then_waiting() {
    let home = Home::new();
    let (gone, mac) = (
        home.gone(620, Platform::Windows),
        home.here(620, Platform::Mac),
    );
    let away = home.away(570, Platform::Mac);
    pretty(&home.file(), &[gone, away.clone(), mac.clone()]);

    let book = home.open();

    assert_eq!(book.shown(), from_ref(&mac));
    assert_eq!(kept(&home.file()), [mac, away]);
    home.restarted(&book);
}

#[test]
fn opening_names_a_folder_that_moved_into_the_games_folder_where_it_is() {
    let home = Home::new();
    let away = home.away(620, Platform::Windows);
    let moved = home.returns(&away);
    let mac = home.here(620, Platform::Mac);
    pretty(&home.file(), &[mac.clone(), away]);

    let book = home.open();

    assert_eq!(book.shown(), [mac.clone(), moved.clone()]);
    assert_eq!(kept(&home.file()), [mac, moved]);
    home.restarted(&book);
}

#[test]
fn an_unreadable_receipts_file_reads_as_empty_raises_a_warning_and_stays_as_it_is() {
    let home = Home::new();
    std::fs::write(home.file(), b"not json").unwrap();

    let mut book = home.open();

    assert!(book.shown().is_empty());
    assert_eq!(book.take_warnings(), [UNAVAILABLE]);
    assert!(book.take_warnings().is_empty());
    assert_eq!(std::fs::read(home.file()).unwrap(), b"not json");
}

#[test]
fn an_unreadable_incomplete_file_only_forgets_the_downloads_it_kept() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    pretty(&home.file(), from_ref(&mac));
    std::fs::write(home.incomplete_file(), b"not json").unwrap();

    let mut book = home.open();

    assert_eq!(book.shown(), [mac]);
    assert!(book.incomplete().is_empty());
    assert!(book.take_warnings().is_empty());
}

#[test]
fn the_incomplete_file_is_the_one_beside_the_receipts_file() {
    let home = Home::new();
    let receipts_file = home.root.join("steam-installations-7.json");
    let record = half(home.here(620, Platform::Mac));
    std::fs::write(
        home.root.join("steam-installations-7.incomplete.json"),
        serde_json::to_vec(from_ref(&record)).unwrap(),
    )
    .unwrap();

    let book = ReceiptBook::<Steam>::open(receipts_file, home.folder.clone());

    assert_eq!(book.incomplete(), [record]);
}

// MARK: Finished Transfers

#[test]
fn an_installation_a_transfer_finished_comes_last_and_the_file_keeps_it_before_those_waiting() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    let away = home.away(570, Platform::Mac);
    pretty(&home.file(), &[mac.clone(), windows.clone(), away.clone()]);
    let mut book = home.open();
    let updated = Receipt {
        total_bytes: 200,
        ..mac
    };

    book.installed(updated.clone());

    assert_eq!(book.shown(), [windows.clone(), updated.clone()]);
    assert_eq!(kept(&home.file()), [windows, updated, away]);
    assert!(book.take_warnings().is_empty());
    home.restarted(&book);
}

#[test]
fn an_installation_a_transfer_finished_replaces_the_receipt_of_its_id_waiting_on_an_absent_disk() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let (old, other) = (
        home.away(620, Platform::Windows),
        home.away(570, Platform::Mac),
    );
    pretty(&home.file(), &[mac.clone(), old, other.clone()]);
    let mut book = home.open();
    let new = home.here(620, Platform::Windows);

    book.installed(new.clone());

    assert_eq!(book.shown(), [mac.clone(), new.clone()]);
    assert_eq!(kept(&home.file()), [mac, new, other]);
    home.restarted(&book);
}

#[test]
fn an_installation_a_transfer_finished_forgets_its_incomplete_record_but_not_one_waiting() {
    let home = Home::new();
    let (record, waiting) = (
        half(home.here(620, Platform::Mac)),
        half(home.away(620, Platform::Windows)),
    );
    let other = half(home.here(570, Platform::Mac));
    pretty(
        &home.incomplete_file(),
        &[record.clone(), waiting.clone(), other.clone()],
    );
    let mut book = home.open();
    assert_eq!(book.incomplete(), [record.clone(), other.clone()]);

    book.installed(home.here(620, Platform::Windows));
    assert_eq!(
        book.incomplete(),
        [record.clone(), other.clone()],
        "the waiting record is another folder's files, not this Transfer's"
    );
    book.installed(Receipt {
        needs_verification: false,
        total_bytes: 100,
        ..record
    });

    assert_eq!(book.incomplete(), from_ref(&other));
    assert_eq!(kept(&home.incomplete_file()), [other, waiting]);
    home.restarted(&book);
}

#[test]
fn the_receipts_file_is_right_when_the_incomplete_file_cannot_be_written() {
    let home = Home::new();
    let mut book = home.open();
    unwritable(&home.incomplete_file());
    book.began(half(home.here(620, Platform::Mac)));
    assert_eq!(book.take_warnings(), [NOT_SAVED]);

    book.installed(home.here(620, Platform::Mac));

    assert_eq!(ids(&kept(&home.file())), ["620-macos"]);
    assert!(book.incomplete().is_empty());
    assert_eq!(book.take_warnings(), [NOT_SAVED]);
}

// MARK: New installations

#[test]
fn a_new_installation_is_kept_as_incomplete_in_its_own_file_only() {
    let home = Home::new();
    let mut book = home.open();
    let record = half(home.here(620, Platform::Mac));

    book.began(record.clone());

    assert_eq!(book.incomplete(), from_ref(&record));
    assert!(book.shown().is_empty());
    assert_eq!(kept(&home.incomplete_file()), [record]);
    assert!(!home.file().exists());
    home.restarted(&book);
}

#[test]
fn a_new_installation_whose_id_is_already_incomplete_and_shown_is_not_kept_twice() {
    let home = Home::new();
    let (shown, waiting) = (
        half(home.here(620, Platform::Mac)),
        half(home.away(570, Platform::Mac)),
    );
    let bytes = pretty(&home.incomplete_file(), &[shown.clone(), waiting]);
    let mut book = home.open();

    book.began(Receipt {
        name: "Again".into(),
        ..shown
    });

    assert_eq!(ids(book.incomplete()), ["620-macos"]);
    assert_eq!(std::fs::read(home.incomplete_file()).unwrap(), bytes);
    assert!(book.take_warnings().is_empty());
}

#[test]
fn a_new_installation_is_kept_beside_the_record_of_its_id_on_a_disk_that_is_not_connected() {
    let home = Home::new();
    let waiting = half(home.away(620, Platform::Mac));
    pretty(&home.incomplete_file(), from_ref(&waiting));
    let mut book = home.open();
    assert!(book.incomplete().is_empty());
    let record = half(home.here(620, Platform::Mac));

    book.began(record.clone());

    assert_eq!(book.incomplete(), from_ref(&record));
    assert_eq!(
        kept(&home.incomplete_file()),
        [record.clone(), waiting.clone()]
    );
    assert!(book.take_warnings().is_empty());
    home.restarted(&book);
}

// MARK: Transfers over an installation

#[test]
fn a_transfer_marks_the_receipt_it_goes_over_for_verification_in_place() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    let away = home.away(570, Platform::Mac);
    pretty(&home.file(), &[mac.clone(), windows.clone(), away.clone()]);
    let mut book = home.open();

    book.unverified(&transfer(620, Platform::Mac));

    let marked = Receipt {
        needs_verification: true,
        ..mac
    };
    assert_eq!(book.shown(), [marked.clone(), windows.clone()]);
    assert_eq!(kept(&home.file()), [marked, windows, away]);
    home.restarted(&book);
}

#[test]
fn a_transfer_over_no_shown_receipt_writes_nothing() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let away = home.away(570, Platform::Mac);
    let bytes = pretty(&home.file(), &[mac.clone(), away]);
    let mut book = home.open();

    book.unverified(&transfer(440, Platform::Mac));
    book.unverified(&transfer(570, Platform::Mac));

    assert_eq!(book.shown(), [mac]);
    assert_eq!(std::fs::read(home.file()).unwrap(), bytes);
}

#[test]
fn a_transfer_finds_the_shown_receipt_it_goes_over_and_the_incomplete_record_of_its_game() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let record = half(home.here(570, Platform::Windows));
    pretty(&home.file(), from_ref(&mac));
    pretty(&home.incomplete_file(), from_ref(&record));
    let book = home.open();
    let storefront_picks = Transfer {
        platform: None,
        ..transfer(570, Platform::Mac)
    };

    assert_eq!(book.landing(&transfer(620, Platform::Mac)), Some(&mac));
    assert_eq!(book.landing(&transfer(620, Platform::Windows)), None);
    assert_eq!(book.landing(&storefront_picks), None);
    assert_eq!(book.landing_or_incomplete(&storefront_picks), Some(&record));
    assert_eq!(
        book.landing_or_incomplete(&transfer(620, Platform::Mac)),
        Some(&mac)
    );
}

// MARK: Moves

#[test]
fn a_move_updates_the_directory_where_the_receipt_stands() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    let away = home.away(570, Platform::Mac);
    pretty(&home.file(), &[mac.clone(), windows.clone(), away.clone()]);
    let mut book = home.open();
    let moved = Receipt {
        directory: home
            .root
            .join("Moved/steam-620-macos")
            .to_string_lossy()
            .into_owned(),
        ..mac
    };

    book.moved(&moved);

    assert_eq!(book.shown(), [moved.clone(), windows.clone()]);
    assert_eq!(kept(&home.file()), [moved, windows, away]);
}

#[test]
fn a_move_reaches_a_receipt_waiting_on_an_absent_disk_and_shows_it_once_its_folder_is_there() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let away = home.away(570, Platform::Windows);
    pretty(&home.file(), &[mac.clone(), away.clone()]);
    let mut book = home.open();
    let moved = home.returns(&away);

    book.moved(&moved);

    assert_eq!(book.shown(), [mac.clone(), moved.clone()]);
    assert_eq!(kept(&home.file()), [mac, moved]);
    home.restarted(&book);
}

#[test]
fn a_move_onto_an_absent_disk_hides_the_receipt_and_the_file_keeps_it_there() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    pretty(&home.file(), &[mac.clone(), windows.clone()]);
    let mut book = home.open();
    let moved = Receipt {
        directory: home.away(620, Platform::Mac).directory,
        ..mac
    };

    book.moved(&moved);

    assert_eq!(book.shown(), from_ref(&windows));
    assert_eq!(kept(&home.file()), [windows, moved]);
}

#[test]
fn a_move_that_ends_after_the_receipt_was_dropped_puts_it_back_shown_or_waiting_by_its_disk() {
    let home = Home::new();
    let mut book = home.open();
    let here = home.here(620, Platform::Mac);
    let away = home.away(570, Platform::Mac);

    book.moved(&here);
    book.moved(&away);

    assert_eq!(book.shown(), from_ref(&here));
    assert_eq!(kept(&home.file()), [here, away]);
}

// MARK: Uninstalls and discards

#[test]
fn forgetting_an_installation_removes_it_from_the_list_and_the_file_but_not_what_waits() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    let away = home.away(570, Platform::Mac);
    pretty(&home.file(), &[mac.clone(), away.clone(), windows.clone()]);
    let mut book = home.open();

    book.forget("620-macos").unwrap();

    assert_eq!(book.shown(), from_ref(&windows));
    assert_eq!(kept(&home.file()), [windows, away]);
    assert!(book.take_warnings().is_empty());
    home.restarted(&book);
}

#[test]
fn forgetting_an_installation_whose_disk_is_away_removes_it_from_the_file() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let away = home.away(570, Platform::Mac);
    pretty(&home.file(), &[mac.clone(), away]);
    let mut book = home.open();

    book.forget("570-macos").unwrap();

    assert_eq!(kept(&home.file()), [mac]);
}

#[test]
fn forgetting_an_installation_that_is_not_there_still_writes_the_file_from_the_lists() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let bytes = pretty(&home.file(), from_ref(&mac));
    let mut book = home.open();

    book.forget("440-macos").unwrap();

    assert_ne!(std::fs::read(home.file()).unwrap(), bytes);
    assert_eq!(kept(&home.file()), [mac]);
}

#[test]
fn a_forget_whose_write_fails_returns_the_error_after_the_receipt_went_and_raises_no_warning() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    pretty(&home.file(), &[mac, windows.clone()]);
    let mut book = home.open();
    unwritable(&home.file());

    assert!(book.forget("620-macos").is_err());

    assert_eq!(book.shown(), [windows]);
    assert!(book.take_warnings().is_empty());
}

#[test]
fn discarding_an_incomplete_installation_keeps_the_others_in_the_file() {
    let home = Home::new();
    let (shown, waiting) = (
        half(home.here(620, Platform::Mac)),
        half(home.away(570, Platform::Mac)),
    );
    let other = half(home.here(440, Platform::Mac));
    pretty(
        &home.incomplete_file(),
        &[shown.clone(), waiting.clone(), other.clone()],
    );
    let mut book = home.open();

    book.discard(&shown);

    assert_eq!(book.incomplete(), from_ref(&other));
    assert_eq!(
        kept(&home.incomplete_file()),
        [other.clone(), waiting.clone()]
    );
    book.discard(&waiting);
    assert_eq!(kept(&home.incomplete_file()), [other]);
    assert!(!home.file().exists());
    home.restarted(&book);
}

#[test]
fn discarding_an_incomplete_installation_keeps_the_same_game_in_another_folder_on_a_disk_that_is_away()
 {
    let home = Home::new();
    let (shown, waiting) = (
        half(home.here(620, Platform::Mac)),
        half(home.away(620, Platform::Mac)),
    );
    pretty(&home.incomplete_file(), &[waiting.clone(), shown.clone()]);
    let mut book = home.open();
    assert_eq!(book.incomplete(), from_ref(&shown));

    book.discard(&shown);

    assert!(book.incomplete().is_empty());
    assert_eq!(kept(&home.incomplete_file()), [waiting]);
    home.restarted(&book);
}

#[test]
fn discarding_an_incomplete_installation_whose_disk_left_meanwhile_removes_its_waiting_record() {
    let home = Home::new();
    let disk = Disk::plugged();
    let (record, twin) = (
        half(on_disk(&disk, 620, Platform::Mac)),
        half(home.here(620, Platform::Mac)),
    );
    pretty(&home.incomplete_file(), &[record.clone(), twin.clone()]);
    let mut book = home.open();
    assert_eq!(book.incomplete(), [record.clone(), twin.clone()]);
    disk.unplug();
    book.refresh(None);
    assert_eq!(book.incomplete(), from_ref(&twin));

    book.discard(&record);

    assert_eq!(book.incomplete(), from_ref(&twin));
    assert_eq!(kept(&home.incomplete_file()), [twin]);
    home.restarted(&book);
}

#[test]
fn discarding_one_of_two_shown_records_of_an_id_keeps_the_other() {
    let home = Home::new();
    let disk = Disk::plugged();
    let (record, twin) = (
        half(on_disk(&disk, 620, Platform::Mac)),
        half(home.here(620, Platform::Mac)),
    );
    pretty(&home.incomplete_file(), &[record.clone(), twin.clone()]);
    let mut book = home.open();
    assert_eq!(book.incomplete(), [record.clone(), twin.clone()]);

    book.discard(&twin);

    assert_eq!(book.incomplete(), from_ref(&record));
    assert_eq!(kept(&home.incomplete_file()), [record]);
    home.restarted(&book);
}

#[test]
fn a_discard_whose_write_fails_raises_one_warning() {
    let home = Home::new();
    let record = half(home.here(620, Platform::Mac));
    pretty(&home.incomplete_file(), from_ref(&record));
    let mut book = home.open();
    unwritable(&home.incomplete_file());

    book.discard(&record);

    assert!(book.incomplete().is_empty());
    assert_eq!(book.take_warnings(), [NOT_SAVED]);
    assert!(book.take_warnings().is_empty());
}

// MARK: Disks

#[test]
fn a_returning_receipt_comes_after_those_shown_and_the_file_follows() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let away = home.away(570, Platform::Windows);
    pretty(&home.file(), &[away.clone(), mac.clone()]);
    let mut book = home.open();
    assert_eq!(book.shown(), from_ref(&mac));

    let back = home.returns(&away);
    book.refresh(None);

    assert_eq!(book.shown(), [mac.clone(), back.clone()]);
    assert_eq!(kept(&home.file()), [mac, back]);
    home.restarted(&book);
}

#[test]
fn a_shown_receipt_whose_folder_went_is_dropped_from_the_list_and_the_file() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    let away = home.away(570, Platform::Mac);
    pretty(&home.file(), &[mac.clone(), windows.clone(), away.clone()]);
    let mut book = home.open();
    std::fs::remove_dir_all(&windows.directory).unwrap();

    book.refresh(None);

    assert_eq!(book.shown(), from_ref(&mac));
    assert_eq!(kept(&home.file()), [mac, away]);
    home.restarted(&book);
}

#[test]
fn a_receipt_whose_disk_left_waits_and_moving_between_the_lists_writes_nothing() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let away = home.away(570, Platform::Mac);
    let mut book = home.open();
    book.installed(mac.clone());
    book.installed(away.clone());
    assert_eq!(book.shown(), [mac.clone(), away.clone()]);
    let bytes = pretty(&home.file(), &[mac.clone(), away.clone()]);

    book.refresh(None);

    assert_eq!(book.shown(), [mac]);
    assert_eq!(std::fs::read(home.file()).unwrap(), bytes);
}

#[test]
fn a_refresh_that_changes_nothing_writes_nothing() {
    let home = Home::new();
    let mac = home.here(620, Platform::Mac);
    let away = home.away(570, Platform::Mac);
    let record = half(home.here(440, Platform::Mac));
    let waiting = half(home.away(440, Platform::Windows));
    let receipts = pretty(&home.file(), &[mac, away]);
    let incomplete = pretty(&home.incomplete_file(), &[record, waiting]);
    let mut book = home.open();

    book.refresh(None);

    assert_eq!(std::fs::read(home.file()).unwrap(), receipts);
    assert_eq!(std::fs::read(home.incomplete_file()).unwrap(), incomplete);
    assert!(book.take_warnings().is_empty());
}

#[test]
fn an_incomplete_record_whose_folder_went_is_dropped_from_the_list_and_the_file() {
    let home = Home::new();
    let record = half(home.here(620, Platform::Mac));
    let other = half(home.here(570, Platform::Mac));
    pretty(&home.incomplete_file(), &[record.clone(), other.clone()]);
    let mut book = home.open();
    std::fs::remove_dir_all(&other.directory).unwrap();

    book.refresh(None);

    assert_eq!(book.incomplete(), from_ref(&record));
    assert_eq!(kept(&home.incomplete_file()), [record]);
}

#[test]
fn refreshing_the_installed_receipts_alone_leaves_the_incomplete_records_as_they_are() {
    let home = Home::new();
    let (mac, windows) = (
        home.here(620, Platform::Mac),
        home.here(620, Platform::Windows),
    );
    let record = half(home.here(570, Platform::Mac));
    pretty(&home.file(), &[mac.clone(), windows.clone()]);
    let incomplete = pretty(&home.incomplete_file(), from_ref(&record));
    let mut book = home.open();
    std::fs::remove_dir_all(&windows.directory).unwrap();
    std::fs::remove_dir_all(&record.directory).unwrap();

    book.refresh_installed();

    assert_eq!(book.shown(), from_ref(&mac));
    assert_eq!(kept(&home.file()), [mac]);
    assert_eq!(book.incomplete(), from_ref(&record));
    assert_eq!(std::fs::read(home.incomplete_file()).unwrap(), incomplete);
}

#[test]
fn the_incomplete_record_of_the_game_under_way_stays_last_whatever_its_folder() {
    let home = Home::new();
    let mut book = home.open();
    let (first, running, last) = (
        half(home.here(570, Platform::Mac)),
        half(home.gone(620, Platform::Mac)),
        half(home.here(440, Platform::Mac)),
    );
    book.began(first.clone());
    book.began(running.clone());
    book.began(last.clone());

    book.refresh(Some(&620));

    assert_eq!(book.incomplete(), [first.clone(), last.clone(), running]);
    assert_eq!(
        ids(&kept(&home.incomplete_file())),
        ["570-macos", "620-macos", "440-macos"],
        "reordering alone writes nothing"
    );

    book.refresh(None);

    assert_eq!(book.incomplete(), [first.clone(), last.clone()]);
    assert_eq!(kept(&home.incomplete_file()), [first, last]);
}

#[test]
fn a_forget_after_a_refresh_that_showed_a_returned_receipt_keeps_it_shown_and_saved() {
    let home = Home::new();
    let (mac, away) = (
        home.here(620, Platform::Mac),
        home.away(570, Platform::Windows),
    );
    pretty(&home.file(), &[mac, away.clone()]);
    let mut book = home.open();
    let back = home.returns(&away);

    book.refresh(None);
    book.forget("620-macos").unwrap();

    assert_eq!(book.shown(), from_ref(&back));
    assert_eq!(kept(&home.file()), [back]);
    home.restarted(&book);
}

#[test]
fn a_forget_after_a_refresh_that_hid_a_receipt_keeps_it_in_the_file_once() {
    let home = Home::new();
    let mut book = home.open();
    book.installed(home.here(620, Platform::Mac));
    let away = home.away(570, Platform::Windows);
    book.installed(away.clone());
    assert_eq!(ids(book.shown()), ["620-macos", "570-windows"]);

    book.refresh(None);
    book.forget("620-macos").unwrap();

    assert!(book.shown().is_empty());
    assert_eq!(kept(&home.file()), [away]);
}

// MARK: Warnings and format

#[test]
fn each_failed_write_raises_one_warning_in_order_and_taking_them_empties_the_queue() {
    let home = Home::new();
    let mut book = home.open();
    book.installed(home.here(620, Platform::Mac));
    assert!(book.take_warnings().is_empty());
    unwritable(&home.file());
    unwritable(&home.incomplete_file());

    book.installed(home.here(570, Platform::Mac));
    let record = half(home.here(440, Platform::Mac));
    book.began(record.clone());
    book.discard(&record);

    assert_eq!(book.take_warnings(), [NOT_SAVED, NOT_SAVED, NOT_SAVED]);
    assert!(book.take_warnings().is_empty());
}

#[test]
fn the_files_are_the_json_array_an_earlier_obelisk_reads() {
    let home = Home::new();
    let mut book = home.open();
    let mac = home.here(620, Platform::Mac);
    book.installed(mac.clone());
    book.began(half(home.here(570, Platform::Windows)));

    assert_eq!(
        std::fs::read(home.file()).unwrap(),
        serde_json::to_vec(from_ref(&mac)).unwrap()
    );
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.file()).unwrap()).unwrap();
    assert_eq!(
        written,
        json!([{
            "appId": 620,
            "language": "french",
            "name": "Game 620",
            "directory": mac.directory,
            "platform": "macos",
            "totalBytes": 100
        }])
    );
    let incomplete: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.incomplete_file()).unwrap()).unwrap();
    assert_eq!(incomplete[0]["needsVerification"], json!(true));
    assert_eq!(incomplete[0]["totalBytes"], json!(0));
}

#[test]
fn load_reads_an_earlier_file_and_no_file_as_no_receipts() {
    let home = Home::new();
    assert!(load::<Steam>(&home.file()).unwrap().is_empty());
    std::fs::write(
        home.file(),
        r#"[{"appId":620,"language":"french","name":"Portal 2","directory":"/games/steam-620-macos","platform":"macos","totalBytes":7}]"#,
    )
    .unwrap();

    let receipts = load::<Steam>(&home.file()).unwrap();

    assert_eq!(
        receipts,
        [Receipt {
            name: "Portal 2".into(),
            total_bytes: 7,
            ..receipt_in(620, Platform::Mac, Path::new("/games/steam-620-macos"))
        }]
    );
    let again = home.root.join("again/receipts.json");
    save(&again, &receipts).unwrap();
    assert_eq!(kept(&again), receipts);
}
