//! GOG's part of the installations through their Content interface, with a
//! network the test stands in for and real folders in a temporary library.
use super::*;
use crate::{
    gog::Store,
    installations::{Content as _, Installations, Offer, Queueing, tests::eventually},
    launch_shell::{
        SignIn,
        tests::{Library, Opened, Setup, folders},
    },
    profile::Game,
    queue::DownloadQueue,
    runtimes::tests::ready,
    sessions::Sessions,
    supervisor::{self, session as lifecycle},
};
use std::sync::Mutex;
use tokio::sync::Notify;

const ACCOUNT: &str = "48628349957132247";

/// What the test's GOG network was asked, in order.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
    /// Renewing credentials waits until the test releases it.
    held: Mutex<bool>,
    released: Notify,
    /// The account signed out: asking for it fails.
    signed_out: Mutex<bool>,
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
            match *self.signed_out.lock().unwrap() {
                true => Err(Failure::Failed("backend.gog.unauthorized".into())),
                false => Ok(ACCOUNT.into()),
            }
        })
    }
    fn platform<'a>(
        &'a self,
        _: &'a Product,
        language: &'a str,
    ) -> BoxFuture<'a, Result<Platform, Failure>> {
        Box::pin(async move {
            self.call(&format!("platform {language}"));
            Ok(Platform::Windows)
        })
    }
    fn download<'a>(
        &'a self,
        transfer: &'a Transfer<Gog>,
        language: &'a str,
    ) -> BoxFuture<'a, Result<gog_kit::client::Installation, Failure>> {
        Box::pin(async move {
            self.call(&format!("download {language}"));
            let product_id = &transfer.product.product_id;
            let platform = transfer.platform.expect("a settled platform");
            Ok(gog_kit::client::Installation {
                product_id: product_id.clone(),
                user_id: ACCOUNT.into(),
                directory: transfer
                    .directory
                    .join(gog_kit::client::installation_directory_name(
                        product_id, platform,
                    ))
                    .to_string_lossy()
                    .into_owned(),
                platform,
                total_bytes: 100,
                build_id: "1".into(),
                version_name: "1.0".into(),
                language: language.into(),
                content_root: String::new(),
                script_interpreter: false,
                products: Vec::new(),
                dependencies: Vec::new(),
            })
        })
    }
    fn progress(&self) -> Option<TransferProgress> {
        None
    }
    fn cancel(&self) {
        self.call("cancel");
    }
    fn update_available<'a>(&'a self, _: &'a Receipt<Gog>) -> BoxFuture<'a, Result<bool, Failure>> {
        Box::pin(async move {
            self.call("update");
            Ok(false)
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
        let fake = Arc::new(Fake::default());
        let saves = fake.clone();
        let content = Content::new(
            fake.clone(),
            Arc::new(move || saves.call("saved")),
            "fr-FR".into(),
            locations.clone(),
        );
        Self {
            _root: root,
            locations,
            fake,
            content,
        }
    }

    fn windows_directory(&self) -> PathBuf {
        self.locations
            .library
            .join("Games/GOG/gog-1207658924-windows")
    }

    fn transfer(&self, platform: Platform) -> Transfer<Gog> {
        Transfer {
            product: witcher(),
            name: "The Witcher".into(),
            platform: Some(platform),
            directory: self.locations.library.join("Games/GOG"),
        }
    }
}

/// The Witcher, as GOG names it.
fn witcher() -> Product {
    Product {
        product_id: "1207658924".into(),
    }
}

#[tokio::test]
async fn a_transfer_saves_renewed_credentials_first_and_installs_the_language() {
    let fixture = Fixture::new();
    let transfer = fixture.transfer(Platform::Windows);
    assert_eq!(
        fixture.content.begin(&transfer).await.unwrap(),
        Platform::Windows
    );
    let receipt = fixture.content.download(&transfer).await.unwrap();
    assert_eq!(receipt.product, witcher());
    assert_eq!(receipt.name, "The Witcher");
    assert_eq!(receipt.platform, Platform::Windows);
    assert!(!receipt.needs_verification);
    assert_eq!(
        Path::new(&receipt.directory),
        fixture
            .locations
            .library
            .join("Games/GOG/gog-1207658924-windows")
    );
    let calls = fixture.fake.calls();
    assert_eq!(&calls[..3], ["prepare", "saved", "download fr-FR"]);
}

#[tokio::test]
async fn a_transfer_without_a_platform_installs_the_build_gog_picks_in_its_language() {
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
        ["prepare", "saved", "platform fr-FR", "saved"]
    );
}

