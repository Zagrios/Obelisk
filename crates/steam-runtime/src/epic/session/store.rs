use super::*;
use std::{collections::BTreeSet, fs, os::unix::fs::MetadataExt, path::Path};
use steam_cloud::{Bindings, SelectedFile, StoreScope, journal};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Context {
    pub config: Configuration,
    pub rules: CloudRules,
    pub account_id: String,
    pub pending: Option<Pending>,
    pub installation_removed: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", deny_unknown_fields)]
pub(super) enum Pending {
    /// Persisted before game admission and kept until exit bytes are durable.
    Capture,
    Upload {
        desired: Manifest,
    },
    /// A Cloud choice is durable before changing the projection or baseline.
    Applying {
        desired: Manifest,
        remote: RemoteView,
        project: bool,
    },
}
impl Pending {
    pub fn files(&self) -> Option<&Manifest> {
        match self {
            Self::Capture => None,
            Self::Upload { desired } | Self::Applying { desired, .. } => Some(desired),
        }
    }
}
impl Context {
    pub fn path(config: &Configuration, account: &str) -> Result<PathBuf> {
        config.validate(account)?;
        let path = StoreScope::from(config.scope(account))
            .directory(&config.root)?
            .join("epic-context.json");
        safe(&path)?;
        Ok(path)
    }
    pub fn load(config: &Configuration, account: &str) -> Result<Option<Self>> {
        let context: Option<Self> = journal::read(&Self::path(config, account)?)?;
        if let Some(context) = &context {
            context.validate(config, account)?;
        }
        Ok(context)
    }
    pub fn validate(&self, config: &Configuration, account: &str) -> Result<()> {
        self.config.validate(account)?;
        if self.account_id != account
            || self.config.root != config.root
            || self.config.home != config.home
            || self.config.install != config.install
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
            &self.config.home,
            &self.config.install,
            &self.account_id,
            &self.config.app_name,
        )?)
    }
    pub fn save(&self) -> Result<()> {
        self.validate(&self.config, &self.account_id)?;
        journal::write(&Self::path(&self.config, &self.account_id)?, self)?;
        Ok(())
    }
    pub fn absent(&self) -> Result<bool> {
        safe(&self.config.install)?;
        match fs::symlink_metadata(&self.config.install) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Ok(metadata) if metadata.is_dir() => Ok(false),
            Ok(_) => Err(Error::Storage),
            Err(error) => Err(error.into()),
        }
    }
}

/// The Steam supervisor's storage guard, which this module shares verbatim.
pub(super) fn safe(path: &Path) -> Result<()> {
    crate::cloud::safe_storage_path(path).map_err(|_| Error::Storage)
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
        let path = forge_core::install::checked_file_path(&rules.directory, Path::new(&name))
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
    safe(path)?;
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
        safe(&directory)?;
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
