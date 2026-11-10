use super::paths::Paths;
use super::*;
use crate::cloud::record;
use crate::cloud::saves::{Archive, Rules};
use crate::cloud::sync::{Pending, Work};
use crate::files::safe_storage_path;
use cloud_vault::{
    AccountIdentity, Bindings, Manifest, RemoteView, SelectedFile, StoreScope, Vault,
};
use serde::de::DeserializeOwned;
use std::{fs, path::Path};
use steam_kit::cloud::{AutoCloudPaths, CloudListing, CloudMetadata};

#[cfg(test)]
mod tests;

/// A Steam game's Cloud record, `context.json`. Its pending work keeps the
/// shape it had before the Cloud sync was shared: `needs_work`, `desired` and
/// `applying`.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Context {
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
    /// A chosen Cloud version being applied over `desired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applying: Option<Applying>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Applying {
    pub remote: RemoteView,
    pub project: bool,
}

pub(super) fn directory(config: &Config, account: u64) -> Result<PathBuf, Failure> {
    for path in [&config.root, &config.home, &config.install] {
        safe_storage_path(path)?;
    }
    let directory = StoreScope::from(scope(config, account)).directory(&config.root)?;
    safe_storage_path(&directory)?;
    Ok(directory)
}

pub(super) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Failure> {
    safe_storage_path(path)?;
    Ok(cloud_vault::journal::read(path)?)
}
pub(super) fn write(path: &Path, value: &impl Serialize) -> Result<(), Failure> {
    safe_storage_path(path)?;
    let parent = path.parent().ok_or(Failure::Storage)?;
    crate::files::private_directory(parent).map_err(|_| Failure::Storage)?;
    Ok(cloud_vault::journal::write(path, value)?)
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
            applying: None,
        }
    }
    /// Every manifest this context references, which cleanup must keep.
    pub fn manifests(&self) -> Vec<Manifest> {
        let mut manifests = vec![self.remote.files.clone()];
        manifests.extend(self.desired.clone());
        manifests.extend(self.applying.iter().map(|a| a.remote.files.clone()));
        manifests
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
    pub(super) fn paths(&self) -> Result<Paths, Failure> {
        // RemoteStorage files live in the macOS Steam engine's storage, whatever
        // the game's platform: a Windows game reaches it through lsteamclient.
        if self.metadata.field("savefiles").is_none() {
            return Ok(Paths::Sdk(self.sdk_paths()?));
        }
        let auto = AutoCloudPaths::new(
            &self.metadata,
            self.account_id,
            &self.config.platform(),
            &self.config.install,
        )?;
        if self.metadata.field("quota").is_some() {
            Ok(Paths::Mixed(auto, self.sdk_paths()?))
        } else {
            Ok(Paths::Auto(auto))
        }
    }
    pub fn refresh_bindings(&mut self, local: Option<&Manifest>) -> Result<(), Failure> {
        let paths = self.paths()?;
        let selected = paths.local_files(&self.listing.files)?;
        let mut foreign_paths = std::collections::BTreeSet::new();
        let mask = self.config.platform().mask();
        for file in &self.listing.files {
            if file.platforms & mask == 0
                && let Some(path) = paths.destination(&file.name, mask)?
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
            safe_storage_path(&binding.path)?;
        }
        self.bindings = bindings;
        Ok(())
    }
    pub fn ensure_parents(&self) -> Result<(), Failure> {
        if !self.config.install.is_dir() {
            return Err(Failure::Recovery);
        }
        for binding in self.bindings.values() {
            safe_storage_path(&binding.path)?;
            fs::create_dir_all(binding.path.parent().ok_or(Failure::Storage)?)?;
        }
        Ok(())
    }
}

impl record::Record for Context {
    type Failure = Failure;
    type Configuration = Config;
    type Account = u64;
    const FILE: &'static str = "context.json";

