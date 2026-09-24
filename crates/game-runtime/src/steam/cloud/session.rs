//! Steam Cloud as a Cloud sync uses it: the app's AutoCloud rules and Steam's
//! preferences, verified remote reads over the CM connection, and upload
//! transactions whose receipt can be settled after a lost reply.
use super::{store::Context, *};
use crate::{
    cloud::sync::{self, Availability, CloudSync, Kind, Storefront, Work},
    cloud_status::UnavailableReason,
    protocol::Credentials,
    supervisor::client::SessionAlive,
};
use cloud_vault::{
    AccountIdentity, Bindings, Manifest, Preferences, RemoteView, StoreScope, Vault,
};
use std::{fs, path::Path};
use steam_kit::{
    cloud::{CloudListing, CloudPreferencesCache, CloudUpload, UploadTransaction},
    cm::SteamConnection,
};

#[cfg(test)]
mod fake;
mod remote;
#[cfg(test)]
mod tests;
pub(crate) use remote::SteamCm;
pub(crate) use remote::{CloudConnection, CloudRemote, RemoteSource};

/// One Steam game's Cloud sync.
pub(crate) type Session<R = SteamCm> = CloudSync<Adapter<R>>;

/// Opens the Cloud sync of one Steam game over Steam's CM.
pub(crate) fn open(
    config: Config,
    credentials: Credentials,
    session_id: String,
    control: Arc<Control>,
) -> Result<Session, Failure> {
    open_with(SteamCm::new()?, config, credentials, session_id, control)
}

pub(crate) fn open_with<R: CloudRemote>(
    remote: R,
    config: Config,
    credentials: Credentials,
    session_id: String,
    control: Arc<Control>,
) -> Result<Session<R>, Failure> {
    store::directory(&config, credentials.steam_id)?;
    let adapter = Adapter {
        config,
        credentials,
        remote,
        preferences: CloudPreferencesCache::default(),
        downloaded: Manifest::default(),
        context: None,
    };
    CloudSync::open(adapter, control, session_id)
}

/// Steam Cloud for one app and account.
pub(crate) struct Adapter<R: CloudRemote = SteamCm> {
    config: Config,
    credentials: Credentials,
    remote: R,
    preferences: CloudPreferencesCache,
    /// Files this operation staged, reused by later reads in it.
    downloaded: Manifest,
    context: Option<Context>,
}

impl<R: CloudRemote> Adapter<R> {
    pub fn native_storage(&self) -> Result<Option<crate::NativeStorage>, crate::Error> {
        self.context
            .as_ref()
            .map(|context| context.prepare_sdk())
            .transpose()
            .map(Option::flatten)
            .map_err(Into::into)
    }
    fn context(&self) -> Result<&Context, Failure> {
        self.context.as_ref().ok_or(Failure::Storage)
    }
    fn context_mut(&mut self) -> Result<&mut Context, Failure> {
        self.context.as_mut().ok_or(Failure::Storage)
    }
    fn set_listing(
        &mut self,
        listing: CloudListing,
        remote: RemoteView,
        vault: &Vault,
    ) -> Result<(), Failure> {
        let context = self.context_mut()?;
        context.listing = listing;
        context.remote = remote;
        context.refresh_bindings(vault.local())?;
        context.save()
    }
    /// Unknown preferences never authorize a transfer, and an explicitly
    /// disabled one is not an error.
    async fn preferences(
        &mut self,
        cm: &mut R::Connection,
        vault: &mut Vault,
    ) -> Result<Availability, Failure> {
        let preferences = cm.preferences(&mut self.preferences).await?;
        vault.set_preferences(Preferences {
            account_enabled: preferences.account_enabled,
            game_enabled: preferences.game_enabled,
        });
        if preferences.account_enabled == Some(false) || preferences.game_enabled == Some(false) {
            return Ok(Availability::Unavailable(UnavailableReason::Disabled));
        }
        if preferences.account_enabled != Some(true) || preferences.game_enabled != Some(true) {
            return Err(Failure::Preferences);
        }
        Ok(Availability::Available)
    }
}

impl sync::Failure for Failure {
    fn kind(&self) -> Kind {
        match self {
            Self::Network | Self::Changed | Self::Uncertain => Kind::Transient,
            Self::Conflict => Kind::Conflict,
            Self::Preferences | Self::Unsupported | Self::Quota => Kind::Unavailable,
            Self::Authentication => Kind::Denied,
            Self::Cancelled => Kind::Cancelled,
            Self::Storage
            | Self::Ownership
            | Self::Recovery
            | Self::Busy
            | Self::SharedSaves
            | Self::RulesChanged => Kind::Blocking,
        }
    }
    fn key(&self) -> String {
        Failure::key(*self).into()
    }
    fn timed_out() -> Self {
        Self::Network
    }
    fn conflict() -> Self {
        Self::Conflict
    }
    fn recovery() -> Self {
        Self::Recovery
    }
    fn unsupported() -> Self {
        Self::Unsupported
    }
}

