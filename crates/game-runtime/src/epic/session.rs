//! EGS Cloud as a Cloud sync uses it: the game's Cloud rules, where its saves
//! live, its listing, fetches and the republication of a complete set.
use super::*;
use crate::cloud::record::{self, Record as _};
use crate::cloud::sync::{self, Availability, CloudSync, Kind, Storefront};
use crate::cloud_status::UnavailableReason;
use crate::supervisor::client::SessionAlive;
use cloud_vault::{Manifest, Preferences, RemoteView, Vault};
use epic_kit::{
    EpicError,
    client::Client as EpicClient,
    cloud::{CloudGame, CloudRules, ResolvedCloudRules},
};
use std::collections::BTreeMap;
mod remote;
mod store;
pub(crate) use remote::CloudRemote;
use remote::{EpicRemote, Kept};
pub(crate) use store::Context;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

/// One Epic game's Cloud sync.
pub(crate) type Session<R> = CloudSync<Adapter<R>>;

/// Opens the Cloud sync of one Epic game for `account`. It reads nothing
/// remote until a sync runs.
pub(crate) fn open<R: CloudRemote>(
    config: Configuration,
    account: String,
    api: Arc<R>,
    control: Arc<Control>,
    session_id: String,
) -> Result<Session<R>> {
    let adapter = Adapter {
        config,
        account,
        api,
        context: None,
        kept: None,
    };
    CloudSync::open(adapter, control, session_id)
}

/// EGS Cloud for one game and account.
pub(crate) struct Adapter<R: CloudRemote> {
    config: Configuration,
    account: String,
    api: Arc<R>,
    context: Option<Context>,
    /// Set by each read view, taken by the next upload.
    kept: Option<Kept>,
}

impl sync::Failure for Error {
    fn kind(&self) -> Kind {
        match self {
            Self::Cancelled | Self::Epic(EpicError::Cancelled) => Kind::Cancelled,
            Self::Epic(EpicError::Unauthorized) => Kind::Denied,
            Self::Epic(EpicError::CloudConflict) => Kind::Conflict,
            Self::Failed
            | Self::Epic(EpicError::Network | EpicError::Http(408 | 429 | 500..=599))
            | Self::Vault(cloud_vault::Error::Stale) => Kind::Transient,
            Self::Unsupported | Self::Epic(EpicError::UnsupportedContent) => Kind::Unavailable,
            _ => Kind::Blocking,
        }
    }
    fn key(&self) -> String {
        self.to_string()
    }
    fn timed_out() -> Self {
        Self::Failed
    }
    fn conflict() -> Self {
        Self::Epic(EpicError::CloudConflict)
    }
    fn recovery() -> Self {
        Self::Recovery
    }
    fn unsupported() -> Self {
        Self::Unsupported
    }
    fn changed() -> Self {
        Self::Epic(EpicError::CloudConflict)
    }
}

impl<R: CloudRemote> Adapter<R> {
    /// Writes in the game's diagnostics why it has no Cloud here: what the
    /// catalog says of it, and the save folder Obelisk could not follow. A
    /// failure to write it changes nothing.
    async fn note_unavailable(&self, why: &str, rules: Option<&CloudRules>) {
        let attributes = self
            .api
            .cloud_attributes(
                &self.config.namespace,
                &self.config.catalog_item_id,
                &self.config.app_name,
            )
            .await
            .unwrap_or_default();
        let mut note = format!(
            "Epic Cloud is unavailable for {} ({}): {why}.\nBuild: {:?}\n",
            self.config.app_name, self.config.namespace, self.config.platform
        );
        if let Some(rules) = rules {
            note.push_str(&format!("Rules: {rules:?}\n"));
        }
        note.push_str("Catalog attributes naming a Cloud:\n");
        for (name, value) in &attributes {
            note.push_str(&format!("  {name} = {value}\n"));
        }
        if attributes.is_empty() {
            note.push_str("  none\n");
        }
        let folder = crate::diagnostics::folder(&self.config.home, crate::queue::Storefront::Epic);
        let file = folder.join(format!("cloud-{}.log", self.config.app_name));
        let _ = std::fs::create_dir_all(&folder);
        let _ = std::fs::write(file, note);
    }
}