    fn identity(account: &u64) -> AccountIdentity {
        (*account).into()
    }
    fn scope(config: &Config, account: &u64) -> Result<StoreScope, Failure> {
        directory(config, *account)?;
        Ok(scope(config, *account).into())
    }
    fn load(config: &Config, account: &u64) -> Result<Option<Self>, Failure> {
        let context: Option<Self> = read(&directory(config, *account)?.join(Self::FILE))?;
        if let Some(context) = &context {
            context.validate(config, *account)?;
        }
        Ok(context)
    }
    fn save(&self) -> Result<(), Failure> {
        write(
            &directory(&self.config, self.account_id)?.join(Self::FILE),
            self,
        )
    }
    fn check(&self) -> Result<(), Failure> {
        self.validate(&self.config, self.account_id)
    }
    fn account(&self) -> &u64 {
        &self.account_id
    }
    fn configuration(&self) -> &Config {
        &self.config
    }
    fn configuration_mut(&mut self) -> &mut Config {
        &mut self.config
    }
    fn work(&self) -> Work {
        let pending = match (self.needs_work, &self.desired, &self.applying) {
            (true, Some(desired), Some(applying)) => Some(Pending::Applying {
                desired: desired.clone(),
                remote: applying.remote.clone(),
                project: applying.project,
            }),
            (true, Some(desired), None) => Some(Pending::Upload {
                desired: desired.clone(),
            }),
            (true, None, _) => Some(Pending::Capture),
            (false, _, _) => None,
        };
        Work {
            pending,
            installation_removed: self.installation_removed,
        }
    }
    fn set_work(&mut self, work: Work) {
        self.installation_removed = work.installation_removed;
        (self.needs_work, self.desired, self.applying) = match work.pending {
            None => (false, None, None),
            Some(Pending::Capture) => (true, None, None),
            Some(Pending::Upload { desired }) => (true, Some(desired), None),
            Some(Pending::Applying {
                desired,
                remote,
                project,
            }) => (true, Some(desired), Some(Applying { remote, project })),
        };
    }
    /// An upload transaction may have committed.
    fn uncertain(&self) -> bool {
        self.transaction.is_some()
    }
    fn bindings(
        &mut self,
        vault: &Vault,
        _: Option<&Manifest>,
        create: bool,
    ) -> Result<Bindings, Failure> {
        // The listing names every remote file, so it alone binds them.
        self.refresh_bindings(vault.local())?;
        if create {
            self.ensure_parents()?;
        }
        Ok(self.bindings.clone())
    }
}

/// Whether Steam keeps saves of this build under rules Obelisk follows. It
/// keeps none behind an app-id redirect, nor when its Auto-Cloud rules are all
/// for other platforms, as KIBORG's Mac build, whose rules are all for Windows
/// and Linux.
pub(super) fn follows(
    metadata: &CloudMetadata,
    config: &Config,
    steam_id: u64,
) -> Result<bool, Failure> {
    if metadata
        .field("appidredirect")
        .is_some_and(|v| v.text() != Some("0"))
    {
        return Ok(false);
    }
    if metadata.field("savefiles").is_some() {
        match AutoCloudPaths::new(metadata, steam_id, &config.platform(), &config.install) {
            Err(steam_kit::cloud::CloudError::OtherPlatforms) => return Ok(false),
            paths => {
                paths?;
            }
        }
    }
    Ok(true)
}

/// What an uninstall archives the saves under: the Cloud rules and listing
/// Steam gives now, for the account they were read for.
pub(crate) fn archive_under(
    config: &Config,
    metadata: CloudMetadata,
    listing: CloudListing,
) -> Result<Archive<Context>, Failure> {
    if listing.app_id != config.app_id {
        return Err(Failure::Authentication);
    }
    let account = listing.steam_id;
    let (vault, saved) = record::locked::<Context>(config, &account)?;
    if !follows(&metadata, config, account)? {
        return Ok(Archive {
            vault,
            rules: Rules::Unfollowed(saved),
        });
    }
    let mut context = saved.unwrap_or_else(|| Context::new(config, account, metadata.clone()));
    context.metadata = metadata;
    context.listing = listing;
    Ok(Archive {
        vault,
        rules: Rules::Followed(context),
    })
}
