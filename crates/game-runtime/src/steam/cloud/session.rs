//! Steam Cloud as a Cloud sync uses it: the app's AutoCloud rules and Steam's
//! preferences, verified remote reads over the CM connection, and upload
//! transactions whose receipt can be settled after a lost reply.
use super::{store::Context, *};
use crate::{
    cloud::{
        record::{self, Record as _},
        sync::{self, Availability, CloudSync, Kind, Storefront},
    },
    cloud_status::UnavailableReason,
    protocol::Credentials,
};
use cloud_vault::{Manifest, Preferences, Vault};
use steam_kit::{
    cloud::{CloudListing, CloudPreferencesCache, UploadTransaction},
    cm::SteamConnection,
};

#[cfg(test)]
mod fake;
mod remote;
#[cfg(test)]
mod tests;
pub(crate) use remote::SteamCm;
use remote::SteamRemote;
pub(crate) use remote::{CloudConnection, CloudRemote};

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
    let adapter = Adapter {
        config,
        credentials,
        remote,
        preferences: CloudPreferencesCache::default(),
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
    context: Option<Context>,
}

impl<R: CloudRemote> Adapter<R> {
    pub fn native_storage(
        &self,
    ) -> Result<Option<crate::steam::host::NativeStorage>, crate::steam::Error> {
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
    fn changed() -> Self {
        Self::Changed
    }
}

impl<R: CloudRemote> Storefront for Adapter<R> {
    type Failure = Failure;
    type Record = Context;
    type Link = R::Connection;
    type Remote<'a>
        = SteamRemote<'a, R::Connection>
    where
        Self: 'a;

    fn configuration(&self) -> &Config {
        &self.config
    }
    fn account(&self) -> &u64 {
        &self.credentials.steam_id
    }
    fn record(&self) -> Option<&Context> {
        self.context.as_ref()
    }
    fn record_mut(&mut self) -> &mut Option<Context> {
        &mut self.context
    }
    /// A game Steam has no saved rules for has nothing in the archive yet.
    fn offline(&self) -> bool {
        true
    }
    fn retained(&mut self) -> Option<Vec<Manifest>> {
        // A failed context save can leave newer in-memory values alongside an
        // older durable decision. Keep both until a later save commits.
        let saved = match Context::load(&self.config, &self.credentials.steam_id) {
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
        if !store::follows(&metadata, &self.config, self.credentials.steam_id)? {
            return Ok(Availability::Unavailable(UnavailableReason::Unsupported));
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
                record::rebind(context, self.config.clone());
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
    fn remote<'a>(
        &'a mut self,
        cm: &'a mut R::Connection,
        // The CM connection already stops with the sync's control.
        _: &'a Control,
    ) -> Result<SteamRemote<'a, R::Connection>, Failure> {
        Ok(SteamRemote {
            cm,
            context: self.context.as_mut().ok_or(Failure::Storage)?,
            config: &self.config,
            steam_id: self.credentials.steam_id,
        })
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

/// Cloud maintenance of one Steam game, run inside Obelisk over Steam's CM.
/// `idle` proves that no Steam launch or supervisor can start while it runs.
/// It blocks.
pub fn maintain(
    config: Config,
    credentials: &Credentials,
    control: Arc<Control>,
    idle: &crate::steam::IdleGuard,
) -> Result<(), Failure> {
    maintain_over(SteamCm::new()?, config, credentials, control, idle)
}

/// `maintain`, over the Steam Cloud `remote` connects to.
pub(crate) fn maintain_over<R: CloudRemote>(
    remote: R,
    config: Config,
    credentials: &Credentials,
    control: Arc<Control>,
    idle: &crate::steam::IdleGuard,
) -> Result<(), Failure> {
    let runtime = crate::executor()?;
    let id = uuid::Uuid::new_v4().to_string();
    runtime.block_on(async {
        open_with(remote, config, credentials.clone(), id, control)?
            .maintain(&|id| idle.session_alive(id))
            .await
    })
}
