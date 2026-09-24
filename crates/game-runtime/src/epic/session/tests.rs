//! EGS Cloud behind the Cloud sync: the shared suite over an in-memory Epic
//! Cloud, and what only Epic has: its snapshot filters, size limits, archive
//! and pending discovery.
use super::fake::{Call, FakeCloud};
use super::store::Pending;
use super::*;
use crate::cloud::sync::tests::{Harness, cloud_suite};
use crate::cloud_status::Indicator;
use crate::supervisor::client::no_other_session;
use std::{fs, path::PathBuf};

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
const OTHER: &str = "abcdef0123456789abcdef0123456789";

pub(crate) fn rules() -> CloudRules {
    CloudRules {
        cloud_save_folder: None,
        cloud_save_folder_mac: "{Home}/Saves".into(),
        include: vec!["*.sav".into()],
        exclude: Vec::new(),
    }
}

pub(crate) struct EpicGame {
    _temporary: tempfile::TempDir,
    pub config: Configuration,
    pub cloud: Arc<FakeCloud>,
    pub control: Arc<Control>,
}

impl Harness for EpicGame {
    type Store = Adapter<FakeCloud>;
    const NO_RULES: UnavailableReason = UnavailableReason::Unsupported;

    fn game() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_path_buf();
        let home = root.join("home");
        let config = Configuration {
            root: root.join("archive"),
            install: home.join("Game"),
            session_directory: root.join("sessions"),
            helper: root.join("helper"),
            home,
            namespace: "namespace".into(),
            catalog_item_id: "catalog".into(),
            app_name: "game".into(),
            platform: "Mac".into(),
            language: "en".into(),
        };
        for directory in [
            config.home.join("Saves"),
            config.install.clone(),
            config.session_directory.clone(),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        Self {
            control: Arc::new(Control::new(|_| {})),
            cloud: FakeCloud::new(ACCOUNT, Some(rules())),
            config,
            _temporary: temporary,
        }
    }
    fn control(&self) -> &Arc<Control> {
        &self.control
    }
    fn open(&self) -> Session<FakeCloud> {
        open(
            self.config.clone(),
            ACCOUNT.into(),
            Arc::clone(&self.cloud),
            Arc::clone(&self.control),
            uuid::Uuid::new_v4().to_string(),
        )
        .unwrap()
    }
    fn save(&self) -> PathBuf {
        self.config.home.join("Saves").join("slot.sav")
    }
    fn slot(&self) -> &'static str {
        "slot.sav"
    }
    fn install(&self) -> PathBuf {
        self.config.install.clone()
    }
    fn publish(&self, bytes: &[u8]) {
        self.cloud.publish(&[("slot.sav", bytes)]);
    }
    fn cloud(&self) -> Option<Vec<u8>> {
        self.cloud.files().get("slot.sav").cloned()
    }
    fn uploads(&self) -> usize {
        self.cloud.uploads()
    }
    fn fail_next(&self, transient: bool) {
        self.cloud.fail_next(
            Call::Download,
            if transient {
                EpicError::Network
            } else {
                EpicError::UnsupportedContent
            },
        );
    }
    fn failure_key(&self) -> String {
        Error::Epic(EpicError::UnsupportedContent).to_string()
    }
    fn set_rules(&self, available: bool) {
        self.cloud.set_rules(available.then(rules));
    }
    async fn uninstall(&self) {
        archive_with(self.config.clone(), self.cloud.as_ref())
            .await
            .unwrap();
        fs::remove_dir_all(&self.config.install).unwrap();
        removed(&self.config, ACCOUNT).unwrap();
    }
    async fn maintain(&self, alive: &SessionAlive<'_>) -> Result<()> {
        let control = Arc::clone(&self.control);
        maintain_with(self.config.clone(), Arc::clone(&self.cloud), control, alive).await
    }
    fn work(&self) -> Option<Work> {
        Context::load(&self.config, ACCOUNT)
            .unwrap()
            .map(|context| context.work())
    }
    fn listed(&self) -> bool {
        pending(&self.config.root, ACCOUNT)
            .unwrap()
            .contains(&self.config)
    }
    fn indicator(&self, requested: bool) -> Option<Indicator> {
        let game = [("namespace".to_owned(), "game".to_owned())];
        let requested: &[(String, String)] = if requested { &game } else { &[] };
        indicators(&self.config.root, ACCOUNT, requested)
            .unwrap()
            .into_iter()
            .find(|shown| shown.app_name == self.config.app_name)
            .map(|shown| shown.indicator)
    }
    fn root(&self) -> PathBuf {
        self.config.root.clone()
    }
    fn scope(&self) -> StoreScope {
        self.config.scope(ACCOUNT).into()
    }
    fn account(&self) -> AccountIdentity {
        ACCOUNT.into()
    }
}

mod suite {
    super::cloud_suite!(super::EpicGame);
}

