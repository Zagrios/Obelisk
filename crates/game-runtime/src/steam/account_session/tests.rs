//! The Account session through its interface, against CM connections and a
//! connector the test stands in for. Time is paused, except where the
//! account's installations run: a wait passes as soon as nothing else can
//! run, and each attempt to connect records when it came.
use super::*;
use crate::{
    Locations,
    installations::{Installations, Offer, Queueing, Report, tests::eventually},
    queue::DownloadQueue,
    runtimes::tests::ready,
    sessions::Sessions,
    steam::{Product, content::Content},
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use steam_kit::{client::ClientError, friends::FriendsEventSource};
use tokio::time::Instant;

/// The account, and its friends list and Alice's persona as Steam sends
/// them: written by hand, as the macOS tests' fixtures are.
const ACCOUNT: u64 = 0x0110_0001_0000_0064;
const FRIENDS_LIST: &[u8] = &[
    0x08, 0x00, 0x12, 0x0b, 0x09, 1, 0, 0, 0, 1, 0, 0x10, 1, 0x10, 3,
];
const ALICE: &[u8] = &[
    0x08, 0x13, 0x12, 21, 0x09, 1, 0, 0, 0, 1, 0, 0x10, 1, 0x10, 1, 0x7a, 5, b'A', b'l', b'i',
    b'c', b'e', 0xfa, 1, 0,
];
/// What a call through a client whose connection ended fails with.
const DISCONNECTED: &str = "backend.steam.cm.disconnected";
/// What a download its client's shutdown stopped fails with.
const STOPPED: &str = "backend.steam.content.cancelled";

/// One CM connection the test stands in for. Its friends go through Steam's
/// own reducer. Every call it serves is recorded, and fails as through a
/// dead client once it ended.
struct Connection {
    friends: FriendsEventSource,
    ended: CancelToken,
    /// Its downloads wait while the test holds them, until it ends.
    held: Mutex<Option<CancelToken>>,
    /// Steam offers a newer build of every installation through it.
    newer: AtomicBool,
    calls: Mutex<Vec<String>>,
}

impl Connection {
    /// A connection whose friends list arrived: Alice, online.
    fn new() -> Arc<Self> {
        let friends = FriendsEventSource::new(ACCOUNT);
        friends.apply_packet(767, FRIENDS_LIST);
        friends.apply_packet(766, ALICE);
        Arc::new(Self {
            friends,
            ended: CancelToken::new(),
            held: Mutex::new(None),
            newer: AtomicBool::new(false),
            calls: Mutex::new(Vec::new()),
        })
    }
    /// A connection through which Steam offers newer builds.
    fn offering_updates() -> Arc<Self> {
        let connection = Self::new();
        connection.newer.store(true, Ordering::Release);
        connection
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn hold(&self) {
        *self.held.lock().unwrap() = Some(CancelToken::new());
    }
    /// Steam drops the connection, as when its socket closes: it ends
    /// before its friends tell it.
    fn drops(&self) {
        self.ended.cancel();
        self.friends.disconnect(false);
    }
    /// Steam logs the account off: it must sign in again.
    fn logs_off(&self) {
        self.ended.cancel();
        self.friends.disconnect(true);
    }
    /// Records `call`, which fails once the connection ended.
    fn serve(&self, call: impl Into<String>) -> Result<(), Failure> {
        self.calls.lock().unwrap().push(call.into());
        if self.ended() {
            return Err(Failure::Failed(DISCONNECTED.into()));
        }
        Ok(())
    }
}

impl Network for Connection {
    fn platform<'a>(&'a self, _: u32, _: &'a str) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move { self.serve("platform").map(|()| Platform::Mac) })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Steam>,
    ) -> BoxFuture<'a, Result<Installation, Failure>> {
        Box::pin(async move {
            let app_id = transfer.product.app_id;
            self.serve(format!("download:{app_id}"))?;
            let held = self.held.lock().unwrap().clone();
            if let Some(held) = held {
                tokio::select! {
                    () = held.cancelled() => {}
                    () = self.ended.cancelled() => {}
                }
            }
            if self.ended() {
                return Err(Failure::Failed(STOPPED.into()));
            }
            Ok(Installation {
                app_id,
                name: transfer.name.clone(),
                directory: transfer
                    .directory
                    .join(format!("steam-{app_id}-macos"))
                    .to_string_lossy()
                    .into_owned(),
                platform: Platform::Mac,
                language: transfer.product.language.clone(),
                total_bytes: 100,
                manifests: Vec::new(),
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        None
    }
    fn cancel(&self) {
        self.calls.lock().unwrap().push("cancel".into());
    }
    fn update_available<'a>(
        &'a self,
        _: &'a Receipt<Steam>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move {
            self.serve("update")
                .map(|()| self.newer.load(Ordering::Acquire))
        })
    }
    fn launch_target<'a>(
        &'a self,
        _: u32,
        _: Platform,
        install: &'a Path,
    ) -> BoxFuture<'a, Result<LaunchTarget, Failure>> {
        Box::pin(async move {
            self.serve("launch")?;
            Ok(LaunchTarget {
                executable: install.join("Portal 2.app").to_string_lossy().into_owned(),
                arguments: Vec::new(),
                working_directory: install.to_string_lossy().into_owned(),
                is_application_bundle: true,
            })
        })
    }
    fn archive_rules(
        &self,
        _: u32,
    ) -> BoxFuture<'_, Result<Option<(CloudMetadata, CloudListing)>, steam_cloud::Failure>> {
        Box::pin(async move {
            self.serve("archive")
                .map(|()| None)
                .map_err(|_| steam_cloud::Failure::Network)
        })
    }
    fn maintain(
        &self,
        _: steam_cloud::MaintenanceTarget,
        _: &Credentials,
        _: Arc<Control>,
        _: &IdleGuard,
    ) -> Result<(), steam_cloud::Failure> {
        Ok(())
    }
}

