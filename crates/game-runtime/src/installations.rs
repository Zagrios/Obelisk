//! One account's installed games of one storefront, and the actions on them:
//! Transfers, moves into the games folder, launches, uninstalls and Cloud
//! maintenance. Every
//! action asks one Reservation gate, which the state publishes so the UI
//! shows exactly what may start. The account's Installation receipts live
//! here, in the file Obelisk keeps per storefront and account, and each
//! action names one of them: the Transfers it needs are built here and wait
//! in the process's download queue. So is the account's one Cloud question,
//! whichever sync asks it.
mod receipt;

use crate::{
    cloud, cloud_status,
    games_folder::{self, GamesFolder, Resident},
    launch_settings,
    queue::{self, DownloadQueue, Owner},
    runtimes::{self, Runtime},
    sessions::{self, Admission, Canceller, Failure, Sessions},
};
use obelisk_core::{
    Platform,
    installation::TransferProgress,
    platform::Spelling,
    transfer::{CancelToken, TransferState},
};
pub use receipt::{Offer, Receipt, Transfer};
use receipt::{load, save};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// How often a running Transfer's progress is read.
const PROGRESS: Duration = Duration::from_millis(250);
/// How long the installations' newer builds stay known before the
/// storefront is asked again.
const UPDATE_CHECK: Duration = Duration::from_secs(10 * 60);
/// How long one installation's update check may take. A launch that waits
/// longer starts the installed build.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(15);

pub use crate::sessions::BoxFuture;

/// What a storefront adds to its installations.
pub trait Storefront: sessions::Storefront {
    /// What an Offer, a Transfer and an Installation receipt name of one game
    /// beside its platform: Steam's app and depot language, Epic's catalog
    /// item, GOG's product. The receipt file keeps its fields beside the
    /// receipt's own.
    type Product: Clone + fmt::Debug + PartialEq + Eq + Serialize + DeserializeOwned + Send + Sync;
    /// How the storefront spells a platform in its receipt file, receipt ids
    /// and folder names: its kit's.
    const PLATFORM: Spelling;
    /// Shown when the receipt file cannot be read, or saved.
    const RECEIPTS_UNAVAILABLE: &'static str;
    const RECEIPTS_NOT_SAVED: &'static str;
    /// Where its installations go, inside the games folder.
    const GAMES_DIRECTORY: &'static str;
    /// The storefront, as the download queue shows it.
    const QUEUE: queue::Storefront;
    /// What a launch may ask its game to join, such as a friend's lobby.
    type Join: Clone + fmt::Debug + Send + Sync + 'static;

    /// The game `product` names.
    fn product_game(product: &Self::Product) -> Self::Game;
    /// The game `product` names, as Obelisk's profiles look it up.
    fn profile_game(product: &Self::Product) -> crate::profile::Game<'_>;
    /// An installation's id, one per game and platform, as Swift names it.
    fn receipt_id(game: &Self::Game, platform: Platform) -> String;
    /// The name a Transfer's progress gives the game.
    fn transfer_name(game: &Self::Game) -> String;
    /// Fills in what an older receipt file did not record, from the
    /// installation itself. `true` when it changed the receipt.
    fn complete(receipt: &mut Receipt<Self>) -> bool {
        let _ = receipt;
        false
    }
}

/// A storefront's part of one account's installations: where its games and
/// their Cloud saves live, and what its session does for them. Steam's and
/// Epic's are `steam::content` and `epic::content`.
pub trait Content<S: Storefront>: Send + Sync {
    /// Starts `transfer` before its runtimes: renews and saves the account's
    /// credentials for a storefront whose session rotates them, and returns
    /// the platform of the build it installs, the one it names or else the
    /// one the storefront picks.
    fn begin<'a>(&'a self, transfer: &'a Transfer<S>) -> BoxFuture<'a, Result<Platform, Failure>>;
    /// Downloads the game's files of `transfer`, whose platform is settled
    /// and whose runtimes are in place, and returns the receipt of the
    /// completed installation.
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<S>,
    ) -> BoxFuture<'a, Result<Receipt<S>, Failure>>;
    /// The progress of the game's files.
    fn progress(&self) -> Option<TransferProgress>;
    /// Stops the Transfer's account calls and the download of its files.
    fn cancel(&self);
    /// Whether the storefront offers a newer build than the one `receipt`
    /// installed. It asks the storefront, never reads the game's files.
    fn update_available<'a>(
        &'a self,
        receipt: &'a Receipt<S>,
    ) -> BoxFuture<'a, Result<bool, Failure>>;
    /// Waits until the storefront's session reaches it through another
    /// connection than `connection`, and returns that one's number; the
    /// first is 0. Only Steam's account session connects again by itself:
    /// the others never return.
    fn reconnected(&self, connection: u64) -> BoxFuture<'_, u64> {
        let _ = connection;
        Box::pin(std::future::pending())
    }
    /// Hands a verified installation to its supervisor for its admission, its
    /// game joining `join` when set, and returns once the session is finished.
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt<S>,
        admission: Admission<S>,
        join: Option<&'a S::Join>,
    ) -> BoxFuture<'a, Result<(), Failure>>;
    /// Saves the game's Cloud archive before its files go.
    fn archive<'a>(&'a self, receipt: &'a Receipt<S>) -> BoxFuture<'a, Result<(), Failure>>;
    /// Records the removal for Cloud sync. Runs even after sign-out.
    fn removed<'a>(&'a self, receipt: &'a Receipt<S>) -> BoxFuture<'a, Result<(), Failure>>;
    /// The dedicated folder name the storefront installed `receipt` into.
    fn directory_name(&self, receipt: &Receipt<S>) -> String;
    /// Follows the installation `receipt` names, which moved from `from`:
    /// what Obelisk keeps of it by its folder names its new place. It blocks
    /// on file writes.
    fn relocated(&self, receipt: &Receipt<S>, from: &Path) -> Result<(), String>;
    /// Deletes a Windows installation's Wine prefix once its folder is gone.
    /// `None` when the installation has none.
    fn remove_prefix(&self, receipt: &Receipt<S>) -> Option<Result<(), String>>;
    /// Syncs the game's Cloud saves on the player's request: those of
    /// `installed`, or, for an uninstalled game, those its saved Cloud context
    /// names. `control` shows the sync's progress and asks the player.
    fn maintain<'a>(
        &'a self,
        game: &'a S::Game,
        installed: Option<&'a Receipt<S>>,
        control: Arc<cloud::Control>,
    ) -> BoxFuture<'a, Result<(), Failure>>;
    /// The Cloud indicators of `games` from their journals, and of every other
    /// game of the account with pending work. It blocks on file reads.
    fn indicators(
        &self,
        games: &[S::Game],
    ) -> Result<Vec<(S::Game, cloud_status::Indicator)>, Failure>;
}

/// Cloud maintenance of one game, which the player asked for: its sync's
/// progress and the Cloud question it waits on.
#[derive(Clone, Debug, PartialEq)]
pub struct Maintenance<S: Storefront> {
    pub game: S::Game,
    pub cloud: cloud::Status,
}

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

/// Why a Cloud sync runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKind {
    /// Before a launch's game starts.
    Launch,
    /// After its game exited.
    Exit,
    /// On the player's request.
    Maintenance,
}

/// What a Cloud question asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    /// Which saves to keep.
    Conflict,
    /// Whether to retry, cancel or launch anyway after a launch check failed.
    Failed,
}

/// The one Cloud question the account asks, until it has its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub decision_id: u64,
    pub kind: QuestionKind,
    /// Localization key of what the sync reports.
    pub error: Option<String>,
    pub choices: Vec<cloud::Choice>,
}

/// The account's Cloud sync running now, whichever asks: a launch's, an
/// exit's or a maintenance's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudSync<G> {
    pub game: G,
    pub kind: SyncKind,
    /// Binds a cancellation to the sync as it shows now.
    pub decision_id: u64,
    /// It compares or transfers saves now.
    pub working: bool,
    /// Its Game session's control connection was lost: nothing reaches it.
    pub reconnecting: bool,
    /// The player may cancel it: a launch check still in progress.
    pub cancellable: bool,
    pub question: Option<Question>,
}

/// A failure the player sees, and what it is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// A storefront's localization key.
    Storefront(String),
    /// The chosen folder could not be created.
    DownloadDirectory(String),
    /// An uninstall failed for this reason, a localization key.
    Uninstall(String),
    /// A move into the games folder failed for this reason, a localization
    /// key or the system's message.
    Move(String),
    /// The games folder's disk is not connected: this folder.
    GamesFolderUnavailable(PathBuf),
}

/// Everything the account's installations present. `revision` grows with
/// every change.
#[derive(Clone, Debug, PartialEq)]
pub struct State<S: Storefront> {
    pub revision: u64,
    pub receipts: Vec<Receipt<S>>,
    /// The last Transfer progress.
    pub transfer: Option<TransferProgress>,
    /// The game the last Transfer progress belongs to.
    pub transfer_game: Option<S::Game>,
    pub downloading: bool,
    pub launching: bool,
    /// The launch runs a Transfer first: it repairs an interrupted one, or
    /// installs the storefront's newer build.
    pub verifying_before_launch: bool,
    pub launch_game: Option<S::Game>,
    pub uninstall_game: Option<S::Game>,
    /// A Transfer that stopped before its receipt, which the player can resume.
    pub last_download: Option<Transfer<S>>,
    /// The receipts of the new installations whose Transfer has not
    /// succeeded yet, kept apart from the installed games': their files
    /// stay in their folder until the player resumes or deletes them, even
    /// after Obelisk quits.
    pub incomplete: Vec<Receipt<S>>,
    /// The failure shown, until the next action clears it.
    pub report: Option<Report>,
    /// A storage warning, a localization key, with an id that changes each
    /// time one is raised.
    pub warning: Option<(u64, String)>,
    pub maintenance: Option<Maintenance<S>>,
    pub gate: Gate<S::Game>,
    pub cloud_sync: Option<CloudSync<S::Game>>,
    /// The receipts whose storefront offers a newer build, by their id.
    pub updates: Vec<String>,
    /// The games whose Transfer waits in the download queue.
    pub queued: Vec<S::Game>,
}