impl<R: CloudRemote> Storefront for Adapter<R> {
    type Failure = Error;
    type Record = Context;
    type Link = ();
    type Remote<'a>
        = EpicRemote<'a, R>
    where
        Self: 'a;

    fn configuration(&self) -> &Configuration {
        &self.config
    }
    fn account(&self) -> &str {
        &self.account
    }
    fn record(&self) -> Option<&Context> {
        self.context.as_ref()
    }
    fn record_mut(&mut self) -> &mut Option<Context> {
        &mut self.context
    }
    /// Without saved rules, nothing tells where the saves live.
    fn offline(&self) -> bool {
        self.context.is_some()
    }
    fn retained(&mut self) -> Option<Vec<Manifest>> {
        self.kept = None;
        // A failed save can leave newer values in memory than on disk.
        let saved = match Context::load(&self.config, &self.account) {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("Epic Cloud context unreadable: {error}");
                return None;
            }
        };
        Some(
            saved
                .iter()
                .chain(self.context.iter())
                .filter_map(|context| context.pending.as_ref())
                .flat_map(|pending| match pending {
                    sync::Pending::Applying {
                        desired, remote, ..
                    } => vec![desired.clone(), remote.files.clone()],
                    pending => pending.files().cloned().into_iter().collect(),
                })
                .collect(),
        )
    }

    async fn connect(&mut self, _: &Arc<Control>) -> Result<()> {
        // The authenticated session, never a remembered account.
        if self.api.account_id().await? != self.account {
            return Err(Error::Session);
        }
        Ok(())
    }
    async fn rules(&mut self, _: &mut (), vault: &mut Vault, _: bool) -> Result<Availability> {
        let rules = self
            .api
            .cloud_configuration(
                &self.config.namespace,
                &self.config.catalog_item_id,
                &self.config.app_name,
                self.config.platform,
            )
            .await?;
        let Some(rules) = rules else {
            self.note_unavailable(
                "the catalog names no Cloud save folder for this build",
                None,
            )
            .await;
            return Ok(Availability::Unavailable(UnavailableReason::Unsupported));
        };
        if !supports_rules(
            &self.config,
            &self.account,
            &rules,
            vault,
            self.context.is_some(),
        )? {
            self.note_unavailable("Obelisk cannot follow the Cloud save rules", Some(&rules))
                .await;
            return Ok(Availability::Unavailable(UnavailableReason::Unsupported));
        }
        // Reinterpreting an existing account snapshot under new save rules can
        // turn unselected files into deletions. Require a dedicated conversion.
        if self
            .context
            .as_ref()
            .is_some_and(|context| context.rules != rules)
        {
            return Err(Error::Unsupported);
        }
        let context = self.context.get_or_insert_with(|| Context {
            config: self.config.clone(),
            rules,
            account_id: self.account.clone(),
            pending: None,
            installation_removed: false,
        });
        record::rebind(context, self.config.clone());
        context.save()?;
        // Supported publisher rules activate Obelisk's Cloud policy. These are
        // not preferences read from Epic's official launcher; no such account
        // preference endpoint has been established for this protocol.
        vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        Ok(Availability::Available)
    }
    /// A revision-checked upload either landed or did not: a fresh view
    /// always decides again.
    async fn settle(&mut self, _: &mut (), _: &Vault) -> Result<bool> {
        Ok(false)
    }
    fn reconciled(&mut self) -> Result<()> {
        Ok(())
    }
    fn remote<'a>(&'a mut self, _: &'a mut (), control: &'a Control) -> Result<EpicRemote<'a, R>> {
        let context = self.context.as_ref().ok_or(Error::Recovery)?;
        Ok(EpicRemote {
            api: &self.api,
            game: CloudGame {
                namespace: &self.config.namespace,
                catalog_item_id: &self.config.catalog_item_id,
                app_name: &self.config.app_name,
            },
            rules: &context.rules,
            kept: &mut self.kept,
            unmanaged: None,
            control,
        })
    }
}