#[tokio::test]
async fn a_transfer_cancelled_while_its_credentials_renew_never_begins() {
    let fixture = Arc::new(Fixture::new());
    *fixture.fake.held.lock().unwrap() = true;
    let beginning = {
        let fixture = fixture.clone();
        tokio::spawn(async move {
            let transfer = fixture.transfer(Platform::Mac);
            fixture.content.begin(&transfer).await
        })
    };
    while fixture.fake.calls().is_empty() {
        tokio::task::yield_now().await;
    }
    fixture.content.cancel();
    fixture.fake.released.notify_one();
    assert_eq!(beginning.await.unwrap(), Err(Failure::Cancelled));
}

#[tokio::test]
async fn gog_keeps_no_cloud_saves() {
    let fixture = Fixture::new();
    let receipt = fixture.receipt(Platform::Mac);
    assert_eq!(fixture.content.archive(&receipt).await, Ok(()));
    assert_eq!(fixture.content.removed(&receipt).await, Ok(()));
    assert!(
        fixture
            .content
            .indicators(&["1207658924".into()])
            .unwrap()
            .is_empty()
    );
    let control = Arc::new(crate::cloud::Control::new(|_| {}));
    assert_eq!(
        fixture
            .content
            .maintain(&"1207658924".into(), Some(&receipt), control)
            .await,
        Err(Failure::Failed("backend.gog.unsupported".into()))
    );
}

#[tokio::test]
async fn only_a_windows_build_runs_in_its_own_prefix() {
    let fixture = Fixture::new();
    let windows = fixture.receipt(Platform::Windows);
    let prefix = windows.wine_prefix(&fixture.locations).await.unwrap();
    let configuration = fixture.content.configuration(&windows, prefix);
    assert_eq!(
        configuration.prefix.as_deref(),
        Some(
            fixture
                .locations
                .wine_prefixes
                .join("gog-1207658924-windows")
                .as_path()
        )
    );
    assert_eq!(
        configuration.session_directory,
        fixture.locations.gog_sessions
    );
    assert_eq!(configuration.title, "The Witcher");
    launch_shell::validate::<Gog>(&configuration, ACCOUNT).unwrap();

    let mac = fixture.receipt(Platform::Mac);
    let prefix = mac.wine_prefix(&fixture.locations).await.unwrap();
    let configuration = fixture.content.configuration(&mac, prefix);
    assert_eq!(configuration.prefix, None);
    launch_shell::validate::<Gog>(&configuration, ACCOUNT).unwrap();
    assert_eq!(fixture.content.directory_name(&mac), "gog-1207658924-macos");
}

#[tokio::test]
async fn an_installed_build_updates_in_place_whether_updated_or_offered_again() {
    let fixture = Fixture::new();
    let library = fixture.locations.library.clone();
    let previous = library.join("Previous location/gog-1207658924-windows");
    std::fs::create_dir_all(&previous).unwrap();
    let file = library.join("gog-installations.json");
    let written = serde_json::json!([{
        "productId": "1207658924",
        "name": "The Witcher",
        "directory": previous,
        "platform": "windows",
        "totalBytes": 100
    }]);
    std::fs::write(&file, serde_json::to_vec(&written).unwrap()).unwrap();
    let sessions = Arc::new(Sessions::<Gog>::new(Box::new(|| Ok(None)), ready()));
    eventually(|| !sessions.state().recovering).await;
    let installations = Installations::new(
        Arc::new(fixture.content),
        sessions,
        ACCOUNT.to_string(),
        file,
        fixture.locations.home.clone(),
        crate::games_folder::GamesFolder::new(&library),
        Queueing {
            queue: DownloadQueue::new(),
            account: ACCOUNT.into(),
            name: |game: &String| game.clone(),
        },
    );
    let idle = || {
        let state = installations.state();
        !state.downloading && state.queued.is_empty()
    };

    installations.update("1207658924:windows");
    eventually(idle).await;
    let receipts = installations.state().receipts;
    assert_eq!(receipts.len(), 1);
    let windows = receipts[0].clone();
    assert_eq!(Path::new(&windows.directory), previous);

    installations.install(Offer {
        product: witcher(),
        name: "The Witcher".into(),
    });
    eventually(idle).await;
    let state = installations.state();
    assert_eq!(state.report, None);
    assert_eq!(
        state.receipts,
        [windows],
        "the offer goes over the installed build"
    );
    assert_eq!(
        fixture
            .fake
            .calls()
            .iter()
            .filter(|call| call.starts_with("download"))
            .count(),
        2
    );
}

mod builds {
    crate::launch_shell::tests::setup_suite!(super::Fixture);
}

mod launches {
    crate::launch_shell::tests::library_suite!(super::Fixture);
}

