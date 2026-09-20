use super::*;
use serde::de::DeserializeOwned;
use std::{fs, path::Path};
use steam_cloud::{Bindings, Manifest, RemoteView, SelectedFile, Vault};
use steam_kit::cloud::{CloudListing, CloudMetadata, MacCloudPaths};

#[cfg(test)]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Context {
    pub config: Config,
    pub account_id: u64,
    pub metadata: CloudMetadata,
    pub listing: CloudListing,
    pub remote: RemoteView,
    pub bindings: Bindings,
    pub needs_work: bool,
    pub desired: Option<Manifest>,
    pub transaction: Option<PathBuf>,
    /// An uninstall may have removed part of the save projection. This records
    /// removal intent, not proof that the installation directory is absent.
    pub installation_removed: bool,
}

pub(super) fn directory(config: &Config, account: u64) -> Result<PathBuf, Failure> {
    scope(config, account).validate()?;
    for path in [&config.root, &config.home, &config.install] {
        safe(path)?;
    }
    let directory = config
        .root
        .join(account.to_string())
        .join(config.app_id.to_string());
    safe(&directory)?;
    Ok(directory)
}

pub(super) fn safe(path: &Path) -> Result<(), Failure> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Failure::Storage);
    }
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(m)
                if m.file_type().is_symlink()
                    && ![Path::new("/var"), Path::new("/tmp")].contains(&parent) =>
            {
                return Err(Failure::Storage);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub(super) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Failure> {
    safe(path)?;
    Ok(steam_cloud::journal::read(path)?)
}
pub(super) fn write(path: &Path, value: &impl Serialize) -> Result<(), Failure> {
    safe(path)?;
    let parent = path.parent().ok_or(Failure::Storage)?;
    crate::files::private_directory(parent).map_err(|_| Failure::Storage)?;
    Ok(steam_cloud::journal::write(path, value)?)
}
impl Context {
    /// A context for a store that has not yet observed its remote view.
    pub fn new(config: &Config, account_id: u64, metadata: CloudMetadata) -> Self {
        Self {
            config: config.clone(),
            account_id,
            metadata,
            listing: CloudListing {
                steam_id: account_id,
                app_id: config.app_id,
                change_number: 0,
                files: Vec::new(),
            },
            remote: RemoteView {
                revision: "unobserved".into(),
                files: Manifest::default(),
            },
            bindings: Bindings::new(),
            needs_work: false,
            desired: None,
            transaction: None,
            installation_removed: false,
        }
    }
    pub fn load(config: &Config, account: u64) -> Result<Option<Self>, Failure> {
        let context: Option<Self> = read(&directory(config, account)?.join("context.json"))?;
        if let Some(context) = &context {
            context.validate(config, account)?;
        }
        Ok(context)
    }
    pub fn load_app(root: &Path, account: u64, app_id: u32) -> Result<Self, Failure> {
        safe(root)?;
        let path = root
            .join(account.to_string())
            .join(app_id.to_string())
            .join("context.json");
        let context: Self = read(&path)?.ok_or(Failure::Unsupported)?;
        if context.config.root != root || context.config.app_id != app_id {
            return Err(Failure::Storage);
        }
        context.validate(&context.config, account)?;
        Ok(context)
    }
    fn validate(&self, config: &Config, account: u64) -> Result<(), Failure> {
        let directory = directory(config, account)?;
        if self.account_id != account
            || self.config.app_id != config.app_id
            || self.config.root != config.root
            || self.config.home != config.home
            || self.listing.steam_id != account
            || self.listing.app_id != config.app_id
        {
            return Err(Failure::Storage);
        }
        self.remote.validate()?;
        if let Some(desired) = &self.desired {
            desired.validate()?;
        }
        if self
            .transaction
            .as_ref()
            .is_some_and(|p| p.parent() != Some(directory.as_path()))
        {
            return Err(Failure::Storage);
        }
        Ok(())
    }
    pub fn removed_installation_absent(&self) -> Result<bool, Failure> {
        if !self.installation_removed {
            return Ok(false);
        }
        safe(&self.config.install)?;
        match fs::symlink_metadata(&self.config.install) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
    pub fn save(&self) -> Result<(), Failure> {
        write(
            &directory(&self.config, self.account_id)?.join("context.json"),
            self,
        )
    }
    pub fn paths(&self) -> Result<MacCloudPaths, Failure> {
        Ok(MacCloudPaths::new(
            &self.metadata,
            self.account_id,
            &self.config.home,
            &self.config.install,
        )?)
    }
    pub fn refresh_bindings(&mut self, local: Option<&Manifest>) -> Result<(), Failure> {
        let paths = self.paths()?;
        let selected = paths.local_files(&self.listing.files)?;
        let mut foreign_paths = std::collections::BTreeSet::new();
        for file in &self.listing.files {
            if file.platforms & 2 == 0
                && let Some(path) = paths.destination(&file.name, 2)?
            {
                foreign_paths.insert(path);
            }
        }
        let mut bindings: Bindings = selected
            .into_iter()
            .map(|f| {
                (
                    f.name,
                    SelectedFile {
                        path: f.path,
                        platforms: f.platforms,
                    },
                )
            })
            .collect();
        // Keep destinations from the account snapshot so switching back can
        // project the complete local set without guessing filesystem paths.
        if let Some(local) = local {
            for (name, file) in &local.0 {
                if bindings.contains_key(name) {
                    continue;
                }
                if let Some(path) = paths.destination(name, file.platforms)? {
                    // A newer listing can exclude a previously managed name.
                    // The archived platform mask cannot override that decision.
                    if foreign_paths.contains(&path) {
                        continue;
                    }
                    bindings.insert(
                        name.clone(),
                        SelectedFile {
                            path,
                            platforms: file.platforms,
                        },
                    );
                }
            }
        }
        for binding in bindings.values() {
            safe(&binding.path)?;
        }
        self.bindings = bindings;
        Ok(())
    }
    pub fn ensure_parents(&self) -> Result<(), Failure> {
        if !self.config.install.is_dir() {
            return Err(Failure::Conflict);
        }
        for binding in self.bindings.values() {
            safe(&binding.path)?;
            fs::create_dir_all(binding.path.parent().ok_or(Failure::Storage)?)?;
        }
        Ok(())
    }
    pub fn token(&self) -> Result<String, Failure> {
        Ok(steam_cloud::ContentHash::of(&serde_json::to_vec(&(
            self.account_id,
            self.config.app_id,
            &self.remote,
            &self.desired,
            self.installation_removed,
        ))?)
        .into())
    }
}

/// The shared archival tail: bind the current save paths, capture their bytes
/// and record the removal fence. A retry of a partial uninstall must keep the
/// files it already removed in the snapshot instead of treating them as
/// deletions, so only a first pass captures strictly what exists.
fn archive_context(vault: &mut Vault, context: &mut Context) -> Result<(), Failure> {
    context.refresh_bindings(vault.local())?;
    context.ensure_parents()?;
    // Uninstall archives bytes; only an observed game exit creates new work.
    // In particular, preserve an older immutable upload still awaiting retry.
    if context.installation_removed {
        vault.capture_preserving_missing(&context.bindings)?;
    } else {
        vault.capture(&context.bindings)?;
    }
    // Persist before removal starts: recursive deletion can fail partway.
    context.installation_removed = true;
    context.save()
}

/// Preserve install-contained saves before removal. The archive itself lives
/// outside the installation; uninstall never deletes it or remote Cloud files.
pub fn archive(config: &Config, account: u64) -> Result<(), Failure> {
    let Some(mut context) = Context::load(config, account)? else {
        return Ok(());
    };
    context.config.install = config.install.clone();
    let mut vault = Vault::open(&config.root, scope(config, account))?;
    archive_context(&mut vault, &mut context)
}

pub fn archive_with_rules(
    config: &Config,
    metadata: CloudMetadata,
    listing: CloudListing,
) -> Result<(), Failure> {
    if listing.app_id != config.app_id {
        return Err(Failure::Authentication);
    }
    let account = listing.steam_id;
    let mut vault = Vault::open(&config.root, scope(config, account))?;
    let mut context = Context::load(config, account)?
        .unwrap_or_else(|| Context::new(config, account, metadata.clone()));
    context.config = config.clone();
    context.metadata = metadata;
    context.listing = listing;
    archive_context(&mut vault, &mut context)
}

/// Mark a completed uninstall without interpreting missing files as deletions.
/// Existing exit snapshots and upload transactions remain independently valid.
pub fn removed(config: &Config, account: u64) -> Result<(), Failure> {
    directory(config, account)?;
    let _vault = Vault::open(&config.root, scope(config, account))?;
    match fs::symlink_metadata(&config.install) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err(Failure::Conflict),
        Err(error) => return Err(error.into()),
    }
    let Some(mut context) = Context::load(config, account)? else {
        return Ok(());
    };
    if context.config.install != config.install {
        return Err(Failure::Changed);
    }
    context.installation_removed = true;
    context.save()
}

pub(super) struct PendingStore {
    pub app_id: u32,
    pub config: Result<Config, Failure>,
}

pub(super) fn pending(root: &Path, account: u64) -> Result<Vec<PendingStore>, Failure> {
    safe(root)?;
    let directory = root.join(account.to_string());
    if !directory.exists() {
        return Ok(Vec::new());
    }
    safe(&directory)?;
    let mut configs = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(app_id) = entry
            .file_name()
            .to_str()
            .and_then(|v| v.parse::<u32>().ok())
        else {
            continue;
        };
        let result = (|| {
            let Some(context): Option<Context> = read(&entry.path().join("context.json"))? else {
                return Ok(None);
            };
            if context.account_id != account
                || context.config.app_id != app_id
                || context.config.root != root
            {
                return Err(Failure::Storage);
            }
            Ok(context.needs_work.then_some(context.config))
        })();
        match result {
            Ok(Some(config)) => configs.push(PendingStore {
                app_id,
                config: Ok(config),
            }),
            Err(error) => configs.push(PendingStore {
                app_id,
                config: Err(error),
            }),
            Ok(None) => {}
        }
    }
    configs.sort_by_key(|store| store.app_id);
    Ok(configs)
}