impl Transport for Connection {
    fn next_friends(&self, after_revision: u64) -> BoxFuture<'_, FriendsSnapshot> {
        Box::pin(self.friends.next(after_revision))
    }
    fn ended(&self) -> bool {
        self.ended.is_cancelled()
    }
    fn shutdown(&self) {
        self.ended.cancel();
        self.friends.disconnect(false);
    }
}

/// How the test's connector answers one attempt to connect.
enum Answer {
    Connects(Arc<Connection>),
    /// Steam refuses the account's token.
    Refuses,
    Fails,
    /// Connects once the token is cancelled.
    Holds(Arc<Connection>, CancelToken),
}

/// Answers each attempt in turn; an attempt without an answer waits.
struct Connector {
    answers: Mutex<VecDeque<Answer>>,
    /// When each attempt came.
    attempts: Mutex<Vec<Instant>>,
}

impl Connector {
    fn new(answers: impl IntoIterator<Item = Answer>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            attempts: Mutex::new(Vec::new()),
        })
    }
    fn attempts(&self) -> Vec<Instant> {
        self.attempts.lock().unwrap().clone()
    }
    /// How long after `since` each attempt from then on came.
    fn waits(&self, since: Instant) -> Vec<Duration> {
        self.attempts()
            .into_iter()
            .filter(|attempt| *attempt >= since)
            .map(|attempt| attempt - since)
            .collect()
    }
    fn connect(self: &Arc<Self>) -> Connect<Connection> {
        let connector = self.clone();
        Box::new(move || {
            let connector = connector.clone();
            Box::pin(async move { connector.answer().await })
        })
    }
    async fn answer(&self) -> Result<Arc<Connection>, ClientConnectError> {
        self.attempts.lock().unwrap().push(Instant::now());
        let answer = self.answers.lock().unwrap().pop_front();
        match answer {
            Some(Answer::Connects(connection)) => Ok(connection),
            Some(Answer::Refuses) => Err(ClientConnectError::AuthenticationRequired(
                ClientError::Failure("backend.steam.friends.authenticationRequired".into()),
            )),
            Some(Answer::Fails) => Err(ClientConnectError::Other(ClientError::Failure(
                DISCONNECTED.into(),
            ))),
            Some(Answer::Holds(connection, released)) => {
                released.cancelled().await;
                Ok(connection)
            }
            None => std::future::pending().await,
        }
    }
}

/// A failed attempt for every attempt the session makes before it gives up.
fn failures() -> impl Iterator<Item = Answer> {
    WAITS.iter().map(|_| Answer::Fails)
}

/// A session opened through `connector`'s first answer. The attempts it
/// records start afterwards.
async fn open(connector: &Arc<Connector>) -> Arc<AccountSession<Connection>> {
    let session = AccountSession::open(connector.connect()).await.unwrap();
    connector.attempts.lock().unwrap().clear();
    session
}

