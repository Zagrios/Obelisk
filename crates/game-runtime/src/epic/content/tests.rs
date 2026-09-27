//! Epic's part of the installations through their Content interface, with a
//! network the test stands in for and real files in a temporary library.
use super::*;
use crate::{installations::Content as _, sessions::Sessions};
use std::{collections::VecDeque, sync::Mutex, time::Duration};
use tokio::sync::Notify;

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
const STRANGER: &str = "fedcba9876543210fedcba9876543210";

/// What the test's Epic network was asked, in order, and what it answers.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
    /// The account each `account_id` call finds, then `account`.
    accounts: Mutex<VecDeque<String>>,
    /// Renewing credentials waits until the test releases it.
    held: Mutex<bool>,
    released: Notify,
    /// Each maintenance's configuration, and whether an Epic launch could
    /// start meanwhile.
    maintained: Mutex<Vec<(Configuration, bool)>>,
    maintenance: Mutex<Option<Error>>,
}

impl Fake {
    fn call(&self, call: &str) {
        self.calls.lock().unwrap().push(call.into());
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Network for Fake {
    fn prepare(&self) -> BoxFuture<'_, Result<(), Failure>> {
        Box::pin(async move {
            self.call("prepare");
            if *self.held.lock().unwrap() {
                self.released.notified().await;
            }
            Ok(())
        })
    }
    fn account_id(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async move {
            self.call("account");
            Ok(self
                .accounts
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| ACCOUNT.into()))
        })
    }
    fn exchange_code(&self) -> BoxFuture<'_, Result<String, Failure>> {
        Box::pin(async move {
            self.call("exchange");
            Ok("fixture-code".into())
        })
    }
    fn download<'a>(
        &'a self,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<epic_kit::client::Installation, Failure>> {
        Box::pin(async move {
            self.call("download");
            Ok(epic_kit::client::Installation {
                app_name: request.app_name.clone(),
                namespace: request.namespace.clone(),
                catalog_item_id: request.catalog_item_id.clone(),
                account_id: ACCOUNT.into(),
                directory: request
                    .directory
                    .join("epic-hash-Mac")
                    .to_string_lossy()
                    .into_owned(),
                platform: request.platform.clone(),
                total_bytes: 100,
                build_version: "1".into(),
                launch_executable: "Game.app".into(),
                launch_command: String::new(),
                deployment_id: None,
                prerequisite: None,
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        None
    }
    fn cancel(&self) {
        self.call("cancel");
    }
    fn archive(&self, _: Configuration) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            self.call("archive");
            Ok(())
        })
    }
    fn maintain<'a>(
        self: Arc<Self>,
        config: Configuration,
        _: Arc<Control>,
        _: &'a IdleGuard,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let launch_could_start =
                crate::epic::Client::idle_guard(&config.session_directory, &config.helper).is_ok();
            self.maintained
                .lock()
                .unwrap()
                .push((config, launch_could_start));
            match self.maintenance.lock().unwrap().take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    locations: Locations,
    fake: Arc<Fake>,
    content: Content<Fake>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().canonicalize().unwrap();
        let locations = Locations::new(
            &library,
            Path::new("/Applications/SailOver.app"),
            &library.join("home"),
        );
        std::fs::create_dir_all(&locations.cloud_archive).unwrap();
        let fake = Arc::new(Fake::default());
        let saves = fake.clone();
        let content = Content::new(
            fake.clone(),
            Arc::new(move || saves.call("saved")),
            ACCOUNT.into(),
            "fr".into(),
            locations.clone(),
        );
        Self {
            _root: root,
            locations,
            fake,
            content,
        }
    }
    /// The build of `platform`, installed.
    fn installed(&self, platform: &str) -> (Receipt, Download) {
        let games = self.locations.library.join("Games/Epic");
        let directory = games.join(format!("epic-hash-{platform}"));
        std::fs::create_dir_all(&directory).unwrap();
        let receipt = Receipt {
            namespace: "sandbox".into(),
            app_name: "game".into(),
            name: "Fixture".into(),
            directory: directory.to_string_lossy().into_owned(),
            platform: platform.into(),
            total_bytes: 100,
            needs_verification: None,
        };
        (receipt, self.request(platform))
    }
    fn request(&self, platform: &str) -> Download {
        Download {
            namespace: "sandbox".into(),
            catalog_item_id: "catalog".into(),
            app_name: "game".into(),
            title: "Fixture".into(),
            platform: platform.into(),
            directory: self.locations.library.join("Games/Epic"),
        }
    }
}

fn game() -> Store {
    Store {
        namespace: "sandbox".into(),
        app_name: "game".into(),
    }
}

fn control() -> Arc<Control> {
    Arc::new(Control::new(|_| {}))
}