/// One of the account's Transfers in the download queue.
struct Queued<S: Storefront> {
    id: u64,
    transfer: Transfer<S>,
    job: Job,
}

/// What a queued Transfer does once its turn comes.
#[derive(PartialEq)]
enum Job {
    /// It transfers files. `automatic` names the receipt Obelisk updates on
    /// its own.
    Transfer { automatic: Option<String> },
    /// A launch's own Transfer, which the launch runs itself.
    Launch,
    /// It moves the installation `transfer` lands on into the games folder,
    /// rather than transferring files.
    Move,
}

/// Where an account's Transfers queue, and how the queue names them.
pub struct Queueing<S: Storefront> {
    pub queue: Arc<DownloadQueue>,
    /// The account, as the queue shows it.
    pub account: String,
    /// A game, as the queue shows it.
    pub name: fn(&S::Game) -> String,
}

struct Inner<S: Storefront> {
    state: State<S>,
    /// The receipts whose disk is not connected: saved, never shown.
    offline: Vec<Receipt<S>>,
    /// The incomplete receipts whose disk is not connected.
    incomplete_offline: Vec<Receipt<S>>,
    signed_out: bool,
    /// A DEBUG diagnostic fences the storefront like a Game session would.
    diagnostic: bool,
    /// Stops the Transfer under way, its runtimes included.
    transfer_cancel: CancelToken,
    /// The player stopped the launch still preparing.
    launch_cancelled: bool,
    /// Cancels the launch still preparing.
    launch: Option<Canceller<S>>,
    warnings: u64,
    /// The running maintenance, which its number tells from a later one.
    maintenance: Option<(u64, Arc<cloud::Control>)>,
    maintenances: u64,
    /// The last question answered, by the sync that asked it: it no longer
    /// shows, until that sync asks another.
    answered: Option<(String, u64)>,
    /// An update check asks the storefront now.
    checking_updates: bool,
    /// The session connected again while it asked: another check follows.
    check_again: bool,
    /// When the last update check finished.
    updates_checked: Option<Instant>,
    /// The player wants each newer build installed as soon as it is known.
    automatic_updates: bool,
    /// The receipts updated automatically since the last update check: one
    /// that failed or was stopped waits for the next check.
    automatic_skipped: Vec<String>,
    /// The account's Transfers in the download queue, waiting or running.
    queued: Vec<Queued<S>>,
    /// The queue id of the account's Transfer under way.
    running: Option<u64>,
}

struct Shared<S: Storefront> {
    inner: Mutex<Inner<S>>,
    published: watch::Sender<State<S>>,
    content: Arc<dyn Content<S>>,
    sessions: Arc<Sessions<S>>,
    account: S::Account,
    receipts_file: PathBuf,
    /// Keeps the incomplete receipts, beside `receipts_file`: an Obelisk
    /// that does not know them never reads them as installed games.
    incomplete_file: PathBuf,
    /// Where new installations go and every installation moves, inside
    /// Obelisk's library directory unless the player chose another.
    folder: Arc<GamesFolder>,
    /// Obelisk's library directory and the user's home: never removed.
    protected: Vec<PathBuf>,
    queueing: Queueing<S>,
    /// Publishes the gate again whenever the Game sessions change, and
    /// queues automatic updates whenever the state lets one start.
    watchers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// What the download queue asks of an account's installations.
impl<S: Storefront> Owner for Shared<S> {
    /// Whether the queued Transfer `id` may start now: no Game session holds
    /// its game, and no Transfer, uninstall or Cloud maintenance holds the
    /// storefront.
    fn ready(&self, id: u64) -> bool {
        let inner = self.inner.lock().unwrap();
        let Some(queued) = inner.queued.iter().find(|queued| queued.id == id) else {
            return false;
        };
        let game = queued.transfer.game();
        let sessions = self.sessions.state();
        let launching = inner.state.launching && inner.state.launch_game.as_ref() == Some(&game);
        !inner.signed_out
            && !inner.diagnostic
            && !inner.state.downloading
            && inner.state.uninstall_game.is_none()
            && inner.state.maintenance.is_none()
            && !sessions.recovering
            && !sessions.active.iter().any(|active| active.game == game)
            && !(queued.job == Job::Move && launching)
    }

    /// Starts the queued Transfer `id`, whose turn came.
    fn start(self: Arc<Self>, id: u64) {
        let moving = self.update(|inner| {
            let queued = inner.queued.iter().find(|queued| queued.id == id)?;
            (queued.job == Job::Move).then(|| queued.transfer.clone())
        });
        if let Some(transfer) = moving {
            // A move goes to the games folder as its turn comes.
            let folder = self.folder.path();
            runtime().spawn(async move { self.relocate(id, &transfer, &folder).await });
            return;
        }
        let started = self.update(|inner| {
            let queued = inner.queued.iter().find(|queued| queued.id == id)?;
            let transfer = queued.transfer.clone();
            let automatic = match &queued.job {
                Job::Transfer { automatic } => automatic.clone(),
                _ => None,
            };
            if let Some(receipt) = &automatic {
                inner.automatic_skipped.push(receipt.clone());
            }
            self.begin_in(inner, id, &transfer);
            Some((transfer, automatic.is_some()))
        });
        let Some((transfer, automatic)) = started else {
            self.queueing.queue.finished(id);
            return;
        };
        runtime().spawn(async move {
            let outcome = self.run(id, &transfer).await;
            self.update(|inner| {
                if let Err(Failure::Failed(key)) = outcome
                    && !automatic
                    && !inner.transfer_cancel.is_cancelled()
                    && !inner.signed_out
                {
                    inner.state.report = Some(Report::Storefront(key));
                }
            });
        });
    }

    /// Stops the Transfer `id` when it is the one under way.
    fn stop(&self, id: u64) {
        let stopping = self.update(|inner| {
            let stopping = inner.running == Some(id);
            if stopping {
                inner.transfer_cancel.cancel();
            }
            // A move has no storefront call to stop.
            stopping
                && inner
                    .queued
                    .iter()
                    .any(|queued| queued.id == id && queued.job != Job::Move)
        });
        if stopping {
            self.content.cancel();
        }
    }

    fn removed(&self, id: u64) {
        self.update(|inner| inner.queued.retain(|queued| queued.id != id));
    }
}

impl<S: Storefront> Drop for Shared<S> {
    fn drop(&mut self) {
        for watcher in self.watchers.lock().unwrap().drain(..) {
            watcher.abort();
        }
    }
}

/// The runtime every installation action runs on.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("obelisk-installations")
            .enable_all()
            .build()
            .expect("the installations runtime starts")
    })
}

impl<S: Storefront> Shared<S> {
    fn update<T>(&self, change: impl FnOnce(&mut Inner<S>) -> T) -> T {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.state.clone();
        let result = change(&mut inner);
        inner.state.downloading = inner.running.is_some();
        let sessions = self.sessions.state();
        inner.state.gate = gate(&inner, &sessions);
        inner.state.cloud_sync = cloud_sync(&inner, &sessions, &self.account);
        if inner.state.receipts != before.receipts {
            let State {
                updates, receipts, ..
            } = &mut inner.state;
            updates.retain(|id| receipts.iter().any(|receipt| receipt.id() == *id));
        }
        // A game whose move waits still launches until its turn.
        let mut queued = Vec::new();
        for waiting in &inner.queued {
            let game = waiting.transfer.game();
            if Some(waiting.id) != inner.running
                && matches!(waiting.job, Job::Transfer { .. })
                && !queued.contains(&game)
            {
                queued.push(game);
            }
        }
        inner.state.queued = queued;
        let mut after = inner.state.clone();
        after.revision = before.revision;
        if after != before {
            inner.state.revision += 1;
            self.published.send_replace(inner.state.clone());
        }
        result
    }

    fn warn(inner: &mut Inner<S>, key: &str) {
        inner.warnings += 1;
        inner.state.warning = Some((inner.warnings, key.to_owned()));
    }

    fn save(&self, inner: &mut Inner<S>) {
        if save(&self.receipts_file, &inner.saved()).is_err() {
            Self::warn(inner, S::RECEIPTS_NOT_SAVED);
        }
    }

    fn save_incomplete(&self, inner: &mut Inner<S>) {
        if save(&self.incomplete_file, &inner.saved_incomplete()).is_err() {
            Self::warn(inner, S::RECEIPTS_NOT_SAVED);
        }
    }

