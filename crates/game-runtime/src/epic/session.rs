//! EGS Cloud as a Cloud sync uses it: the game's Cloud rules, where its saves
//! live, its listing, fetches and the republication of a complete set.
use super::*;
use crate::cloud::record::{self, Record as _};
use crate::cloud::saves::{Archive, Rules};
use crate::cloud::sync::{self, Availability, CloudSync, Kind, Storefront};
use crate::cloud_status::UnavailableReason;
use crate::supervisor::client::SessionAlive;
use cloud_vault::{Manifest, Preferences, RemoteView, Vault};
use epic_kit::{
    EpicError,
    client::Client as EpicClient,
    cloud::{CloudCatalog, CloudGame, CloudRules, ResolvedCloudRules},
};
use std::{collections::BTreeMap, future::Future};
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

/// The note of why the game under `config` has no Cloud.
fn cloud_note(config: &Configuration) -> std::path::PathBuf {
    crate::diagnostics::cloud_note(
        &config.home,
        crate::diagnostics::Game::Epic(&config.app_name),
    )
}

/// Writes in the diagnostics of the game under `config` why it has no Cloud
/// here: its catalog names no save folder for its build, or `rules` Obelisk
/// cannot follow. `attributes` are what the catalog says of a Cloud. A
/// failure to write it changes nothing.
fn note_unavailable(
    config: &Configuration,
    rules: Option<&CloudRules>,
    attributes: &BTreeMap<String, String>,
) {
    use std::fmt::Write as _;
    let why = match rules {
        None => "the catalog names no Cloud save folder for this build",
        Some(_) => "Obelisk cannot follow the Cloud save rules",
    };
    let mut note = format!(
        "Epic Cloud is unavailable for {} ({}): {why}.\nBuild: {:?}\n",
        config.app_name, config.namespace, config.platform
    );
    if let Some(rules) = rules {
        let _ = writeln!(note, "Rules: {rules:?}");
    }
    note.push_str("Catalog attributes naming a Cloud:\n");
    for (name, value) in attributes {
        let _ = writeln!(note, "  {name} = {value}");
    }
    if attributes.is_empty() {
        note.push_str("  none\n");
    }
    let file = cloud_note(config);
    if file
        .parent()
        .is_some_and(|folder| crate::files::private_directory(folder).is_ok())
    {
        let _ = obelisk_core::write_atomic(&file, note.as_bytes());
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
                .flat_map(sync::Pending::manifests)
                .cloned()
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
        let catalog = self
            .api
            .cloud_configuration(
                &self.config.namespace,
                &self.config.catalog_item_id,
                &self.config.app_name,
                self.config.platform,
            )
            .await?;
        let rules = match catalog.rules {
            Some(rules)
                if supports_rules(
                    &self.config,
                    &self.account,
                    &rules,
                    vault,
                    self.context.is_some(),
                )? =>
            {
                rules
            }
            rules => {
                note_unavailable(&self.config, rules.as_ref(), &catalog.attributes);
                return Ok(Availability::Unavailable(UnavailableReason::Unsupported));
            }
        };
        // A game with Cloud now needs no note of why it had none.
        let _ = std::fs::remove_file(cloud_note(&self.config));
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
pub(crate) async fn maintain<R: CloudRemote>(
    config: Configuration,
    api: Arc<R>,
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

/// What an uninstall archives the saves of `account` under. A game with a
/// record keeps the rules it saved, as its Cloud sync refuses changed ones:
/// `rules`, the Cloud rules Epic gives now, is asked only without one. A
/// record of the game's other build whose rules name no folder for this
/// build has nothing of this build to capture.
pub(crate) async fn archive_under(
    config: &Configuration,
    account: String,
    rules: impl Future<Output = Result<Option<CloudRules>>>,
) -> Result<Archive<Context>> {
    let (vault, saved) = record::locked::<Context>(config, &account)?;
    if let Some(saved) = saved {
        let rules = if record::other_build(&saved, config) && !names_folder(&saved.rules, config) {
            Rules::Unfollowed(Some(saved))
        } else {
            Rules::Followed(saved)
        };
        return Ok(Archive { vault, rules });
    }
    let rules = match rules.await? {
        Some(rules) if supports_rules(config, &account, &rules, &vault, false)? => rules,
        _ => {
            return Ok(Archive {
                vault,
                rules: Rules::Unfollowed(None),
            });
        }
    };
    let context = Context {
        config: config.clone(),
        account_id: account,
        rules,
        pending: None,
        installation_removed: false,
    };
    Ok(Archive {
        vault,
        rules: Rules::Followed(context),
    })
}

/// Whether `rules` name a save folder for the build under `config`: Epic
/// keeps none of the saves of a build they name none for.
fn names_folder(rules: &CloudRules, config: &Configuration) -> bool {
    match config.cloud_platform() {
        epic_kit::cloud::CloudPlatform::Mac { .. } => rules.cloud_save_folder_mac.is_some(),
        epic_kit::cloud::CloudPlatform::Windows { .. } => rules.cloud_save_folder.is_some(),
    }
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
