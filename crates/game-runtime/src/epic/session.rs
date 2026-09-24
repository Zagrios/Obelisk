//! EGS Cloud as a Cloud sync uses it: the game's Cloud rules, where its saves
//! live, the complete remote snapshot and its republication.
use super::*;
use crate::cloud::sync::{self, Availability, CloudSync, Kind, Storefront, Work};
use crate::cloud_status::{self, UnavailableReason};
use crate::supervisor::client::SessionAlive;
use cloud_vault::{
    AccountIdentity, Bindings, Manifest, Preferences, RemoteView, StoreScope, Vault,
};
use epic_kit::{
    EpicError,
    client::Client as EpicClient,
    cloud::{CloudGame, CloudRules, ResolvedCloudRules, Snapshot as CloudSnapshot},
};
use std::{collections::BTreeMap, path::Path};
mod remote;
mod store;
pub(crate) use remote::CloudRemote;
use store::Context;
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
    config.validate(&account)?;
    store::safe(&config.root)?;
    let adapter = Adapter {
        config,
        account,
        api,
        context: None,
        snapshot: None,
    };
    CloudSync::open(adapter, control, session_id)
}

/// EGS Cloud for one game and account.
pub(crate) struct Adapter<R: CloudRemote> {
    config: Configuration,
    account: String,
    api: Arc<R>,
    context: Option<Context>,
    /// The complete snapshot the last view verified. An upload republishes
    /// its files that Forge does not manage.
    snapshot: Option<CloudSnapshot>,
}

impl<R: CloudRemote> Adapter<R> {
    fn context(&self) -> Result<&Context> {
        self.context.as_ref().ok_or(Error::Recovery)
    }
    fn cloud_game(&self) -> CloudGame<'_> {
        CloudGame {
            namespace: &self.config.namespace,
            catalog_item_id: &self.config.catalog_item_id,
            app_name: &self.config.app_name,
        }
    }
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
}

impl<R: CloudRemote> Storefront for Adapter<R> {
    type Failure = Error;
    type Link = ();

