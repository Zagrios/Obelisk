//! Installations through their interface, against a storefront session the
//! test drives. Steam stands in for every storefront.
use super::*;
use crate::{
    HostState,
    sessions::{Observation, Supervised},
    steam::{Download, Receipt, Steam, Store},
    supervisor::{FailureReason, Phase},
};
use std::{
    collections::VecDeque,
    sync::{
        Condvar,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

/// How a scripted launch ends: behind a supervisor, at once, or failed.
type Launched = Result<Option<Arc<dyn Supervised<Steam>>>, Failure>;

/// What the fake storefront does with each call, in order.
#[derive(Default)]
struct Script {
    /// Each download's outcome; a missing one waits until `release`.
    downloads: VecDeque<Result<(), Failure>>,
    launches: VecDeque<Launched>,
    archive: Option<Failure>,
    prefix: Option<Result<(), String>>,
    held: bool,
    calls: Vec<String>,
    progress: Option<TransferProgress>,
}

struct Fake {
    script: Mutex<Script>,
    released: Condvar,
    cancelled: AtomicBool,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(Script::default()),
            released: Condvar::new(),
            cancelled: AtomicBool::new(false),
        })
    }
    fn with<T>(&self, change: impl FnOnce(&mut Script) -> T) -> T {
        change(&mut self.script.lock().unwrap())
    }
    fn calls(&self) -> Vec<String> {
        self.with(|script| script.calls.clone())
    }
    fn hold(&self) {
        self.with(|script| script.held = true);
    }
    fn release(&self) {
        self.with(|script| script.held = false);
        self.released.notify_all();
    }
    /// Waits while the test holds the storefront, off the async runtime.
    async fn wait(self: &Arc<Self>) {
        let fake = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut script = fake.script.lock().unwrap();
            while script.held && !fake.cancelled.load(Ordering::Acquire) {
                script = fake.released.wait(script).unwrap();
            }
        })
        .await
        .unwrap();
    }
}

struct Content(Arc<Fake>);

impl super::Content<Steam> for Content {
    fn download<'a>(&'a self, request: &'a Download) -> BoxFuture<'a, Result<Receipt, Failure>> {
        Box::pin(async move {
            self.0.with(|script| {
                script.calls.push(format!("download:{}", request.app_id));
                script.progress = Some(TransferProgress {
                    state: TransferState::Downloading,
                    completed_bytes: 50,
                    total_bytes: 100,
                    ..TransferProgress::new(request.app_id.to_string())
                });
            });
            self.0.wait().await;
            if self.0.cancelled.load(Ordering::Acquire) {
                self.0.with(|script| {
                    if let Some(progress) = &mut script.progress {
                        progress.state = TransferState::Cancelled;
                    }
                });
                return Err(Failure::Cancelled);
            }
            let outcome = self
                .0
                .with(|script| script.downloads.pop_front())
                .unwrap_or(Ok(()));
            outcome?;
            let directory = request
                .directory
                .join(format!("steam-{}-{}", request.app_id, request.platform));
            std::fs::create_dir_all(&directory).unwrap();
            self.0.with(|script| {
                if let Some(progress) = &mut script.progress {
                    progress.state = TransferState::Complete;
                    progress.completed_bytes = 100;
                }
            });
            Ok(Receipt {
                app_id: request.app_id,
                name: request.name.clone(),
                directory: directory.to_string_lossy().into_owned(),
                platform: request.platform.clone(),
                language: request.language.clone(),
                total_bytes: 100,
                needs_verification: None,
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        self.0.with(|script| script.progress.clone())
    }
    fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.released.notify_all();
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt,
        _: &'a Download,
        admission: Admission<Steam>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.0
                .with(|script| script.calls.push(format!("launch:{}", receipt.app_id)));
            self.0.wait().await;
            let outcome = self
                .0
                .with(|script| script.launches.pop_front())
                .unwrap_or(Err(Failure::Failed(
                    "backend.steam.runtime.startFailed".into(),
                )));
            match outcome {
                Ok(Some(supervisor)) => admission.follow(supervisor).await,
                Ok(None) => Ok(()),
                Err(failure) => Err(failure),
            }
        })
    }
    fn archive<'a>(
        &'a self,
        receipt: &'a Receipt,
        _: &'a Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.0.with(|script| {
                script.calls.push(format!("archive:{}", receipt.app_id));
                script.archive.take().map_or(Ok(()), Err)
            })
        })
    }
    fn removed<'a>(
        &'a self,
        receipt: &'a Receipt,
        _: &'a Download,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.0
                .with(|script| script.calls.push(format!("removed:{}", receipt.app_id)));
            Ok(())
        })
    }
    fn directory_name(&self, receipt: &Receipt, _: &Download) -> String {
        format!("steam-{}-{}", receipt.app_id, receipt.platform)
    }
    fn remove_prefix(&self, receipt: &Receipt) -> Option<Result<(), String>> {
        (receipt.platform == "windows").then(|| {
            self.0.with(|script| {
                script.calls.push("prefix".into());
                script.prefix.take().unwrap_or(Ok(()))
            })
        })
    }
}