    /// Records the new installation `transfer`, whose platform is settled,
    /// as incomplete before its files arrive: a download Obelisk quits
    /// during still resumes where it stopped, or has its files deleted. A
    /// folder already there is not this download's: its folder carries no
    /// account, so it may be another account's installation.
    fn record_incomplete(&self, inner: &mut Inner<S>, transfer: &Transfer<S>) {
        let Some(platform) = transfer.platform else {
            return;
        };
        if inner
            .state
            .receipts
            .iter()
            .chain(&inner.state.incomplete)
            .any(|kept| transfer.lands_on(kept))
        {
            return;
        }
        let mut receipt = Receipt {
            product: transfer.product.clone(),
            name: transfer.name.clone(),
            directory: String::new(),
            platform,
            total_bytes: 0,
            needs_verification: true,
        };
        let directory = transfer
            .directory
            .join(self.content.directory_name(&receipt));
        if directory.exists() {
            return;
        }
        receipt.directory = directory.to_string_lossy().into_owned();
        inner.state.incomplete.push(receipt);
        self.save_incomplete(inner);
    }

    /// Marks `game`'s files as going, when the gate lets them.
    fn begin_removal(&self, inner: &mut Inner<S>, game: S::Game) -> bool {
        if !self.gate(inner).uninstall.allows(&game) {
            return false;
        }
        inner.state.uninstall_game = Some(game);
        inner.state.report = None;
        true
    }

    /// The Reservation gate every action asks.
    fn gate(&self, inner: &Inner<S>) -> Gate<S::Game> {
        gate(inner, &self.sessions.state())
    }

    /// Shows maintenance `number`'s progress while it runs. Retry and Launch
    /// anyway belong to a launch.
    fn show_maintenance(&self, number: u64, mut status: cloud::Status) {
        status
            .allowed_choices
            .retain(|choice| !matches!(choice, cloud::Choice::Retry | cloud::Choice::LaunchAnyway));
        self.update(|inner| {
            if inner
                .maintenance
                .as_ref()
                .is_some_and(|(running, _)| *running == number)
                && let Some(maintenance) = &mut inner.state.maintenance
            {
                maintenance.cloud = status;
            }
        });
    }

    /// Marks the queued Transfer `id`'s game before its first progress
    /// snapshot arrives.
    fn begin_in(&self, inner: &mut Inner<S>, id: u64, transfer: &Transfer<S>) {
        inner.running = Some(id);
        inner.transfer_cancel = CancelToken::new();
        inner.state.last_download = Some(transfer.clone());
        inner.state.transfer_game = Some(transfer.game());
        inner.state.transfer = Some(TransferProgress::new(transfer.progress_name()));
        if let Some(receipt) = inner
            .state
            .receipts
            .iter_mut()
            .find(|receipt| transfer.lands_on(receipt))
        {
            receipt.needs_verification = true;
            self.save(inner);
        }
    }

    /// What the queue shows of `transfer`.
    fn queue_request(
        &self,
        transfer: &Transfer<S>,
        kind: queue::Kind,
        automatic: bool,
    ) -> queue::Request {
        queue::Request {
            storefront: S::QUEUE,
            account: self.queueing.account.clone(),
            game: (self.queueing.name)(&transfer.game()),
            name: transfer.name.clone(),
            kind,
            automatic,
        }
    }

    /// Queues `transfer` when the gate lets it. `automatic` names the receipt
    /// Obelisk updates on its own: its failure shows only on its game, and it
    /// waits for the next update check.
    fn download(self: &Arc<Self>, transfer: Transfer<S>, automatic: Option<String>) {
        let queued = self.update(|inner| {
            let game = transfer.game();
            if !self.gate(inner).modify.allows(&game) {
                return false;
            }
            if !transfer.directory.is_dir() && !self.folder.connected() {
                if automatic.is_none() {
                    inner.state.report = Some(Report::GamesFolderUnavailable(self.folder.path()));
                }
                return false;
            }
            if let Err(error) = std::fs::create_dir_all(&transfer.directory) {
                if automatic.is_none() {
                    inner.state.report = Some(Report::DownloadDirectory(error.to_string()));
                }
                return false;
            }
            if automatic.is_none() {
                inner.state.report = None;
            }
            let installed = inner
                .state
                .receipts
                .iter()
                .find(|receipt| transfer.lands_on(receipt));
            let kind = match installed {
                None => queue::Kind::Install,
                Some(receipt) if inner.state.updates.contains(&receipt.id()) => queue::Kind::Update,
                Some(_) => queue::Kind::Repair,
            };
            // The queue calls no owner while it adds: the game is queued at once.
            let id = self.queueing.queue.push(
                &self.owner(),
                self.queue_request(&transfer, kind, automatic.is_some()),
            );
            inner.queued.push(Queued {
                id,
                transfer: transfer.clone(),
                job: Job::Transfer {
                    automatic: automatic.clone(),
                },
            });
            true
        });
        if queued {
            self.queueing.queue.advance();
        }
    }

    /// Runs a launch's own Transfer: it goes first in the queue, and the one
    /// under way pauses until it is done. A Transfer of the game already
    /// queued becomes this one.
    async fn launch_transfer(
        self: &Arc<Self>,
        transfer: &Transfer<S>,
        kind: queue::Kind,
    ) -> Result<Receipt<S>, Failure> {
        let game = transfer.game();
        // The game's move waits behind it.
        let superseded: Vec<u64> = self.update(|inner| {
            inner
                .queued
                .iter()
                .filter(|queued| queued.transfer.game() == game && queued.job != Job::Move)
                .map(|queued| queued.id)
                .collect()
        });
        for id in superseded {
            self.queueing.queue.remove(id);
        }
        let (id, turn) = self
            .queueing
            .queue
            .push_first(&self.owner(), self.queue_request(transfer, kind, false));
        self.update(|inner| {
            inner.queued.push(Queued {
                id,
                transfer: transfer.clone(),
                job: Job::Launch,
            })
        });
        if turn.await.is_err() {
            self.update(|inner| inner.queued.retain(|queued| queued.id != id));
            return Err(Failure::Cancelled);
        }
        self.update(|inner| self.begin_in(inner, id, transfer));
        self.run(id, transfer).await
    }

    /// Runs the queued Transfer `id` `begin_in` announced, and ends it in
    /// the queue. One the queue stopped to pause it stays queued.
    async fn run(self: &Arc<Self>, id: u64, transfer: &Transfer<S>) -> Result<Receipt<S>, Failure> {
        let outcome = self.perform(transfer).await;
        self.finish(id);
        outcome
    }

    /// Ends the queued Transfer `id` in the queue. One the queue stopped to
    /// pause it stays queued.
    fn finish(&self, id: u64) {
        if !self.queueing.queue.finished(id) {
            self.update(|inner| inner.queued.retain(|queued| queued.id != id));
        }
    }

    fn owner(self: &Arc<Self>) -> Arc<dyn Owner> {
        self.clone()
    }

    /// Queues the Transfer of every installation with a newer build when the
    /// player keeps games up to date. The queue runs them one at a time, and
    /// holds them while a game runs.
    fn update_automatically(self: &Arc<Self>) {
        let transfers: Vec<(Transfer<S>, String)> = {
            let inner = self.inner.lock().unwrap();
            if !inner.automatic_updates || inner.signed_out {
                return;
            }
            let modify = self.gate(&inner).modify;
            inner
                .state
                .receipts
                .iter()
                .filter(|receipt| {
                    let id = receipt.id();
                    inner.state.updates.contains(&id)
                        && !inner.automatic_skipped.contains(&id)
                        && modify.allows(&receipt.game())
                })
                .map(|receipt| {
                    let transfer = inner
                        .resumable(&receipt.game())
                        .unwrap_or_else(|| receipt.transfer());
                    (transfer, receipt.id())
                })
                .collect()
        };
        for (transfer, id) in transfers {
            self.download(transfer, Some(id));
        }
    }

    /// Whether the storefront offers a newer build of `receipt`'s
    /// installation. `None` when it could not tell in time.
    async fn ask_update(&self, receipt: &Receipt<S>) -> Option<bool> {
        tokio::time::timeout(UPDATE_TIMEOUT, self.content.update_available(receipt))
            .await
            .ok()?
            .ok()
    }

    /// Records whether the receipt `id` has a newer build, which the state
    /// then shows.
    fn mark_update(inner: &mut Inner<S>, id: String, available: bool) {
        inner.state.updates.retain(|kept| *kept != id);
        if available {
            inner.state.updates.push(id);
        }
    }

    /// Asks whether each installation has a newer build, unless a check ran
    /// less than ten minutes ago. Once the session connected `again`, it
    /// asks at once, as a new sign-in would, or as soon as the check under
    /// way ends.
    fn check_updates(self: &Arc<Self>, again: bool) {
        let started = self.update(|inner| {
            if inner.signed_out {
                return None;
            }
            if inner.checking_updates {
                inner.check_again |= again;
                return None;
            }
            let recent = inner
                .updates_checked
                .is_some_and(|checked| checked.elapsed() < UPDATE_CHECK);
            if recent && !again {
                return None;
            }
            inner.checking_updates = true;
            Some(inner.state.receipts.clone())
        });
        let Some(receipts) = started else {
            return;
        };
        let shared = self.clone();
        runtime().spawn(async move {
            // Every installation is asked at once, each within its timeout.
            let mut checks = tokio::task::JoinSet::new();
            for receipt in receipts {
                // An interrupted Transfer is repaired before its game launches.
                if receipt.needs_verification {
                    continue;
                }
                let shared = shared.clone();
                checks.spawn(async move {
                    let available = shared.ask_update(&receipt).await;
                    (receipt, available)
                });
            }
            let mut found = Vec::new();
            while let Some(checked) = checks.join_next().await {
                if let Ok((receipt, Some(available))) = checked {
                    found.push((receipt, available));
                }
            }
            let again = shared.update(|inner| {
                inner.checking_updates = false;
                inner.updates_checked = Some(Instant::now());
                inner.automatic_skipped.clear();
                for (receipt, available) in found {
                    // A Transfer replaced the receipt meanwhile: its answer is
                    // about the build it replaced.
                    if inner.state.receipts.contains(&receipt) {
                        Shared::mark_update(inner, receipt.id(), available);
                    }
                }
                std::mem::take(&mut inner.check_again)
            });
            shared.update_automatically();
            if again {
                shared.check_updates(true);
            }
        });
    }