#[cfg(test)]
mod removal_tests {
    use super::*;
    use steam_cloud::Preferences;

    const ACCOUNT: u64 = 0x0110_0001_0000_002a;
    const NAME: &str = "%GameInstall%saves/slot.sav";

    #[test]
    fn uninstall_archival_and_removal_preserve_the_existing_exit_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let config = Config {
            root: base.join("cloud"),
            home: base.join("home"),
            install: base.join("game"),
            app_id: 42,
        };
        for path in [&config.root, &config.home, &config.install.join("saves")] {
            crate::files::private_directory(path).unwrap();
        }
        let save = config.install.join("saves/slot.sav");
        fs::write(&save, b"captured at game exit").unwrap();
        let metadata = super::tests::save_rule("saves", "*.sav", "0");
        let listing = CloudListing {
            steam_id: ACCOUNT,
            app_id: config.app_id,
            change_number: 1,
            files: Vec::new(),
        };
        archive_with_rules(&config, metadata.clone(), listing.clone()).unwrap();
        let mut context = Context::load(&config, ACCOUNT).unwrap().unwrap();
        assert!(!context.needs_work);
        assert!(context.desired.is_none());
        let mut vault = Vault::open(&config.root, scope(&config, ACCOUNT)).unwrap();
        let exited = vault.capture(&context.bindings).unwrap();
        vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        let operation = vault
            .queue_upload(
                &exited,
                &context.remote,
                "test-exit",
                vault.authorize(ACCOUNT).unwrap(),
            )
            .unwrap();
        vault
            .begin_upload(
                &operation,
                vault.authorize(ACCOUNT).unwrap(),
                &context.remote,
            )
            .unwrap();
        context.desired = Some(exited.clone());
        context.needs_work = true;
        context.transaction = Some(directory(&config, ACCOUNT).unwrap().join("upload-existing"));
        context.installation_removed = false;
        context.save().unwrap();
        let before_archive_token = context.token().unwrap();
        drop(vault);