async fn eventually(predicate: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(std::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn every_account_call_renews_and_saves_the_credentials_first() {
    let fixture = Fixture::new();
    let request = fixture.request("Mac");

    let receipt = fixture.content.download(&request).await.unwrap();

    assert_eq!(receipt.namespace, "sandbox");
    assert_eq!(receipt.name, "Fixture");
    assert_eq!(receipt.needs_verification, Some(false));
    // The Transfer's own rotation is saved after it, without holding it.
    eventually(|| fixture.fake.calls().len() == 4).await;
    assert_eq!(
        fixture.fake.calls(),
        ["prepare", "saved", "download", "saved"]
    );

    let (receipt, request) = fixture.installed("Mac");
    fixture.content.archive(&receipt, &request).await.unwrap();
    eventually(|| fixture.fake.calls().len() == 8).await;
    assert_eq!(
        fixture.fake.calls()[4..],
        ["prepare", "saved", "archive", "saved"]
    );
}

#[tokio::test]
async fn a_transfer_cancelled_while_its_credentials_renew_never_starts() {
    let fixture = Fixture::new();
    let request = fixture.request("Mac");
    *fixture.fake.held.lock().unwrap() = true;

    let (downloaded, ()) = tokio::join!(fixture.content.download(&request), async {
        eventually(|| fixture.fake.calls().contains(&"prepare".to_string())).await;
        fixture.content.cancel();
        fixture.fake.released.notify_one();
    });

    assert_eq!(downloaded, Err(Failure::Cancelled));
    assert!(!fixture.fake.calls().contains(&"download".to_string()));
}

#[tokio::test]
async fn a_windows_build_keeps_its_saves_in_its_wine_prefix_and_a_mac_build_has_none() {
    let fixture = Fixture::new();
    for platform in ["Windows", "Mac"] {
        let (receipt, request) = fixture.installed(platform);
        fixture
            .content
            .maintain(&game(), Some((&receipt, &request)), control())
            .await
            .unwrap();
    }

    let maintained = fixture.fake.maintained.lock().unwrap().clone();
    let (windows, mac) = (&maintained[0].0, &maintained[1].0);
    assert_eq!(
        windows.prefix,
        Some(fixture.locations.wine_prefixes.join("epic-hash-Windows"))
    );
    assert_eq!(windows.catalog_item_id, "catalog");
    assert_eq!(windows.language, "fr");
    assert_eq!(windows.root, fixture.locations.cloud_archive);
    assert_eq!(windows.session_directory, fixture.locations.epic_sessions);
    assert_eq!(mac.prefix, None);
    assert!(
        maintained
            .iter()
            .all(|(_, launch_could_start)| !launch_could_start),
        "maintenance holds Epic's idle guard"
    );
}

#[tokio::test]
async fn an_uninstalled_game_without_a_saved_context_cannot_sync_and_a_cancelled_sync_ends_silently()
 {
    let fixture = Fixture::new();

    assert_eq!(
        fixture.content.maintain(&game(), None, control()).await,
        Err(Failure::Failed("backend.epic.cloud.failed".into()))
    );
    assert!(fixture.fake.maintained.lock().unwrap().is_empty());

    let (receipt, request) = fixture.installed("Mac");
    *fixture.fake.maintenance.lock().unwrap() = Some(Error::Cancelled);
    assert_eq!(
        fixture
            .content
            .maintain(&game(), Some((&receipt, &request)), control())
            .await,
        Err(Failure::Cancelled)
    );
}

#[tokio::test]
async fn a_launch_code_that_another_account_signed_is_refused_before_the_game_starts() {
    let fixture = Fixture::new();
    let (receipt, request) = fixture.installed("Mac");
    fixture
        .fake
        .accounts
        .lock()
        .unwrap()
        .extend([ACCOUNT.to_string(), STRANGER.to_string()]);
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)));
    eventually(|| !sessions.state().recovering).await;
    let admission = sessions.admit(game(), ACCOUNT.into()).unwrap();

    let launched = fixture.content.launch(&receipt, &request, admission).await;

    assert_eq!(
        launched,
        Err(Failure::Failed("backend.epic.unauthorized".into()))
    );
    assert_eq!(
        fixture.fake.calls()[..3],
        ["account", "exchange", "account"]
    );
    assert!(
        sessions.state().active.is_none(),
        "a refused launch gives its Reservation back"
    );
}

#[test]
fn an_uninstalled_windows_build_loses_its_prefix_and_a_mac_build_has_none() {
    let fixture = Fixture::new();
    let (windows, request) = fixture.installed("Windows");
    let (mac, _) = fixture.installed("Mac");
    let prefix = fixture.locations.wine_prefixes.join("epic-hash-Windows");
    std::fs::create_dir_all(prefix.join("drive_c")).unwrap();

    assert_eq!(fixture.content.remove_prefix(&windows), Some(Ok(())));
    assert!(!prefix.exists());
    assert_eq!(fixture.content.remove_prefix(&mac), None);
    assert_eq!(
        fixture.content.directory_name(&windows, &request),
        epic_kit::client::installation_directory_name("sandbox", "catalog", "game", "Windows")
    );
}