impl<R: CloudRemote> Storefront for Adapter<R> {
    type Failure = Failure;
    type Link = R::Connection;

    fn root(&self) -> &Path {
        &self.config.root
    }
    fn scope(&self) -> StoreScope {
        scope(&self.config, self.credentials.steam_id).into()
    }
    fn account(&self) -> AccountIdentity {
        self.credentials.steam_id.into()
    }
    fn load(&mut self) -> Result<(), Failure> {
        self.context = Context::load(&self.config, self.credentials.steam_id)?;
        Ok(())
    }
    fn work(&self) -> Option<Work> {
        self.context.as_ref().map(Context::work)
    }
    fn save(&mut self, work: Work) -> Result<(), Failure> {
        let context = self.context_mut()?;
        context.set_work(work);
        context.save()
    }
    fn installation_absent(&self) -> Result<bool, Failure> {
        store::safe(&self.config.install)?;
        match fs::symlink_metadata(&self.config.install) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
    fn bindings(
        &mut self,
        vault: &Vault,
        _: Option<&Manifest>,
        create: bool,
    ) -> Result<Bindings, Failure> {
        // The listing names every remote file, so it alone binds them.
        let context = self.context_mut()?;
        context.refresh_bindings(vault.local())?;
        if create {
            context.ensure_parents()?;
        }
        context.save()?;
        Ok(context.bindings.clone())
    }
    /// A game Steam has no saved rules for has nothing in the archive yet.
    fn offline(&self) -> bool {
        true
    }
    fn retained(&mut self) -> Option<Vec<Manifest>> {
        self.downloaded = Manifest::default();
        // A failed context save can leave newer in-memory values alongside an
        // older durable decision. Keep both until a later save commits.
        let saved = match Context::load(&self.config, self.credentials.steam_id) {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("Steam Cloud context unreadable: {error}");
                return None;
            }
        };
        Some(
            saved
                .iter()
                .chain(self.context.iter())
                .flat_map(Context::manifests)
                .collect(),
        )
    }

