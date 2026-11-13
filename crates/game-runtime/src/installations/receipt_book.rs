//! The Receipt book: the Installation receipts one storefront account keeps,
//! in a file of its own, and beside them the records of its new installations
//! whose Transfer has not succeeded, in a second file. The receipts of
//! installations whose disk is connected show; those whose disk is not
//! connected stay in the file, hidden, until it comes back. Every change names
//! a receipt and applies to the lists as they are when it is called, so no
//! caller keeps a list across an await to write it back later.
use super::{Receipt, Storefront, Transfer};
use crate::games_folder::{self, GamesFolder};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// One account's receipts of one storefront. It is not thread-safe by itself:
/// `Shared`'s lock guards it, and every call runs under that lock.
///
/// Among the installed receipts, those shown and those whose disk is not
/// connected, there is one per id; a file that already holds two is loaded as
/// it is, and a change that names the id removes every receipt of it. The
/// incomplete records are not one per id: a record whose disk is not
/// connected is another folder's files, and a new installation of the same
/// game keeps its own record beside it, so both show once the disk is back.
/// A change to an incomplete record names that record, not its id. A file
/// holds the receipts shown, then those whose disk is not connected. A write
/// that fails leaves the lists ahead of the file, which the next write
/// catches up.
pub(super) struct ReceiptBook<S: Storefront> {
    installed: Shelf<S>,
    incomplete: Shelf<S>,
    folder: Arc<GamesFolder>,
    warnings: Vec<&'static str>,
}

/// One file's receipts: those shown, and those whose disk is not connected.
struct Shelf<S: Storefront> {
    file: PathBuf,
    shown: Vec<Receipt<S>>,
    offline: Vec<Receipt<S>>,
}

impl<S: Storefront> Shelf<S> {
    fn new(file: PathBuf) -> Self {
        Self {
            file,
            shown: Vec::new(),
            offline: Vec::new(),
        }
    }

    /// Every receipt the file keeps: those shown, then those whose disk is not
    /// connected.
    fn kept(&self) -> Vec<Receipt<S>> {
        self.shown.iter().chain(&self.offline).cloned().collect()
    }

    fn save(&self) -> io::Result<()> {
        save(&self.file, &self.kept())
    }

    /// Files `receipts` as the shelf now holds them, shown or waiting for
    /// their disk, as `sort` splits them for `folder`. `true` when sorting
    /// changed a receipt.
    fn refile(&mut self, receipts: Vec<Receipt<S>>, folder: Option<&GamesFolder>) -> bool {
        let (shown, offline, changed) = sort(receipts, folder);
        self.shown = shown;
        self.offline = offline;
        changed
    }

    /// Removes every receipt the installation `id` names.
    fn remove(&mut self, id: &str) {
        self.shown.retain(|kept| kept.id() != id);
        self.offline.retain(|kept| kept.id() != id);
    }

    /// Removes `receipt` itself, wherever it is now, and no other receipt of
    /// its id.
    fn remove_record(&mut self, receipt: &Receipt<S>) {
        self.shown.retain(|kept| kept != receipt);
        self.offline.retain(|kept| kept != receipt);
    }
}

impl<S: Storefront> ReceiptBook<S> {
    /// Reads the account's receipts from `receipts_file` and its incomplete
    /// installations from the file beside it. Neither read fails: an
    /// unreadable receipts file reads as empty and raises
    /// `S::RECEIPTS_UNAVAILABLE`, an unreadable incomplete file only forgets
    /// the downloads it kept, as an Obelisk before it did. A file is written
    /// only when sorting or completing changed a receipt.
    pub(super) fn open(receipts_file: PathBuf, folder: Arc<GamesFolder>) -> Self {
        let incomplete_file = receipts_file.with_extension("incomplete.json");
        let mut book = Self {
            installed: Shelf::new(receipts_file),
            incomplete: Shelf::new(incomplete_file),
            folder,
            warnings: Vec::new(),
        };
        match load(&book.installed.file) {
            Ok(receipts) => {
                let mut changed = book.installed.refile(receipts, Some(&book.folder));
                // Every receipt is completed, not only the first that changes.
                for receipt in &mut book.installed.shown {
                    changed |= S::complete(receipt);
                }
                if changed {
                    book.save_installed();
                }
            }
            Err(_) => book.warnings.push(S::RECEIPTS_UNAVAILABLE),
        }
        let incomplete = load(&book.incomplete.file).unwrap_or_default();
        if book.incomplete.refile(incomplete, None) {
            book.save_incomplete();
        }
        book
    }

