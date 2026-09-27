//! One account's installed games of one storefront, and the actions on them:
//! Transfers, verified launches, uninstalls and Cloud maintenance. Every
//! action asks one Reservation gate, which the state publishes so the UI
//! shows exactly what may start. The account's Installation receipts live
//! here, in the file SailOver keeps per storefront and account, and each
//! action names one of them: the Transfers it needs are built here. So is the
//! account's one Cloud question, whichever sync asks it.
use crate::{
    cloud, cloud_status,
    sessions::{self, Admission, Canceller, Failure, Sessions},
};
use sailover_core::{installation::TransferProgress, transfer::TransferState};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::watch;

/// How often a running Transfer's progress is read.
const PROGRESS: Duration = Duration::from_millis(250);

pub use crate::sessions::BoxFuture;

/// What a storefront adds to its installations.
pub trait Storefront: sessions::Storefront {
    /// An Installation receipt, as the account's receipt file records it.
    type Receipt: Clone + fmt::Debug + PartialEq + Serialize + DeserializeOwned + Send + Sync;
    /// One Transfer: a game's platform, installed under a parent folder.
    type Request: Clone + fmt::Debug + PartialEq + Send + Sync;
    /// What the storefront offers to install: one game's build, before any
    /// folder is chosen.
    type Offer: Clone + fmt::Debug + PartialEq + Send + Sync;
    /// Shown when the receipt file cannot be read, or saved.
    const RECEIPTS_UNAVAILABLE: &'static str;
    const RECEIPTS_NOT_SAVED: &'static str;
    /// Where new installations go, inside SailOver's library directory.
    const GAMES_DIRECTORY: &'static str;

    fn receipt_game(receipt: &Self::Receipt) -> Self::Game;
    /// A receipt's identity: one per game and platform.
    fn receipt_id(receipt: &Self::Receipt) -> String;
    fn receipt_platform(receipt: &Self::Receipt) -> &str;
    fn receipt_directory(receipt: &Self::Receipt) -> &str;
    fn needs_verification(receipt: &Self::Receipt) -> bool;
    fn mark_needs_verification(receipt: &mut Self::Receipt);
    fn request_game(request: &Self::Request) -> Self::Game;
    fn request_platform(request: &Self::Request) -> &str;
    /// The parent folder the storefront's installation directory goes in.
    fn request_directory(request: &Self::Request) -> &Path;
    /// The name a Transfer's progress gives the game.
    fn transfer_name(request: &Self::Request) -> String;
    fn offer_game(offer: &Self::Offer) -> Self::Game;
    fn offer_platform(offer: &Self::Offer) -> &str;
    /// The Transfer that installs `offer` under `directory`.
    fn install_request(offer: &Self::Offer, directory: &Path) -> Self::Request;
    /// The Transfer that verifies `receipt`'s installation, or updates it in
    /// place, with the build it records.
    fn update_request(receipt: &Self::Receipt) -> Self::Request;
    /// Fills in what an older receipt file did not record, from the
    /// installation itself. `true` when it changed the receipt.
    fn complete(receipt: &mut Self::Receipt) -> bool {
        let _ = receipt;
        false
    }
}