    /// Asks whether `receipt`'s installation has a newer build, and records it.
    async fn newer_build(&self, receipt: &Receipt<S>) -> Option<bool> {
        let available = self.ask_update(receipt).await?;
        self.update(|inner| Self::mark_update(inner, receipt.id(), available));
        Some(available)
    }

    /// Runs the Transfer `begin_in` announced, whose turn came in the queue:
    /// the storefront begins it, its runtimes come, then the game's files.
    async fn perform(self: &Arc<Self>, transfer: &Transfer<S>) -> Result<Receipt<S>, Failure> {
        let name = transfer.progress_name();
        let cancel = self.inner.lock().unwrap().transfer_cancel.clone();
        if cancel.is_cancelled() {
            self.update(|inner| {
                if let Some(progress) = &mut inner.state.transfer {
                    progress.state = TransferState::Cancelled;
                }
                inner.running = None;
            });
            return Err(Failure::Cancelled);
        }
        // How the Transfer stopped while it got a runtime.
        let mut stopped = None;
        let result = async {
            let platform = self.content.begin(transfer).await?;
            let got = self
                .sessions
                .runtimes()
                .get::<S>(platform, &cancel, |progress| {
                    self.update(|inner| inner.state.transfer = Some(shown(&name, progress)));
                })
                .await;
            if let Err(failure) = &got {
                // No bytes: once the Transfer stops, they would read as the
                // game's own.
                let (state, message) = match failure {
                    Failure::Cancelled => (TransferState::Cancelled, String::new()),
                    Failure::Failed(key) => (TransferState::Failed, key.clone()),
                };
                stopped = Some(TransferProgress {
                    state,
                    message,
                    ..TransferProgress::new(&name)
                });
            }
            got?;
            let transfer = Transfer {
                platform: Some(platform),
                ..transfer.clone()
            };
            self.update(|inner| self.record_incomplete(inner, &transfer));
            let polling = {
                let (shared, name) = (self.clone(), name.clone());
                runtime().spawn(async move {
                    loop {
                        if let Some(progress) = shared.content.progress()
                            && progress.game == name
                            && active(progress.state)
                        {
                            shared.update(|inner| {
                                if inner.state.transfer.as_ref() != Some(&progress) {
                                    inner.state.transfer = Some(progress);
                                }
                            });
                        }
                        tokio::time::sleep(PROGRESS).await;
                    }
                })
            };
            let downloaded = self.content.download(&transfer).await;
            polling.abort();
            downloaded
        }
        .await;
        let outcome = match result {
            // A receipt that arrives after its cancellation is refused.
            Ok(_) if cancel.is_cancelled() => Err(Failure::Cancelled),
            Ok(receipt) => Ok(receipt),
            Err(failure) => {
                let cancelled = failure == Failure::Cancelled || cancel.is_cancelled();
                Err(if cancelled {
                    Failure::Cancelled
                } else {
                    failure
                })
            }
        };
        let settled = match &outcome {
            Ok(_) => TransferState::Complete,
            Err(Failure::Cancelled) => TransferState::Cancelled,
            Err(_) => TransferState::Failed,
        };
        let last = stopped
            .or_else(|| self.content.progress())
            .filter(|progress| progress.game == name && progress.state == settled);
        self.update(|inner| {
            if let Ok(receipt) = &outcome {
                let id = receipt.id();
                inner.state.receipts.retain(|kept| kept.id() != id);
                inner.state.receipts.push(receipt.clone());
                inner.state.updates.retain(|kept| *kept != id);
                inner.state.last_download = None;
                self.save(inner);
                if inner.state.incomplete.iter().any(|kept| kept.id() == id) {
                    inner.state.incomplete.retain(|kept| kept.id() != id);
                    self.save_incomplete(inner);
                }
            }
            match last {
                Some(progress) => inner.state.transfer = Some(progress),
                None => {
                    if let Some(progress) = &mut inner.state.transfer {
                        progress.message.clear();
                    }
                }
            }
            if let Some(progress) = &mut inner.state.transfer {
                progress.state = settled;
            }
            inner.running = None;
        });
        outcome
    }
}

/// Blocking work off the async runtime; `failed` names a lost join.
pub(crate) async fn blocking<T: Send + 'static>(
    failed: impl Into<String>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Failure> {
    let failed = failed.into();
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Failure::Failed(failed))
}

/// The credentials every account call renews and saves first, for a
/// storefront whose session rotates them, as Epic's and GOG's do.
pub(crate) struct Credentials {
    /// Waits until the credentials an account call rotated are saved. It
    /// blocks.
    pub(crate) saved: Arc<dyn Fn() + Send + Sync>,
    /// What a save lost with its thread reports.
    failed: &'static str,
    /// A Transfer was cancelled while its credentials were being renewed.
    cancelled: AtomicBool,
}

impl Credentials {
    pub(crate) fn new(saved: Arc<dyn Fn() + Send + Sync>, failed: &'static str) -> Self {
        Self {
            saved,
            failed,
            cancelled: AtomicBool::new(false),
        }
    }

    /// Renewed by `renew`, then saved, before an account call. A
    /// cancellation meanwhile stops the call before it starts.
    pub(crate) async fn prepare(
        &self,
        renew: impl Future<Output = Result<(), Failure>>,
    ) -> Result<(), Failure> {
        self.cancelled.store(false, Ordering::Release);
        renew.await?;
        self.save().await?;
        match self.cancelled.load(Ordering::Acquire) {
            true => Err(Failure::Cancelled),
            false => Ok(()),
        }
    }

    /// Waits until the credentials an account call rotated are saved.
    pub(crate) async fn save(&self) -> Result<(), Failure> {
        let saved = self.saved.clone();
        blocking(self.failed, move || saved()).await
    }

    /// Saves them once the caller moved on: a completed Transfer's receipt
    /// never waits for Keychain.
    pub(crate) fn save_later(&self) {
        let saved = self.saved.clone();
        tokio::task::spawn_blocking(move || saved());
    }

    /// Stops the account call whose credentials `prepare` renews.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

fn active(state: TransferState) -> bool {
    matches!(
        state,
        TransferState::Preparing | TransferState::Verifying | TransferState::Downloading
    )
}

/// What the Reservation lets start, given the account's installations and the
/// storefront's Game sessions.
fn gate<S: Storefront>(inner: &Inner<S>, sessions: &sessions::State<S>) -> Gate<S::Game> {
    let state = &inner.state;
    let transferring = state.transfer_game.clone().filter(|_| state.downloading);
    let uninstalled = state.uninstall_game.clone();
    let held =
        inner.signed_out || inner.diagnostic || state.maintenance.is_some() || sessions.recovering;
    // The games a Game session holds, or whose Transfer is queued or under way.
    let busy: Vec<S::Game> = sessions
        .active
        .iter()
        .map(|active| active.game.clone())
        .chain(inner.queued.iter().map(|queued| queued.transfer.game()))
        .collect();
    // A Transfer waits in the queue: one per game, never of a game a Game
    // session, a Transfer or an uninstall holds.
    let modify = match held {
        true => Opening::Closed,
        false => Opening::Open {
            except: busy.iter().cloned().chain(uninstalled.clone()).collect(),
        },
    };
    // An uninstall has the storefront's installations to itself.
    let uninstall = match held || state.downloading || state.launching || uninstalled.is_some() {
        true => Opening::Closed,
        false => Opening::Open { except: busy },
    };
    // One Cloud operation at a time: a Game session, a launch or maintenance
    // keeps every game out, a Transfer or uninstall only its own.
    let cloud = inner.signed_out
        || inner.diagnostic
        || state.maintenance.is_some()
        || state.launching
        || sessions.busy();
    let maintain = match cloud {
        true => Opening::Closed,
        false => Opening::Open {
            except: [transferring.clone(), uninstalled]
                .into_iter()
                .flatten()
                .collect(),
        },
    };
    // A launch waits for another launch and every Game session, and for an
    // uninstall or Cloud maintenance. A Transfer holds only its own game: a
    // launch that needs one goes first in the queue.
    let launches = held || state.launching || state.uninstall_game.is_some() || sessions.busy();
    let launch = match launches {
        true => Opening::Closed,
        false => Opening::Open {
            except: transferring.into_iter().collect(),
        },
    };
    Gate {
        modify,
        uninstall,
        launch,
        maintain,
    }
}

/// The account's Cloud sync now: its maintenance's, else its Game session's
/// before the game starts or after it exited. A revoked account, or another
/// account's session, asks nothing.
fn cloud_sync<S: Storefront>(
    inner: &Inner<S>,
    sessions: &sessions::State<S>,
    account: &S::Account,
) -> Option<CloudSync<S::Game>> {
    let asks = |source: String, status: &cloud::Status, kind: QuestionKind| {
        (!status.allowed_choices.is_empty()
            && inner.answered.as_ref() != Some(&(source, status.decision_id)))
        .then(|| Question {
            decision_id: status.decision_id,
            kind,
            error: status.error.clone(),
            choices: status.allowed_choices.clone(),
        })
    };
    if let (Some(maintenance), Some((number, _))) = (&inner.state.maintenance, &inner.maintenance) {
        let status = &maintenance.cloud;
        let conflict = status.phase == cloud::Phase::Conflict;
        return Some(CloudSync {
            game: maintenance.game.clone(),
            kind: SyncKind::Maintenance,
            decision_id: status.decision_id,
            working: !conflict,
            reconnecting: false,
            cancellable: false,
            question: conflict
                .then(|| asks(maintenance_source(*number), status, QuestionKind::Conflict))
                .flatten(),
        });
    }
    let active = sessions
        .active
        .as_ref()
        .filter(|active| active.observed && &active.account == account)?;
    let kind = match active.phase {
        crate::supervisor::Phase::Preparing => SyncKind::Launch,
        crate::supervisor::Phase::Finalizing => SyncKind::Exit,
        _ => return None,
    };
    let status = &active.cloud;
    let working = matches!(status.phase, cloud::Phase::Checking | cloud::Phase::Syncing);
    let question = match status.phase {
        _ if sessions.revoked.contains(account) => None,
        cloud::Phase::Conflict => asks(active.id.clone(), status, QuestionKind::Conflict),
        cloud::Phase::Failed => asks(active.id.clone(), status, QuestionKind::Failed),
        _ => None,
    };
    Some(CloudSync {
        game: active.game.clone(),
        kind,
        decision_id: status.decision_id,
        working,
        reconnecting: active.reconnecting,
        cancellable: kind == SyncKind::Launch && working && !active.reconnecting,
        question,
    })
}

/// What tells a maintenance's questions from a later maintenance's.
fn maintenance_source(number: u64) -> String {
    format!("maintenance {number}")
}

impl<S: Storefront> Inner<S> {
    /// Every receipt the account's file keeps: those shown, then those whose
    /// disk is not connected.
    fn saved(&self) -> Vec<Receipt<S>> {
        self.state
            .receipts
            .iter()
            .chain(&self.offline)
            .cloned()
            .collect()
    }

