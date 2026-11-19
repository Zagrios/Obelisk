//! The accounts of one storefront signed in at once, as their installations
//! reach each other. A storefront's installations are the Mac's: the
//! accounts that own a game share its files, its Wine prefix and its launch
//! settings, each with a receipt of its own. So what one account's actions
//! hold, its claims, holds for its neighbours, a change one makes to an
//! installation reaches the neighbours that hold it, and an uninstall is
//! theirs too. An account's installations opened twice are not neighbours.
//!
//! Locks are taken in one order: the storefront's turn, then an account's
//! installations, then the seats. Nothing takes the turn, or a neighbour's
//! installations, while it holds an account's.
use super::{Shared, Storefront, reservation::Claims};
use std::sync::{
    Arc, Mutex, MutexGuard, Weak,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;

/// One storefront's accounts, which its Game sessions keep for them.
pub(super) struct Neighbours<S: Storefront> {
    /// One change at a time among the storefront's accounts: an action reads
    /// its neighbours' claims and posts its own as one step.
    turn: Mutex<()>,
    seats: Mutex<Vec<Seat<S>>>,
    /// The seats given so far.
    given: AtomicU64,
    /// Told each change of a claim.
    changed: watch::Sender<()>,
}

/// One account's installations among its storefront's.
struct Seat<S: Storefront> {
    id: u64,
    account: S::Account,
    installations: Weak<Shared<S>>,
    claims: Claims<S::Game>,
}

impl<S: Storefront> Default for Neighbours<S> {
    fn default() -> Self {
        Self {
            turn: Mutex::new(()),
            seats: Mutex::new(Vec::new()),
            given: AtomicU64::new(0),
            changed: watch::channel(()).0,
        }
    }
}

impl<S: Storefront> Neighbours<S> {
    /// Seats `account`'s `installations`, which claim `claims`, and returns
    /// their seat.
    pub(super) fn join(
        &self,
        account: &S::Account,
        installations: Weak<Shared<S>>,
        claims: Claims<S::Game>,
    ) -> u64 {
        let id = self.given.fetch_add(1, Ordering::Relaxed);
        self.seats.lock().unwrap().push(Seat {
            id,
            account: account.clone(),
            installations,
            claims,
        });
        id
    }

    /// The installations of `seat` are gone, with what they claimed.
    pub(super) fn leave(&self, seat: u64) {
        self.seats.lock().unwrap().retain(|taken| taken.id != seat);
        self.changed.send_replace(());
    }

    /// Held for one change of an account's installations.
    pub(super) fn turn(&self) -> MutexGuard<'_, ()> {
        self.turn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Posts what the installations of `seat` claim now, and wakes the
    /// storefront's accounts when it changed.
    pub(super) fn claim(&self, seat: u64, claims: Claims<S::Game>) {
        let mut seats = self.seats.lock().unwrap();
        if let Some(taken) = seats
            .iter_mut()
            .find(|taken| taken.id == seat && taken.claims != claims)
        {
            taken.claims = claims;
            self.changed.send_replace(());
        }
    }

    /// What the neighbours of `account` claim.
    pub(super) fn claims(&self, account: &S::Account) -> Vec<Claims<S::Game>> {
        self.seats
            .lock()
            .unwrap()
            .iter()
            .filter(|taken| taken.account != *account)
            .map(|taken| taken.claims.clone())
            .collect()
    }

    /// The installations of the neighbours of `account`.
    pub(super) fn of(&self, account: &S::Account) -> Vec<Arc<Shared<S>>> {
        let seats = self.seats.lock().unwrap();
        seats
            .iter()
            .filter(|taken| taken.account != *account)
            .filter_map(|taken| taken.installations.upgrade())
            .collect()
    }

    /// Tells each change of a claim.
    pub(super) fn changes(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }
}