/// A storefront's part of one account's installations: where its games and
/// their Cloud saves live, and what its session does for them. Steam's and
/// Epic's are `steam::content` and `epic::content`.
pub trait Content<S: Storefront>: Send + Sync {
    /// Runs one Transfer and returns the receipt of the completed installation.
    fn download<'a>(
        &'a self,
        request: &'a S::Request,
    ) -> BoxFuture<'a, Result<S::Receipt, Failure>>;
    fn progress(&self) -> Option<TransferProgress>;
    fn cancel(&self);
    /// Hands a verified installation to its supervisor for its admission, and
    /// returns once the session is finished.
    fn launch<'a>(
        &'a self,
        receipt: &'a S::Receipt,
        request: &'a S::Request,
        admission: Admission<S>,
    ) -> BoxFuture<'a, Result<(), Failure>>;
    /// Saves the game's Cloud archive before its files go.
    fn archive<'a>(
        &'a self,
        receipt: &'a S::Receipt,
        request: &'a S::Request,
    ) -> BoxFuture<'a, Result<(), Failure>>;
    /// Records the removal for Cloud sync. Runs even after sign-out.
    fn removed<'a>(
        &'a self,
        receipt: &'a S::Receipt,
        request: &'a S::Request,
    ) -> BoxFuture<'a, Result<(), Failure>>;
    /// The dedicated folder name the storefront installed `receipt` into.
    fn directory_name(&self, receipt: &S::Receipt, request: &S::Request) -> String;
    /// Deletes a Windows installation's Wine prefix once its folder is gone.
    /// `None` when the installation has none.
    fn remove_prefix(&self, receipt: &S::Receipt) -> Option<Result<(), String>>;
    /// Syncs the game's Cloud saves on the player's request: those of
    /// `installed`, or, for an uninstalled game, those its saved Cloud context
    /// names. `control` shows the sync's progress and asks the player.
    fn maintain<'a>(
        &'a self,
        game: &'a S::Game,
        installed: Option<(&'a S::Receipt, &'a S::Request)>,
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

/// What the Reservation lets start now. One action of the storefront at a time
/// holds every game; a Game session holds only its own game, and a Transfer or
/// uninstall keeps only its own game's Cloud maintenance out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gate<G> {
    /// A Transfer or uninstall.
    pub modify: Opening<G>,
    /// A launch, of any game a Transfer may touch: no Game session runs.
    pub launch: bool,
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
}

/// Everything the account's installations present. `revision` grows with
/// every change.
#[derive(Clone, Debug, PartialEq)]
pub struct State<S: Storefront> {
    pub revision: u64,
    pub receipts: Vec<S::Receipt>,
    /// The last Transfer progress.
    pub transfer: Option<TransferProgress>,
    /// The game the last Transfer progress belongs to.
    pub transfer_game: Option<S::Game>,
    pub downloading: bool,
    pub launching: bool,
    pub verifying_before_launch: bool,
    pub launch_game: Option<S::Game>,
    pub uninstalling: bool,
    pub uninstall_game: Option<S::Game>,
    /// A Transfer that stopped before its receipt, which the player can resume.
    pub last_download: Option<S::Request>,
    /// The failure shown, until the next action clears it.
    pub report: Option<Report>,
    /// A storage warning, a localization key, with an id that changes each
    /// time one is raised.
    pub warning: Option<(u64, String)>,
    pub maintenance: Option<Maintenance<S>>,
    pub gate: Gate<S::Game>,
    pub cloud_sync: Option<CloudSync<S::Game>>,
}

struct Inner<S: Storefront> {
    state: State<S>,
    signed_out: bool,
    /// A DEBUG diagnostic fences the storefront like a Game session would.
    diagnostic: bool,
    cancellation_requested: bool,
    /// Cancels the launch still preparing.
    launch: Option<Canceller<S>>,
    warnings: u64,
    /// The running maintenance, which its number tells from a later one.
    maintenance: Option<(u64, Arc<cloud::Control>)>,
    maintenances: u64,
    /// The last question answered, by the sync that asked it: it no longer
    /// shows, until that sync asks another.
    answered: Option<(String, u64)>,
}