    async fn connect(&mut self, control: &Arc<Control>) -> Result<R::Connection, Failure> {
        control.check()?;
        let cm = self
            .remote
            .connect(&self.credentials, self.config.app_id, control.clone())
            .await?;
        control.check()?;
        Ok(cm)
    }
    async fn rules(
        &mut self,
        cm: &mut R::Connection,
        vault: &mut Vault,
        adopt: bool,
    ) -> Result<Availability, Failure> {
        let Some(metadata) = cm.cloud_metadata().await? else {
            return Ok(Availability::Unavailable(UnavailableReason::NoCloud));
        };
        if metadata
            .field("appidredirect")
            .is_some_and(|v| v.text() != Some("0"))
        {
            return Ok(Availability::Unavailable(UnavailableReason::Unsupported));
        }
        if metadata.field("savefiles").is_some() {
            steam_kit::cloud::MacCloudPaths::new(
                &metadata,
                self.credentials.steam_id,
                &self.config.home,
                &self.config.install,
            )?;
        }
        match &mut self.context {
            // A captured save can only be sent under the rules it was
            // captured with. A reinstall compares it under the new ones.
            Some(context) if !adopt && context.metadata != metadata => {
                return Err(if context.installation_removed {
                    Failure::RulesChanged
                } else {
                    Failure::Unsupported
                });
            }
            Some(context) => {
                context.config = self.config.clone();
                context.metadata = metadata;
            }
            None => {
                self.context = Some(Context::new(
                    &self.config,
                    self.credentials.steam_id,
                    metadata,
                ));
            }
        }
        // Learn the server's exact remote identities before adopting local
        // files. Optional root separators must not create archive aliases.
        let listing = cm.listing().await?;
        let context = self.context_mut()?;
        context.listing = listing;
        context.save()?;
        self.preferences(cm, vault).await
    }
    async fn settle(&mut self, cm: &mut R::Connection, vault: &Vault) -> Result<bool, Failure> {
        let pending = vault.pending().ok_or(Failure::Storage)?;
        if !pending.may_have_committed {
            return Ok(true);
        }
        let path = self
            .context()?
            .transaction
            .as_ref()
            .ok_or(Failure::Storage)?;
        let mut transaction = UploadTransaction::open(path)?;
        if transaction.steam_id() != self.credentials.steam_id
            || transaction.app_id() != self.config.app_id
        {
            return Err(Failure::Authentication);
        }
        // Keep the proven cleanup for an interrupted HTTP body. A lost
        // receipt must not permanently prevent a new comparison, though.
        let result = cm.settle_upload(&mut transaction).await;
        recovery_is_settled(result, transaction.is_settled())
    }
    fn reconciled(&mut self) -> Result<(), Failure> {
        let context = self.context_mut()?;
        context.transaction = None;
        context.save()
    }
    async fn view(
        &mut self,
        cm: &mut R::Connection,
        vault: &mut Vault,
        control: &Control,
    ) -> Result<RemoteView, Failure> {
        let listing = cm.listing().await?;
        let remote = self.read_remote(cm, vault, control, &listing).await?;
        self.set_listing(listing, remote.clone(), vault)?;
        Ok(remote)
    }
    async fn publish(
        &mut self,
        cm: &mut R::Connection,
        vault: &mut Vault,
        control: &Control,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<(), Failure> {
        let mut uploads = Vec::new();
        for (name, file) in &local.0 {
            if remote.files.0.get(name) != Some(file) {
                uploads.push(CloudUpload {
                    name: name.clone(),
                    bytes: vault.read_content(file)?,
                    timestamp: now(),
                    platforms: file.platforms,
                });
            }
        }
        let deletes = remote
            .files
            .0
            .keys()
            .filter(|name| !local.0.contains_key(*name))
            .cloned()
            .collect();
        // Full listing preserves foreign-platform entries. Only the managed
        // Mac subset supplies upload/deletion operations and the Vault baseline.
        let mut transaction = UploadTransaction::create(
            &store::directory(&self.config, self.credentials.steam_id)?,
            self.context()?.listing.clone(),
            uploads,
            deletes,
        )?;
        let authorization = vault.authorize(self.credentials.steam_id)?;
        let id = vault.queue_upload(local, remote, "steam-cm-autocloud", authorization)?;
        let context = self.context_mut()?;
        context.transaction = Some(transaction.directory().to_path_buf());
        context.save()?;
        let permit = vault.begin_upload(&id, authorization, remote)?;
        let paths = self.context()?.paths()?;
        let mut files = Manifest::default();
        let listing = cm
            .upload_verified(&mut transaction, |file, bytes| {
                if paths.existing_file(file)?.is_some() {
                    let version = vault
                        .stage(&bytes, file.platforms)
                        .map_err(|_| steam_kit::cloud::CloudError::Storage)?;
                    files.0.insert(file.name.clone(), version);
                }
                Ok(())
            })
            .await?;
        // The adapter read every surviving remote file and checked the complete
        // listing again. Only that successful receipt publishes staged bytes.
        let verified = RemoteView {
            revision: listing.change_number.to_string(),
            files,
        };
        control.check()?;
        vault.complete_upload(&permit, true, &verified)?;
        self.context_mut()?.transaction = None;
        self.set_listing(listing, verified, vault)
    }
}

// A retry can recompare after a lost receipt or failed best-effort close, while
// retaining hard failures such as revocation, corrupt storage and bad metadata.
fn recovery_is_settled(
    result: Result<(), steam_kit::cloud::CloudError>,
    settled: bool,
) -> Result<bool, Failure> {
    match result.map_err(Failure::from) {
        Ok(()) => Ok(settled),
        Err(Failure::Network | Failure::Changed | Failure::Uncertain) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Cloud maintenance of one Steam game, run inside Forge. `idle` proves that
/// no Steam launch or supervisor can start while it runs.
pub fn maintain(
    target: MaintenanceTarget,
    credentials: &Credentials,
    control: Arc<Control>,
    idle: &crate::steam::IdleGuard,
) -> Result<(), Failure> {
    let runtime = crate::executor()?;
    runtime.block_on(maintain_with(
        SteamCm::new()?,
        target,
        credentials,
        control,
        &|id| idle.session_alive(id),
    ))
}

pub(crate) async fn maintain_with<R: CloudRemote>(
    remote: R,
    target: MaintenanceTarget,
    credentials: &Credentials,
    control: Arc<Control>,
    alive: &SessionAlive<'_>,
) -> Result<(), Failure> {
    let config = match target {
        MaintenanceTarget::Installed(config) => config,
        MaintenanceTarget::Uninstalled { root, app_id } => {
            Context::load_app(&root, credentials.steam_id, app_id)?.config
        }
    };
    let id = uuid::Uuid::new_v4().to_string();
    open_with(remote, config, credentials.clone(), id, control)?
        .maintain(alive)
        .await
}
