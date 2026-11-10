//! Steam's part of the installations through their Content interface, with a
//! network the test stands in for and real files in a temporary library.
use super::*;
use crate::{
    installations::{Content as _, tests::eventually},
    runtimes::{
        Runtime, Runtimes,
        tests::{FakeRuntimes, fake_offer, recorded},
    },
    sessions::Sessions,
};
use std::{path::PathBuf, sync::Mutex};

/// The Cloud rules an archive finds.
type Rules = Result<Option<(CloudMetadata, CloudListing)>, steam_cloud::Failure>;

/// What the test's Steam network was asked, and what it answers.
#[derive(Default)]
struct Fake {
    /// The platform pick and the game's files, in the order the Transfer
    /// asked for them.
    calls: Mutex<Vec<String>>,
    /// The build Steam picks; the Mac one without it.
    picked: Mutex<Option<Platform>>,
    downloads: Mutex<Vec<Transfer<Steam>>>,
    archive: Mutex<Option<Rules>>,
}

impl Fake {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl Network for Fake {
    fn platform<'a>(&'a self, _: u32, _: &'a str) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("platform".into());
            let picked = *self.picked.lock().unwrap();
            Ok(picked.unwrap_or(Platform::Mac))
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Steam>,
    ) -> BoxFuture<'a, Result<steam_kit::client::Installation, Failure>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("game".into());
            self.downloads.lock().unwrap().push(transfer.clone());
            let platform = transfer.platform.unwrap_or(Platform::Mac);
            Ok(steam_kit::client::Installation {
                app_id: transfer.product.app_id,
                name: transfer.name.clone(),
                directory: transfer
                    .directory
                    .join(steam_kit::client::installation_directory_name(
                        transfer.product.app_id,
                        platform,
                    ))
                    .to_string_lossy()
                    .into_owned(),
                platform,
                language: transfer.product.language.clone(),
                total_bytes: 100,
                manifests: Vec::new(),
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        None
    }
    fn cancel(&self) {}
    fn update_available<'a>(
        &'a self,
        _: &'a Receipt<Steam>,
    ) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async { Ok(false) })
    }
    /// The game's own app, which no installation of the test holds.
    fn launch_target<'a>(
        &'a self,
        _: u32,
        _: Platform,
        install: &'a Path,
    ) -> BoxFuture<'a, Result<LaunchTarget, Failure>> {
        Box::pin(async move {
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
        let rules = self.archive.lock().unwrap().take().unwrap_or(Ok(None));
        Box::pin(async move { rules })
    }
    fn maintain(
        &self,
        _: steam_cloud::Config,
        _: &Credentials,
        _: Arc<Control>,
        _: &crate::steam::IdleGuard,
    ) -> Result<(), steam_cloud::Failure> {
        Ok(())
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
            Path::new("/Applications/Obelisk.app"),
            &library.join("home"),
        );
        let fake = Arc::new(Fake::default());
        let credentials = Credentials {
            steam_id: 42,
            account_name: "fixture".into(),
            refresh_token: "fixture-only-never-sent".into(),
        };
        Self {
            content: Content::new(fake.clone(), credentials, locations.clone()),
            _root: root,
            locations,
            fake,
        }
    }
    /// Portal 2, installed for `platform`.
    fn installed(&self, platform: Platform) -> Receipt<Steam> {
        let directory = self.locations.library.join("Games/Steam").join(
            steam_kit::client::installation_directory_name(620, platform),
        );
        std::fs::create_dir_all(&directory).unwrap();
        Receipt {
            product: portal(),
            name: "Portal 2".into(),
            directory: directory.to_string_lossy().into_owned(),
            platform,
            total_bytes: 100,
            needs_verification: false,
        }
    }
    /// Portal 2's Transfer for `platform`.
    fn transfer(&self, platform: Platform) -> Transfer<Steam> {
        Transfer {
            product: portal(),
            name: "Portal 2".into(),
            platform: Some(platform),
            directory: self.locations.library.join("Games/Steam"),
        }
    }
}

/// Portal 2, with its French depots.
fn portal() -> Product {
    Product {
        app_id: 620,
        language: "french".into(),
    }
}

#[tokio::test]
async fn a_completed_transfer_is_an_installation_that_needs_no_verification() {
    let fixture = Fixture::new();
    let transfer = fixture.transfer(Platform::Mac);

    let receipt = fixture.content.download(&transfer).await.unwrap();

    assert_eq!(
        *fixture.fake.downloads.lock().unwrap(),
        vec![transfer.clone()]
    );
    assert_eq!(receipt.product, portal());
    assert_eq!(receipt.platform, Platform::Mac);
    assert!(!receipt.needs_verification);
    assert_eq!(
        PathBuf::from(&receipt.directory),
        transfer.directory.join("steam-620-macos")
    );
}