struct Shared<S: Storefront> {
    inner: Mutex<Inner<S>>,
    published: watch::Sender<State<S>>,
    content: Arc<dyn Content<S>>,
    sessions: Arc<Sessions<S>>,
    account: S::Account,
    receipts_file: PathBuf,
    /// SailOver's library directory, where new installations go.
    library: PathBuf,
    /// SailOver's library directory and the user's home: never removed.
    protected: Vec<PathBuf>,
    /// Publishes the gate again whenever the Game sessions change.
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl<S: Storefront> Drop for Shared<S> {
    fn drop(&mut self) {
        if let Some(watcher) = self.watcher.lock().unwrap().take() {
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
            .thread_name("sailover-installations")
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
        let sessions = self.sessions.state();
        inner.state.gate = gate(&inner, &sessions);
        inner.state.cloud_sync = cloud_sync(&inner, &sessions, &self.account);
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
        if save(&self.receipts_file, &inner.state.receipts).is_err() {
            Self::warn(inner, S::RECEIPTS_NOT_SAVED);
        }
    }

    /// The Reservation gate every installation-mutating action asks.
    fn can_modify(&self, inner: &Inner<S>, game: &S::Game) -> bool {
        gate(inner, &self.sessions.state()).modify.allows(game)
    }

    fn can_maintain_cloud(&self, inner: &Inner<S>, game: &S::Game) -> bool {
        gate(inner, &self.sessions.state()).maintain.allows(game)
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

    /// Marks the Transfer's game before its first progress snapshot arrives.
    fn begin(&self, request: &S::Request) {
        self.update(|inner| {
            inner.state.last_download = Some(request.clone());
            inner.state.transfer_game = Some(S::request_game(request));
            inner.state.transfer = Some(TransferProgress::new(S::transfer_name(request)));
            inner.state.downloading = true;
            let (game, platform) = (S::request_game(request), S::request_platform(request));
            if let Some(receipt) = inner.state.receipts.iter_mut().find(|receipt| {
                S::receipt_game(receipt) == game && S::receipt_platform(receipt) == platform
            }) {
                S::mark_needs_verification(receipt);
                self.save(inner);
            }
        });
    }

    /// Runs the Transfer `begin` announced: download, resume and verification
    /// before launch share this path.
    async fn perform(self: &Arc<Self>, request: &S::Request) -> Result<S::Receipt, Failure> {
        let name = S::transfer_name(request);
        if self.inner.lock().unwrap().cancellation_requested {
            self.update(|inner| {
                if let Some(transfer) = &mut inner.state.transfer {
                    transfer.state = TransferState::Cancelled;
                }
                inner.state.downloading = false;
            });
            return Err(Failure::Cancelled);
        }
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
        let result = self.content.download(request).await;
        polling.abort();
        let outcome = match result {
            // A receipt that arrives after its cancellation is refused.
            Ok(_) if self.inner.lock().unwrap().cancellation_requested => Err(Failure::Cancelled),
            Ok(receipt) => Ok(receipt),
            Err(failure) => {
                let cancelled = failure == Failure::Cancelled
                    || self.inner.lock().unwrap().cancellation_requested;
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
        let last = self
            .content
            .progress()
            .filter(|progress| progress.game == name && progress.state == settled);
        self.update(|inner| {
            if let Ok(receipt) = &outcome {
                let id = S::receipt_id(receipt);
                inner
                    .state
                    .receipts
                    .retain(|kept| S::receipt_id(kept) != id);
                inner.state.receipts.push(receipt.clone());
                inner.state.last_download = None;
                self.save(inner);
            }
            match last {
                Some(progress) => inner.state.transfer = Some(progress),
                None => {
                    if let Some(transfer) = &mut inner.state.transfer {
                        transfer.message.clear();
                    }
                }
            }
            if let Some(transfer) = &mut inner.state.transfer {
                transfer.state = settled;
            }
            inner.state.downloading = false;
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
    // One action of the storefront at a time; recovery may hold any game.
    let storefront = inner.signed_out
        || inner.diagnostic
        || state.downloading
        || state.launching
        || state.uninstalling
        || state.maintenance.is_some()
        || sessions.recovering;
    let modify = match storefront {
        true => Opening::Closed,
        false => Opening::Open {
            except: sessions
                .active
                .iter()
                .map(|active| active.game.clone())
                .collect(),
        },
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
            except: [
                state.transfer_game.clone().filter(|_| state.downloading),
                state.uninstall_game.clone().filter(|_| state.uninstalling),
            ]
            .into_iter()
            .flatten()
            .collect(),
        },
    };
    Gate {
        modify,
        launch: !storefront && !sessions.busy(),
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
    /// The receipt `id` names, of this account's installations.
    fn receipt(&self, id: &str) -> Option<S::Receipt> {
        self.state
            .receipts
            .iter()
            .find(|receipt| S::receipt_id(receipt) == id)
            .cloned()
    }

    /// The Transfer of `game` that stopped before its receipt.
    fn resumable(&self, game: &S::Game) -> Option<S::Request> {
        self.state
            .last_download
            .clone()
            .filter(|last| &S::request_game(last) == game)
    }
}

/// The receipts in `file`, without those whose folder has vanished: the game
/// was removed outside SailOver.
fn load<R: DeserializeOwned>(
    file: &Path,
    directory: impl Fn(&R) -> &str,
) -> std::io::Result<Vec<R>> {
    let bytes = match std::fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let receipts: Vec<R> = serde_json::from_slice(&bytes)?;
    Ok(receipts
        .into_iter()
        .filter(|receipt| Path::new(directory(receipt)).is_dir())
        .collect())
}

fn save<R: Serialize>(file: &Path, receipts: &[R]) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(receipts)?;
    let partial = file.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, file)
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
    /// Loads the account's receipts from `receipts_file`. `library`, SailOver's
    /// library directory, and `home` are never removed.
    pub fn new(
        content: Arc<dyn Content<S>>,
        sessions: Arc<Sessions<S>>,
        account: S::Account,
        receipts_file: PathBuf,
        library: PathBuf,
        home: PathBuf,
    ) -> Self {
        let mut inner = Inner {
            state: State {
                revision: 1,
                receipts: Vec::new(),
                transfer: None,
                transfer_game: None,
                downloading: false,
                launching: false,
                verifying_before_launch: false,
                launch_game: None,
                uninstalling: false,
                uninstall_game: None,
                last_download: None,
                report: None,
                warning: None,
                maintenance: None,
                gate: Gate {
                    modify: Opening::Closed,
                    launch: false,
                    maintain: Opening::Closed,
                },
                cloud_sync: None,
            },
            signed_out: false,
            diagnostic: false,
            cancellation_requested: false,
            launch: None,
            warnings: 0,
            maintenance: None,
            maintenances: 0,
            answered: None,
        };
        match load(&receipts_file, |receipt: &S::Receipt| {
            S::receipt_directory(receipt)
        }) {
            Ok(mut receipts) => {
                // Every receipt is completed, not only the first that changes.
                let mut completed = false;
                for receipt in &mut receipts {
                    completed |= S::complete(receipt);
                }
                if completed && save(&receipts_file, &receipts).is_err() {
                    Shared::<S>::warn(&mut inner, S::RECEIPTS_NOT_SAVED);
                }
                inner.state.receipts = receipts;
            }
            Err(_) => Shared::<S>::warn(&mut inner, S::RECEIPTS_UNAVAILABLE),
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
            protected: vec![library.clone(), home],
            library,
            watcher: Mutex::new(None),
        });
        let weak = Arc::downgrade(&shared);
        let watcher = runtime().spawn(async move {
            let mut after = seen.revision;
            loop {
                after = sessions.changed(after).await.revision;
                let Some(shared) = weak.upgrade() else { return };
                shared.update(|_| {});
            }
        });
        *shared.watcher.lock().unwrap() = Some(watcher);
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

    /// Installs what `offer` names under SailOver's library, or updates the
    /// installation of its platform in place. A Transfer of the game that
    /// stopped before its receipt resumes instead.
    pub fn install(&self, offer: S::Offer) {
        let request = {
            let inner = self.shared.inner.lock().unwrap();
            let game = S::offer_game(&offer);
            let installed = inner.state.receipts.iter().find(|receipt| {
                S::receipt_game(receipt) == game
                    && S::receipt_platform(receipt) == S::offer_platform(&offer)
            });
            inner
                .resumable(&game)
                .or_else(|| installed.map(S::update_request))
                .unwrap_or_else(|| {
                    S::install_request(&offer, &self.shared.library.join(S::GAMES_DIRECTORY))
                })
        };
        self.download(request);
    }

    /// Updates the installation `receipt` names in place, with the build it
    /// records. A Transfer of the game that stopped before its receipt
    /// resumes instead.
    pub fn update(&self, receipt: &str) {
        let request = {
            let inner = self.shared.inner.lock().unwrap();
            inner.receipt(receipt).map(|receipt| {
                inner
                    .resumable(&S::receipt_game(&receipt))
                    .unwrap_or_else(|| S::update_request(&receipt))
            })
        };
        if let Some(request) = request {
            self.download(request);
        }
    }

    fn download(&self, request: S::Request) {
        let shared = &self.shared;
        let started = shared.update(|inner| {
            if !shared.can_modify(inner, &S::request_game(&request)) {
                return false;
            }
            if let Err(error) = std::fs::create_dir_all(S::request_directory(&request)) {
                inner.state.report = Some(Report::DownloadDirectory(error.to_string()));
                return false;
            }
            inner.cancellation_requested = false;
            inner.state.report = None;
            true
        });
        if !started {
            return;
        }
        shared.begin(&request);
        let shared = shared.clone();
        runtime().spawn(async move {
            if let Err(Failure::Failed(key)) = shared.perform(&request).await {
                shared.update(|inner| {
                    if !inner.cancellation_requested && !inner.signed_out {
                        inner.state.report = Some(Report::Storefront(key));
                    }
                });
            }
        });
    }

    /// Stops the Transfer, and a launch still preparing. A running game keeps
    /// its Game session.
    pub fn cancel(&self) {
        let launch = self.shared.update(|inner| {
            if !inner.state.downloading && !inner.state.launching {
                return None;
            }
            inner.cancellation_requested = true;
            Some((inner.state.launching && !inner.state.downloading).then(|| inner.launch.clone()))
        });
        let Some(launch) = launch else { return };
        self.shared.content.cancel();
        if let Some(Some(launch)) = launch {
            launch.cancel();
        }
    }

    /// Launches the installation `receipt` names, verifying it first when
    /// `verify` asks for it or its receipt needs it.
    pub fn launch(&self, receipt: &str, verify: bool) {
        let shared = &self.shared;
        let admission = shared.update(|inner| {
            let receipt = inner.receipt(receipt)?;
            let game = S::receipt_game(&receipt);
            if !shared.can_modify(inner, &game) || shared.sessions.state().busy() {
                return None;
            }
            let admission = shared
                .sessions
                .admit(game.clone(), shared.account.clone())?;
            let verify = verify || S::needs_verification(&receipt);
            inner.state.launching = true;
            inner.state.verifying_before_launch = verify;
            inner.state.launch_game = Some(game.clone());
            inner.state.report = None;
            inner.cancellation_requested = false;
            inner.launch = Some(admission.canceller());
            Some((admission, verify, receipt, game))
        });
        let Some((admission, verify, receipt, game)) = admission else {
            return;
        };
        let verification = S::update_request(&receipt);
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
                if shared.inner.lock().unwrap().cancellation_requested {
                    return Err(Failure::Cancelled);
                }
                if verify {
                    shared.begin(&verification);
                    ready = shared.perform(&verification).await?;
                }
                if shared.inner.lock().unwrap().cancellation_requested {
                    return Err(Failure::Cancelled);
                }
                shared.update(|inner| inner.state.verifying_before_launch = false);
                shared
                    .content
                    .launch(&ready, &verification, admission)
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
                    && (was_running || !(inner.cancellation_requested || cancelled))
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
            let game = S::receipt_game(&receipt);
            if !shared.can_modify(inner, &game) {
                return None;
            }
            inner.state.uninstalling = true;
            inner.state.uninstall_game = Some(game.clone());
            inner.state.report = None;
            let remaining: Vec<_> = inner
                .state
                .receipts
                .iter()
                .filter(|kept| **kept != receipt)
                .cloned()
                .collect();
            Some((remaining, receipt, game))
        });
        let Some((remaining, receipt, game)) = remaining else {
            return;
        };
        let request = S::update_request(&receipt);
        let shared = shared.clone();
        runtime().spawn(async move {
            let outcome = shared.remove(&receipt, &request, &remaining).await;
            shared.update(|inner| {
                inner.state.uninstalling = false;
                inner.state.uninstall_game = None;
                match outcome {
                    Ok(true) => {
                        inner.state.receipts = remaining;
                        if inner.state.last_download.as_ref().is_some_and(|last| {
                            S::request_game(last) == game
                                && S::request_platform(last) == S::receipt_platform(&receipt)
                        }) {
                            inner.state.last_download = None;
                        }
                        if inner.state.transfer_game.as_ref() == Some(&game) {
                            inner.state.transfer = None;
                            inner.state.transfer_game = None;
                        }
                    }
                    Ok(false) => {}
                    Err(reason) => {
                        inner
                            .state
                            .receipts
                            .retain(|kept| Path::new(S::receipt_directory(kept)).is_dir());
                        if !inner.signed_out {
                            inner.state.report = Some(Report::Uninstall(reason));
                        }
                    }
                }
            });
        });
    }

    /// Stops the account's Transfer, Cloud maintenance and launch still
    /// preparing. A running game keeps its Game session; an uninstall already
    /// removing files finishes.
    pub fn signed_out(&self) {
        let (launch, maintenance) = self.shared.update(|inner| {
            inner.signed_out = true;
            inner.cancellation_requested = true;
            let maintenance = inner
                .maintenance
                .as_ref()
                .map(|(_, control)| control.clone());
            (inner.launch.take(), maintenance)
        });
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
                Some(id) => {
                    let receipt = inner
                        .receipt(id)
                        .filter(|receipt| S::receipt_game(receipt) == game)?;
                    let request = S::update_request(&receipt);
                    Some((receipt, request))
                }
            };
            if !shared.can_maintain_cloud(inner, &game) {
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
            let installed = installed
                .as_ref()
                .map(|(receipt, request)| (receipt, request));
            let _ = shared.content.maintain(&game, installed, control).await;
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
    /// Removes the installation; `Ok(false)` when sign-out or a cancellation
    /// came first. A failure is a localization key.
    async fn remove(
        &self,
        receipt: &S::Receipt,
        request: &S::Request,
        remaining: &[S::Receipt],
    ) -> Result<bool, String> {
        match self.removal(receipt, request, remaining).await {
            Err(Failure::Cancelled) => Ok(false),
            Err(Failure::Failed(reason)) => Err(reason),
            Ok(removed) => Ok(removed),
        }
    }

    async fn removal(
        &self,
        receipt: &S::Receipt,
        request: &S::Request,
        remaining: &[S::Receipt],
    ) -> Result<bool, Failure> {
        let signed_out = || self.inner.lock().unwrap().signed_out;
        if signed_out() {
            return Ok(false);
        }
        self.content.archive(receipt, request).await?;
        if signed_out() {
            return Ok(false);
        }
        let directory = PathBuf::from(S::receipt_directory(receipt));
        let expected = self.content.directory_name(receipt, request);
        let protected: Vec<_> = self.protected.clone();
        let file = self.receipts_file.clone();
        let remaining = remaining.to_vec();
        let removal = tokio::task::spawn_blocking(move || {
            let protected: Vec<&Path> = protected.iter().map(PathBuf::as_path).collect();
            sailover_core::installation::remove(&directory, &expected, &protected)
                .map_err(removal_reason)?;
            // The captured account's receipts are updated even if it signs
            // out while the files go.
            save(&file, &remaining).map_err(|error| Failure::Failed(error.to_string()))
        })
        .await
        .unwrap_or_else(|_| Err(Failure::Failed("model.game.installationBusy".into())));
        if let Err(error) = removal {
            // Receipt persistence can fail after the folder was removed. The
            // Cloud fence must still record that removal.
            if !Path::new(S::receipt_directory(receipt)).exists() {
                self.content.removed(receipt, request).await?;
            }
            return Err(error);
        }
        // Removal is never a Cloud delete.
        self.content.removed(receipt, request).await?;
        // As Steam deletes a Proton prefix, the prefix goes with its game,
        // after the Cloud archive took its saves.
        if let Some(Err(_)) = self.content.remove_prefix(receipt) {
            self.update(|inner| Self::warn(inner, "model.installations.prefixNotRemoved"));
        }
        Ok(!signed_out())
    }
}

fn removal_reason(failure: sailover_core::installation::RemovalFailure) -> Failure {
    use sailover_core::installation::RemovalFailure;
    Failure::Failed(match failure {
        RemovalFailure::Unsafe(_) => "model.game.unsafeInstallation".into(),
        RemovalFailure::Busy => "model.game.installationBusy".into(),
        RemovalFailure::Io(error) => error.to_string(),
    })
}

#[cfg(test)]
mod tests;