    /// The installations whose disk is connected, in the order the account
    /// shows them.
    pub(super) fn shown(&self) -> &[Receipt<S>] {
        &self.installed.shown
    }

    /// The new installations whose Transfer has not succeeded and whose disk
    /// is connected, or whose Transfer is under way.
    pub(super) fn incomplete(&self) -> &[Receipt<S>] {
        &self.incomplete.shown
    }

    /// The shown receipt `id` names.
    pub(super) fn receipt(&self, id: &str) -> Option<&Receipt<S>> {
        self.installed.shown.iter().find(|kept| kept.id() == id)
    }

    /// The shown receipt `transfer` goes over.
    pub(super) fn landing(&self, transfer: &Transfer<S>) -> Option<&Receipt<S>> {
        self.installed
            .shown
            .iter()
            .find(|kept| transfer.lands_on(kept))
    }

    /// As `landing`, among the shown receipts, then the shown incomplete ones.
    pub(super) fn landing_or_incomplete(&self, transfer: &Transfer<S>) -> Option<&Receipt<S>> {
        self.installed
            .shown
            .iter()
            .chain(&self.incomplete.shown)
            .find(|kept| transfer.lands_on(kept))
    }

    /// A Transfer succeeded with `receipt`: it is now the installation of its
    /// id, last of those shown. It replaces every receipt of that id, shown or
    /// whose disk is not connected, and the shown incomplete record of that
    /// id; an incomplete record whose disk is not connected stays.
    pub(super) fn installed(&mut self, receipt: Receipt<S>) {
        let id = receipt.id();
        self.installed.remove(&id);
        self.installed.shown.push(receipt);
        self.save_installed();
        let records = self.incomplete.shown.len();
        self.incomplete.shown.retain(|kept| kept.id() != id);
        if self.incomplete.shown.len() != records {
            self.save_incomplete();
        }
    }

    /// A new installation's Transfer began: keeps `receipt` as incomplete.
    /// Nothing happens when its id is already incomplete and shown. A record
    /// of that id whose disk is not connected is another folder's files: it
    /// stays, and `receipt` is kept beside it.
    pub(super) fn began(&mut self, receipt: Receipt<S>) {
        let id = receipt.id();
        if self.incomplete.shown.iter().any(|kept| kept.id() == id) {
            return;
        }
        self.incomplete.shown.push(receipt);
        self.save_incomplete();
    }

    /// A Transfer started over the shown receipt it lands on: it needs
    /// verification until a Transfer succeeds. Nothing happens when it lands
    /// on none.
    pub(super) fn unverified(&mut self, transfer: &Transfer<S>) {
        if let Some(receipt) = self
            .installed
            .shown
            .iter_mut()
            .find(|kept| transfer.lands_on(kept))
        {
            receipt.needs_verification = true;
            self.save_installed();
        }
    }

    /// The installation `moved` names now sits where `moved` says, in the
    /// same place of its list, shown or waiting; it waits when that is on a
    /// disk that is not connected and shows when the disk is back. A receipt
    /// dropped meanwhile, as when its new disk left, is put back.
    pub(super) fn moved(&mut self, moved: &Receipt<S>) {
        let id = moved.id();
        let away = games_folder::disconnected(Path::new(&moved.directory));
        let Shelf { shown, offline, .. } = &mut self.installed;
        let listed_shown = shown.iter().any(|kept| kept.id() == id);
        let (listed, other) = if listed_shown {
            (shown, offline)
        } else {
            (offline, shown)
        };
        if listed.iter().any(|kept| kept.id() == id) {
            for kept in listed.iter_mut().filter(|kept| kept.id() == id) {
                kept.directory.clone_from(&moved.directory);
            }
            // A receipt shows exactly when its disk is connected.
            let misplaced = listed_shown == away;
            if misplaced {
                let (named, rest): (Vec<_>, Vec<_>) = std::mem::take(listed)
                    .into_iter()
                    .partition(|kept| kept.id() == id);
                *listed = rest;
                other.extend(named);
            }
        } else if away {
            listed.push(moved.clone());
        } else {
            other.push(moved.clone());
        }
        self.save_installed();
    }