#[tokio::test]
async fn a_windows_game_keeps_its_cloud_saves_in_its_wine_prefix_and_a_mac_game_has_none() {
    let fixture = Fixture::new();
    let (windows, mac) = (
        fixture.installed(Platform::Windows),
        fixture.installed(Platform::Mac),
    );

    let windows = saves::Storefront::configuration(&fixture.content, &windows)
        .await
        .unwrap();
    let mac = saves::Storefront::configuration(&fixture.content, &mac)
        .await
        .unwrap();

    assert_eq!(
        windows.prefix,
        Some(fixture.locations.wine_prefixes.join("steam-620-windows"))
    );
    assert_eq!(windows.root, fixture.locations.cloud_archive);
    assert_eq!(windows.home, fixture.locations.home);
    assert_eq!(windows.app_id, 620);
    assert_eq!(mac.prefix, None);
}

#[tokio::test]
async fn a_game_without_steam_cloud_archives_nothing_and_a_failure_is_its_cloud_key() {
    let fixture = Fixture::new();
    let receipt = fixture.installed(Platform::Mac);

    let cloud = fixture.content.cloud().unwrap();

    cloud.archive(&receipt).await.unwrap();

    *fixture.fake.archive.lock().unwrap() = Some(Err(steam_cloud::Failure::Network));
    assert_eq!(
        cloud.archive(&receipt).await,
        Err(Failure::Failed(steam_cloud::Failure::Network.key().into()))
    );
}

#[tokio::test]
async fn each_build_of_a_game_installs_into_a_folder_named_after_its_app_and_platform() {
    let fixture = Fixture::new();
    let (windows, mac) = (
        fixture.installed(Platform::Windows),
        fixture.installed(Platform::Mac),
    );

    assert_eq!(
        fixture.content.directory_name(&windows),
        "steam-620-windows"
    );
    assert_eq!(fixture.content.directory_name(&mac), "steam-620-macos");
}

#[tokio::test]
async fn a_transfer_without_a_platform_begins_with_the_build_steam_picks_and_one_with_a_platform_asks_nothing()
 {
    let fixture = Fixture::new();
    *fixture.fake.picked.lock().unwrap() = Some(Platform::Windows);
    let mut transfer = fixture.transfer(Platform::Windows);
    transfer.platform = None;

    assert_eq!(
        fixture.content.begin(&transfer).await.unwrap(),
        Platform::Windows
    );
    assert_eq!(fixture.fake.calls(), ["platform"]);

    let transfer = fixture.transfer(Platform::Mac);
    assert_eq!(
        fixture.content.begin(&transfer).await.unwrap(),
        Platform::Mac
    );
    assert_eq!(fixture.fake.calls(), ["platform"]);
}

#[tokio::test]
async fn a_launch_gets_steam_components_for_every_build_and_voile_for_a_windows_one() {
    let fixture = Fixture::new();
    let (events, record) = recorded();
    let runtimes = FakeRuntimes::new(&[], record);
    let sessions = Sessions::<Steam>::new(
        Box::new(|| Ok(None)),
        Runtimes::from_source(runtimes.clone()),
    );
    eventually(|| !sessions.state().recovering).await;

    // The Mac build has no Steam API library once prepared.
    let admission = sessions.admit(620, 42).unwrap();
    let launched = fixture
        .content
        .launch(&fixture.installed(Platform::Mac), admission, None)
        .await;
    assert_eq!(
        launched,
        Err(Failure::Failed(crate::steam::Error::ApiMissing.to_string()))
    );
    assert_eq!(*events.lock().unwrap(), ["steam components"]);

    runtimes.hold();
    let admission = sessions.admit(620, 42).unwrap();
    let canceller = admission.canceller();
    let windows = fixture.installed(Platform::Windows);
    let (launched, ()) = tokio::join!(fixture.content.launch(&windows, admission, None), async {
        eventually(|| events.lock().unwrap().len() == 2).await;
        canceller.cancel();
    });

    assert_eq!(launched, Err(Failure::Cancelled), "nothing is prepared");
    assert_eq!(*events.lock().unwrap(), ["steam components", "voile"]);
}

#[tokio::test]
async fn a_windows_launch_requires_the_voile_release_its_profile_names() {
    let fixture = Fixture::new();
    let runtimes = FakeRuntimes::new(&[Runtime::SteamComponents], |_| {});
    runtimes.outdate(Runtime::Voile);
    let sessions = Sessions::<Steam>::new(Box::new(|| Ok(None)), Runtimes::from_source(runtimes));
    eventually(|| !sessions.state().recovering).await;
    let rocket_league = Receipt {
        product: Product {
            app_id: 252950,
            language: "english".into(),
        },
        ..fixture.installed(Platform::Windows)
    };
    let shown = || {
        sessions
            .state()
            .active
            .and_then(|active| active.voile_update)
    };

    let admission = sessions.admit(252950, 42).unwrap();
    let canceller = admission.canceller();
    let (launched, ()) = tokio::join!(
        fixture.content.launch(&rocket_league, admission, None),
        async {
            eventually(|| shown().is_some()).await;
            assert_eq!(shown(), Some(fake_offer(true)));
            canceller.cancel();
        }
    );

    assert_eq!(launched, Err(Failure::Cancelled), "nothing is prepared");
}