fn fixture() -> (tempfile::TempDir, Vault, Context) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let home = root.join("home");
    let install = home.join("Game");
    let session_directory = root.join("sessions");
    for directory in [
        home.join("Saves"),
        install.clone(),
        session_directory.clone(),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    let config = Configuration {
        root: root.join("archive"),
        home,
        install,
        session_directory,
        helper: root.join("helper"),
        namespace: "namespace".into(),
        catalog_item_id: "catalog".into(),
        app_name: "game".into(),
        platform: "Mac".into(),
        language: "en".into(),
    };
    let mut vault = Vault::open(&config.root, config.scope(ACCOUNT)).unwrap();
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let context = Context {
        config,
        account_id: ACCOUNT.into(),
        rules: CloudRules {
            cloud_save_folder: None,
            cloud_save_folder_mac: "{Home}/Saves".into(),
            include: vec!["*.sav".into()],
            exclude: vec!["excluded.sav".into()],
        },
        pending: None,
        installation_removed: false,
    };
    context.save().unwrap();
    (temporary, vault, context)
}
fn staged(vault: &Vault, name: &str, bytes: &[u8]) -> Manifest {
    Manifest([(name.to_owned(), vault.stage(bytes, 2).unwrap())].into())
}

#[test]
fn uploads_preserve_excluded_remote_bytes_and_apply_managed_deletions() {
    let (_temporary, vault, context) = fixture();
    let full = CloudSnapshot {
        revision: "original".into(),
        files: [
            ("deleted.sav".into(), b"old save".to_vec()),
            ("excluded.sav".into(), b"other platform".to_vec()),
            ("settings.ini".into(), b"foreign config".to_vec()),
        ]
        .into(),
    };
    let managed = stage_remote(&vault, &context.rules, &full).unwrap();
    assert_eq!(managed.files.0.keys().collect::<Vec<_>>(), ["deleted.sav"]);
    let local = staged(&vault, "new.sav", b"new bytes");
    let upload = upload_files(&vault, &context.rules, full.files.clone(), &local).unwrap();
    assert_eq!(upload["excluded.sav"], b"other platform");
    assert_eq!(upload["settings.ini"], b"foreign config");
    assert_eq!(upload["new.sav"], b"new bytes");
    assert!(!upload.contains_key("deleted.sav"));
    let excluded = staged(&vault, "excluded.sav", b"wrong local selection");
    assert!(matches!(
        upload_files(&vault, &context.rules, full.files.clone(), &excluded),
        Err(Error::Unsupported)
    ));
}

#[test]
fn partial_uninstall_archives_survivors_preserves_missing_saves_and_keeps_exit_work() {
    let (_temporary, mut vault, mut context) = fixture();
    let directory = context.config.home.join("Saves");
    let first = directory.join("first.sav");
    let nested = directory.join("nested");
    fs::create_dir(&nested).unwrap();
    let second = nested.join("second.sav");
    fs::write(&first, b"first").unwrap();
    fs::write(&second, b"second").unwrap();
    let exit = staged(&vault, "first.sav", b"prior pending exit");
    context.pending = Some(Pending::Upload {
        desired: exit.clone(),
    });
    archive_context(&mut vault, &mut context).unwrap();
    assert!(
        Context::load(&context.config, ACCOUNT)
            .unwrap()
            .unwrap()
            .installation_removed
    );
    fs::remove_dir_all(&nested).unwrap();
    fs::write(&first, b"surviving latest").unwrap();
    archive_context(&mut vault, &mut context).unwrap();
    assert!(!nested.exists());
    let archived = vault.local().unwrap();
    assert_eq!(
        vault.read_content(&archived.0["first.sav"]).unwrap(),
        b"surviving latest"
    );
    assert_eq!(
        vault
            .read_content(&archived.0["nested/second.sav"])
            .unwrap(),
        b"second"
    );
    assert_eq!(context.pending.as_ref().unwrap().files(), Some(&exit));
}

#[test]
fn pending_discovery_skips_a_context_copied_into_another_account_or_game() {
    let (_temporary, vault, mut context) = fixture();
    context.pending = Some(Pending::Upload {
        desired: staged(&vault, "slot.sav", b"pending"),
    });
    context.save().unwrap();
    let config = context.config.clone();
    drop(vault);
    let vault = Vault::open(&config.root, config.scope(OTHER)).unwrap();
    let copied = Context::path(&config, OTHER).unwrap();
    fs::copy(Context::path(&config, ACCOUNT).unwrap(), &copied).unwrap();
    assert!(pending(&config.root, OTHER).unwrap().is_empty());
    assert!(copied.exists());
    drop(vault);
    let mut other_config = config.clone();
    other_config.app_name = "other-game".into();
    let _vault = Vault::open(&config.root, other_config.scope(ACCOUNT)).unwrap();
    fs::copy(
        Context::path(&config, ACCOUNT).unwrap(),
        Context::path(&other_config, ACCOUNT).unwrap(),
    )
    .unwrap();
    // One unreadable game never hides another game's pending work.
    assert_eq!(pending(&config.root, ACCOUNT).unwrap(), [config]);
}