    /// The installation `id` names is gone: every receipt of that id, wherever
    /// it is now, leaves the book and the file is written from what remains.
    /// The one change that reports its failed write: an uninstall fails with
    /// it.
    pub(super) fn forget(&mut self, id: &str) -> io::Result<()> {
        self.installed.remove(id);
        self.installed.save()
    }

    /// The incomplete installation `receipt` records is gone: that record
    /// leaves the book, wherever it is now, shown or waiting. A record of the
    /// same id in another folder stays.
    pub(super) fn discard(&mut self, receipt: &Receipt<S>) {
        self.incomplete.remove_record(receipt);
        self.save_incomplete();
    }

    /// A disk came or left: shows the receipts whose folder is there or moved
    /// into the games folder, keeps those whose disk is away, and drops those
    /// whose folder went, in both files. A file is written only when a
    /// receipt was renamed or dropped. `running` is the game of the Transfer
    /// under way: its incomplete record stays, last, whatever its folder.
    pub(super) fn refresh(&mut self, running: Option<&S::Game>) {
        self.refresh_installed();
        let (underway, rest): (Vec<_>, Vec<_>) = self
            .incomplete
            .kept()
            .into_iter()
            .partition(|kept| running == Some(&kept.game()));
        let changed = self.incomplete.refile(rest, None);
        self.incomplete.shown.extend(underway);
        if changed {
            self.save_incomplete();
        }
    }

    /// As `refresh`, for the installed receipts alone: the incomplete records
    /// stay as they are.
    pub(super) fn refresh_installed(&mut self) {
        let kept = self.installed.kept();
        if self.installed.refile(kept, Some(&self.folder)) {
            self.save_installed();
        }
    }

    /// The localization keys of the writes that failed since the last call, in
    /// order; empties the queue.
    pub(super) fn take_warnings(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.warnings)
    }

    fn save_installed(&mut self) {
        if self.installed.save().is_err() {
            self.warnings.push(S::RECEIPTS_NOT_SAVED);
        }
    }

    fn save_incomplete(&mut self) {
        if self.incomplete.save().is_err() {
            self.warnings.push(S::RECEIPTS_NOT_SAVED);
        }
    }
}

/// The receipts in `file`, whose folders the book then sorts.
pub(super) fn load<S: Storefront>(file: &Path) -> io::Result<Vec<Receipt<S>>> {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) fn save<S: Storefront>(file: &Path, receipts: &[Receipt<S>]) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    obelisk_core::write_atomic(file, &serde_json::to_vec(receipts)?)
}

/// Sorts receipts by their installation's folder: those there, and those
/// whose disk is not connected. A receipt whose folder moved into
/// `folder`, as when another account of the storefront moved the
/// installation they share, names it there; incomplete installations give
/// none, as they never move with the games folder. Any other receipt whose
/// folder vanished goes: its game was removed outside Obelisk. `true` when a
/// receipt changed.
fn sort<S: Storefront>(
    receipts: Vec<Receipt<S>>,
    folder: Option<&GamesFolder>,
) -> (Vec<Receipt<S>>, Vec<Receipt<S>>, bool) {
    let (mut present, mut offline, mut changed) = (Vec::new(), Vec::new(), false);
    for mut receipt in receipts {
        let directory = PathBuf::from(&receipt.directory);
        if directory.is_dir() {
            present.push(receipt);
        } else if let Some(found) =
            folder.and_then(|folder| folder.installation(S::GAMES_DIRECTORY, &directory))
        {
            receipt.directory = found.to_string_lossy().into_owned();
            changed = true;
            present.push(receipt);
        } else if games_folder::disconnected(&directory) {
            offline.push(receipt);
        } else {
            changed = true;
        }
    }
    (present, offline, changed)
}

#[cfg(test)]
mod tests;