    /// The receipt `id` names, of this account's installations.
    fn receipt(&self, id: &str) -> Option<Receipt<S>> {
        self.state
            .receipts
            .iter()
            .find(|receipt| receipt.id() == id)
            .cloned()
    }

    /// Every incomplete receipt the account's file keeps.
    fn saved_incomplete(&self) -> Vec<Receipt<S>> {
        self.state
            .incomplete
            .iter()
            .chain(&self.incomplete_offline)
            .cloned()
            .collect()
    }

    /// The Transfer of `game` that stopped before its receipt.
    fn resumable(&self, game: &S::Game) -> Option<Transfer<S>> {
        self.state
            .last_download
            .clone()
            .filter(|last| &last.game() == game)
    }

    /// Forgets the Transfers over the installation `receipt` names, once its
    /// files went.
    fn forget_transfers(&mut self, receipt: &Receipt<S>) {
        if self
            .state
            .last_download
            .as_ref()
            .is_some_and(|last| last.lands_on(receipt))
        {
            self.state.last_download = None;
        }
        if self.state.transfer_game.as_ref() == Some(&receipt.game()) {
            self.state.transfer = None;
            self.state.transfer_game = None;
        }
    }

    /// Sorts the incomplete installations as `sort` does, but for the
    /// running Transfer's, whose folder may not be there yet; none moves
    /// with the games folder. `true` when one changed.
    fn sort_incomplete(&mut self) -> bool {
        let running = self.running.and(self.state.transfer_game.clone());
        let (running, saved): (Vec<_>, Vec<_>) = self
            .saved_incomplete()
            .into_iter()
            .partition(|receipt| running.as_ref() == Some(&receipt.game()));
        let (mut incomplete, offline, changed) = sort(saved, None);
        incomplete.extend(running);
        self.state.incomplete = incomplete;
        self.incomplete_offline = offline;
        changed
    }
}

/// One account's installations of one storefront. A clone is the same
/// installations.
pub struct Installations<S: Storefront> {
    shared: Arc<Shared<S>>,
}

impl<S: Storefront> Clone for Installations<S> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<S: Storefront> Installations<S> {
    /// Loads the account's receipts from `receipts_file`. New installations
    /// go in `folder`. Obelisk's library directory, which holds the games
    /// folder by default, and `home` are never removed.
    pub fn new(
        content: Arc<dyn Content<S>>,
        sessions: Arc<Sessions<S>>,
        account: S::Account,
        receipts_file: PathBuf,
        home: PathBuf,
        folder: Arc<GamesFolder>,
        queueing: Queueing<S>,
    ) -> Self {
        let library = folder.library().to_owned();
        let mut inner = Inner {
            offline: Vec::new(),
            incomplete_offline: Vec::new(),
            state: State {
                revision: 1,
                receipts: Vec::new(),
                transfer: None,
                transfer_game: None,
                downloading: false,
                launching: false,
                verifying_before_launch: false,
                launch_game: None,
                uninstall_game: None,
                last_download: None,
                incomplete: Vec::new(),
                report: None,
                warning: None,
                maintenance: None,
                gate: Gate {
                    modify: Opening::Closed,
                    uninstall: Opening::Closed,
                    launch: Opening::Closed,
                    maintain: Opening::Closed,
                },
                cloud_sync: None,
                updates: Vec::new(),
                queued: Vec::new(),
            },
            signed_out: false,
            diagnostic: false,
            transfer_cancel: CancelToken::new(),
            launch_cancelled: false,
            launch: None,
            warnings: 0,
            maintenance: None,
            maintenances: 0,
            answered: None,
            checking_updates: false,
            check_again: false,
            updates_checked: None,
            automatic_updates: false,
            automatic_skipped: Vec::new(),
            queued: Vec::new(),
            running: None,
        };
        match load(&receipts_file) {
            Ok(receipts) => {
                let (mut receipts, offline, mut changed) = sort(receipts, Some(&folder));
                // Every receipt is completed, not only the first that changes.
                for receipt in &mut receipts {
                    changed |= S::complete(receipt);
                }
                inner.state.receipts = receipts;
                inner.offline = offline;
                if changed && save(&receipts_file, &inner.saved()).is_err() {
                    Shared::<S>::warn(&mut inner, S::RECEIPTS_NOT_SAVED);
                }
            }
            Err(_) => Shared::<S>::warn(&mut inner, S::RECEIPTS_UNAVAILABLE),
        }
        let incomplete_file = receipts_file.with_extension("incomplete.json");
        // An unreadable file only forgets the downloads it kept, as an
        // Obelisk before it did.
        inner.state.incomplete = load(&incomplete_file).unwrap_or_default();
        if inner.sort_incomplete() && save(&incomplete_file, &inner.saved_incomplete()).is_err() {
            Shared::<S>::warn(&mut inner, S::RECEIPTS_NOT_SAVED);
        }
        // The watcher follows the sessions from the state this gate shows.
        let seen = sessions.state();
        inner.state.gate = gate(&inner, &seen);
        inner.state.cloud_sync = cloud_sync(&inner, &seen, &account);
        let (published, _) = watch::channel(inner.state.clone());
        let shared = Arc::new(Shared {
            inner: Mutex::new(inner),
            published,
            content,
            sessions: sessions.clone(),
            account,
            receipts_file,
            incomplete_file,
            protected: vec![library, home],
            folder,
            queueing,
            watchers: Mutex::new(Vec::new()),
        });
        let resident: Arc<dyn Resident> = shared.clone();
        shared.folder.register(Arc::downgrade(&resident));
        // A game of the storefront running holds every storefront's
        // automatic updates.
        shared.queueing.queue.hold(&shared.owner(), seen.busy());
        let weak = Arc::downgrade(&shared);
        let sessions_watcher = runtime().spawn(async move {
            let mut after = seen.revision;
            loop {
                let state = sessions.changed(after).await;
                after = state.revision;
                let Some(shared) = weak.upgrade() else { return };
                shared.update(|_| {});
                shared.queueing.queue.hold(&shared.owner(), state.busy());
            }
        });
        // Whatever ends, a Transfer, a launch, an uninstall, a game or an
        // update check, may let a queued Transfer start or an update queue.
        // A running Transfer's progress alone changes neither.
        let weak = Arc::downgrade(&shared);
        let mut published = shared.published.subscribe();
        let updater = runtime().spawn(async move {
            let mut seen = None;
            while published.changed().await.is_ok() {
                let mut state = published.borrow_and_update().clone();
                (state.revision, state.transfer) = (0, None);
                if seen.as_ref() == Some(&state) {
                    continue;
                }
                seen = Some(state);
                let Some(shared) = weak.upgrade() else { return };
                shared.update_automatically();
                shared.queueing.queue.advance();
            }
        });
        // A session that connected again is asked for newer builds at once,
        // as a new sign-in would be.
        let weak = Arc::downgrade(&shared);
        let content = shared.content.clone();
        let reconnections = runtime().spawn(async move {
            let mut connection = 0;
            loop {
                connection = content.reconnected(connection).await;
                let Some(shared) = weak.upgrade() else { return };
                shared.check_updates(true);
            }
        });
        shared
            .watchers
            .lock()
            .unwrap()
            .extend([sessions_watcher, updater, reconnections]);
        Self { shared }
    }