/// A supervisor whose game runs until the test ends it.
struct Game {
    state: Mutex<Observation<Steam>>,
    changed: Condvar,
}
impl Game {
    fn running() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Observation {
                id: "session".into(),
                account: 42,
                store: Store {
                    app_id: 620,
                    host: HostState::Online,
                },
                phase: Phase::Running,
                finished: false,
                failure: None,
                error: None,
                revision: 1,
                reconnecting: false,
                rosetta: None,
                cloud: Default::default(),
            }),
            changed: Condvar::new(),
        })
    }
    fn end(&self, failure: Option<&str>) {
        let mut state = self.state.lock().unwrap();
        state.phase = if failure.is_some() {
            Phase::Failed
        } else {
            Phase::Finished
        };
        state.failure = failure.map(|_| FailureReason::Failed);
        state.error = failure.map(Into::into);
        state.finished = true;
        state.revision += 1;
        self.changed.notify_all();
    }
}
impl Supervised<Steam> for Game {
    fn snapshot(&self, after: Option<u64>) -> Result<Observation<Steam>, String> {
        let mut state = self.state.lock().unwrap();
        while after == Some(state.revision) && !state.finished {
            state = self.changed.wait(state).unwrap();
        }
        Ok(state.clone())
    }
    fn cancel(&self) {}
    fn stop(&self) -> Result<(), String> {
        self.end(None);
        Ok(())
    }
    fn begin(&self) -> Result<(), String> {
        Ok(())
    }
    fn choose(&self, _: &u64, _: u64, _: crate::cloud::Choice) -> Result<(), String> {
        Ok(())
    }
    fn resume_after_rosetta(&self, _: &u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    fn revoke(&self, _: &u64) -> Result<(), String> {
        Ok(())
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    fake: Arc<Fake>,
    sessions: Arc<Sessions<Steam>>,
    installations: Installations<Steam>,
}

impl Fixture {
    /// An account with Portal 2 installed for macOS, and `receipts` written
    /// before the installations load.
    async fn new(installed: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let games = root.join("Games/Steam");
        std::fs::create_dir_all(&games).unwrap();
        let file = root.join("steam-installations-42.json");
        if installed {
            let installation = games.join("steam-620-macos");
            std::fs::create_dir_all(&installation).unwrap();
            save(&file, &[receipt(&installation, "macos")]).unwrap();
        }
        let sessions = Arc::new(Sessions::new(Box::new(|| Ok(None))));
        eventually(|| !sessions.state().recovering).await;
        let fake = Fake::new();
        let installations = Installations::new(
            Arc::new(Content(fake.clone())),
            sessions.clone(),
            42,
            file,
            root.clone(),
            root.join("home"),
        );
        Self {
            _root: directory,
            root,
            fake,
            sessions,
            installations,
        }
    }
    fn request(&self, platform: &str) -> Download {
        Download {
            app_id: 620,
            name: "Portal 2".into(),
            platform: platform.into(),
            language: "french".into(),
            directory: self.root.join("Games/Steam"),
        }
    }
    fn receipt(&self) -> Receipt {
        self.installations.state().receipts[0].clone()
    }
    fn saved(&self) -> Vec<Receipt> {
        load(
            &self.root.join("steam-installations-42.json"),
            |receipt: &Receipt| &receipt.directory,
        )
        .unwrap()
    }
}

fn receipt(directory: &Path, platform: &str) -> Receipt {
    Receipt {
        app_id: 620,
        name: "Portal 2".into(),
        directory: directory.to_string_lossy().into_owned(),
        platform: platform.into(),
        language: "french".into(),
        total_bytes: 100,
        needs_verification: None,
    }
}

async fn eventually(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn receipts_load_from_the_account_file_without_games_removed_outside_sailover() {
    let fixture = Fixture::new(true).await;
    assert_eq!(fixture.installations.state().receipts.len(), 1);

    std::fs::remove_dir_all(fixture.root.join("Games/Steam/steam-620-macos")).unwrap();
    let file = fixture.root.join("steam-installations-42.json");
    let reloaded = Installations::new(
        Arc::new(Content(Fake::new())),
        fixture.sessions.clone(),
        42,
        file.clone(),
        fixture.root.clone(),
        fixture.root.join("home"),
    );
    assert!(reloaded.state().receipts.is_empty());

    std::fs::write(&file, b"not json").unwrap();
    let unreadable = Installations::new(
        Arc::new(Content(Fake::new())),
        fixture.sessions.clone(),
        42,
        file,
        fixture.root.clone(),
        fixture.root.join("home"),
    );
    assert_eq!(
        unreadable.state().warning.map(|(_, key)| key).as_deref(),
        Some("model.steam.receiptsUnavailable")
    );
}

#[tokio::test]
async fn a_transfer_reports_its_progress_and_publishes_its_receipt_once_complete() {
    let fixture = Fixture::new(false).await;
    fixture.fake.hold();
    fixture.installations.download(fixture.request("windows"));
    let state = fixture.installations.state();
    assert!(state.downloading);
    assert_eq!(state.transfer_game, Some(620));
    assert_eq!(state.transfer.unwrap().state, TransferState::Preparing);
    assert!(!fixture.installations.can_modify(&620));

    eventually(|| {
        fixture
            .installations
            .state()
            .transfer
            .is_some_and(|transfer| transfer.completed_bytes == 50)
    })
    .await;
    fixture.fake.release();
    eventually(|| !fixture.installations.state().downloading).await;

    let state = fixture.installations.state();
    assert_eq!(state.transfer.unwrap().state, TransferState::Complete);
    assert_eq!(state.receipts.len(), 1);
    assert_eq!(state.last_download, None);
    assert_eq!(state.report, None);
    assert_eq!(fixture.saved(), state.receipts);
}

#[tokio::test]
async fn a_failed_transfer_is_reported_and_stays_resumable_and_its_installation_unverified() {
    let fixture = Fixture::new(true).await;
    fixture.fake.with(|script| {
        script
            .downloads
            .push_back(Err(Failure::Failed("backend.steam.content.network".into())))
    });
    fixture.installations.download(fixture.request("macos"));
    eventually(|| !fixture.installations.state().downloading).await;

    let state = fixture.installations.state();
    assert_eq!(
        state.report,
        Some(Report::Storefront("backend.steam.content.network".into()))
    );
    assert_eq!(state.transfer.unwrap().state, TransferState::Failed);
    assert_eq!(state.last_download, Some(fixture.request("macos")));
    assert_eq!(state.receipts[0].needs_verification, Some(true));
    assert_eq!(fixture.saved()[0].needs_verification, Some(true));

    fixture.installations.resume_download();
    eventually(|| !fixture.installations.state().downloading).await;
    let state = fixture.installations.state();
    assert_eq!(state.report, None);
    assert_eq!(state.receipts[0].needs_verification, None);
    assert_eq!(fixture.fake.calls(), ["download:620", "download:620"]);
}

#[tokio::test]
async fn a_cancelled_transfer_ends_silently() {
    let fixture = Fixture::new(false).await;
    fixture.fake.hold();
    fixture.installations.download(fixture.request("macos"));
    eventually(|| fixture.fake.calls().len() == 1).await;
    fixture.installations.cancel();
    eventually(|| !fixture.installations.state().downloading).await;

    let state = fixture.installations.state();
    assert_eq!(state.report, None);
    assert_eq!(state.transfer.unwrap().state, TransferState::Cancelled);
    assert!(state.receipts.is_empty());
    assert!(state.last_download.is_some());
}

#[tokio::test]
async fn a_folder_that_cannot_be_created_is_reported_before_any_transfer() {
    let fixture = Fixture::new(false).await;
    std::fs::write(fixture.root.join("file"), b"").unwrap();
    let mut request = fixture.request("macos");
    request.directory = fixture.root.join("file/games");
    fixture.installations.download(request);
    let state = fixture.installations.state();
    assert!(matches!(state.report, Some(Report::DownloadDirectory(_))));
    assert!(!state.downloading);
    assert!(fixture.fake.calls().is_empty());
}

#[tokio::test]
async fn a_launch_verifies_first_when_asked_then_ends_its_preparation_once_the_game_runs() {
    let fixture = Fixture::new(true).await;
    let game = Game::running();
    fixture.fake.with(|script| {
        script
            .launches
            .push_back(Ok(Some(game.clone() as Arc<dyn Supervised<Steam>>)))
    });
    fixture
        .installations
        .launch(fixture.receipt(), fixture.request("macos"), true);
    let state = fixture.installations.state();
    assert!(state.launching);
    assert!(state.verifying_before_launch);
    assert_eq!(state.launch_game, Some(620));
    assert!(
        fixture.sessions.state().busy(),
        "the Reservation holds from admission"
    );

    eventually(|| !fixture.installations.state().launching).await;
    assert_eq!(fixture.fake.calls(), ["download:620", "launch:620"]);
    assert!(fixture.sessions.state().busy());
    assert!(!fixture.installations.can_modify(&620));

    game.end(None);
    eventually(|| !fixture.sessions.state().busy()).await;
    eventually(|| fixture.installations.state().launch_game.is_none()).await;
    assert_eq!(fixture.installations.state().report, None);
}

#[tokio::test]
async fn an_unverified_installation_is_verified_before_it_launches_and_a_failure_is_reported() {
    let fixture = Fixture::new(true).await;
    fixture.fake.with(|script| {
        script
            .downloads
            .push_back(Err(Failure::Failed("backend.steam.content.network".into())))
    });
    fixture.installations.download(fixture.request("macos"));
    eventually(|| !fixture.installations.state().downloading).await;

    fixture
        .installations
        .launch(fixture.receipt(), fixture.request("macos"), false);
    eventually(|| !fixture.installations.state().launching).await;
    assert_eq!(
        fixture.fake.calls(),
        ["download:620", "download:620", "launch:620"]
    );
    assert_eq!(
        fixture.installations.state().report,
        Some(Report::Storefront(
            "backend.steam.runtime.startFailed".into()
        ))
    );
    assert!(!fixture.sessions.state().busy());
}

#[tokio::test]
async fn cancelling_a_launch_still_preparing_never_hands_the_game_off() {
    let fixture = Fixture::new(true).await;
    let game = Game::running();
    fixture.fake.with(|script| {
        script
            .launches
            .push_back(Ok(Some(game.clone() as Arc<dyn Supervised<Steam>>)))
    });
    fixture.fake.hold();
    fixture
        .installations
        .launch(fixture.receipt(), fixture.request("macos"), false);
    eventually(|| fixture.fake.calls() == ["launch:620"]).await;

    fixture.installations.cancel();
    fixture.fake.release();
    eventually(|| !fixture.installations.state().launching).await;
    eventually(|| !fixture.sessions.state().busy()).await;
    assert_eq!(fixture.installations.state().report, None);
    assert!(fixture.installations.can_modify(&620));
}

#[tokio::test]
async fn nothing_touches_an_installation_its_game_session_or_another_action_holds() {
    let fixture = Fixture::new(true).await;
    let admission = fixture.sessions.admit(620, 42).unwrap();
    fixture.installations.download(fixture.request("macos"));
    fixture
        .installations
        .uninstall(fixture.receipt(), fixture.request("macos"));
    fixture
        .installations
        .launch(fixture.receipt(), fixture.request("macos"), false);
    let state = fixture.installations.state();
    assert!(!state.downloading && !state.uninstalling && !state.launching);
    drop(admission);

    fixture.installations.set_diagnostic(true);
    assert!(!fixture.installations.can_modify(&620));
    fixture.installations.set_diagnostic(false);
    assert!(fixture.installations.can_modify(&620));
    assert!(fixture.fake.calls().is_empty());
}

#[tokio::test]
async fn an_uninstall_archives_removes_records_and_deletes_the_prefix_in_order() {
    let fixture = Fixture::new(false).await;
    fixture.installations.download(fixture.request("windows"));
    eventually(|| !fixture.installations.state().downloading).await;
    let receipt = fixture.receipt();

    fixture
        .installations
        .uninstall(receipt.clone(), fixture.request("windows"));
    assert!(fixture.installations.state().uninstalling);
    eventually(|| !fixture.installations.state().uninstalling).await;

    let state = fixture.installations.state();
    assert!(state.receipts.is_empty());
    assert_eq!(state.transfer, None);
    assert_eq!(state.report, None);
    assert!(!Path::new(&receipt.directory).exists());
    assert!(fixture.saved().is_empty());
    assert_eq!(
        fixture.fake.calls(),
        ["download:620", "archive:620", "removed:620", "prefix"]
    );
}

#[tokio::test]
async fn a_failed_archive_keeps_the_game_and_a_lost_prefix_is_only_a_warning() {
    let fixture = Fixture::new(true).await;
    fixture.fake.with(|script| {
        script.archive = Some(Failure::Failed("backend.steam.cloud.network".into()))
    });
    let receipt = fixture.receipt();
    fixture
        .installations
        .uninstall(receipt.clone(), fixture.request("macos"));
    eventually(|| !fixture.installations.state().uninstalling).await;
    let state = fixture.installations.state();
    assert_eq!(
        state.report,
        Some(Report::Uninstall("backend.steam.cloud.network".into()))
    );
    assert_eq!(state.receipts, [receipt]);
    assert!(Path::new(&state.receipts[0].directory).is_dir());

    let fixture = Fixture::new(false).await;
    fixture.installations.download(fixture.request("windows"));
    eventually(|| !fixture.installations.state().downloading).await;
    fixture
        .fake
        .with(|script| script.prefix = Some(Err("busy".into())));
    fixture
        .installations
        .uninstall(fixture.receipt(), fixture.request("windows"));
    eventually(|| !fixture.installations.state().uninstalling).await;
    let state = fixture.installations.state();
    assert!(state.receipts.is_empty());
    assert_eq!(
        state.warning.map(|(_, key)| key).as_deref(),
        Some("model.installations.prefixNotRemoved")
    );
}

#[tokio::test]
async fn a_removal_is_recorded_even_when_the_receipts_cannot_be_saved_after_it() {
    let fixture = Fixture::new(true).await;
    let receipt = fixture.receipt();
    let file = fixture.root.join("steam-installations-42.json");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir_all(&file).unwrap();

    fixture
        .installations
        .uninstall(receipt.clone(), fixture.request("macos"));
    eventually(|| !fixture.installations.state().uninstalling).await;

    let state = fixture.installations.state();
    assert!(!Path::new(&receipt.directory).exists());
    assert!(
        state.receipts.is_empty(),
        "the vanished folder leaves the receipts"
    );
    assert!(matches!(state.report, Some(Report::Uninstall(_))));
    assert_eq!(fixture.fake.calls(), ["archive:620", "removed:620"]);
}

#[tokio::test]
async fn signing_out_stops_the_transfer_and_later_actions() {
    let fixture = Fixture::new(false).await;
    fixture.fake.hold();
    fixture.installations.download(fixture.request("macos"));
    eventually(|| fixture.fake.calls().len() == 1).await;
    fixture.installations.signed_out();
    eventually(|| !fixture.installations.state().downloading).await;
    assert_eq!(fixture.installations.state().report, None);
    fixture.installations.download(fixture.request("macos"));
    assert!(!fixture.installations.state().downloading);
}
