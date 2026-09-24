use super::*;
use steam_kit::cloud::CloudFile;

const ACCOUNT: u64 = 0x0110_0001_0000_002a;
const OTHER_ACCOUNT: u64 = 0x0110_0001_0000_002b;
const APP: u32 = 42;
const SLOT: &str = "%GameInstall%saves/slot.sav";

struct Fixture {
    _temporary: tempfile::TempDir,
    config: Config,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().canonicalize().unwrap();
        let config = Config {
            root: directory.join("cloud"),
            home: directory.join("home"),
            install: directory.join("game"),
            app_id: APP,
        };
        for path in [&config.root, &config.home, &config.install] {
            crate::files::private_directory(path).unwrap();
        }
        Self {
            _temporary: temporary,
            config,
        }
    }

    fn save(&self, name: &str, bytes: &[u8]) {
        let path = self.config.install.join("saves").join(name);
        crate::files::private_directory(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn vault(&self) -> Vault {
        Vault::open(&self.config.root, scope(&self.config, ACCOUNT)).unwrap()
    }

    fn context(&self) -> Context {
        let mut context = Context::new(&self.config, ACCOUNT, metadata());
        context.listing = listing();
        context
    }
}

/// One AutoCloud `savefiles` rule rooted at the install directory.
pub(super) fn save_rule(path: &str, pattern: &str, recursive: &str) -> CloudMetadata {
    CloudMetadata::Object(vec![(
        "savefiles".into(),
        CloudMetadata::Object(vec![(
            "0".into(),
            CloudMetadata::Object(
                [
                    ("root", "GameInstall"),
                    ("path", path),
                    ("pattern", pattern),
                    ("recursive", recursive),
                ]
                .into_iter()
                .map(|(key, value)| (key.into(), CloudMetadata::Value(value.into())))
                .collect(),
            ),
        )]),
    )])
}

fn metadata() -> CloudMetadata {
    save_rule("saves", "*.sav", "1")
}

#[test]
fn one_corrupt_store_does_not_hide_other_pending_games() {
    let fixture = Fixture::new();
    let mut context = fixture.context();
    context.needs_work = true;
    context.save().unwrap();
    let broken = fixture.config.root.join(ACCOUNT.to_string()).join("41");
    crate::files::private_directory(&broken).unwrap();
    fs::write(broken.join("context.json"), b"not valid JSON").unwrap();

    let stores = pending(&fixture.config.root, ACCOUNT);

    assert!(
        stores.is_ok(),
        "A corrupt game must not abort account maintenance"
    );
    assert_eq!(
        stores.unwrap().len(),
        2,
        "Report the failed game and retain the valid one"
    );
}

fn listing() -> CloudListing {
    CloudListing {
        steam_id: ACCOUNT,
        app_id: APP,
        change_number: 7,
        files: Vec::new(),
    }
}

#[test]
fn archived_binding_cannot_reclassify_a_now_foreign_only_identity() {
    let fixture = Fixture::new();
    fixture.save("slot.sav", b"local progress");
    let mut context = fixture.context();
    let mut vault = fixture.vault();
    context.refresh_bindings(vault.local()).unwrap();
    vault.capture(&context.bindings).unwrap();
    context.listing.files.push(CloudFile {
        name: SLOT.into(),
        sha1: "00".repeat(20),
        size: 0,
        timestamp: 0,
        platforms: 1,
        persist_state: 0,
        reupload_requested: false,
    });

    context.refresh_bindings(vault.local()).unwrap();

    assert!(
        context.bindings.is_empty(),
        "Archived Mac bindings must not re-adopt a foreign-only identity"
    );
    assert_eq!(
        fs::read(fixture.config.install.join("saves/slot.sav")).unwrap(),
        b"local progress"
    );
}

#[test]
fn refreshing_remote_and_local_bindings_never_recreates_removed_install() {
    let fixture = Fixture::new();
    fixture.save("slot.sav", b"archived save");
    let mut context = fixture.context();
    let mut vault = fixture.vault();
    context.refresh_bindings(vault.local()).unwrap();
    vault.capture(&context.bindings).unwrap();
    context.listing.files.push(CloudFile {
        name: "%GameInstall%saves/nested/remote.sav".into(),
        sha1: "00".repeat(20),
        size: 0,
        timestamp: 0,
        platforms: 7,
        persist_state: 0,
        reupload_requested: false,
    });
    fs::remove_dir_all(&fixture.config.install).unwrap();

    context.refresh_bindings(vault.local()).unwrap();

    assert_eq!(context.bindings.len(), 2);
    assert_eq!(
        context.bindings[SLOT].path,
        fixture.config.install.join("saves/slot.sav")
    );
    assert_eq!(
        context.bindings["%GameInstall%saves/nested/remote.sav"].path,
        fixture.config.install.join("saves/nested/remote.sav")
    );
    assert!(!fixture.config.install.exists());
    assert!(matches!(context.ensure_parents(), Err(Failure::Conflict)));
    assert!(!fixture.config.install.exists());
}

#[test]
fn fresh_uninstall_archive_keeps_complete_install_saves_after_removal() {
    let fixture = Fixture::new();
    fixture.save("slot.sav", b"slot before uninstall");
    fixture.save("nested/boss.sav", b"boss before uninstall");
    fixture.save("ignored.txt", b"not a save");
    assert!(Context::load(&fixture.config, ACCOUNT).unwrap().is_none());

    archive_with_rules(&fixture.config, metadata(), listing()).unwrap();

    let context = Context::load(&fixture.config, ACCOUNT).unwrap().unwrap();
    assert!(!context.needs_work);
    assert!(context.desired.is_none());
    let desired = fixture.vault().local().unwrap().clone();
    assert_eq!(desired.0.len(), 2);
    fs::remove_dir_all(&fixture.config.install).unwrap();
    let vault = fixture.vault();
    assert_eq!(vault.local(), Some(&desired));
    assert_eq!(
        vault.read_content(&desired.0[SLOT]).unwrap(),
        b"slot before uninstall"
    );
    assert_eq!(
        vault
            .read_content(&desired.0["%GameInstall%saves/nested/boss.sav"])
            .unwrap(),
        b"boss before uninstall"
    );
    assert!(vault.pending().is_none());
    assert!(pending(&fixture.config.root, ACCOUNT).unwrap().is_empty());
    assert!(!fixture.config.install.exists());
}

#[test]
fn context_copied_between_accounts_is_rejected() {
    let fixture = Fixture::new();
    fixture.save("slot.sav", b"first account private save");
    archive_with_rules(&fixture.config, metadata(), listing()).unwrap();
    let context = Context::load(&fixture.config, ACCOUNT).unwrap().unwrap();
    assert!(
        Context::load(&fixture.config, OTHER_ACCOUNT)
            .unwrap()
            .is_none()
    );
    let foreign_path = directory(&fixture.config, OTHER_ACCOUNT)
        .unwrap()
        .join("context.json");
    write(&foreign_path, &context).unwrap();

    assert!(matches!(
        Context::load(&fixture.config, OTHER_ACCOUNT),
        Err(Failure::Storage)
    ));
    assert!(matches!(
        archive(&fixture.config, OTHER_ACCOUNT),
        Err(Failure::Storage)
    ));
    assert!(
        Vault::open(&fixture.config.root, scope(&fixture.config, OTHER_ACCOUNT))
            .unwrap()
            .local()
            .is_none()
    );
    assert_eq!(
        fs::read(fixture.config.install.join("saves/slot.sav")).unwrap(),
        b"first account private save"
    );
}