    pub fn state(&self) -> State<S> {
        self.shared.published.borrow().clone()
    }

    /// Waits for a revision newer than `after`, or returns at once.
    pub async fn changed(&self, after: u64) -> State<S> {
        let mut receiver = self.shared.published.subscribe();
        let _ = receiver.wait_for(|state| state.revision > after).await;
        receiver.borrow().clone()
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        self.shared.update(|inner| inner.diagnostic = running);
    }

    /// Installs what `offer` names under Obelisk's library, or updates the
    /// game's installation in place. A Transfer of the game that stopped
    /// before its receipt resumes instead.
    pub fn install(&self, offer: Offer<S>) {
        let fresh = offer.transfer(self.shared.folder.path().join(S::GAMES_DIRECTORY));
        let transfer = {
            let inner = self.shared.inner.lock().unwrap();
            // An installed build first, else the folder a stopped download
            // left.
            let installed = inner
                .state
                .receipts
                .iter()
                .chain(&inner.state.incomplete)
                .find(|receipt| fresh.lands_on(receipt));
            inner
                .resumable(&fresh.game())
                .or_else(|| installed.map(Receipt::transfer))
                .unwrap_or(fresh)
        };
        self.download(transfer);
    }

    /// Verifies the files of the installation `receipt` names, and repairs or
    /// updates them in place with the build it records. The player asked:
    /// every file is hashed again, whatever the last Transfer recorded. A
    /// Transfer of the game that stopped before its receipt resumes instead.
    pub fn update(&self, receipt: &str) {
        let found = {
            let inner = self.shared.inner.lock().unwrap();
            inner.receipt(receipt).map(|receipt| {
                let transfer = inner
                    .resumable(&receipt.game())
                    .unwrap_or_else(|| receipt.transfer());
                (PathBuf::from(receipt.directory), transfer)
            })
        };
        if let Some((directory, transfer)) = found {
            let _ = obelisk_core::verification::forget_verified(&directory);
            self.download(transfer);
        }
    }

    fn download(&self, transfer: Transfer<S>) {
        self.shared.download(transfer, None);
    }

    /// Whether Obelisk queues each newer build as soon as it is known,
    /// rather than installing it at the game's next launch. Turned off, the
    /// automatic updates leave the queue; one under way keeps what it
    /// verified for the next time.
    pub fn set_automatic_updates(&self, enabled: bool) {
        let withdrawn: Vec<u64> = self.shared.update(|inner| {
            inner.automatic_updates = enabled;
            if enabled {
                return Vec::new();
            }
            let automatic: Vec<(u64, String)> = inner
                .queued
                .iter()
                .filter_map(|queued| match &queued.job {
                    Job::Transfer {
                        automatic: Some(receipt),
                    } => Some((queued.id, receipt.clone())),
                    _ => None,
                })
                .collect();
            inner
                .automatic_skipped
                .retain(|receipt| !automatic.iter().any(|(_, withdrawn)| withdrawn == receipt));
            automatic.into_iter().map(|(id, _)| id).collect()
        });
        for id in withdrawn {
            self.shared.queueing.queue.remove(id);
        }
        self.shared.update_automatically();
    }

    /// Takes `game`'s Transfer out of the queue, waiting or under way, or
    /// stops its launch still preparing with the Transfer that launch runs.
    /// Another game's operation goes on, and a running game keeps its Game
    /// session.
    pub fn cancel(&self, game: &S::Game) {
        let (queued, launch) = self.shared.update(|inner| {
            let launch = inner.state.launching && inner.state.launch_game.as_ref() == Some(game);
            inner.launch_cancelled |= launch;
            let queued: Vec<u64> = inner
                .queued
                .iter()
                .filter(|queued| &queued.transfer.game() == game)
                .map(|queued| queued.id)
                .collect();
            // A launch's own Transfer ends it once taken out.
            let launch = (launch && queued.is_empty())
                .then(|| inner.launch.clone())
                .flatten();
            (queued, launch)
        });
        for id in queued {
            self.shared.queueing.queue.remove(id);
        }
        if let Some(launch) = launch {
            launch.cancel();
        }
    }

    /// The launch settings of the Windows installation `receipt` names;
    /// `None` for a Mac build or an installation the account no longer has.
    pub fn launch_settings(&self, receipt: &str) -> Option<launch_settings::View> {
        let receipt = self.windows_receipt(receipt)?;
        Some(receipt.launch_settings(self.shared.folder.library()))
    }

    /// Keeps `choices` as the player's launch settings of the Windows
    /// installation `receipt` names, without those its game's profile already
    /// gives, and returns its launch settings from its next launch on.
    pub fn set_launch_settings(
        &self,
        receipt: &str,
        choices: launch_settings::Choices,
    ) -> std::io::Result<Option<launch_settings::View>> {
        let Some(receipt) = self.windows_receipt(receipt) else {
            return Ok(None);
        };
        receipt
            .choose_launch_settings(self.shared.folder.library(), choices)
            .map(Some)
    }

    /// The Windows installation `receipt` names; `None` for a Mac build or an
    /// installation the account no longer has.
    fn windows_receipt(&self, receipt: &str) -> Option<Receipt<S>> {
        let receipt = self.shared.inner.lock().unwrap().receipt(receipt)?;
        (receipt.platform == Platform::Windows).then_some(receipt)
    }

    /// Asks the storefront whether each installation has a newer build, at
    /// most once every ten minutes; the state then names those that do. An
    /// installation it cannot tell about keeps what was known.
    pub fn check_updates(&self) {
        self.shared.check_updates(false);
    }

    /// Launches the installation `receipt` names. A Transfer runs first,
    /// ahead of the queue, when one over it has not succeeded since it
    /// started, or when the storefront offers a newer build; a storefront
    /// that cannot tell in time leaves the installed build to start.
    pub fn launch(&self, receipt: &str) {
        self.start(receipt, None);
    }

    /// Launches the installation `receipt` names as `launch` does, its game
    /// joining `join`, such as the lobby a friend invited the account into.
    pub fn join(&self, receipt: &str, join: S::Join) {
        self.start(receipt, Some(join));
    }

    fn start(&self, receipt: &str, join: Option<S::Join>) {
        let shared = &self.shared;
        let admission = shared.update(|inner| {
            let receipt = inner.receipt(receipt)?;
            let game = receipt.game();
            if !shared.gate(inner).launch.allows(&game) {
                return None;
            }
            let admission = shared
                .sessions
                .admit(game.clone(), shared.account.clone())?;
            let verify = receipt.needs_verification;
            inner.state.launching = true;
            inner.state.verifying_before_launch = verify;
            inner.state.launch_game = Some(game.clone());
            inner.state.report = None;
            inner.launch_cancelled = false;
            inner.launch = Some(admission.canceller());
            Some((admission, verify, receipt, game))
        });
        let Some((admission, verify, receipt, game)) = admission else {
            return;
        };
        let verification = receipt.transfer();
        let canceller = admission.canceller();
        let shared = shared.clone();
        runtime().spawn(async move {
            // The game running ends the launch, whatever follows.
            let running = {
                let (shared, game) = (shared.clone(), game.clone());
                runtime().spawn(async move {
                    let mut after = 0;
                    loop {
                        let state = shared.sessions.changed(after).await;
                        after = state.revision;
                        if state.active.is_some_and(|active| {
                            active.game == game
                                && active.observed
                                && active.phase == crate::supervisor::Phase::Running
                        }) {
                            shared.update(|inner| inner.state.launching = false);
                            return true;
                        }
                    }
                })
            };
            let result = async {
                let mut ready = receipt;
                if shared.inner.lock().unwrap().launch_cancelled {
                    return Err(Failure::Cancelled);
                }
                // Voile's feed and the game profiles, which decide how a
                // Windows game runs and in which release, are read anew
                // while its storefront is asked for a newer build.
                let refresh = async {
                    if ready.platform == Platform::Windows {
                        shared.sessions.runtimes().refresh_before_launch().await;
                    }
                };
                let (update, ()) = tokio::join!(
                    async { !verify && shared.newer_build(&ready).await == Some(true) },
                    refresh,
                );
                if update {
                    shared.update(|inner| inner.state.verifying_before_launch = true);
                }
                if verify || update {
                    let kind = if verify {
                        queue::Kind::Repair
                    } else {
                        queue::Kind::Update
                    };
                    ready = shared.launch_transfer(&verification, kind).await?;
                }
                if shared.inner.lock().unwrap().launch_cancelled {
                    return Err(Failure::Cancelled);
                }
                shared.update(|inner| inner.state.verifying_before_launch = false);
                let mut admission = admission;
                if ready.platform == Platform::Windows {
                    admission.needs_voile(ready.profile().minimum_voile);
                }
                shared
                    .content
                    .launch(&ready, admission, join.as_ref())
                    .await
            }
            .await;
            let was_running = running.is_finished();
            running.abort();
            // A launch cancelled through its Game session, such as a declined
            // Rosetta requirement, ends silently too.
            let cancelled = canceller.cancelled();
            shared.update(|inner| {
                inner.state.launching = false;
                inner.state.verifying_before_launch = false;
                inner.state.launch_game = None;
                inner.launch = None;
                if let Err(Failure::Failed(key)) = result
                    && !inner.signed_out
                    && (was_running || !(inner.launch_cancelled || cancelled))
                {
                    inner.state.report = Some(Report::Storefront(key));
                }
            });
        });
    }

