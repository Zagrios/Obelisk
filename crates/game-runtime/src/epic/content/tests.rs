//! Epic's part of the installations through their Content interface, with a
//! network the test stands in for and real files in a temporary library.
use super::*;
use crate::{
    installations::{Content as _, tests::eventually},
    runtimes::{
        Runtime, Runtimes,
        tests::{FakeRuntimes, fake_offer, ready, recorded},
    },
    sessions::Sessions,
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
    overlay_failure: Mutex<bool>,
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
    fn install_social_overlay<'a>(&'a self, _: &'a Path) -> BoxFuture<'a, Result<(), Failure>> {
        Box::pin(async move {
            self.call("overlay");
            if *self.overlay_failure.lock().unwrap() {
                Err(Failure::Failed("overlay fixture failure".into()))
            } else {
                Ok(())
            }
        })
    }
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

#[tokio::test]
async fn fall_guys_prepares_its_linux_overlay_before_minting_a_game_login_code() {
    let fixture = Fixture::new();
    crate::wine::tests::engine_runtime(&fixture.locations);
    let mut receipt = fixture.installed(Platform::Windows);
    receipt.product.namespace = "50118b7f954e450f8823df1614b24e80".into();
    receipt.product.app_name = "0a2d9f6403244d12969e11da6713137b".into();
    let install = Path::new(&receipt.directory);
    std::fs::write(
        install.join("start_protected_game.exe"),
        crate::wine::tests::pe(),
    )
    .unwrap();
    let installed = epic_kit::client::Installation {
        app_name: receipt.product.app_name.clone(),
        namespace: receipt.product.namespace.clone(),
        catalog_item_id: receipt.product.catalog_item_id.clone(),
        account_id: ACCOUNT.into(),
        directory: receipt.directory.clone(),
        platform: Platform::Windows,
        total_bytes: 1,
        build_version: "1".into(),
        launch_executable: "start_protected_game.exe".into(),
        launch_command: String::new(),
        deployment_id: None,
        prerequisite: None,
    };
    std::fs::write(
        install.join(obelisk_core::install::RECEIPT_FILE),
        serde_json::to_vec(&installed).unwrap(),
    )
    .unwrap();
    let windows = crate::wine::tests::windows_of(
        &fixture.locations,
        <Epic as installations::Storefront>::profile_game(&receipt.product),
        install,
    );
    std::fs::create_dir_all(windows.prefix().join("drive_c/windows/system32")).unwrap();
    std::fs::write(windows.prefix().join("system.reg"), "registry").unwrap();
    std::fs::write(
        windows.prefix().join("user.reg"),
        "WINE REGISTRY Version 2\n",
    )
    .unwrap();
    let handover = Handover {
        network: fixture.fake.clone(),
        configuration: fixture
            .content
            .configuration(&receipt, Some(windows.prefix().to_owned())),
        windows: Some(windows.clone()),
        locations: fixture.locations.clone(),
        saved: Arc::new(|| {}),
    };
    let (_, _, wine) = sessions::Handover::prepare(&handover).await.unwrap();
    let wine = wine.unwrap();
    assert_eq!(wine.settings.engine, crate::wine::Engine::Linux);
    assert_eq!(
        fixture.fake.calls(),
        ["overlay", "account", "exchange", "account"]
    );
    assert!(
        std::fs::read_to_string(windows.prefix().join("user.reg"))
            .unwrap()
            .contains("C:/obelisk/EOSOverlay")
    );
    fixture.fake.calls.lock().unwrap().clear();
    *fixture.fake.overlay_failure.lock().unwrap() = true;
    assert!(sessions::Handover::prepare(&handover).await.is_err());
    assert_eq!(
        fixture.fake.calls(),
        ["overlay"],
        "A failed setup never spends a game login code"
    );
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
            Path::new("/Applications/Obelisk.app"),
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
    fn installed(&self, platform: Platform) -> Receipt<Epic> {
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

    let receipt = fixture.installed(Platform::Mac);
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
        let receipt = fixture.installed(platform);
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
async fn an_uninstalled_game_without_a_saved_context_cannot_sync_and_a_cancelled_sync_ends_silently()
 {
    let fixture = Fixture::new();

    assert_eq!(
        fixture.content.maintain(&game(), None, control()).await,
        Err(Failure::Failed("backend.epic.cloud.failed".into()))
    );
    assert!(fixture.fake.maintained.lock().unwrap().is_empty());

    let receipt = fixture.installed(Platform::Mac);
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
    let receipt = fixture.installed(Platform::Mac);
    std::fs::create_dir_all(fixture.locations.home.join("Saves")).unwrap();
    // An earlier Obelisk, whose sessions and helper lived elsewhere, saved
    // the record of the game it installed.
    let prefix = receipt.wine_prefix(&fixture.locations);
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
    let receipt = fixture.installed(Platform::Mac);
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

#[tokio::test]
async fn a_windows_launch_requires_the_voile_release_its_profile_names() {
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
        ..fixture.installed(Platform::Windows)
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

#[tokio::test]
async fn a_windows_launch_gets_voile_before_it_is_prepared_and_a_mac_launch_gets_none() {
    let fixture = Fixture::new();
    let (events, record) = recorded();
    let runtimes = FakeRuntimes::new(&[], record);
    runtimes.hold();
    let sessions = Sessions::<Epic>::new(
        Box::new(|| Ok(None)),
        Runtimes::from_source(runtimes.clone()),
    );
    eventually(|| !sessions.state().recovering).await;
    let admission = sessions.admit(game(), ACCOUNT.into()).unwrap();
    let canceller = admission.canceller();
    let windows = fixture.installed(Platform::Windows);

    let (launched, ()) = tokio::join!(fixture.content.launch(&windows, admission, None), async {
        eventually(|| !events.lock().unwrap().is_empty()).await;
        canceller.cancel();
    });

    assert_eq!(launched, Err(Failure::Cancelled), "nothing is prepared");
    assert_eq!(*events.lock().unwrap(), ["voile"]);

    // Downloads no longer wait: one the Mac build got would show.
    runtimes.release();
    // A launch code of another account stops the Mac launch once prepared.
    fixture
        .fake
        .accounts
        .lock()
        .unwrap()
        .extend([ACCOUNT.to_string(), STRANGER.to_string()]);
    let admission = sessions.admit(game(), ACCOUNT.into()).unwrap();
    let launched = fixture
        .content
        .launch(&fixture.installed(Platform::Mac), admission, None)
        .await;
    assert_eq!(
        launched,
        Err(Failure::Failed("backend.epic.unauthorized".into()))
    );
    assert_eq!(*events.lock().unwrap(), ["voile"], "a Mac build needs none");
}

#[test]
fn a_build_installs_into_the_folder_epic_names_for_it() {
    let fixture = Fixture::new();
    let windows = fixture.installed(Platform::Windows);

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
    let mut receipt = fixture.installed(Platform::Mac);
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