        fs::write(&save, b"edited before uninstall").unwrap();
        archive(&config, ACCOUNT).unwrap();
        archive_with_rules(&config, metadata, listing).unwrap();
        let archived = Context::load(&config, ACCOUNT).unwrap().unwrap();
        assert_eq!(archived.desired, Some(exited.clone()));
        assert_eq!(archived.transaction, context.transaction);
        assert!(archived.needs_work);
        assert!(archived.installation_removed);
        let old_token = archived.token().unwrap();
        assert_ne!(old_token, before_archive_token);
        assert!(matches!(removed(&config, ACCOUNT), Err(Failure::Conflict)));
        fs::remove_dir_all(&config.install).unwrap();

        removed(&config, ACCOUNT).unwrap();
        removed(&config, ACCOUNT).unwrap();

        let absent = Context::load(&config, ACCOUNT).unwrap().unwrap();
        assert!(absent.installation_removed);
        assert_eq!(absent.desired, Some(exited.clone()));
        assert_eq!(absent.transaction, context.transaction);
        assert_eq!(absent.remote, context.remote);
        assert!(absent.needs_work);
        assert_eq!(absent.token().unwrap(), old_token);
        assert!(!config.install.exists());
        let vault = Vault::open(&config.root, scope(&config, ACCOUNT)).unwrap();
        assert_eq!(vault.pending().unwrap().id, operation);
        assert_eq!(vault.pending().unwrap().desired, exited);
        assert!(vault.pending().unwrap().may_have_committed);
        assert_eq!(
            vault.read_content(&vault.local().unwrap().0[NAME]).unwrap(),
            b"edited before uninstall"
        );
        assert_eq!(
            vault.read_content(&exited.0[NAME]).unwrap(),
            b"captured at game exit"
        );

