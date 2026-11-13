use super::*;
use crate::cloud::record;
pub(super) use crate::cloud::sync::Pending;
use crate::cloud::sync::Work;
use crate::files::safe_storage_path;
use cloud_vault::{AccountIdentity, Bindings, SelectedFile, StoreScope, journal};
use std::{collections::BTreeSet, fs, os::unix::fs::MetadataExt, path::Path};

/// An Epic game's Cloud record, `epic-context.json`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Context {
    pub config: Configuration,
    pub rules: CloudRules,
    pub account_id: String,
    pub pending: Option<Pending>,
    pub installation_removed: bool,
}

impl Context {
    pub fn path(config: &Configuration, account: &str) -> Result<PathBuf> {
        crate::launch_shell::validate::<Epic>(config, account)?;
        let path = StoreScope::from(config.scope(account))
            .directory(&config.root)?
            .join(Self::FILE);
        safe_storage_path(&path)?;
        Ok(path)
    }
    pub fn validate(&self, config: &Configuration, account: &str) -> Result<()> {
        crate::launch_shell::validate::<Epic>(&self.config, account)?;
        // A moved installation keeps its saves: only the game's identity binds.
        if self.account_id != account
            || self.config.root != config.root
            || self.config.home != config.home
            || self.config.namespace != config.namespace
            || self.config.app_name != config.app_name
            || self.config.catalog_item_id != config.catalog_item_id
            || self.config.session_directory != config.session_directory
        {
            return Err(Error::Storage);
        }
        if let Some(pending) = self.pending.as_ref().and_then(Pending::files) {
            pending.validate()?;
        }
        if let Some(Pending::Applying { remote, .. }) = &self.pending {
            remote.validate()?;
        }
        self.resolve()?;
        Ok(())
    }
    pub fn resolve(&self) -> Result<ResolvedCloudRules> {
        Ok(self.rules.resolve(
            &self.config.cloud_platform(),
            &self.config.install,
            &self.account_id,
            &self.config.app_name,
        )?)
    }
}

impl record::Record for Context {
    type Failure = Error;
    type Configuration = Configuration;
    type Account = str;
    const FILE: &'static str = "epic-context.json";

    fn identity(account: &str) -> AccountIdentity {
        account.into()
    }
    fn scope(config: &Configuration, account: &str) -> Result<StoreScope> {
        Self::path(config, account)?;
        Ok(config.scope(account).into())
    }
    fn load(config: &Configuration, account: &str) -> Result<Option<Self>> {
        let context: Option<Self> = journal::read(&Self::path(config, account)?)?;
        if let Some(context) = &context {
            context.validate(config, account)?;
        }
        Ok(context)
    }
    fn save(&self) -> Result<()> {
        self.validate(&self.config, &self.account_id)?;
        journal::write(&Self::path(&self.config, &self.account_id)?, self)?;
        Ok(())
    }
    fn check(&self) -> Result<()> {
        self.validate(&self.config, &self.account_id)
    }
    fn account(&self) -> &str {
        &self.account_id
    }
    fn configuration(&self) -> &Configuration {
        &self.config
    }
    fn configuration_mut(&mut self) -> &mut Configuration {
        &mut self.config
    }
    fn work(&self) -> Work {
        Work {
            pending: self.pending.clone(),
            installation_removed: self.installation_removed,
        }
    }
    fn set_work(&mut self, work: Work) {
        self.pending = work.pending;
        self.installation_removed = work.installation_removed;
    }
    fn bindings(
        &mut self,
        vault: &Vault,
        remote: Option<&Manifest>,
        create: bool,
    ) -> Result<Bindings> {
        bindings(self, vault, remote, create)
    }
}

/// Discover names before ownership activation without reading another account's bytes.
fn local_names(rules: &ResolvedCloudRules) -> Result<BTreeSet<String>> {
    rules.check_directory()?;
    let mut names = BTreeSet::new();
    if !rules.directory.exists() {
        return Ok(names);
    }
    let mut pending = vec![rules.directory.clone()];
    let mut visited = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            visited += 1;
            if visited > epic_kit::cloud::MAX_FILES * 4 {
                return Err(Error::Unsupported);
            }
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(Error::Storage);
            }
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(Error::Storage);
            }
            let relative = path
                .strip_prefix(&rules.directory)
                .map_err(|_| Error::Storage)?
                .to_str()
                .ok_or(Error::Unsupported)?;
            if rules.accepts(relative)? {
                if metadata.len() > epic_kit::cloud::MAX_FILE_BYTES as u64
                    || names.len() >= epic_kit::cloud::MAX_FILES
                {
                    return Err(Error::Unsupported);
                }
                names.insert(relative.to_owned());
            }
        }
    }
    Ok(names)
}

/// Bind the complete union, including deleted local and newly downloaded names.
/// The caller decides whether absent install paths may be created.
pub(super) fn bindings(
    context: &Context,
    vault: &Vault,
    remote: Option<&Manifest>,
    create: bool,
) -> Result<Bindings> {
    let rules = context.resolve()?;
    let mut names = local_names(&rules)?;
    for files in [
        vault.local(),
        context.pending.as_ref().and_then(Pending::files),
        vault.pending().map(|p| &p.desired),
        remote,
    ]
    .into_iter()
    .flatten()
    {
        for name in files.0.keys() {
            if !rules.accepts(name)? {
                return Err(Error::Unsupported);
            }
            names.insert(name.clone());
        }
    }
    if names.len() > epic_kit::cloud::MAX_FILES {
        return Err(Error::Unsupported);
    }
    let mut output = Bindings::new();
    for name in names {
        if !rules.accepts(&name)? {
            return Err(Error::Unsupported);
        }
        rules.check_directory()?;
        let path = obelisk_core::install::checked_file_path(&rules.directory, Path::new(&name))
            .map_err(epic_kit::EpicError::from)?;
        if create {
            ensure_parent(path.parent().ok_or(Error::Storage)?)?;
        }
        output.insert(name, SelectedFile { path, platforms: 2 });
    }
    rules.check_directory()?;
    Ok(output)
}

fn ensure_parent(path: &Path) -> Result<()> {
    safe_storage_path(path)?;
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => break,
            Ok(_) => return Err(Error::Storage),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current.parent().ok_or(Error::Storage)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for directory in missing.into_iter().rev() {
        safe_storage_path(&directory)?;
        match fs::create_dir(&directory) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Storage);
        }
    }
    Ok(())
}