    fn root(&self) -> &Path {
        &self.config.root
    }
    fn scope(&self) -> StoreScope {
        self.config.scope(&self.account).into()
    }
    fn account(&self) -> AccountIdentity {
        self.account.as_str().into()
    }
    fn load(&mut self) -> Result<()> {
        self.context = Context::load(&self.config, &self.account)?;
        Ok(())
    }
    fn work(&self) -> Option<Work> {
        self.context.as_ref().map(Context::work)
    }
    fn save(&mut self, work: Work) -> Result<()> {
        let context = self.context.as_mut().ok_or(Error::Recovery)?;
        context.pending = work.pending;
        context.installation_removed = work.installation_removed;
        context.save()
    }
    fn installation_absent(&self) -> Result<bool> {
        store::absent(&self.config)
    }
    fn bindings(
        &mut self,
        vault: &Vault,
        remote: Option<&Manifest>,
        create: bool,
    ) -> Result<Bindings> {
        store::bindings(self.context()?, vault, remote, create)
    }
    /// Without saved rules, nothing tells where the saves live.
    fn offline(&self) -> bool {
        self.context.is_some()
    }
    fn retained(&mut self) -> Option<Vec<Manifest>> {
        self.snapshot = None;
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
                &self.config.platform,
            )
            .await?;
        let Some(rules) = rules else {
            return Ok(Availability::Unavailable(UnavailableReason::Unsupported));
        };
        if !supports_rules(
            &self.config,
            &self.account,
            &rules,
            vault,
            self.context.as_ref(),
        )? {
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
        context.config = self.config.clone();
        context.save()?;
        // Supported publisher rules activate Forge's Cloud policy. These are
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
    async fn view(
        &mut self,
        _: &mut (),
        vault: &mut Vault,
        control: &Control,
    ) -> Result<RemoteView> {
        let full = self
            .api
            .cloud_download(self.cloud_game(), control.cancellation())
            .await?;
        let remote = stage_remote(vault, &self.context()?.rules, &full)?;
        self.snapshot = Some(full);
        Ok(remote)
    }
    async fn publish(
        &mut self,
        _: &mut (),
        vault: &mut Vault,
        control: &Control,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<()> {
        let full = self
            .snapshot
            .take()
            .filter(|full| full.revision == remote.revision)
            .ok_or(Error::Recovery)?;
        let rules = self.context()?.rules.clone();
        // Retained remote bytes move into the upload set; the snapshot's copy
        // would otherwise stay live alongside the packaged one.
        let revision = full.revision;
        let files = upload_files(vault, &rules, full.files, local)?;
        control.check()?;
        let (api, game) = (&self.api, self.cloud_game());
        vault
            .upload(
                self.account.as_str(),
                local,
                remote,
                "epic-egs-cloud",
                || Ok(()),
                async |vault| {
                    let verified = api
                        .cloud_upload(game, &rules, &files, &revision, control.cancellation())
                        .await?;
                    control.check()?;
                    Ok((stage_remote(vault, &rules, &verified)?, ()))
                },
            )
            .await
    }
}

fn stage_remote(vault: &Vault, rules: &CloudRules, full: &CloudSnapshot) -> Result<RemoteView> {
    let filters = rules.filters()?;
    let mut files = BTreeMap::new();
    for (name, bytes) in &full.files {
        if filters.accepts(name)? {
            files.insert(name.clone(), vault.stage(bytes, 2)?);
        }
    }
    let view = RemoteView {
        revision: full.revision.clone(),
        files: Manifest(files),
    };
    view.validate()?;
    Ok(view)
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

pub(crate) fn pending(root: &Path, account: &str) -> Result<Vec<Configuration>> {
    store::safe(root)?;
    let directory = cloud_vault::epic_account_directory(root, account)?;
    store::safe(&directory)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut configs = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        // One unreadable game must not hide the others' pending work.
        match pending_game(root, account, &entry.path()) {
            Ok(Some(config)) => configs.push(config),
            Ok(None) => {}
            Err(error) => eprintln!("Epic Cloud pending work unreadable: {error}"),
        }
    }
    Ok(configs)
}

fn pending_game(root: &Path, account: &str, directory: &Path) -> Result<Option<Configuration>> {
    let path = directory.join("epic-context.json");
    store::safe(&path)?;
    let Some(context): Option<Context> = cloud_vault::journal::read(&path)? else {
        return Ok(None);
    };
    context.validate(&context.config, account)?;
    if context.config.root != root || Context::path(&context.config, account)? != path {
        return Err(Error::Storage);
    }
    // Vault work outlives the context marker: an uncertain upload or an
    // interrupted replacement still needs this game's next sync.
    let vault = cloud_vault::peek(root, context.config.scope(account))?;
    Ok(
        (context.pending.is_some() || vault.pending.is_some() || vault.applying)
            .then_some(context.config),
    )
}

/// One Epic game's Cloud indicator, with the saved configuration that Cloud
/// maintenance of an uninstalled game needs.
pub struct EpicIndicator {
    pub namespace: String,
    pub app_name: String,
    pub indicator: cloud_status::Indicator,
    pub configuration: Option<Configuration>,
}

/// The Cloud indicators of `games` (namespace, app name), plus every other
/// game of this account with pending work, such as an uninstalled one.
/// Nothing here takes a lock: a game or maintenance may be writing meanwhile.
pub fn indicators(
    root: &Path,
    account: &str,
    games: &[(String, String)],
) -> Result<Vec<EpicIndicator>> {
    let mut wanted: std::collections::BTreeSet<(String, String)> = games.iter().cloned().collect();
    wanted.extend(
        pending(root, account)?
            .into_iter()
            .map(|config| (config.namespace, config.app_name)),
    );
    let mut indicators = Vec::new();
    for (namespace, app_name) in wanted {
        match indicator(root, account, &namespace, &app_name) {
            Ok((indicator, configuration)) => indicators.push(EpicIndicator {
                namespace,
                app_name,
                indicator,
                configuration,
            }),
            Err(error) => eprintln!("Epic Cloud indicator of {app_name} unavailable: {error}"),
        }
    }
    Ok(indicators)
}

fn indicator(
    root: &Path,
    account: &str,
    namespace: &str,
    app_name: &str,
) -> Result<(cloud_status::Indicator, Option<Configuration>)> {
    store::safe(root)?;
    let scope = cloud_vault::StoreScope::from(cloud_vault::EpicScope {
        account_id: account.into(),
        namespace: namespace.into(),
        app_name: app_name.into(),
    });
    let vault = cloud_vault::peek(root, scope.clone())?;
    // An uninstalled game has neither its installation nor a session to
    // validate against: check only that the context names this game.
    let path = scope.directory(root)?.join("epic-context.json");
    let context: Option<Context> = cloud_vault::journal::read(&path)?;
    if let Some(context) = &context
        && (context.account_id != account
            || context.config.namespace != namespace
            || context.config.app_name != app_name
            || context.config.root != root)
    {
        return Err(Error::Storage);
    }
    let journal = Work::journal(context.as_ref().map(Context::work).as_ref(), false);
    let record = cloud_status::read(root, &scope)?;
    Ok((
        cloud_status::indicator(&scope, &vault, &journal, &record),
        context.map(|context| context.config),
    ))
}

/// Cloud maintenance of one Epic game, run inside Forge with the UI's
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
    if config.platform == "Windows" {
        return Ok(());
    }
    let account = api.account_id().await?;
    config.validate(&account)?;
    store::safe(&config.root)?;
    let mut vault = Vault::open(&config.root, config.scope(&account))?;
    let saved = Context::load(&config, &account)?;
    let mut context = if let Some(saved) = saved {
        saved
    } else {
        let Some(rules) = api
            .cloud_configuration(
                &config.namespace,
                &config.catalog_item_id,
                &config.app_name,
                &config.platform,
            )
            .await?
        else {
            if has_saved_state(&vault, None) {
                return Err(Error::Unsupported);
            }
            return Ok(());
        };
        if !supports_rules(&config, &account, &rules, &vault, None)? {
            return Ok(());
        }
        Context {
            config,
            account_id: account,
            rules,
            pending: None,
            installation_removed: false,
        }
    };
    // Capture rehashes every save file. The caller shares one runtime with
    // downloads and the UI, so that work does not belong on it.
    tokio::task::spawn_blocking(move || archive_context(&mut vault, &mut context))
        .await
        .map_err(|_| Error::Session)?
}

/// A removed installation keeps already archived files that no longer exist on
/// disk; an intact one captures the bound set as it stands.
fn capture_set(vault: &mut Vault, removed: bool, bindings: &Bindings) -> Result<Manifest> {
    Ok(if removed {
        vault.capture_preserving_missing(bindings)?
    } else {
        vault.capture(bindings)?
    })
}

fn archive_context(vault: &mut Vault, context: &mut Context) -> Result<()> {
    // Archive has no permission to reassign another account's physical saves.
    let mut bindings = store::bindings(context, vault, None, false)?;
    bindings.retain(|_, file| file.path.parent().is_some_and(Path::is_dir));
    capture_set(vault, context.installation_removed, &bindings)?;
    context.installation_removed = true;
    context.save()
}

pub fn removed(config: &Configuration, account: &str) -> Result<()> {
    if config.platform == "Windows" {
        return Ok(());
    }
    config.validate(account)?;
    store::safe(&config.root)?;
    let _vault = Vault::open(&config.root, config.scope(account))?;
    let Some(mut context) = Context::load(config, account)? else {
        return Ok(());
    };
    if !context.absent()? {
        return Err(Error::Recovery);
    }
    context.installation_removed = true;
    context.save()
}

fn has_saved_state(vault: &Vault, context: Option<&Context>) -> bool {
    context.is_some()
        || vault.pending().is_some()
        || vault.game_session().is_some()
        || vault.local().is_some()
        || vault.baseline().is_some()
}

fn supports_rules(
    config: &Configuration,
    account: &str,
    rules: &CloudRules,
    vault: &Vault,
    saved: Option<&Context>,
) -> Result<bool> {
    match rules.resolve(&config.home, &config.install, account, &config.app_name) {
        Ok(_) => Ok(true),
        Err(EpicError::UnsupportedContent) if !has_saved_state(vault, saved) => Ok(false),
        Err(error) => Err(error.into()),
    }
}