        let mut incomplete = serde_json::to_value(&absent).unwrap();
        incomplete
            .as_object_mut()
            .unwrap()
            .remove("installation_removed");
        assert!(serde_json::from_value::<Context>(incomplete).is_err());
    }

    #[test]
    fn interrupted_partial_uninstall_keeps_the_original_archive_and_removal_fence() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let config = Config {
            root: base.join("cloud"),
            home: base.join("home"),
            install: base.join("game"),
            app_id: 42,
        };
        for path in [&config.root, &config.home, &config.install] {
            crate::files::private_directory(path).unwrap();
        }
        fs::write(config.install.join("one.sav"), b"original one").unwrap();
        fs::write(config.install.join("two.sav"), b"original two").unwrap();
        let metadata = super::tests::save_rule("", "*.sav", "0");
        let listing = CloudListing {
            steam_id: ACCOUNT,
            app_id: 42,
            change_number: 0,
            files: Vec::new(),
        };
        archive_with_rules(&config, metadata.clone(), listing.clone()).unwrap();
        let before = Vault::open(&config.root, scope(&config, ACCOUNT))
            .unwrap()
            .local()
            .unwrap()
            .clone();
        assert_eq!(before.0.len(), 2);
        assert!(
            Context::load(&config, ACCOUNT)
                .unwrap()
                .unwrap()
                .installation_removed
        );

        // Recursive removal deletes one save, then fails with the directory and
        // another save still present. No successful `removed` notification runs.
        fs::remove_file(config.install.join("one.sav")).unwrap();
        fs::write(config.install.join("two.sav"), b"edited survivor").unwrap();
        assert!(config.install.is_dir());
        assert!(matches!(removed(&config, ACCOUNT), Err(Failure::Conflict)));
        archive(&config, ACCOUNT).unwrap();
        // The fresh-metadata path must obey the same retry policy.
        archive_with_rules(&config, metadata, listing).unwrap();

        let context = Context::load(&config, ACCOUNT).unwrap().unwrap();
        assert!(context.installation_removed);
        assert!(!context.removed_installation_absent().unwrap());
        assert!(!context.needs_work);
        assert!(context.desired.is_none());
        assert!(pending(&config.root, ACCOUNT).unwrap().is_empty());
        let vault = Vault::open(&config.root, scope(&config, ACCOUNT)).unwrap();
        assert!(vault.pending().is_none());
        let current = vault.local().unwrap();
        assert_eq!(current.0.len(), 2);
        assert_eq!(
            current.0["%GameInstall%one.sav"],
            before.0["%GameInstall%one.sav"]
        );
        assert_eq!(
            vault
                .read_content(&before.0["%GameInstall%one.sav"])
                .unwrap(),
            b"original one"
        );
        assert_eq!(
            vault
                .read_content(&current.0["%GameInstall%two.sav"])
                .unwrap(),
            b"edited survivor"
        );
        assert_eq!(
            fs::read(config.install.join("two.sav")).unwrap(),
            b"edited survivor"
        );
        assert!(!config.install.join("one.sav").exists());
    }
}