    /// Removes the installation `receipt` names. The Cloud archive takes the
    /// saves first; the removal is recorded for Cloud sync even if the account
    /// signs out meanwhile, and a Windows installation's prefix goes last.
    pub fn uninstall(&self, receipt: &str) {
        let shared = &self.shared;
        let remaining = shared.update(|inner| {
            let receipt = inner.receipt(receipt)?;
            if !shared.begin_removal(inner, receipt.game()) {
                return None;
            }
            let remaining: Vec<_> = inner
                .state
                .receipts
                .iter()
                .filter(|kept| **kept != receipt)
                .cloned()
                .collect();
            // The file keeps the receipts whose disk is not connected.
            let saved: Vec<_> = inner
                .saved()
                .into_iter()
                .filter(|kept| *kept != receipt)
                .collect();
            Some((remaining, saved, receipt))
        });
        let Some((remaining, saved, receipt)) = remaining else {
            return;
        };
        let shared = shared.clone();
        runtime().spawn(async move {
            let outcome = shared.remove(&receipt, &saved).await;
            shared.update(|inner| {
                inner.state.uninstall_game = None;
                match outcome {
                    Ok(true) => {
                        inner.state.receipts = remaining;
                        inner.forget_transfers(&receipt);
                    }
                    Ok(false) | Err(Failure::Cancelled) => {}
                    Err(Failure::Failed(reason)) => {
                        inner
                            .state
                            .receipts
                            .retain(|kept| Path::new(&kept.directory).is_dir());
                        if !inner.signed_out {
                            inner.state.report = Some(Report::Uninstall(reason));
                        }
                    }
                }
            });
        });
    }

    /// Deletes the files of the incomplete installation `receipt` names,
    /// and forgets it. Nothing ran from them: there are no Cloud saves to
    /// archive and no prefix to delete.
    pub fn discard(&self, receipt: &str) {
        let shared = &self.shared;
        let started = shared.update(|inner| {
            let receipt = inner
                .state
                .incomplete
                .iter()
                .find(|kept| kept.id() == receipt)
                .cloned()?;
            shared
                .begin_removal(inner, receipt.game())
                .then_some(receipt)
        });
        let Some(receipt) = started else {
            return;
        };
        let directory = PathBuf::from(&receipt.directory);
        let expected = shared.content.directory_name(&receipt);
        let protected = shared.protected.clone();
        let shared = shared.clone();
        runtime().spawn(async move {
            let removal = blocking("model.game.installationBusy", move || {
                let protected: Vec<&Path> = protected.iter().map(PathBuf::as_path).collect();
                obelisk_core::installation::remove_unfinished(&directory, &expected, &protected)
                    .map_err(removal_reason)
            })
            .await
            .and_then(|removal| removal);
            shared.update(|inner| {
                inner.state.uninstall_game = None;
                match removal {
                    Ok(_) => {
                        inner.state.incomplete.retain(|kept| *kept != receipt);
                        shared.save_incomplete(inner);
                        inner.forget_transfers(&receipt);
                    }
                    Err(Failure::Failed(reason)) if !inner.signed_out => {
                        inner.state.report = Some(Report::Uninstall(reason));
                    }
                    Err(_) => {}
                }
            });
        });
    }

    /// Takes the account's Transfers out of the queue, and stops its Cloud
    /// maintenance and launch still preparing. A running game keeps its Game
    /// session; an uninstall already removing files finishes.
    pub fn signed_out(&self) {
        let (launch, maintenance) = self.shared.update(|inner| {
            inner.signed_out = true;
            inner.transfer_cancel.cancel();
            inner.launch_cancelled = true;
            let maintenance = inner
                .maintenance
                .as_ref()
                .map(|(_, control)| control.clone());
            (inner.launch.take(), maintenance)
        });
        self.shared.queueing.queue.withdraw(&self.shared.owner());
        self.shared.content.cancel();
        if let Some(launch) = launch {
            launch.cancel();
        }
        if let Some(maintenance) = maintenance {
            maintenance.cancel();
        }
    }

    /// Syncs `game`'s Cloud saves: those of the installation `installed`
    /// names, or, uninstalled, those its saved Cloud context names. Its idle
    /// guard keeps every launch of the storefront out, so it holds the
    /// Reservation of every game until it ends. A failure shows only in the
    /// game's Cloud indicator: Sync now is the retry.
    pub fn maintain_cloud(&self, game: S::Game, installed: Option<&str>) {
        let shared = &self.shared;
        let started = shared.update(|inner| {
            let installed = match installed {
                None => None,
                Some(id) => Some(inner.receipt(id).filter(|receipt| receipt.game() == game)?),
            };
            if !shared.gate(inner).maintain.allows(&game) {
                return None;
            }
            inner.maintenances += 1;
            let number = inner.maintenances;
            let weak = Arc::downgrade(shared);
            let control = Arc::new(cloud::Control::new(move |status| {
                if let Some(shared) = weak.upgrade() {
                    shared.show_maintenance(number, status);
                }
            }));
            inner.maintenance = Some((number, control.clone()));
            inner.state.maintenance = Some(Maintenance {
                game: game.clone(),
                cloud: cloud::Status {
                    phase: cloud::Phase::Checking,
                    ..Default::default()
                },
            });
            Some((number, control, installed))
        });
        let Some((number, control, installed)) = started else {
            return;
        };
        let shared = shared.clone();
        runtime().spawn(async move {
            let _ = shared
                .content
                .maintain(&game, installed.as_ref(), control)
                .await;
            shared.update(|inner| {
                if inner
                    .maintenance
                    .as_ref()
                    .is_some_and(|(running, _)| *running == number)
                {
                    inner.maintenance = None;
                    inner.state.maintenance = None;
                }
            });
        });
    }

    /// Answers the Cloud question `game` shows with `decision_id`: its
    /// maintenance's, else its Game session's. Cancel stops either sync. An
    /// answered question no longer shows.
    pub fn answer_cloud(
        &self,
        game: &S::Game,
        decision_id: u64,
        choice: cloud::Choice,
    ) -> Result<(), Failure> {
        let refused = || Failure::Failed(S::INVALID_SESSION.to_owned());
        let (maintenance, asked) = {
            let inner = self.shared.inner.lock().unwrap();
            if inner.signed_out {
                return Err(refused());
            }
            // Only a question the account shows closes with its answer.
            let asked = inner.state.cloud_sync.as_ref().is_some_and(|sync| {
                &sync.game == game
                    && sync
                        .question
                        .as_ref()
                        .is_some_and(|question| question.decision_id == decision_id)
            });
            let maintenance = match (&inner.state.maintenance, &inner.maintenance) {
                (Some(maintenance), Some((number, control))) if &maintenance.game == game => {
                    Some((*number, control.clone()))
                }
                _ => None,
            };
            (maintenance, asked)
        };
        let source = match maintenance {
            Some((number, control)) => {
                control.answer(decision_id, choice).map_err(|_| refused())?;
                maintenance_source(number)
            }
            None => {
                let active = self
                    .shared
                    .sessions
                    .state()
                    .active
                    .filter(|active| &active.game == game && active.account == self.shared.account)
                    .ok_or_else(refused)?;
                self.shared
                    .sessions
                    .choose(&active.id, decision_id, choice)?;
                active.id
            }
        };
        if asked {
            self.shared
                .update(|inner| inner.answered = Some((source, decision_id)));
        }
        Ok(())
    }

    /// The Cloud indicators of `games`, and of every other game of the
    /// account with pending work, such as an uninstalled one. Nothing here
    /// takes a lock: a game or maintenance may be writing meanwhile. It
    /// blocks on file reads.
    pub fn cloud_indicators(
        &self,
        games: &[S::Game],
    ) -> Result<Vec<(S::Game, cloud_status::Indicator)>, Failure> {
        self.shared.content.indicators(games)
    }
}

impl<S: Storefront> Shared<S> {
    /// Removes the installation, then saves the receipts `remaining`;
    /// `Ok(false)` when sign-out came first.
    async fn remove(
        &self,
        receipt: &Receipt<S>,
        remaining: &[Receipt<S>],
    ) -> Result<bool, Failure> {
        let signed_out = || self.inner.lock().unwrap().signed_out;
        if signed_out() {
            return Ok(false);
        }
        self.content.archive(receipt).await?;
        if signed_out() {
            return Ok(false);
        }
        let directory = PathBuf::from(&receipt.directory);
        let expected = self.content.directory_name(receipt);
        let protected: Vec<_> = self.protected.clone();
        let file = self.receipts_file.clone();
        let remaining = remaining.to_vec();
        let removal = tokio::task::spawn_blocking(move || {
            let protected: Vec<&Path> = protected.iter().map(PathBuf::as_path).collect();
            obelisk_core::installation::remove(&directory, &expected, &protected)
                .map_err(removal_reason)?;
            // The captured account's receipts are updated even if it signs
            // out while the files go.
            save(&file, &remaining).map_err(sessions::failed)
        })
        .await
        .unwrap_or_else(|_| Err(Failure::Failed("model.game.installationBusy".into())));
        if let Err(error) = removal {
            // Receipt persistence can fail after the folder was removed. The
            // Cloud fence must still record that removal.
            if !Path::new(&receipt.directory).exists() {
                self.content.removed(receipt).await?;
            }
            return Err(error);
        }
        // Removal is never a Cloud delete.
        self.content.removed(receipt).await?;
        // As Steam deletes a Proton prefix, the prefix goes with its game,
        // after the Cloud archive took its saves.
        let (content, receipt) = (self.content.clone(), receipt.clone());
        let prefix = tokio::task::spawn_blocking(move || content.remove_prefix(&receipt)).await;
        if !matches!(prefix, Ok(None | Some(Ok(())))) {
            self.update(|inner| Self::warn(inner, "model.installations.prefixNotRemoved"));
        }
        Ok(!signed_out())
    }
}

/// What the games folder asks of an account's installations.
impl<S: Storefront> Resident for Shared<S> {
    fn installations(&self) -> Vec<(PathBuf, u64)> {
        let inner = self.inner.lock().unwrap();
        inner
            .state
            .receipts
            .iter()
            .map(|receipt| (PathBuf::from(&receipt.directory), receipt.total_bytes))
            .collect()
    }

