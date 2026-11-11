//! The update watch: which of an account's installations have a newer build
//! on their storefront, when the storefront was last asked, and which
//! automatic updates Obelisk already tried. It reads no clock, asks no
//! storefront and queues nothing: the installations tell it what happens,
//! with the instant and the receipts as the Receipt book shows them then, and
//! act on what it answers. It remembers installations by their receipt's id
//! alone.
use super::{Job, Opening, Receipt, Storefront};
use crate::queue;
use std::time::{Duration, Instant};

/// How long the installations' newer builds stay known before the
/// storefront is asked again.
const CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Why the storefront is asked for newer builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Asked {
    /// Obelisk asks as it shows the account: what a check learned less than
    /// ten minutes ago still stands.
    Routine,
    /// The account's session connected again: it is asked at once, as a new
    /// sign-in would be, or as soon as the check under way ends.
    Reconnected,
}

/// What an account's installations know of their storefront's newer builds.
#[derive(Debug, Default)]
pub(super) struct UpdateWatch {
    /// The receipts whose storefront offers a newer build, by their id, the
    /// last learned last.
    known: Vec<String>,
    /// An update check asks the storefront now.
    checking: bool,
    /// The session connected again while it asked: another check follows.
    again: bool,
    /// The receipts a Transfer installed over while it asked, by their id:
    /// its answers about them are about the build the Transfer replaced,
    /// though the receipt shows as it was asked.
    replaced: Vec<String>,
    /// When the last update check ended.
    checked: Option<Instant>,
    /// The player wants each newer build installed as soon as it is known.
    automatic: bool,
    /// The receipts updated automatically since the last update check: one
    /// that failed or was stopped waits for the next check.
    tried: Vec<String>,
}

impl UpdateWatch {
    /// The receipts whose storefront offers a newer build, by their id, as
    /// the state shows them.
    pub(super) fn known(&self) -> &[String] {
        &self.known
    }

    /// An update check is asked for at `now`. When one starts, the
    /// installations to ask about, of those `shown`: all but the ones an
    /// interrupted Transfer left, which are repaired before their game
    /// launches. None starts while one asks, which a reconnection makes ask
    /// again once it ends, nor while the last one still stands.
    pub(super) fn check<S: Storefront>(
        &mut self,
        asked: Asked,
        now: Instant,
        shown: &[Receipt<S>],
    ) -> Option<Vec<Receipt<S>>> {
        if self.checking {
            self.again |= asked == Asked::Reconnected;
            return None;
        }
        let recent = self
            .checked
            .is_some_and(|checked| now.saturating_duration_since(checked) < CHECK_INTERVAL);
        if recent && asked == Asked::Routine {
            return None;
        }
        self.checking = true;
        Some(
            shown
                .iter()
                .filter(|receipt| !receipt.needs_verification)
                .cloned()
                .collect(),
        )
    }

    /// The update check under way ended at `now` with `answers`, each
    /// installation as it was asked about and whether it has a newer build.
    /// An answer counts only when its installation still shows as it was
    /// asked and no Transfer installed over it meanwhile: a Transfer
    /// replaced any other, and the answer is about the build it replaced.
    /// Every automatic update may be tried again. `true` when another check
    /// follows at once.
    #[must_use]
    pub(super) fn checked<S: Storefront>(
        &mut self,
        now: Instant,
        answers: impl IntoIterator<Item = (Receipt<S>, bool)>,
        shown: &[Receipt<S>],
    ) -> bool {
        self.checking = false;
        self.checked = Some(now);
        self.tried.clear();
        let replaced = std::mem::take(&mut self.replaced);
        for (receipt, available) in answers {
            let id = receipt.id();
            if shown.contains(&receipt) && !replaced.contains(&id) {
                self.learned(id, available);
            }
        }
        std::mem::take(&mut self.again)
    }

    /// The storefront told whether the installation `receipt` names has a
    /// newer build, outside an update check: its launch asked.
    pub(super) fn learned(&mut self, receipt: String, available: bool) {
        self.known.retain(|known| *known != receipt);
        if available {
            self.known.push(receipt);
        }
    }

    /// A Transfer installed the storefront's build over the installation
    /// `receipt` names: nothing newer is known of it, whatever the update
    /// check under way answers about it.
    pub(super) fn installed(&mut self, receipt: &str) {
        self.known.retain(|known| known != receipt);
        if self.checking {
            self.replaced.push(receipt.to_owned());
        }
    }

    /// The Receipt book shows other installations than it did: what was
    /// known of one that no longer shows is forgotten.
    pub(super) fn receipts_changed<S: Storefront>(&mut self, shown: &[Receipt<S>]) {
        self.known
            .retain(|known| shown.iter().any(|receipt| receipt.id() == *known));
    }

    /// What a Transfer does to the installation it lands on: it installs a
    /// build the account has not, updates one whose storefront offers a
    /// newer build, and repairs any other.
    pub(super) fn transfer_kind<S: Storefront>(&self, landing: Option<&Receipt<S>>) -> queue::Kind {
        match landing {
            None => queue::Kind::Install,
            Some(receipt) if self.known.contains(&receipt.id()) => queue::Kind::Update,
            Some(_) => queue::Kind::Repair,
        }
    }

    /// The player turns automatic updates on or off. Turned off, the
    /// `queued` Transfers to take out of the queue: Obelisk's own updates,
    /// waiting or under way, each free to queue again once the player turns
    /// them back on.
    pub(super) fn set_automatic<'a>(
        &mut self,
        enabled: bool,
        queued: impl IntoIterator<Item = (u64, &'a Job)>,
    ) -> Vec<u64> {
        self.automatic = enabled;
        if enabled {
            return Vec::new();
        }
        let mut withdrawn = Vec::new();
        for (id, job) in queued {
            if let Some(receipt) = job.automatic() {
                self.tried.retain(|tried| tried != receipt);
                withdrawn.push(id);
            }
        }
        withdrawn
    }

    /// The queued `job`'s Transfer starts. An automatic update is tried
    /// once: should it fail or be stopped, it waits for the next update
    /// check.
    pub(super) fn started(&mut self, job: &Job) {
        if let Some(receipt) = job.automatic() {
            self.tried.push(receipt.to_owned());
        }
    }

    /// The installations Obelisk updates on its own now, of those `shown`:
    /// the player keeps games up to date, a newer build is known, no
    /// automatic update of it was tried since the last update check, and
    /// `modify` lets its Transfer queue.
    pub(super) fn due<'a, S: Storefront>(
        &'a self,
        shown: &'a [Receipt<S>],
        modify: &'a Opening<S::Game>,
    ) -> impl Iterator<Item = &'a Receipt<S>> {
        shown.iter().filter(move |receipt| {
            let id = receipt.id();
            self.automatic
                && self.known.contains(&id)
                && !self.tried.contains(&id)
                && modify.allows(&receipt.game())
        })
    }
}

#[cfg(test)]
mod tests;
