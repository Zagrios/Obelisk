//! Each game's Cloud record, whatever the storefront: the configuration that
//! places its saves, the work a Cloud sync leaves and the removal fence. A
//! storefront plugs in its saved record as a `Record`, which its Cloud sync
//! adapter holds during a sync and which `saves` reads between Game sessions.
use super::sync::{self, Work};
use crate::files::safe_storage_path;
use cloud_vault::{AccountIdentity, Bindings, Manifest, StoreScope, Vault};
use serde::de::DeserializeOwned;
use std::{fs, path::Path};

/// Where a game is installed and keeps its saves, as its storefront configures it.
pub(crate) trait Configuration: Clone {
    /// The Cloud archive.
    fn root(&self) -> &Path;
    fn install(&self) -> &Path;
    /// Set for a Windows build: its saves live in this Wine prefix.
    fn prefix(&self) -> Option<&Path>;
}

/// A storefront's saved Cloud record of one game: the configuration that holds
/// its saves, its Cloud rules and the work a Cloud sync left.
pub(crate) trait Record: Sized + DeserializeOwned {
    type Failure: sync::Failure;
    type Configuration: Configuration;
    /// Steam's SteamID, Epic's account id.
    type Account: ?Sized;
    /// The record's file in the game's directory of the Cloud archive.
    const FILE: &'static str;

    /// The account as the vault checks it.
    fn identity(account: &Self::Account) -> AccountIdentity;
    /// The game's scope, once `configuration` names only safe paths for `account`.
    fn scope(
        configuration: &Self::Configuration,
        account: &Self::Account,
    ) -> Result<StoreScope, Self::Failure>;
    /// Under the vault's lock. `None` until the game's Cloud rules were known once.
    fn load(
        configuration: &Self::Configuration,
        account: &Self::Account,
    ) -> Result<Option<Self>, Self::Failure>;
    fn save(&self) -> Result<(), Self::Failure>;
    /// A record read without the lock names one account and holds valid manifests.
    fn check(&self) -> Result<(), Self::Failure>;
    fn account(&self) -> &Self::Account;
    fn configuration(&self) -> &Self::Configuration;
    fn configuration_mut(&mut self) -> &mut Self::Configuration;
    fn work(&self) -> Work;
    fn set_work(&mut self, work: Work);
    /// An upload whose outcome is unknown waits to be settled.
    fn uncertain(&self) -> bool {
        false
    }
    /// Where each save lives: the local set, and `remote`'s names. `create`
    /// makes their directories inside an installation that exists. Nothing
    /// is saved: the caller saves the record once it holds what it needs.
    fn bindings(
        &mut self,
        vault: &Vault,
        remote: Option<&Manifest>,
        create: bool,
    ) -> Result<Bindings, Self::Failure>;
}

/// Whether the installation is gone. Anything but a folder there is a storage
/// failure.
pub(crate) fn absent(install: &Path) -> cloud_vault::Result<bool> {
    safe_storage_path(install)?;
    match fs::symlink_metadata(install) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Ok(metadata) if metadata.is_dir() => Ok(false),
        Ok(_) => Err(cloud_vault::Error::Invalid),
        Err(error) => Err(error.into()),
    }
}

/// Obelisk holds this game's saves, so its Cloud cannot silently vanish.
/// `recorded` says whether the game has a record.
pub(crate) fn holds_saves(vault: &Vault, recorded: bool) -> bool {
    recorded
        || vault.pending().is_some()
        || vault.game_session().is_some()
        || vault.local().is_some()
        || vault.baseline().is_some()
}

/// Captures the saves `bindings` name. A save whose folder is gone is left
/// out: a capture never recreates a folder. Behind the removal fence, each
/// save missing now keeps its archived version.
pub(crate) fn capture(
    vault: &mut Vault,
    mut bindings: Bindings,
    fenced: bool,
) -> cloud_vault::Result<Manifest> {
    bindings.retain(|_, file| file.path.parent().is_some_and(Path::is_dir));
    if fenced {
        vault.capture_preserving_missing(&bindings)
    } else {
        vault.capture(&bindings)
    }
}

/// Adopts the configuration the game runs with now. A Windows game whose
/// engine moved it to another Wine prefix saves there, still without the
/// saves of the old one. As after a reinstall, the removal fence goes up: a
/// save missing there keeps its archived version and is restored, never sent
/// as deleted.
pub(crate) fn rebind<R: Record>(record: &mut R, configuration: R::Configuration) {
    if record.configuration().prefix() != configuration.prefix() {
        fence(record);
    }
    *record.configuration_mut() = configuration;
}

/// Whether `record` places the saves of the game's other build than the one
/// under `configuration`: a Windows build keeps its saves in a Wine prefix, a
/// Mac build in none. A build its engine moved to another prefix is the same.
pub(crate) fn other_build<R: Record>(record: &R, configuration: &R::Configuration) -> bool {
    record.configuration().prefix().is_some() != configuration.prefix().is_some()
}

/// From now on, a save missing locally may be one an uninstall removed.
pub(crate) fn fence<R: Record>(record: &mut R) {
    let mut work = record.work();
    work.installation_removed = true;
    record.set_work(work);
}

/// The game's vault, which holds its storefront's lock, and its record.
pub(crate) fn locked<R: Record>(
    configuration: &R::Configuration,
    account: &R::Account,
) -> Result<(Vault, Option<R>), R::Failure> {
    let scope = R::scope(configuration, account)?;
    let vault = Vault::open(configuration.root(), scope)?;
    let record = R::load(configuration, account)?;
    Ok((vault, record))
}
