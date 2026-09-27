//! One account's installed games of one storefront, and the actions on them:
//! Transfers, verified launches, uninstalls and Cloud maintenance. Every
//! action asks one Reservation gate, so a new one cannot touch a running
//! game's files. The account's Installation receipts live here, in the file
//! SailOver keeps per storefront and account. So does the account's one Cloud
//! question, whichever sync asks it.
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
    /// One Transfer the player asked for: a game's platform, installed under
    /// a parent folder.
    type Request: Clone + fmt::Debug + PartialEq + Send + Sync;
    /// Shown when the receipt file cannot be read, or saved.
    const RECEIPTS_UNAVAILABLE: &'static str;
    const RECEIPTS_NOT_SAVED: &'static str;

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
}

struct Shared<S: Storefront> {
    inner: Mutex<Inner<S>>,
    published: watch::Sender<State<S>>,
    content: Arc<dyn Content<S>>,
    sessions: Arc<Sessions<S>>,
    account: S::Account,
    receipts_file: PathBuf,
    /// SailOver's library directory and the user's home: never removed.
    protected: Vec<PathBuf>,
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
        let state = &inner.state;
        !inner.signed_out
            && !state.downloading
            && !state.launching
            && !state.uninstalling
            && !inner.diagnostic
            && state.maintenance.is_none()
            && !protects(&self.sessions.state(), game)
    }

    /// One Cloud operation at a time: a Game session or recovery, a launch,
    /// or maintenance of any game keeps it closed, and so does this game's
    /// Transfer or uninstall.
    fn can_maintain_cloud(&self, inner: &Inner<S>, game: &S::Game) -> bool {
        let state = &inner.state;
        !inner.signed_out
            && !inner.diagnostic
            && state.maintenance.is_none()
            && !state.launching
            && !(state.downloading && state.transfer_game.as_ref() == Some(game))
            && !(state.uninstalling && state.uninstall_game.as_ref() == Some(game))
            && !self.sessions.state().busy()
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

/// Game sessions or recovery hold the game.
fn protects<S: sessions::Storefront>(state: &sessions::State<S>, game: &S::Game) -> bool {
    state.recovering
        || state
            .active
            .as_ref()
            .is_some_and(|active| &active.game == game)
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
            },
            signed_out: false,
            diagnostic: false,
            cancellation_requested: false,
            launch: None,
            warnings: 0,
            maintenance: None,
            maintenances: 0,
        };
        match load(&receipts_file, |receipt: &S::Receipt| {
            S::receipt_directory(receipt)
        }) {
            Ok(receipts) => inner.state.receipts = receipts,
            Err(_) => Shared::<S>::warn(&mut inner, S::RECEIPTS_UNAVAILABLE),
        }
        let (published, _) = watch::channel(inner.state.clone());
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(inner),
                published,
                content,
                sessions,
                account,
                receipts_file,
                protected: vec![library, home],
            }),
        }
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

    /// Whether an action may touch `game`'s installation now.
    pub fn can_modify(&self, game: &S::Game) -> bool {
        let inner = self.shared.inner.lock().unwrap();
        self.shared.can_modify(&inner, game)
    }

    /// A DEBUG diagnostic holds the storefront while it runs.
    pub fn set_diagnostic(&self, running: bool) {
        self.shared.update(|inner| inner.diagnostic = running);
    }

    /// Installs `request`'s game, or updates its installation in place.
    pub fn download(&self, request: S::Request) {
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

    /// Resumes the Transfer that stopped before its receipt.
    pub fn resume_download(&self) {
        let last = self
            .shared
            .inner
            .lock()
            .unwrap()
            .state
            .last_download
            .clone();
        if let Some(request) = last {
            self.download(request);
        }
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

    /// Launches the installed game, verifying it first with `verification`
    /// when `verify` asks for it or its receipt needs it.
    pub fn launch(&self, receipt: S::Receipt, verification: S::Request, verify: bool) {
        let shared = &self.shared;
        let game = S::receipt_game(&receipt);
        let admission = shared.update(|inner| {
            if !shared.can_modify(inner, &game)
                || !inner.state.receipts.contains(&receipt)
                || shared.sessions.state().busy()
            {
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
            Some((admission, verify))
        });
        let Some((admission, verify)) = admission else {
            return;
        };
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

    /// Removes the installation of `receipt`. `request` names the game as the
    /// storefront knows it. The Cloud archive takes the saves first; the
    /// removal is recorded for Cloud sync even if the account signs out
    /// meanwhile, and a Windows installation's prefix goes last.
    pub fn uninstall(&self, receipt: S::Receipt, request: S::Request) {
        let shared = &self.shared;
        let game = S::receipt_game(&receipt);
        let remaining = shared.update(|inner| {
            if !shared.can_modify(inner, &game) || !inner.state.receipts.contains(&receipt) {
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
            Some(remaining)
        });
        let Some(remaining) = remaining else { return };
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

    /// Whether the player may sync `game`'s Cloud saves now.
    pub fn can_maintain_cloud(&self, game: &S::Game) -> bool {
        let inner = self.shared.inner.lock().unwrap();
        self.shared.can_maintain_cloud(&inner, game)
    }

    /// Syncs `game`'s Cloud saves: those of its installation `installed`
    /// names, or, uninstalled, those its saved Cloud context names. Its idle
    /// guard keeps every launch of the storefront out, so it holds the
    /// Reservation of every game until it ends. A failure shows only in the
    /// game's Cloud indicator: Sync now is the retry.
    pub fn maintain_cloud(&self, game: S::Game, installed: Option<(S::Receipt, S::Request)>) {
        let shared = &self.shared;
        let started = shared.update(|inner| {
            if !shared.can_maintain_cloud(inner, &game)
                || installed.as_ref().is_some_and(|(receipt, _)| {
                    S::receipt_game(receipt) != game || !inner.state.receipts.contains(receipt)
                })
            {
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
            Some((number, control))
        });
        let Some((number, control)) = started else {
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
    /// maintenance's, else its Game session's. Cancel stops either sync.
    pub fn answer_cloud(
        &self,
        game: &S::Game,
        decision_id: u64,
        choice: cloud::Choice,
    ) -> Result<(), Failure> {
        let refused = || Failure::Failed(S::INVALID_SESSION.to_owned());
        let maintenance = {
            let inner = self.shared.inner.lock().unwrap();
            if inner.signed_out {
                return Err(refused());
            }
            match (&inner.state.maintenance, &inner.maintenance) {
                (Some(maintenance), Some((_, control))) if &maintenance.game == game => {
                    Some(control.clone())
                }
                _ => None,
            }
        };
        if let Some(control) = maintenance {
            return control.answer(decision_id, choice).map_err(|_| refused());
        }
        let active = self
            .shared
            .sessions
            .state()
            .active
            .filter(|active| &active.game == game && active.account == self.shared.account)
            .ok_or_else(refused)?;
        self.shared.sessions.choose(&active.id, decision_id, choice)
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