    /// Queues one move per installation outside `folder`, behind the
    /// Transfers waiting: a Transfer of its game queued first runs where the
    /// game is. The new installations still waiting go there instead, as do
    /// the moves, which go to the games folder of their turn.
    fn gather(self: Arc<Self>, folder: &Path) {
        let destination = folder.join(S::GAMES_DIRECTORY);
        let queued = self.update(|inner| {
            if inner.signed_out || inner.diagnostic {
                return false;
            }
            let fresh: Vec<u64> = inner
                .queued
                .iter()
                .filter(|waiting| {
                    Some(waiting.id) != inner.running
                        && matches!(waiting.job, Job::Transfer { .. })
                        && inner.state.last_download.as_ref() != Some(&waiting.transfer)
                        && !inner
                            .state
                            .receipts
                            .iter()
                            .chain(&inner.state.incomplete)
                            .any(|receipt| waiting.transfer.lands_on(receipt))
                })
                .map(|waiting| waiting.id)
                .collect();
            for waiting in &mut inner.queued {
                if fresh.contains(&waiting.id) {
                    waiting.transfer.directory = destination.clone();
                }
            }
            let outside: Vec<Receipt<S>> = inner
                .state
                .receipts
                .iter()
                .filter(|receipt| {
                    let game = receipt.game();
                    !Path::new(&receipt.directory).starts_with(folder)
                        && inner.state.uninstall_game.as_ref() != Some(&game)
                        && !inner.queued.iter().any(|waiting| {
                            waiting.job == Job::Move && waiting.transfer.lands_on(receipt)
                        })
                })
                .cloned()
                .collect();
            for receipt in &outside {
                let transfer = receipt.transfer();
                // The queue calls no owner while it adds.
                let id = self.queueing.queue.push(
                    &self.owner(),
                    self.queue_request(&transfer, queue::Kind::Move, false),
                );
                inner.queued.push(Queued {
                    id,
                    transfer,
                    job: Job::Move,
                });
            }
            !outside.is_empty()
        });
        if queued {
            self.queueing.queue.advance();
        }
    }

    /// Shows the installations whose disk is connected, the receipts whose
    /// folder moved into the games folder naming it, and saves them.
    fn refresh(&self) {
        self.update(|inner| {
            let (receipts, offline, changed) = sort(inner.saved(), Some(&self.folder));
            inner.state.receipts = receipts;
            inner.offline = offline;
            if changed {
                self.save(inner);
            }
            if inner.sort_incomplete() {
                self.save_incomplete(inner);
            }
        });
    }
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

impl<S: Storefront> Shared<S> {
    /// Moves the installation `transfer` lands on into the games folder
    /// `folder`, as the queue's job `id`, and ends it in the queue. Its game
    /// shows the move as its Transfer, which a cancellation stops: a copy to
    /// another disk then goes, and the installation stays where it was.
    async fn relocate(self: &Arc<Self>, id: u64, transfer: &Transfer<S>, folder: &Path) {
        let game = transfer.game();
        let begun = self.update(|inner| {
            let receipt = inner
                .state
                .receipts
                .iter()
                .find(|receipt| transfer.lands_on(receipt))
                .cloned()?;
            inner.running = Some(id);
            inner.transfer_cancel = CancelToken::new();
            // The last Transfer's progress shows again once the move ends.
            let parked = (
                inner.state.transfer.take(),
                inner.state.transfer_game.take(),
            );
            inner.state.transfer_game = Some(game.clone());
            inner.state.transfer = Some(TransferProgress {
                state: TransferState::Moving,
                message: String::new(),
                ..TransferProgress::new(transfer.progress_name())
            });
            inner.state.report = None;
            Some((receipt, inner.transfer_cancel.clone(), parked))
        });
        if let Some((receipt, cancel, parked)) = begun {
            let outcome = self.relocation(&receipt, folder, cancel).await;
            self.update(|inner| {
                if inner.running == Some(id) {
                    inner.running = None;
                    (inner.state.transfer, inner.state.transfer_game) = parked;
                }
                match outcome {
                    Ok(Some(moved)) => {
                        let id = moved.id();
                        for receipt in &mut inner.state.receipts {
                            if receipt.id() == id {
                                receipt.directory = moved.directory.clone();
                            }
                        }
                        self.save(inner);
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        if !inner.signed_out {
                            inner.state.report = Some(Report::Move(reason));
                        }
                    }
                }
            });
        }
        self.finish(id);
    }

    /// Moves the installation `receipt` names into the games folder
    /// `folder`, and returns its receipt there; `Ok(None)` when it already
    /// was there or the move was stopped. A failure is a localization key,
    /// or the system's own message.
    async fn relocation(
        self: &Arc<Self>,
        receipt: &Receipt<S>,
        folder: &Path,
        cancel: CancelToken,
    ) -> Result<Option<Receipt<S>>, String> {
        use obelisk_core::installation::RelocationFailure;
        let source = PathBuf::from(&receipt.directory);
        if source.starts_with(folder) {
            return Ok(None);
        }
        let parent = folder.join(S::GAMES_DIRECTORY);
        let expected = self.content.directory_name(receipt);
        let protected = self.protected.clone();
        let shared = self.clone();
        let moved = blocking("model.game.installationBusy", move || {
            let source = source.canonicalize().unwrap_or(source);
            std::fs::create_dir_all(&parent)?;
            let protected: Vec<&Path> = protected.iter().map(PathBuf::as_path).collect();
            let mut shown: Option<Instant> = None;
            obelisk_core::installation::relocate(
                &source,
                &parent,
                &expected,
                &protected,
                &cancel,
                |copied, total| {
                    if copied == total || shown.is_none_or(|shown| shown.elapsed() >= PROGRESS) {
                        shown = Some(Instant::now());
                        shared.update(|inner| {
                            if let Some(progress) = &mut inner.state.transfer {
                                progress.completed_bytes = copied;
                                progress.total_bytes = total;
                                // The copy's pace reads as a download's.
                                progress.network_bytes = copied;
                            }
                        });
                    }
                },
            )
        })
        .await
        .map_err(|failure| match failure {
            Failure::Failed(reason) => reason,
            Failure::Cancelled => "model.game.installationBusy".into(),
        })?;
        let relocation = match moved {
            Ok(relocation) => relocation,
            Err(RelocationFailure::Cancelled) => return Ok(None),
            Err(RelocationFailure::Unsafe(_)) => return Err("model.game.unsafeInstallation".into()),
            Err(RelocationFailure::Busy) => return Err("model.game.installationBusy".into()),
            Err(RelocationFailure::Occupied(_)) => {
                return Err("model.installations.moveOccupied".into());
            }
            Err(RelocationFailure::Io(error))
                if error.kind() == std::io::ErrorKind::StorageFull =>
            {
                return Err("model.installations.moveNoSpace".into());
            }
            Err(RelocationFailure::Io(error)) => return Err(error.to_string()),
        };
        let moved = Receipt {
            directory: relocation.directory.to_string_lossy().into_owned(),
            ..receipt.clone()
        };
        let (content, games, followed) = (self.content.clone(), self.folder.clone(), moved.clone());
        let from = relocation.origin.clone();
        let records = blocking("model.game.installationBusy", move || {
            // The Cloud archive names saves by their canonical path.
            let records = content.relocated(&followed, &from);
            if let Some(storefront) = from.parent() {
                games.tidy(storefront);
            }
            records
        })
        .await;
        self.update(|inner| {
            if !matches!(records, Ok(Ok(()))) {
                Self::warn(inner, "model.installations.moveRecordsNotUpdated");
            } else if !relocation.original_removed {
                Self::warn(inner, "model.installations.originalNotRemoved");
            }
        });
        Ok(Some(moved))
    }
}

fn removal_reason(failure: obelisk_core::installation::RemovalFailure) -> Failure {
    use obelisk_core::installation::RemovalFailure;
    Failure::Failed(match failure {
        RemovalFailure::Unsafe(_) => "model.game.unsafeInstallation".into(),
        RemovalFailure::Busy => "model.game.installationBusy".into(),
        RemovalFailure::Io(error) => error.to_string(),
    })
}

/// A runtime's download as its Transfer's progress: the bytes received, and
/// the runtime the game page names.
fn shown(game: &str, progress: runtimes::Progress) -> TransferProgress {
    TransferProgress {
        state: TransferState::Downloading,
        completed_bytes: progress.received,
        total_bytes: progress.total,
        network_bytes: progress.received,
        message: match progress.runtime {
            Runtime::Voile => "model.game.installingVoile",
            Runtime::SteamComponents => "model.game.installingSteamComponents",
        }
        .into(),
        ..TransferProgress::new(game)
    }
}

#[cfg(test)]
pub(crate) mod tests;