/// What the session published last.
fn state(session: &AccountSession<Connection>) -> State {
    session.state.borrow().clone()
}

/// The first state after the one the session published last.
async fn changed(session: &AccountSession<Connection>) -> State {
    session.next(state(session).revision).await
}

/// Lets the session's tasks run until `done`, time passing as they all
/// wait.
async fn until(done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(300);
    while !done() {
        assert!(Instant::now() < deadline, "it never happened");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn product(app_id: u32) -> Product {
    Product {
        app_id,
        language: "english".into(),
    }
}

/// The Mac build of `app_id`.
fn transfer(app_id: u32) -> Transfer<Steam> {
    Transfer {
        product: product(app_id),
        name: format!("Game {app_id}"),
        platform: Some(Platform::Mac),
        directory: PathBuf::from("/Games/Steam"),
    }
}

/// Portal 2, installed for macOS.
fn receipt() -> Receipt<Steam> {
    Receipt {
        product: product(620),
        name: "Portal 2".into(),
        directory: "/Games/Steam/steam-620-macos".into(),
        platform: Platform::Mac,
        total_bytes: 100,
        needs_verification: false,
    }
}

/// The account's installations, which reach Steam through its session and
/// keep their receipts in a library of their own. They run on their own
/// runtime, so tests that use them run in real time.
struct Account {
    _root: tempfile::TempDir,
    receipts: PathBuf,
    installations: Installations<Steam>,
}

impl Account {
    async fn new(session: &Arc<AccountSession<Connection>>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().canonicalize().unwrap();
        let locations = Locations::new(
            &library,
            Path::new("/Applications/Obelisk.app"),
            &library.join("home"),
        );
        let sessions = Arc::new(Sessions::<Steam>::new(Box::new(|| Ok(None)), ready()));
        eventually(|| !sessions.state().recovering).await;
        let receipts = library.join("steam-installations-42.json");
        let credentials = Credentials {
            steam_id: 42,
            account_name: "fixture".into(),
            refresh_token: "fixture-only-never-sent".into(),
        };
        let installations = Installations::new(
            Arc::new(Content::new(session.clone(), credentials, locations)),
            sessions,
            42,
            receipts.clone(),
            library.join("home"),
            crate::games_folder::GamesFolder::new(&library),
            Queueing {
                queue: DownloadQueue::new(),
                account: "42".into(),
                name: |game: &u32| game.to_string(),
            },
        );
        Self {
            _root: root,
            receipts,
            installations,
        }
    }

    /// Installs `app_id`, whose Mac build Steam picks.
    fn install(&self, app_id: u32) {
        self.installations.install(Offer {
            product: product(app_id),
            name: format!("Game {app_id}"),
        });
    }
}

/// Downloads `app_id` through `session`, as the account's installations do.
fn downloading(
    session: &Arc<AccountSession<Connection>>,
    app_id: u32,
) -> tokio::task::JoinHandle<Result<u32, Failure>> {
    let session = session.clone();
    tokio::spawn(async move {
        let installed = session.download(&transfer(app_id)).await?;
        Ok(installed.app_id)
    })
}

#[tokio::test(start_paused = true)]
async fn a_dropped_connection_is_replaced_and_the_session_reaches_steam_through_the_new_one() {
    let (a, b) = (Connection::new(), Connection::new());
    let connector = Connector::new([Answer::Connects(a.clone()), Answer::Connects(b.clone())]);
    let session = open(&connector).await;
    let opened = state(&session);
    assert_eq!((opened.phase, opened.transport), (Phase::Connected, 0));
    assert_eq!(opened.friends, a.friends.snapshot());

    let dropped = Instant::now();
    a.drops();
    let reconnecting = changed(&session).await;
    assert_eq!(reconnecting.phase, Phase::Reconnecting);
    assert_eq!(
        reconnecting.friends.friends, opened.friends.friends,
        "the friends stay, stale"
    );
    assert!(a.ended());
    assert!(Arc::ptr_eq(&session.current(), &a));

    let connected = changed(&session).await;
    assert_eq!(
        (connected.phase, connected.transport),
        (Phase::Connected, 1)
    );
    assert_eq!(connected.friends, b.friends.snapshot());
    assert_eq!(connector.waits(dropped), [Duration::from_secs(1)]);
    assert!(Arc::ptr_eq(&session.current(), &b));
    assert!(opened.revision < reconnecting.revision && reconnecting.revision < connected.revision);
}

#[tokio::test(start_paused = true)]
async fn reconnecting_waits_longer_each_time_and_gives_up_after_about_a_minute() {
    let a = Connection::new();
    let connector = Connector::new(
        [Answer::Connects(a.clone())]
            .into_iter()
            .chain(failures())
            .chain([Answer::Connects(Connection::new())]),
    );
    let session = open(&connector).await;

    let dropped = Instant::now();
    a.drops();
    until(|| state(&session).phase == Phase::Unreachable).await;
    assert_eq!(
        connector.waits(dropped),
        [1, 3, 7, 15, 31, 61].map(Duration::from_secs)
    );

    tokio::time::sleep(Duration::from_secs(600)).await;
    assert_eq!(
        connector.waits(dropped).len(),
        WAITS.len(),
        "only a retry tries again"
    );
    let unreachable = state(&session);
    assert_eq!(unreachable.phase, Phase::Unreachable);
    assert_eq!(unreachable.transport, 0);
    assert_eq!(unreachable.friends.friends, a.friends.snapshot().friends);
    assert!(Arc::ptr_eq(&session.current(), &a));
}

#[tokio::test(start_paused = true)]
async fn steam_refusing_the_account_while_reconnecting_ends_the_session_for_good() {
    let a = Connection::new();
    let connector = Connector::new([
        Answer::Connects(a.clone()),
        Answer::Fails,
        Answer::Refuses,
        Answer::Connects(Connection::new()),
    ]);
    let session = open(&connector).await;

    let dropped = Instant::now();
    a.drops();
    until(|| state(&session).phase == Phase::AuthenticationRequired).await;
    assert_eq!(connector.waits(dropped), [1, 3].map(Duration::from_secs));

    session.reconnect();
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(connector.waits(dropped).len(), 2);
    assert_eq!(state(&session).phase, Phase::AuthenticationRequired);
    assert!(Arc::ptr_eq(&session.current(), &a));
}

#[tokio::test(start_paused = true)]
async fn steam_logging_the_account_off_requires_signing_in_again_without_reconnecting() {
    let a = Connection::new();
    let connector = Connector::new([
        Answer::Connects(a.clone()),
        Answer::Connects(Connection::new()),
    ]);
    let session = open(&connector).await;

    a.logs_off();
    let refused = changed(&session).await;
    assert_eq!(refused.phase, Phase::AuthenticationRequired);
    assert!(a.ended());

    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(connector.attempts().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_refused_first_connection_fails_the_sign_in_without_retrying() {
    let connector = Connector::new([Answer::Refuses, Answer::Connects(Connection::new())]);

    let opened = AccountSession::open(connector.connect()).await;
    assert!(matches!(
        opened,
        Err(ClientConnectError::AuthenticationRequired(_))
    ));

    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(connector.attempts().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_retry_replaces_a_live_connection_and_starts_again_after_one_second() {
    let (a, b) = (Connection::new(), Connection::new());
    let connector = Connector::new([Answer::Connects(a.clone()), Answer::Connects(b.clone())]);
    let session = open(&connector).await;

    let retried = Instant::now();
    session.reconnect();
    assert_eq!(
        state(&session).phase,
        Phase::Reconnecting,
        "published before the retry returns"
    );
    assert!(a.ended());

    let connected = changed(&session).await;
    assert_eq!(
        (connected.phase, connected.transport),
        (Phase::Connected, 1)
    );
    assert_eq!(connector.waits(retried), [Duration::from_secs(1)]);
    assert!(Arc::ptr_eq(&session.current(), &b));
}

#[tokio::test(start_paused = true)]
async fn a_retry_starts_again_once_reconnecting_gave_up() {
    let (a, b) = (Connection::new(), Connection::new());
    let connector = Connector::new(
        [Answer::Connects(a.clone())]
            .into_iter()
            .chain(failures())
            .chain([Answer::Connects(b.clone())]),
    );
    let session = open(&connector).await;
    a.drops();
    until(|| state(&session).phase == Phase::Unreachable).await;

    let retried = Instant::now();
    session.reconnect();
    assert_eq!(state(&session).phase, Phase::Reconnecting);
    let connected = changed(&session).await;
    assert_eq!(
        (connected.phase, connected.transport),
        (Phase::Connected, 1)
    );
    assert_eq!(connector.waits(retried), [Duration::from_secs(1)]);
    assert!(Arc::ptr_eq(&session.current(), &b));
}

#[tokio::test(start_paused = true)]
async fn a_retry_discards_an_attempt_under_way() {
    let (a, b, late) = (Connection::new(), Connection::new(), Connection::new());
    let released = CancelToken::new();
    let connector = Connector::new([
        Answer::Connects(a.clone()),
        Answer::Holds(late.clone(), released.clone()),
        Answer::Connects(b.clone()),
    ]);
    let session = open(&connector).await;
    a.drops();
    until(|| connector.attempts().len() == 1).await;

    let retried = Instant::now();
    session.reconnect();
    released.cancel();
    let connected = changed(&session).await;
    assert_eq!(
        (connected.phase, connected.transport),
        (Phase::Connected, 1)
    );
    assert_eq!(connector.waits(retried), [Duration::from_secs(1)]);

    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(state(&session).transport, 1);
    assert!(Arc::ptr_eq(&session.current(), &b));
}

#[tokio::test(start_paused = true)]
async fn closing_shuts_the_connection_down_wakes_every_wait_and_stops_reconnecting() {
    let a = Connection::new();
    let connector = Connector::new([Answer::Connects(a.clone())]);
    let session = open(&connector).await;
    a.drops();
    let reconnecting = changed(&session).await;
    let waiting = tokio::spawn({
        let session = session.clone();
        async move { session.next(reconnecting.revision).await }
    });
    let downloaded = downloading(&session, 620);
    until(|| session.waiting().is_some()).await;

    session.close();
    let closed = waiting.await.unwrap();
    assert_eq!(closed.phase, Phase::Closed);
    assert_eq!(
        session.next(closed.revision).await,
        closed,
        "a closed session answers at once"
    );
    assert_eq!(
        downloaded.await.unwrap(),
        Err(Failure::Failed(DISCONNECTED.into())),
        "the waiting Transfer goes through the dropped connection"
    );
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert!(connector.attempts().is_empty());

    let c = Connection::new();
    let session = open(&Connector::new([Answer::Connects(c.clone())])).await;
    drop(session);
    assert!(c.ended(), "a session that goes shuts its connection down");
}

#[tokio::test(start_paused = true)]
async fn calls_go_through_the_connection_of_the_moment_and_fail_at_once_while_reconnecting() {
    let (a, b) = (Connection::new(), Connection::new());
    let connector = Connector::new([Answer::Connects(a.clone()), Answer::Connects(b.clone())]);
    let session = open(&connector).await;
    a.drops();
    changed(&session).await;

    let asked = Instant::now();
    let install = Path::new("/Games/Steam/steam-620-macos");
    assert_eq!(
        session.update_available(&receipt()).await,
        Err(Failure::Failed(DISCONNECTED.into()))
    );
    assert!(
        session
            .launch_target(620, Platform::Mac, install)
            .await
            .is_err()
    );
    assert!(matches!(
        session.archive_rules(620).await,
        Err(steam_cloud::Failure::Network)
    ));
    assert_eq!(Instant::now(), asked, "nothing waits for the reconnection");
    assert_eq!(a.calls(), ["update", "launch", "archive"]);

    changed(&session).await;
    assert_eq!(session.update_available(&receipt()).await, Ok(false));
    assert!(
        session
            .launch_target(620, Platform::Mac, install)
            .await
            .is_ok()
    );
    assert_eq!(b.calls(), ["update", "launch"]);
}

#[tokio::test(start_paused = true)]
async fn a_transfer_waits_for_the_reconnection_and_downloads_through_the_new_connection() {
    let (a, b) = (Connection::new(), Connection::new());
    let connector = Connector::new([Answer::Connects(a.clone()), Answer::Connects(b.clone())]);
    let session = open(&connector).await;
    let dropped = Instant::now();
    a.drops();
    changed(&session).await;

    let picked = tokio::spawn({
        let session = session.clone();
        async move { session.platform(620, "english").await }
    });
    assert_eq!(picked.await.unwrap(), Ok(Platform::Mac));
    assert_eq!(Instant::now() - dropped, Duration::from_secs(1));
    assert_eq!(downloading(&session, 620).await.unwrap(), Ok(620));
    assert!(a.calls().is_empty());
    assert_eq!(b.calls(), ["platform", "download:620"]);
}

#[tokio::test(start_paused = true)]
async fn a_transfer_waiting_for_the_reconnection_stops_when_cancelled() {
    let a = Connection::new();
    let connector = Connector::new([Answer::Connects(a.clone())]);
    let session = open(&connector).await;
    a.drops();
    changed(&session).await;

    let downloaded = downloading(&session, 620);
    until(|| session.waiting().is_some()).await;
    session.cancel();
    assert_eq!(downloaded.await.unwrap(), Err(Failure::Cancelled));
    assert_eq!(a.calls(), ["cancel"]);
}

#[tokio::test(start_paused = true)]
async fn a_transfer_waiting_when_reconnecting_gives_up_fails_through_the_dropped_connection() {
    let a = Connection::new();
    let connector = Connector::new([Answer::Connects(a.clone())].into_iter().chain(failures()));
    let session = open(&connector).await;
    a.drops();
    changed(&session).await;

    assert_eq!(
        downloading(&session, 620).await.unwrap(),
        Err(Failure::Failed(DISCONNECTED.into()))
    );
    assert_eq!(state(&session).phase, Phase::Unreachable);
    assert_eq!(a.calls(), ["download:620"]);
}

#[tokio::test(start_paused = true)]
async fn a_transfer_whose_turn_comes_before_the_session_heard_of_the_drop_waits_for_the_new_connection()
 {
    let (a, b) = (Connection::new(), Connection::new());
    let connector = Connector::new([Answer::Connects(a.clone()), Answer::Connects(b.clone())]);
    let session = open(&connector).await;

    // The Transfer before it failed as the connection ended, and handed its
    // turn on at once.
    a.drops();
    assert_eq!(state(&session).phase, Phase::Connected);
    let installed = session.download(&transfer(620)).await;
    assert_eq!(installed.map(|installed| installed.app_id), Ok(620));
    assert!(a.calls().is_empty());
    assert_eq!(b.calls(), ["download:620"]);
}

/// The fix this session exists for: a Transfer whose turn comes while the
/// connection is replaced is neither lost nor run through the dead one. It
/// runs in real time, about a second.
#[tokio::test]
async fn queued_transfers_wait_out_a_reconnection_and_download_through_the_new_connection() {
    let (a, b) = (Connection::new(), Connection::new());
    a.hold();
    let connector = Connector::new([Answer::Connects(a.clone()), Answer::Connects(b.clone())]);
    let session = open(&connector).await;
    let account = Account::new(&session).await;
    let installations = &account.installations;
    account.install(620);
    account.install(730);
    eventually(|| a.calls() == ["platform", "download:620"]).await;
    assert_eq!(installations.state().queued, [730]);

    // 620's download ends with its connection, as it always did. 730's turn
    // comes while the session reconnects, and it waits.
    a.drops();
    eventually(|| {
        let state = installations.state();
        state.transfer_game == Some(730) && state.report.is_some()
    })
    .await;
    assert_eq!(
        installations.state().report,
        Some(Report::Storefront(STOPPED.into()))
    );
    assert_eq!(state(&session).phase, Phase::Reconnecting);
    assert!(b.calls().is_empty());

    eventually(|| !installations.state().downloading).await;
    assert_eq!(a.calls(), ["platform", "download:620"]);
    assert_eq!(b.calls(), ["platform", "download:730"]);
    let saved: Vec<Receipt<Steam>> =
        serde_json::from_slice(&std::fs::read(&account.receipts).unwrap()).unwrap();
    assert_eq!(
        saved
            .iter()
            .map(|receipt| receipt.product.app_id)
            .collect::<Vec<_>>(),
        [730]
    );
}

/// An update check made through the connection that dropped holds nothing
/// back: the new one is asked at once, as a new sign-in's would be. It runs
/// in real time, about a second.
#[tokio::test]
async fn a_reconnection_asks_the_new_connection_for_newer_builds_at_once() {
    let (a, b) = (Connection::new(), Connection::offering_updates());
    let connector = Connector::new([Answer::Connects(a.clone()), Answer::Connects(b.clone())]);
    let session = open(&connector).await;
    let account = Account::new(&session).await;
    let installations = &account.installations;
    account.install(620);
    eventually(|| installations.state().receipts.len() == 1).await;
    installations.check_updates();
    eventually(|| a.calls() == ["platform", "download:620", "update"]).await;

    a.drops();
    eventually(|| installations.state().updates == ["620-macos"]).await;
    assert_eq!(b.calls(), ["update"]);
    assert_eq!(state(&session).transport, 1);
}
