//! Steam's part of the installations through their Content interface, with a
//! network the test stands in for and real files in a temporary library.
use super::*;
use crate::installations::Content as _;
use std::{path::PathBuf, sync::Mutex};

/// The Cloud rules an archive finds.
type Rules = Result<Option<(CloudMetadata, CloudListing)>, steam_cloud::Failure>;

/// What the test's Steam network was asked, and what it answers.
#[derive(Default)]
struct Fake {
    downloads: Mutex<Vec<Download>>,
    /// Each maintenance's target, and whether a Steam launch could start
    /// meanwhile.
    maintained: Mutex<Vec<(steam_cloud::MaintenanceTarget, bool)>>,
    archive: Mutex<Option<Rules>>,
    locations: Mutex<Option<Locations>>,
}

impl Network for Fake {
    fn download<'a>(
        &'a self,
        request: &'a Download,
    ) -> BoxFuture<'a, Result<steam_kit::client::Installation, Failure>> {
        Box::pin(async move {
            self.downloads.lock().unwrap().push(request.clone());
            Ok(steam_kit::client::Installation {
                app_id: request.app_id,
                name: request.name.clone(),
                directory: request
                    .directory
                    .join(format!("steam-{}-{}", request.app_id, request.platform))
                    .to_string_lossy()
                    .into_owned(),
                platform: request.platform.clone(),
                language: request.language.clone(),
                total_bytes: 100,
                manifests: Vec::new(),
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        None
    }
    fn cancel(&self) {}
    fn launch_target<'a>(
        &'a self,
        _: u32,
        _: &'a str,
        _: &'a Path,
    ) -> BoxFuture<'a, Result<LaunchTarget, Failure>> {
        Box::pin(async {
            Err(Failure::Failed(
                "backend.steam.launch.directoryMissing".into(),
            ))
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
        target: steam_cloud::MaintenanceTarget,
        _: &Credentials,
        _: Arc<Control>,
        _: &crate::steam::IdleGuard,
    ) -> Result<(), steam_cloud::Failure> {
        let locations = self.locations.lock().unwrap().clone().unwrap();
        let launch_could_start =
            crate::steam::Client::idle_guard(&locations.steam_sessions, &locations.helper).is_ok();
        self.maintained
            .lock()
            .unwrap()
            .push((target, launch_could_start));
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
            Path::new("/Applications/SailOver.app"),
            &library.join("home"),
        );
        let fake = Arc::new(Fake::default());
        *fake.locations.lock().unwrap() = Some(locations.clone());
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
    fn installed(&self, platform: &str) -> (Receipt, Download) {
        let games = self.locations.library.join("Games/Steam");
        let directory = games.join(format!("steam-620-{platform}"));
        std::fs::create_dir_all(&directory).unwrap();
        let receipt = Receipt {
            app_id: 620,
            name: "Portal 2".into(),
            directory: directory.to_string_lossy().into_owned(),
            platform: platform.into(),
            language: "french".into(),
            total_bytes: 100,
            needs_verification: None,
        };
        let request = Download {
            app_id: 620,
            name: "Portal 2".into(),
            platform: platform.into(),
            language: "french".into(),
            directory: games,
        };
        (receipt, request)
    }
    fn maintained(&self) -> Vec<(steam_cloud::MaintenanceTarget, bool)> {
        self.fake.maintained.lock().unwrap().clone()
    }
}

fn control() -> Arc<Control> {
    Arc::new(Control::new(|_| {}))
}

fn installed_config(target: &steam_cloud::MaintenanceTarget) -> steam_cloud::Config {
    match target {
        steam_cloud::MaintenanceTarget::Installed(config) => config.clone(),
        target => panic!("an installed game syncs its installation, not {target:?}"),
    }
}

#[tokio::test]
async fn a_completed_transfer_is_an_installation_that_needs_no_verification() {
    let fixture = Fixture::new();
    let (_, request) = fixture.installed("macos");

    let receipt = fixture.content.download(&request).await.unwrap();

    assert_eq!(
        *fixture.fake.downloads.lock().unwrap(),
        vec![request.clone()]
    );
    assert_eq!(receipt.app_id, 620);
    assert_eq!(receipt.platform, "macos");
    assert_eq!(receipt.language, "french");
    assert_eq!(receipt.needs_verification, None);
    assert_eq!(
        PathBuf::from(&receipt.directory),
        request.directory.join("steam-620-macos")
    );
}

#[tokio::test]
async fn a_windows_game_keeps_its_cloud_saves_in_its_wine_prefix_and_a_mac_game_has_none() {
    let fixture = Fixture::new();
    let (windows, windows_request) = fixture.installed("windows");
    let (mac, mac_request) = fixture.installed("macos");

    for (receipt, request) in [(&windows, &windows_request), (&mac, &mac_request)] {
        fixture
            .content
            .maintain(&620, Some((receipt, request)), control())
            .await
            .unwrap();
    }

    let maintained = fixture.maintained();
    let (windows, mac) = (
        installed_config(&maintained[0].0),
        installed_config(&maintained[1].0),
    );
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
async fn cloud_maintenance_keeps_every_steam_launch_out_while_it_runs() {
    let fixture = Fixture::new();
    let (receipt, request) = fixture.installed("macos");

    fixture
        .content
        .maintain(&620, Some((&receipt, &request)), control())
        .await
        .unwrap();

    let (_, launch_could_start) = fixture.maintained()[0].clone();
    assert!(!launch_could_start, "maintenance holds Steam's idle guard");
    assert!(
        crate::steam::Client::idle_guard(
            &fixture.locations.steam_sessions,
            &fixture.locations.helper
        )
        .is_ok(),
        "the guard goes with the maintenance"
    );
}

#[tokio::test]
async fn an_uninstalled_game_syncs_what_its_saved_context_names() {
    let fixture = Fixture::new();

    fixture
        .content
        .maintain(&620, None, control())
        .await
        .unwrap();

    match &fixture.maintained()[0].0 {
        steam_cloud::MaintenanceTarget::Uninstalled { root, app_id } => {
            assert_eq!(root, &fixture.locations.cloud_archive);
            assert_eq!(*app_id, 620);
        }
        target => panic!("an uninstalled game has no installation to sync: {target:?}"),
    }
}

#[tokio::test]
async fn a_game_without_steam_cloud_archives_nothing_and_a_failure_is_its_cloud_key() {
    let fixture = Fixture::new();
    let (receipt, request) = fixture.installed("macos");

    fixture.content.archive(&receipt, &request).await.unwrap();

    *fixture.fake.archive.lock().unwrap() = Some(Err(steam_cloud::Failure::Network));
    assert_eq!(
        fixture.content.archive(&receipt, &request).await,
        Err(Failure::Failed(steam_cloud::Failure::Network.key().into()))
    );
}

#[tokio::test]
async fn an_uninstalled_windows_game_loses_its_prefix_and_a_mac_game_has_none() {
    let fixture = Fixture::new();
    let (windows, windows_request) = fixture.installed("windows");
    let (mac, mac_request) = fixture.installed("macos");
    let prefix = fixture.locations.wine_prefixes.join("steam-620-windows");
    std::fs::create_dir_all(prefix.join("drive_c")).unwrap();

    assert_eq!(
        fixture.content.directory_name(&windows, &windows_request),
        "steam-620-windows"
    );
    assert_eq!(fixture.content.remove_prefix(&windows), Some(Ok(())));
    assert!(!prefix.exists());
    assert_eq!(fixture.content.remove_prefix(&mac), None);
    assert_eq!(
        fixture.content.directory_name(&mac, &mac_request),
        "steam-620-macos"
    );
}