fn upload_files(
    vault: &Vault,
    rules: &CloudRules,
    remote: BTreeMap<String, Vec<u8>>,
    local: &Manifest,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let filters = rules.filters()?;
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for (name, bytes) in remote {
        if !filters.accepts(&name)? {
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or(Error::Unsupported)?;
            if bytes.len() > epic_kit::cloud::MAX_FILE_BYTES
                || total > epic_kit::cloud::MAX_TOTAL_BYTES as u64
            {
                return Err(Error::Unsupported);
            }
            files.insert(name, bytes);
        }
    }
    for (name, file) in &local.0 {
        if !filters.accepts(name)? {
            return Err(Error::Unsupported);
        }
        total = total.checked_add(file.size).ok_or(Error::Unsupported)?;
        if file.size > epic_kit::cloud::MAX_FILE_BYTES as u64
            || total > epic_kit::cloud::MAX_TOTAL_BYTES as u64
        {
            return Err(Error::Unsupported);
        }
        files.insert(name.clone(), vault.read_content(file)?);
    }
    if files.len() > epic_kit::cloud::MAX_FILES {
        return Err(Error::Unsupported);
    }
    Ok(files)
}

/// Cloud maintenance of one Epic game, run inside Obelisk with the UI's
/// authenticated client. `idle` proves no Epic launch or supervisor can
/// start while it runs.
pub async fn maintain(
    config: Configuration,
    api: Arc<EpicClient>,
    control: Arc<Control>,
    idle: &IdleGuard,
) -> Result<()> {
    maintain_with(config, api, control, &|id| idle.session_alive(id)).await
}

pub(crate) async fn maintain_with<R: CloudRemote>(
    config: Configuration,
    api: Arc<R>,
    control: Arc<Control>,
    alive: &SessionAlive<'_>,
) -> Result<()> {
    let account = api.account_id().await?;
    let id = uuid::Uuid::new_v4().to_string();
    open(config, account, api, control, id)?
        .maintain(alive)
        .await
}

pub async fn archive(config: Configuration, api: &EpicClient) -> Result<()> {
    archive_with(config, api).await
}

async fn archive_with(config: Configuration, api: &impl CloudRemote) -> Result<()> {
    let account = api.account_id().await?;
    let (mut vault, saved) = record::locked::<Context>(&config, &account)?;
    let context = if let Some(saved) = saved {
        saved
    } else {
        let Some(rules) = api
            .cloud_configuration(
                &config.namespace,
                &config.catalog_item_id,
                &config.app_name,
                config.platform,
            )
            .await?
        else {
            if record::holds_saves(&vault, false) {
                return Err(Error::Unsupported);
            }
            return Ok(());
        };
        if !supports_rules(&config, &account, &rules, &vault, false)? {
            return Ok(());
        }
        Context {
            config: config.clone(),
            account_id: account,
            rules,
            pending: None,
            installation_removed: false,
        }
    };
    // Capture rehashes every save file. The caller shares one runtime with
    // downloads and the UI, so that work does not belong on it.
    tokio::task::spawn_blocking(move || record::archive(&mut vault, context, config))
        .await
        .map_err(|_| Error::Session)?
}

/// Whether Obelisk can follow `rules`. Rules it cannot follow leave a game
/// without Cloud, unless Obelisk holds its saves or `saved` says it has a
/// record.
fn supports_rules(
    config: &Configuration,
    account: &str,
    rules: &CloudRules,
    vault: &Vault,
    saved: bool,
) -> Result<bool> {
    match rules.resolve(
        &config.cloud_platform(),
        &config.install,
        account,
        &config.app_name,
    ) {
        Ok(_) => Ok(true),
        Err(EpicError::UnsupportedContent) if !record::holds_saves(vault, saved) => Ok(false),
        Err(error) => Err(error.into()),
    }
}