#[tokio::test]
async fn uninstall_archive_needs_only_the_epic_storefront_lock() {
    let game = EpicGame::game();
    fs::write(Harness::save(&game), b"progress").unwrap();
    game.cloud.publish(&[("slot.sav", b"progress")]);
    game.open().prepare(&no_other_session).await.unwrap();
    let steam_game = Vault::open(
        &game.config.root,
        cloud_vault::Scope {
            steam_id: 0x0110_0001_0000_0001,
            app_id: 10,
            cloud_app_id: 10,
        },
    )
    .unwrap();

    archive_with(game.config.clone(), game.cloud.as_ref())
        .await
        .unwrap();

    assert!(
        Context::load(&game.config, ACCOUNT)
            .unwrap()
            .unwrap()
            .installation_removed
    );
    let other_epic_game = Vault::open(
        &game.config.root,
        cloud_vault::EpicScope {
            account_id: ACCOUNT.into(),
            namespace: "namespace".into(),
            app_name: "other-game".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        archive_with(game.config.clone(), game.cloud.as_ref()).await,
        Err(Error::Busy)
    ));
    drop((steam_game, other_epic_game));
}

#[tokio::test]
async fn changed_epic_cloud_rules_need_a_dedicated_conversion() {
    let game = EpicGame::game();
    fs::write(Harness::save(&game), b"progress").unwrap();
    game.cloud.publish(&[("slot.sav", b"progress")]);
    game.open().prepare(&no_other_session).await.unwrap();
    let mut changed = rules();
    changed.include.push("*.cfg".into());
    game.cloud.set_rules(Some(changed));

    let refused = maintain_with(
        game.config.clone(),
        Arc::clone(&game.cloud),
        Arc::clone(&game.control),
        &no_other_session,
    )
    .await;

    assert!(matches!(refused, Err(Error::Unsupported)));
    assert_eq!(game.cloud.uploads(), 0);
}

#[test]
fn save_name_scan_refuses_links_before_adopting_or_reading_bytes() {
    let (_temporary, vault, context) = fixture();
    let outside = context.config.home.join("private.sav");
    fs::write(&outside, b"private bytes").unwrap();
    std::os::unix::fs::symlink(&outside, context.config.home.join("Saves/slot.sav")).unwrap();
    assert!(matches!(
        store::bindings(&context, &vault, None, true),
        Err(Error::Storage)
    ));
    assert!(vault.local().is_none());
    assert_eq!(fs::read(outside).unwrap(), b"private bytes");
}

#[test]
fn unsupported_mac_rules_allow_only_a_new_unmanaged_game_to_start_without_cloud() {
    let (_temporary, _vault, mut context) = fixture();
    let mut rules = context.rules.clone();
    rules.cloud_save_folder_mac = "{AppData}/Game".into();
    context.config.root = context.config.root.with_file_name("fresh-archive");
    let mut vault = Vault::open(&context.config.root, context.config.scope(ACCOUNT)).unwrap();
    assert!(!supports_rules(&context.config, ACCOUNT, &rules, &vault, None).unwrap());
    assert!(matches!(
        supports_rules(&context.config, ACCOUNT, &rules, &vault, Some(&context)),
        Err(Error::Epic(EpicError::UnsupportedContent))
    ));
    vault
        .game_started(&uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(matches!(
        supports_rules(&context.config, ACCOUNT, &rules, &vault, None),
        Err(Error::Epic(EpicError::UnsupportedContent))
    ));
}

#[test]
fn removal_needs_only_the_epic_storefront_lock() {
    let (_temporary, vault, context) = fixture();
    let config = context.config.clone();
    fs::remove_dir_all(&config.install).unwrap();
    assert!(matches!(removed(&config, ACCOUNT), Err(Error::Busy)));
    drop(vault);
    let steam = cloud_vault::Scope {
        steam_id: 0x0110_0001_0000_0001,
        app_id: 10,
        cloud_app_id: 10,
    };
    let _steam_game = Vault::open(&config.root, steam).unwrap();

    removed(&config, ACCOUNT).unwrap();

    assert!(
        Context::load(&config, ACCOUNT)
            .unwrap()
            .unwrap()
            .installation_removed
    );
}

#[test]
fn archive_errors_name_the_storefront_that_holds_the_saves() {
    assert!(matches!(Error::from(cloud_vault::Error::Busy), Error::Busy));
    let shared = Error::from(cloud_vault::Error::OtherStoreBusy);
    assert_eq!(shared.to_string(), "backend.cloud.sharedSavesInUse");
    assert_eq!(
        Error::from(cloud_vault::Error::Stale).to_string(),
        "backend.epic.cloud.storage"
    );
}