impl Setup for Fixture {
    type G = Gog;
    const SUPERVISE: &'static str = "gog-supervise";
    const UNSUPPORTED: &'static str = "backend.gog.unsupported";
    const SESSION: &'static str = "backend.gog.session";
    const CANCELLED: &'static str = "backend.gog.cancelled";
    const BAD_ACCOUNT: &'static str = "backend.gog.unsupported";
    const WINDOWS: &'static str = "windows";
    const TITLE: &'static str = "The Witcher";
    const GAME: Game<'static> = Game::Gog {
        product_id: "1207658924",
    };
    const LAUNCH_KEYS: [(crate::launch::Error, &'static str); 7] = {
        use crate::launch::Error as Launch;
        [
            (
                Launch::IncompatibleArchitecture,
                "backend.runtime.macBuildNotNative",
            ),
            (Launch::WineUnavailable, "backend.runtime.wineUnavailable"),
            (Launch::InvalidApplication, "backend.gog.unsupported"),
            (Launch::NotExecutable, "backend.gog.unsupported"),
            (Launch::Storage, "backend.gog.session"),
            (Launch::StartFailed, "backend.gog.session"),
            (Launch::Unavailable, "backend.gog.session"),
        ]
    };
    const SUPERVISOR_KEYS: [(supervisor::Error, &'static str); 8] = {
        use supervisor::Error as Core;
        [
            (Core::Storage, "backend.gog.storage"),
            (Core::NewerSession, "backend.gog.busy"),
            (Core::Busy, "backend.gog.busy"),
            (Core::Cancelled, "backend.gog.cancelled"),
            (
                Core::RosettaUnavailable,
                "backend.runtime.rosettaUnavailable",
            ),
            (Core::InvalidSession, "backend.gog.session"),
            (Core::Unavailable, "backend.gog.session"),
            (Core::StartFailed, "backend.gog.session"),
        ]
    };

    fn account() -> String {
        ACCOUNT.into()
    }
    fn credential() -> NoCredential {
        NoCredential {}
    }
    fn credential_json() -> serde_json::Value {
        serde_json::json!({})
    }
    fn configuration_in(root: &Path, platform: Platform, prefix: Option<PathBuf>) -> Configuration {
        Configuration {
            home: root.join("home"),
            install: root.join("library/Games/GOG").join(
                gog_kit::client::installation_directory_name("1207658924", platform),
            ),
            session_directory: root.join("library/GOGSessions"),
            helper: root.join("Obelisk.app/Contents/Helpers/SteamNative/obelisk-steam-host"),
            product_id: "1207658924".into(),
            platform,
            title: "The Witcher".into(),
            prefix,
        }
    }
    fn wire(configuration: &Configuration) -> serde_json::Value {
        serde_json::json!({
            "home": configuration.home,
            "install": configuration.install,
            "session_directory": configuration.session_directory,
            "helper": configuration.helper,
            "product_id": "1207658924",
            "platform": match configuration.platform {
                Platform::Mac => "macos",
                Platform::Windows => "windows",
            },
            "title": "The Witcher",
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
        let mac = Self::configuration_in(root, Platform::Mac, None);
        let product = |product_id: &str| Configuration {
            product_id: product_id.into(),
            ..mac.clone()
        };
        let refused = [
            (mac.clone(), "not-a-user".to_owned()),
            (mac.clone(), String::new()),
            (product(""), ACCOUNT.to_owned()),
            (product("1207658924x"), ACCOUNT.to_owned()),
            (product(&"1".repeat(21)), ACCOUNT.to_owned()),
        ];
        refused
            .into_iter()
            .map(|(configuration, account)| (configuration, account, Self::UNSUPPORTED))
            .collect()
    }
    fn handoff() -> serde_json::Value {
        serde_json::json!({
            "config": {
                "home": "/Users/player",
                "install": "/library/Games/GOG/gog-1207658924-macos",
                "session_directory": "/library/GOGSessions",
                "helper": "/Obelisk.app/Contents/Helpers/SteamNative/obelisk-steam-host",
                "product_id": "1207658924",
                "platform": "macos",
                "title": "The Witcher",
                "prefix": null,
            },
            "id": "6f1c1a0e-8a0f-4a3e-9a7b-0d2f6f3a8b11",
            "account_id": ACCOUNT,
            "environment": {"HOME": "/Users/player"},
            "wine": null,
        })
    }
    fn session_root() -> &'static str {
        "/library/GOGSessions"
    }
    fn store() -> Store {
        Store {
            product_id: "1207658924".into(),
        }
    }
    fn open(input: &mut dyn std::io::Read) -> crate::gog::Result<Opened<Gog>> {
        let (opening, launch) = <crate::gog::launch::Launch as lifecycle::Launch>::open(input)?;
        Ok((opening, lifecycle::Launch::wine(&launch)))
    }
}

impl Library for Fixture {
    const PREREQUISITE_ARGUMENTS: &'static str = "/install /quiet";
    const INCOMPLETE: &'static str = "backend.gog.launch.incompleteInstallation";
    const CREDENTIAL_CALLS: &'static [&'static str] = &["account"];

    fn new() -> Self {
        Self::in_library(false)
    }
    fn bundled() -> Self {
        Self::in_library(true)
    }
    fn locations(&self) -> &Locations {
        &self.locations
    }
    fn receipt(&self, platform: Platform) -> Receipt<Gog> {
        let directory = self.locations.library.join("Games/GOG").join(
            gog_kit::client::installation_directory_name("1207658924", platform),
        );
        std::fs::create_dir_all(&directory).unwrap();
        Receipt {
            product: witcher(),
            name: "The Witcher".into(),
            directory: directory.to_string_lossy().into_owned(),
            platform,
            total_bytes: 100,
            needs_verification: false,
        }
    }
    fn nameless(&self) -> Receipt<Gog> {
        // A folder with no name of its own, such as `..`.
        let nameless = self.locations.library.join("Games/GOG/..");
        Receipt {
            directory: nameless.to_string_lossy().into_owned(),
            ..self.receipt(Platform::Windows)
        }
    }
    fn game() -> String {
        witcher().product_id
    }
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt<Gog>,
        admission: Admission<Gog>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        self.content.launch(receipt, admission, None)
    }
    fn configuration(&self, receipt: &Receipt<Gog>, prefix: Option<PathBuf>) -> Configuration {
        self.content.configuration(receipt, prefix)
    }
    fn signing(&self) -> impl SignIn<Gog> {
        Signing(self.fake.clone())
    }
    fn wire_config(&self, receipt: &Receipt<Gog>, prefix: Option<&Path>) -> serde_json::Value {
        let locations = &self.locations;
        serde_json::json!({
            "home": locations.home,
            "install": receipt.directory,
            "session_directory": locations.gog_sessions,
            "helper": locations.helper,
            "product_id": "1207658924",
            "platform": match receipt.platform {
                Platform::Mac => "macos",
                Platform::Windows => "windows",
            },
            "title": "The Witcher",
            "prefix": prefix,
        })
    }
    fn prefix(&self) -> PathBuf {
        self.locations.wine_prefixes.join("gog-1207658924-windows")
    }
    /// What a completed Windows installation holds: the program its primary
    /// play task starts, `program`, its receipt, and the redistributable the
    /// receipt names. Returns the redistributable.
    fn complete_windows_build(&self, program: &[u8]) -> PathBuf {
        let install = self.windows_directory();
        let redistributable = install.join("__redist/MSVC2017/VC_redist.x86.exe");
        std::fs::create_dir_all(install.join("System")).unwrap();
        std::fs::create_dir_all(redistributable.parent().unwrap()).unwrap();
        std::fs::write(install.join("System/witcher.exe"), program).unwrap();
        std::fs::write(&redistributable, "").unwrap();
        let info = serde_json::json!({"playTasks": [
            {"category": "game", "isPrimary": true, "path": "System\\witcher.exe", "type": "FileTask"},
        ]});
        std::fs::write(
            install.join("goggame-1207658924.info"),
            serde_json::to_vec(&info).unwrap(),
        )
        .unwrap();
        let receipt = gog_kit::client::Installation {
            product_id: "1207658924".into(),
            user_id: ACCOUNT.into(),
            directory: install.to_string_lossy().into_owned(),
            platform: Platform::Windows,
            total_bytes: 1,
            build_id: "1".into(),
            version_name: "1.0".into(),
            language: "en-US".into(),
            content_root: String::new(),
            script_interpreter: false,
            products: Vec::new(),
            dependencies: serde_json::from_value(serde_json::json!([{
                "id": "MSVC2017",
                "path": "__redist/MSVC2017/VC_redist.x86.exe",
                "arguments": "/install /quiet",
            }]))
            .unwrap(),
        };
        std::fs::write(
            install.join(obelisk_core::install::RECEIPT_FILE),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        redistributable
    }
    fn asked(&self) -> Vec<String> {
        self.fake.calls()
    }
    fn refuse_credential(&self) -> &'static str {
        *self.fake.signed_out.lock().unwrap() = true;
        "backend.gog.unauthorized"
    }
    fn saved(&self) -> bool {
        self.fake.calls().iter().any(|call| call == "saved")
    }
}
