//! The Reservation rule: what holds an account's installations now, and so
//! what may start. The gate the state publishes and the download queue's
//! turns both come from here, so a button and the queue never disagree.
use super::{Inner, Job, Storefront};
use crate::sessions;

/// What may start now, of one kind of action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opening<G> {
    Closed,
    /// It may start, for any game but these.
    Open {
        except: Vec<G>,
    },
}

impl<G: PartialEq> Opening<G> {
    pub fn allows(&self, game: &G) -> bool {
        match self {
            Self::Closed => false,
            Self::Open { except } => !except.contains(game),
        }
    }
}

/// What the Reservation lets start now. A Game session holds its own game's
/// files, a Transfer its own game, and an uninstall or Cloud maintenance the
/// whole storefront's installations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate<G> {
    /// A Transfer, which waits in the download queue.
    pub modify: Opening<G>,
    /// An uninstall: no Transfer, launch or other uninstall of the
    /// storefront runs.
    pub uninstall: Opening<G>,
    /// A launch: no Game session runs. A Transfer holds only its own game.
    pub launch: Opening<G>,
    pub maintain: Opening<G>,
}

/// What holds an account's installations now: the account, its actions and
/// its storefront's Game sessions.
#[derive(Clone, Debug)]
pub(super) struct Reservation<G> {
    signed_out: bool,
    /// A DEBUG diagnostic fences the storefront like a Game session would.
    diagnostic: bool,
    /// Cloud maintenance runs: it holds every game of the storefront.
    maintaining: bool,
    /// A session a previous Obelisk left may still run.
    recovering: bool,
    /// The game of the storefront's Game session, whichever account's.
    session: Option<G>,
    /// The game of the account's launch still preparing.
    launching: Option<G>,
    /// The game whose files go.
    uninstalling: Option<G>,
    /// A Transfer or move of the account is under way.
    downloading: bool,
    /// The game of that Transfer or move.
    transferring: Option<G>,
    /// The game of each of the account's queued jobs, waiting or under way,
    /// in queue order.
    queued: Vec<G>,
}

impl<G: Clone + PartialEq> Reservation<G> {
    /// What holds `inner`'s installations, beside the Game sessions
    /// `sessions` shows.
    pub(super) fn of<S: Storefront<Game = G>>(
        inner: &Inner<S>,
        sessions: &sessions::State<S>,
    ) -> Self {
        let state = &inner.state;
        Reservation {
            signed_out: inner.signed_out,
            diagnostic: inner.diagnostic,
            maintaining: state.maintenance.is_some(),
            recovering: sessions.recovering,
            session: sessions.active.as_ref().map(|active| active.game.clone()),
            launching: state.launch_game.clone().filter(|_| state.launching),
            uninstalling: state.uninstall_game.clone(),
            downloading: state.downloading,
            transferring: state.transfer_game.clone().filter(|_| state.downloading),
            queued: inner
                .queued
                .iter()
                .map(|queued| queued.transfer.game())
                .collect(),
        }
    }

    /// Nothing starts: the account signed out, a diagnostic or Cloud
    /// maintenance holds the storefront, or a recovery has not settled.
    fn held(&self) -> bool {
        self.signed_out || self.diagnostic || self.maintaining || self.recovering
    }

    /// What may start now, as the state shows it.
    pub(super) fn gate(&self) -> Gate<G> {
        let held = self.held();
        // The games a Game session holds, or whose Transfer is queued or under way.
        let busy: Vec<G> = self.session.iter().chain(&self.queued).cloned().collect();
        // A Transfer waits in the queue: one per game, never of a game a Game
        // session, a Transfer or an uninstall holds.
        let modify = opening(held, busy.iter().chain(&self.uninstalling));
        // An uninstall has the storefront's installations to itself.
        let uninstall = opening(
            held || self.downloading || self.launching.is_some() || self.uninstalling.is_some(),
            &busy,
        );
        // One Cloud operation at a time: a Game session, a launch or maintenance
        // keeps every game out, a Transfer or uninstall only its own.
        let maintain = opening(
            held || self.launching.is_some() || self.session.is_some(),
            self.transferring.iter().chain(&self.uninstalling),
        );
        // A launch waits for another launch and every Game session, and for an
        // uninstall or Cloud maintenance. A Transfer holds only its own game: a
        // launch that needs one goes first in the queue.
        let launch = opening(
            held || self.launching.is_some()
                || self.uninstalling.is_some()
                || self.session.is_some(),
            &self.transferring,
        );
        Gate {
            modify,
            uninstall,
            launch,
            maintain,
        }
    }

    /// Whether the queued `job` of `game` may start now: no Game session
    /// holds its game, and no Transfer, uninstall or Cloud maintenance holds
    /// the storefront. A move also waits for its game's launch, which goes
    /// before it. A launch's own Transfer never asks: it goes first, and
    /// would have a Transfer's answer.
    pub(super) fn starts(&self, game: &G, job: &Job) -> bool {
        !self.held()
            && !self.downloading
            && self.uninstalling.is_none()
            && self.session.as_ref() != Some(game)
            && !(*job == Job::Move && self.launching.as_ref() == Some(game))
    }

    /// Where the games folder's moves may queue now: anywhere but the game
    /// whose files go, unless the account signed out or a diagnostic runs.
    /// Whatever else holds the storefront only delays their turn.
    /// Closed, nothing of the account follows a new games folder either: no
    /// move queues and no new installation still waiting goes there.
    pub(super) fn moves(&self) -> Opening<G> {
        opening(self.signed_out || self.diagnostic, &self.uninstalling)
    }
}

/// `Closed` when `closed`, else open for every game but `except`.
fn opening<'a, G: Clone + 'a>(closed: bool, except: impl IntoIterator<Item = &'a G>) -> Opening<G> {
    match closed {
        true => Opening::Closed,
        false => Opening::Open {
            except: except.into_iter().cloned().collect(),
        },
    }
}

#[cfg(test)]
mod tests;
