//! Epic's part of the installations through their Content interface, with a
//! network the test stands in for and real files in a temporary library.
use super::*;
use crate::{
    installations::{Content as _, tests::eventually},
    launch_shell::{
        SignIn,
        tests::{Library, Opened, Setup, folders, ready_sessions},
    },
    profile::Game,
    runtimes::{
        Runtime, Runtimes,
        tests::{FakeRuntimes, fake_offer, ready},
    },
    sessions::Sessions,
    supervisor::{self, session as lifecycle},
};
use std::{collections::VecDeque, sync::Mutex};
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
    /// Each configuration an archive was taken with.
    archived: Mutex<Vec<Configuration>>,
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
            Ok("fixture-exchange-code".into())
        })
    }
    fn platform<'a>(&'a self, _: &'a Product) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            self.call("platform");
            Ok(Platform::Windows)
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Epic>,
    ) -> BoxFuture<'a, Result<epic_kit::client::Installation, Failure>> {
        Box::pin(async move {
            self.call("download");
            Ok(epic_kit::client::Installation {
                app_name: transfer.product.app_name.clone(),
                namespace: transfer.product.namespace.clone(),
                catalog_item_id: transfer.product.catalog_item_id.clone(),
                account_id: ACCOUNT.into(),
                directory: transfer
                    .directory
                    .join("epic-hash-Mac")
                    .to_string_lossy()
                    .into_owned(),
                platform: transfer.platform.expect("a settled platform"),
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
    fn update_available<'a>(
        &'a self,
        _: &'a Receipt<Epic>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move {
            self.call("update");
            Ok(true)
        })
    }
    fn archive(&self, config: Configuration) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(async move {
            self.call("archive");
            self.archived.lock().unwrap().push(config);
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

pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    locations: Locations,
    fake: Arc<Fake>,
    content: Content<Fake>,
}

impl Fixture {
    /// A library with Obelisk's bundle outside it, or inside it when `bundled`.
    fn in_library(bundled: bool) -> Self {
        let (root, locations) = folders(bundled);
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
    fn transfer(&self, platform: Platform) -> Transfer<Epic> {
        Transfer {
            product: product(),
            name: "Fixture".into(),
            platform: Some(platform),
            directory: self.locations.library.join("Games/Epic"),
        }
    }
}

/// The game's catalog item.
fn product() -> Product {
    Product {
        namespace: "sandbox".into(),
        catalog_item_id: "catalog".into(),
        app_name: "game".into(),
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

#[tokio::test]
async fn every_account_call_renews_and_saves_the_credentials_first() {
    let fixture = Fixture::new();
    let transfer = fixture.transfer(Platform::Mac);

    assert_eq!(
        fixture.content.begin(&transfer).await.unwrap(),
        Platform::Mac
    );
    let receipt = fixture.content.download(&transfer).await.unwrap();

    assert_eq!(receipt.product, product());
    assert_eq!(receipt.name, "Fixture");
    assert!(!receipt.needs_verification);
    // The Transfer's own rotation is saved after it, without holding it.
    eventually(|| fixture.fake.calls().len() == 4).await;
    assert_eq!(
        fixture.fake.calls(),
        ["prepare", "saved", "download", "saved"]
    );

    let receipt = fixture.receipt(Platform::Mac);
    fixture.content.archive(&receipt).await.unwrap();
    eventually(|| fixture.fake.calls().len() == 8).await;
    assert_eq!(
        fixture.fake.calls()[4..],
        ["prepare", "saved", "archive", "saved"]
    );

    // A short call renews the token itself, then its rotation is saved.
    assert!(fixture.content.update_available(&receipt).await.unwrap());
    eventually(|| fixture.fake.calls().len() == 10).await;
    assert_eq!(fixture.fake.calls()[8..], ["update", "saved"]);
}

#[tokio::test]
async fn a_transfer_without_a_platform_installs_the_build_epic_picks() {
    let fixture = Fixture::new();
    let transfer = Transfer {
        platform: None,
        ..fixture.transfer(Platform::Mac)
    };

    assert_eq!(
        fixture.content.begin(&transfer).await.unwrap(),
        Platform::Windows
    );
    // Picking the build may renew the credentials: they are saved after it.
    eventually(|| fixture.fake.calls().len() == 4).await;
    assert_eq!(
        fixture.fake.calls(),
        ["prepare", "saved", "platform", "saved"]
    );
    // A Transfer that names its build installs that one.
    let fixture = Fixture::new();
    assert_eq!(
        fixture
            .content
            .begin(&fixture.transfer(Platform::Mac))
            .await
            .unwrap(),
        Platform::Mac
    );
    assert!(!fixture.fake.calls().contains(&"platform".to_string()));
}

#[tokio::test]
async fn a_transfer_cancelled_while_its_credentials_renew_never_begins() {
    let fixture = Fixture::new();
    let transfer = fixture.transfer(Platform::Mac);
    *fixture.fake.held.lock().unwrap() = true;

    let (begun, ()) = tokio::join!(fixture.content.begin(&transfer), async {
        eventually(|| fixture.fake.calls().contains(&"prepare".to_string())).await;
        fixture.content.cancel();
        fixture.fake.released.notify_one();
    });

    assert_eq!(begun, Err(Failure::Cancelled));
}

#[tokio::test]
async fn a_windows_build_keeps_its_saves_in_its_wine_prefix_and_a_mac_build_has_none() {
    let fixture = Fixture::new();
    for platform in [Platform::Windows, Platform::Mac] {
        let receipt = fixture.receipt(platform);
        fixture
            .content
            .maintain(&game(), Some(&receipt), control())
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
async fn a_windows_build_s_cloud_archive_is_taken_in_its_wine_prefix_and_a_mac_build_s_has_none() {
    let fixture = Fixture::new();
    for platform in [Platform::Windows, Platform::Mac] {
        fixture
            .content
            .archive(&fixture.receipt(platform))
            .await
            .unwrap();
    }

    let archived = fixture.fake.archived.lock().unwrap().clone();
    assert_eq!(
        archived
            .iter()
            .map(|archive| archive.prefix.clone())
            .collect::<Vec<_>>(),
        [
            Some(fixture.locations.wine_prefixes.join("epic-hash-Windows")),
            None
        ]
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

    let receipt = fixture.receipt(Platform::Mac);
    *fixture.fake.maintenance.lock().unwrap() = Some(Error::Cancelled);
    assert_eq!(
        fixture
            .content
            .maintain(&game(), Some(&receipt), control())
            .await,
        Err(Failure::Cancelled)
    );
}

#[tokio::test]
async fn an_uninstalled_game_syncs_the_configuration_its_record_names_with_this_obelisks_paths() {
    use crate::cloud::record::{self, Record as _};
    let fixture = Fixture::new();
    let receipt = fixture.receipt(Platform::Mac);
    std::fs::create_dir_all(fixture.locations.home.join("Saves")).unwrap();
    // An earlier Obelisk, whose sessions and helper lived elsewhere, saved
    // the record of the game it installed.
    let prefix = receipt.wine_prefix(&fixture.locations).await.unwrap();
    let mut configuration = fixture.content.configuration(&receipt, prefix);
    configuration.session_directory = fixture.locations.library.join("Older/Sessions");
    configuration.helper = fixture.locations.library.join("Older/helper");
    let (vault, _) = record::locked::<Context>(&configuration, ACCOUNT).unwrap();
    Context {
        config: configuration.clone(),
        rules: epic_kit::cloud::CloudRules {
            cloud_save_folder: None,
            cloud_save_folder_mac: Some("{Home}/Saves".into()),
            include: vec!["*.sav".into()],
            exclude: Vec::new(),
        },
        account_id: ACCOUNT.into(),
        pending: None,
        installation_removed: true,
    }
    .save()
    .unwrap();
    drop(vault);
    std::fs::remove_dir_all(&receipt.directory).unwrap();

    fixture
        .content
        .maintain(&game(), None, control())
        .await
        .unwrap();

    let maintained = fixture.fake.maintained.lock().unwrap()[0].0.clone();
    assert_eq!(maintained.install, configuration.install);
    assert_eq!(maintained.title, "Fixture");
    assert_eq!(maintained.root, fixture.locations.cloud_archive);
    assert_eq!(
        maintained.session_directory,
        fixture.locations.epic_sessions
    );
    assert_eq!(maintained.helper, fixture.locations.helper);
}

#[tokio::test]
async fn a_launch_code_that_another_account_signed_is_refused_before_the_game_starts() {
    let fixture = Fixture::new();
    let receipt = fixture.receipt(Platform::Mac);
    fixture
        .fake
        .accounts
        .lock()
        .unwrap()
        .extend([ACCOUNT.to_string(), STRANGER.to_string()]);
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    let admission = sessions.admit(game(), ACCOUNT.into()).unwrap();

    let launched = fixture.content.launch(&receipt, admission, None).await;

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

/// The wait itself is the launch shell's; this one finds the profile by the
/// game's Epic ids.
#[tokio::test]
async fn an_epic_game_whose_profile_names_a_voile_release_makes_its_windows_launch_wait_for_it() {
    let fixture = Fixture::new();
    let runtimes = FakeRuntimes::new(&[], |_| {});
    runtimes.outdate(Runtime::Voile);
    let sessions = Sessions::<Epic>::new(Box::new(|| Ok(None)), Runtimes::from_source(runtimes));
    eventually(|| !sessions.state().recovering).await;
    // Dead by Daylight, from Epic.
    let (namespace, app_name) = ("611482b8586142cda48a0786eb8a127c", "Brill");
    let dead_by_daylight = Receipt {
        product: Product {
            namespace: namespace.into(),
            catalog_item_id: "catalog".into(),
            app_name: app_name.into(),
        },
        ..fixture.receipt(Platform::Windows)
    };
    let shown = || {
        sessions
            .state()
            .active
            .and_then(|active| active.voile_update)
    };
    let game = Store {
        namespace: namespace.into(),
        app_name: app_name.into(),
    };

    let admission = sessions.admit(game, ACCOUNT.into()).unwrap();
    let canceller = admission.canceller();
    let (launched, ()) = tokio::join!(
        fixture.content.launch(&dead_by_daylight, admission, None),
        async {
            eventually(|| shown().is_some()).await;
            assert_eq!(shown(), Some(fake_offer(true)));
            canceller.cancel();
        }
    );

    assert_eq!(launched, Err(Failure::Cancelled), "nothing is prepared");
}

#[test]
fn a_build_installs_into_the_folder_epic_names_for_it() {
    let fixture = Fixture::new();
    let windows = fixture.receipt(Platform::Windows);

    assert_eq!(
        fixture.content.directory_name(&windows),
        epic_kit::client::installation_directory_name(
            "sandbox",
            "catalog",
            "game",
            Platform::Windows
        )
    );
}

#[tokio::test]
async fn an_older_receipt_learns_its_catalog_item_from_the_installation_itself() {
    let fixture = Fixture::new();
    let mut receipt = fixture.receipt(Platform::Mac);
    let installed = epic_kit::client::Installation {
        app_name: "game".into(),
        namespace: "sandbox".into(),
        catalog_item_id: "catalog".into(),
        account_id: ACCOUNT.into(),
        directory: receipt.directory.clone(),
        platform: Platform::Mac,
        total_bytes: 100,
        build_version: "1".into(),
        launch_executable: "Game.app".into(),
        launch_command: String::new(),
        deployment_id: None,
        prerequisite: None,
    };
    std::fs::write(
        Path::new(&receipt.directory).join(".obelisk-install.json"),
        serde_json::to_vec(&installed).unwrap(),
    )
    .unwrap();
    receipt.product.catalog_item_id.clear();
    let file = fixture.locations.library.join("epic-installations.json");
    // An older receipt file has no catalog item at all.
    let mut older = serde_json::to_value([&receipt]).unwrap();
    older[0].as_object_mut().unwrap().remove("catalogItemId");
    std::fs::write(&file, serde_json::to_vec(&older).unwrap()).unwrap();
    let sessions = Arc::new(Sessions::<Epic>::new(Box::new(|| Ok(None)), ready()));

    let installations = crate::installations::Installations::new(
        Arc::new(Content::new(
            fixture.fake.clone(),
            Arc::new(|| {}),
            ACCOUNT.into(),
            "fr".into(),
            fixture.locations.clone(),
        )),
        sessions,
        ACCOUNT.to_string(),
        file.clone(),
        fixture.locations.home.clone(),
        crate::games_folder::GamesFolder::new(&fixture.locations.library),
        crate::installations::Queueing {
            queue: crate::queue::DownloadQueue::new(),
            account: ACCOUNT.into(),
            name: |game: &Store| game.app_name.clone(),
        },
    );

    assert_eq!(
        installations.state().receipts[0].product.catalog_item_id,
        "catalog"
    );
    let saved: Vec<Receipt<Epic>> = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        saved[0].product.catalog_item_id, "catalog",
        "the file keeps what it learnt"
    );
}

/// What the network was asked for the launch, without the saves of the
/// credentials a launch ends with.
fn asked(fake: &Fake) -> Vec<String> {
    fake.calls()
        .into_iter()
        .filter(|call| call != "saved")
        .collect()
}

#[tokio::test]
async fn a_launch_saves_the_credentials_its_exchange_code_rotated_however_it_ended() {
    let fixture = Fixture::bundled();
    let receipt = fixture.receipt(Platform::Mac);
    let sessions = ready_sessions::<Fixture>().await;
    let admission = sessions.admit(game(), ACCOUNT.into()).unwrap();

    let launched = fixture.content.launch(&receipt, admission, None).await;

    // The recording supervisor never acknowledges: the launch ends with the
    // key a lost start shows.
    assert_eq!(
        launched,
        Err(Failure::Failed("backend.epic.cloud.session".into()))
    );
    assert_eq!(asked(&fixture.fake), ["account", "exchange", "account"]);
    eventually(|| fixture.fake.calls().len() == 4).await;
    assert_eq!(fixture.fake.calls()[3], "saved");
}

#[test]
fn an_epic_launch_saves_the_credentials_its_exchange_rotated_before_its_game_begins() {
    let fixture = Fixture::new();

    fixture.signing().before_begin()();

    assert_eq!(fixture.fake.calls(), ["saved"]);
}

mod builds {
    crate::launch_shell::tests::setup_suite!(super::Fixture);
}

mod launches {
    crate::launch_shell::tests::library_suite!(super::Fixture);
}

impl Setup for Fixture {
    type G = Epic;
    const SUPERVISE: &'static str = "epic-supervise";
    const UNSUPPORTED: &'static str = "backend.epic.cloud.unsupported";
    const SESSION: &'static str = "backend.epic.cloud.session";
    const CANCELLED: &'static str = "backend.epic.cloud.cancelled";
    const BAD_ACCOUNT: &'static str = "backend.epic.cloud.storage";
    const WINDOWS: &'static str = "Windows";
    const TITLE: &'static str = "Rocket League";
    const GAME: Game<'static> = Game::Epic {
        namespace: "sandbox",
        app_name: "game",
    };
    const LAUNCH_KEYS: [(crate::launch::Error, &'static str); 7] = {
        use crate::launch::Error as Launch;
        [
            (
                Launch::IncompatibleArchitecture,
                "backend.runtime.macBuildNotNative",
            ),
            (Launch::WineUnavailable, "backend.runtime.wineUnavailable"),
            (Launch::InvalidApplication, "backend.epic.cloud.unsupported"),
            (Launch::NotExecutable, "backend.epic.cloud.unsupported"),
            (Launch::Storage, "backend.epic.cloud.session"),
            (Launch::StartFailed, "backend.epic.cloud.session"),
            (Launch::Unavailable, "backend.epic.cloud.session"),
        ]
    };
    const SUPERVISOR_KEYS: [(supervisor::Error, &'static str); 8] = {
        use supervisor::Error as Core;
        [
            (Core::Storage, "backend.epic.cloud.storage"),
            (Core::NewerSession, "backend.epic.cloud.busy"),
            (Core::Busy, "backend.epic.cloud.busy"),
            (Core::Cancelled, "backend.epic.cloud.cancelled"),
            (
                Core::RosettaUnavailable,
                "backend.runtime.rosettaUnavailable",
            ),
            (Core::InvalidSession, "backend.epic.cloud.session"),
            (Core::Unavailable, "backend.epic.cloud.session"),
            (Core::StartFailed, "backend.epic.cloud.session"),
        ]
    };

    fn account() -> String {
        ACCOUNT.into()
    }
    fn credential() -> ExchangeCode {
        ExchangeCode {
            exchange_code: "fixture-exchange-code".into(),
        }
    }
    fn credential_json() -> serde_json::Value {
        serde_json::json!({"exchange_code": "fixture-exchange-code"})
    }
    fn configuration_in(root: &Path, platform: Platform, prefix: Option<PathBuf>) -> Configuration {
        Configuration {
            root: root.join("library/SteamCloud"),
            home: root.join("home"),
            install: root.join("library/Games/Epic").join(format!(
                "epic-hash-{}",
                epic_kit::client::PLATFORM.name(platform)
            )),
            session_directory: root.join("library/EpicSessions"),
            helper: root.join("Obelisk.app/Contents/Helpers/SteamNative/obelisk-steam-host"),
            namespace: "sandbox".into(),
            catalog_item_id: "catalog".into(),
            app_name: "game".into(),
            platform,
            language: "en".into(),
            title: "Rocket League".into(),
            prefix,
        }
    }
    fn wire(configuration: &Configuration) -> serde_json::Value {
        serde_json::json!({
            "root": configuration.root,
            "home": configuration.home,
            "install": configuration.install,
            "session_directory": configuration.session_directory,
            "helper": configuration.helper,
            "namespace": "sandbox",
            "catalog_item_id": "catalog",
            "app_name": "game",
            "platform": match configuration.platform {
                Platform::Mac => "Mac",
                Platform::Windows => "Windows",
            },
            "language": "en",
            "title": "Rocket League",
            "prefix": configuration.prefix,
        })
    }
    fn paths(configuration: &mut Configuration) -> [&mut PathBuf; 4] {
        [
            &mut configuration.home,
            &mut configuration.install,
            &mut configuration.session_directory,
            &mut configuration.helper,
        ]
    }
    fn refused_by_the_store(root: &Path) -> Vec<(Configuration, String, &'static str)> {
        // An account Epic would not name is refused as a save scope is.
        vec![(
            Self::configuration_in(root, Platform::Mac, None),
            "not-an-account".into(),
            Self::BAD_ACCOUNT,
        )]
    }
    fn handoff() -> serde_json::Value {
        serde_json::json!({
            "config": {
                "root": "/library/SteamCloud",
                "home": "/Users/player",
                "install": "/library/Games/Epic/epic-hash-Mac",
                "session_directory": "/library/EpicSessions",
                "helper": "/Obelisk.app/Contents/Helpers/SteamNative/obelisk-steam-host",
                "namespace": "sandbox",
                "catalog_item_id": "catalog",
                "app_name": "game",
                "platform": "Mac",
                "language": "en",
                "title": "Rocket League",
                "prefix": null,
            },
            "id": "6f1c1a0e-8a0f-4a3e-9a7b-0d2f6f3a8b11",
            "account_id": ACCOUNT,
            "exchange_code": "fixture-exchange-code",
            "environment": {"HOME": "/Users/player"},
            "wine": null,
        })
    }
    fn session_root() -> &'static str {
        "/library/EpicSessions"
    }
    fn store() -> Store {
        game()
    }
    fn open(input: &mut dyn std::io::Read) -> crate::epic::Result<Opened<Epic>> {
        let (opening, launch) = <crate::epic::launch::Launch as lifecycle::Launch>::open(input)?;
        Ok((opening, lifecycle::Launch::wine(&launch)))
    }
}

impl Library for Fixture {
    const PREREQUISITE_ARGUMENTS: &'static str = "/quiet /norestart";
    const INCOMPLETE: &'static str = "backend.epic.launch.incompleteInstallation";
    const CREDENTIAL_CALLS: &'static [&'static str] = &["account", "exchange", "account"];

    fn new() -> Self {
        Self::in_library(false)
    }
    fn bundled() -> Self {
        Self::in_library(true)
    }
    fn locations(&self) -> &Locations {
        &self.locations
    }
    /// The build of `platform`, installed.
    fn receipt(&self, platform: Platform) -> Receipt<Epic> {
        let directory = self.locations.library.join(format!(
            "Games/Epic/epic-hash-{}",
            epic_kit::client::PLATFORM.name(platform)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        Receipt {
            product: product(),
            name: "Fixture".into(),
            directory: directory.to_string_lossy().into_owned(),
            platform,
            total_bytes: 100,
            needs_verification: false,
        }
    }

    fn nameless(&self) -> Receipt<Epic> {
        // A folder with no name of its own, such as `..`.
        let nameless = self.locations.library.join("Games/Epic/..");
        Receipt {
            directory: nameless.to_string_lossy().into_owned(),
            ..self.receipt(Platform::Windows)
        }
    }
    fn game() -> Store {
        game()
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt<Epic>,
        admission: Admission<Epic>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        self.content.launch(receipt, admission, None)
    }
    fn configuration(&self, receipt: &Receipt<Epic>, prefix: Option<PathBuf>) -> Configuration {
        self.content.configuration(receipt, prefix)
    }
    fn signing(&self) -> impl SignIn<Epic> {
        Signing {
            network: self.fake.clone(),
            saved: self.content.credentials.saved.clone(),
        }
    }
    fn wire_config(&self, receipt: &Receipt<Epic>, prefix: Option<&Path>) -> serde_json::Value {
        let locations = &self.locations;
        serde_json::json!({
            "root": locations.cloud_archive,
            "home": locations.home,
            "install": receipt.directory,
            "session_directory": locations.epic_sessions,
            "helper": locations.helper,
            "namespace": "sandbox",
            "catalog_item_id": "catalog",
            "app_name": "game",
            "platform": match receipt.platform {
                Platform::Mac => "Mac",
                Platform::Windows => "Windows",
            },
            "language": "fr",
            "title": "Fixture",
            "prefix": prefix,
        })
    }
    fn prefix(&self) -> PathBuf {
        self.locations.wine_prefixes.join("epic-hash-Windows")
    }
    /// What a completed Windows installation holds: the program its receipt
    /// launches, `program`, the receipt, and the prerequisite it names.
    /// Returns the prerequisite's program.
    fn complete_windows_build(&self, program: &[u8]) -> PathBuf {
        let install = self.locations.library.join("Games/Epic/epic-hash-Windows");
        std::fs::create_dir_all(install.join("Binaries")).unwrap();
        std::fs::create_dir_all(install.join("Redist")).unwrap();
        std::fs::write(install.join("Binaries/Game.exe"), program).unwrap();
        std::fs::write(install.join("Redist/setup.exe"), "").unwrap();
        let receipt = epic_kit::client::Installation {
            app_name: "game".into(),
            namespace: "sandbox".into(),
            catalog_item_id: "catalog".into(),
            account_id: ACCOUNT.into(),
            directory: install.to_string_lossy().into_owned(),
            platform: Platform::Windows,
            total_bytes: 1,
            build_version: "1.0".into(),
            launch_executable: "Binaries/Game.exe".into(),
            launch_command: String::new(),
            deployment_id: None,
            prerequisite: Some(epic_kit::client::Prerequisite {
                ids: vec!["dx".into()],
                name: "Prerequisites".into(),
                path: "Redist\\setup.exe".into(),
                arguments: "/quiet /norestart".into(),
            }),
        };
        std::fs::write(
            install.join(obelisk_core::install::RECEIPT_FILE),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        install.join("Redist/setup.exe")
    }
    fn asked(&self) -> Vec<String> {
        asked(&self.fake)
    }
    fn refuse_credential(&self) -> &'static str {
        // A launch code of another account.
        self.fake
            .accounts
            .lock()
            .unwrap()
            .extend([ACCOUNT.to_string(), STRANGER.to_string()]);
        "backend.epic.unauthorized"
    }
    fn saved(&self) -> bool {
        self.fake.calls().iter().any(|call| call == "saved")
    }
}
