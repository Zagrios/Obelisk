use super::*;
use crate::cloud_status::{self, ProblemKind, UnavailableReason};
use crate::supervisor::client::SessionAlive;
use cloud_vault::{
    Bindings, Decision, Manifest, Preferences, RemoteView, Retry, RetryBudget, Vault,
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
use store::{Context, Pending};
#[cfg(test)]
mod cycle_tests;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub(super) struct Session<R: CloudRemote> {
    context: Context,
    vault: Vault,
    api: Arc<R>,
    control: Arc<dyn CloudChannel>,
    shown: Option<(RemoteView, Manifest)>,
    offline: bool,
    /// How the current operation ended without a sync, for the status file.
    problem: Option<(ProblemKind, String)>,
}

/// Display state only: a failed write must not fail the sync itself.
fn mark(config: &Configuration, account: &str, change: impl FnOnce(&mut cloud_status::Record)) {
    let scope = cloud_vault::StoreScope::from(config.scope(account));
    if let Err(error) = cloud_status::update(&config.root, &scope, change) {
        eprintln!("Epic Cloud status not recorded: {error}");
    }
}

impl<R: CloudRemote> Session<R> {
    pub async fn new(
        config: Configuration,
        account: String,
        api: Arc<R>,
        control: Arc<dyn CloudChannel>,
    ) -> Result<Option<Self>> {
        config.validate(&account)?;
        store::safe(&config.root)?;
        if api.account_id().await? != account {
            return Err(Error::Session);
        }
        control.check()?;
        let mut vault = Vault::open(&config.root, config.scope(&account))?;
        let saved = Context::load(&config, &account)?;
        let (rules, offline) =
            match configuration_rules(&config, saved.as_ref(), api.as_ref(), &control).await {
                Ok(rules) => rules,
                Err(error) => {
                    if !has_pending_work(&vault, saved.as_ref()) {
                        mark(
                            &config,
                            &account,
                            cloud_status::problem(ProblemKind::Failed, &error.to_string()),
                        );
                    }
                    return Err(error);
                }
            };
        control.check()?;
        let unsupported = || {
            mark(
                &config,
                &account,
                cloud_status::unavailable(UnavailableReason::Unsupported),
            )
        };
        let Some(rules) = rules else {
            unsupported();
            if has_saved_state(&vault, saved.as_ref()) {
                return Err(Error::Unsupported);
            }
            control.show(CloudPhase::Unsupported, None)?;
            return Ok(None);
        };
        if !supports_rules(&config, &account, &rules, &vault, saved.as_ref())? {
            unsupported();
            control.show(CloudPhase::Unsupported, None)?;
            return Ok(None);
        }
        mark(&config, &account, cloud_status::available);
        let mut context = saved.unwrap_or_else(|| Context {
            config: config.clone(),
            rules: rules.clone(),
            account_id: account,
            pending: None,
            installation_removed: false,
        });
        // Reinterpreting an existing account snapshot under new save rules can
        // turn unselected files into deletions. Require a dedicated conversion.
        if context.rules != rules {
            return Err(Error::Unsupported);
        }
        context.config = config;
        context.validate(&context.config, &context.account_id)?;
        context.save()?;
        // Supported publisher rules activate Forge's Cloud policy. These are
        // not preferences read from Epic's official launcher; no such account
        // preference endpoint has been established for this protocol.
        vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        Ok(Some(Self {
            context,
            vault,
            api,
            control,
            shown: None,
            offline,
            problem: None,
        }))
    }

    fn mark(&self, change: impl FnOnce(&mut cloud_status::Record)) {
        mark(&self.context.config, &self.context.account_id, change);
    }

    /// A sync clears the last problem. Otherwise a failed operation leaves a
    /// problem, unless pending work already shows it or `exit` says the game
    /// just exited; an unanswered conflict is recorded either way.
    fn record_outcome(&mut self, syncs: u64, exit: bool) {
        let problem = self.problem.take();
        if self.vault.syncs() != syncs {
            self.mark(cloud_status::synced);
        } else if let Some((kind, key)) = problem
            && (kind == ProblemKind::Conflict
                || !exit && !has_pending_work(&self.vault, Some(&self.context)))
        {
            self.mark(cloud_status::problem(kind, &key));
        }
    }

    /// `alive` answers for a previous session whose game guard remains.
    pub async fn prepare(&mut self, alive: &SessionAlive<'_>) -> Result<()> {
        let syncs = self.vault.syncs();
        self.problem = None;
        let result = self.prepare_inner(alive).await;
        if let Err(error) = &result
            && self.problem.is_none()
        {
            self.problem = Some((ProblemKind::Failed, error.to_string()));
        }
        self.record_outcome(syncs, false);
        result
    }

    async fn prepare_inner(&mut self, alive: &SessionAlive<'_>) -> Result<()> {
        self.recover_dead_owner(alive)?;
        if self.offline {
            self.capture_offline()?;
            return self.control.show(CloudPhase::Pending, None);
        }
        self.run(true).await
    }

    pub fn started(&mut self, id: &str) -> Result<()> {
        self.control.check()?;
        // A persisted marker also schedules recovery if Forge disappears while
        // the game is running, before its exit snapshot can be captured.
        self.context.pending = Some(Pending::Capture);
        self.context.save()?;
        self.vault.game_started(id)?;
        Ok(())
    }

    pub async fn finished(&mut self, id: &str) -> Result<()> {
        let syncs = self.vault.syncs();
        self.problem = None;
        let result = self.finish(id).await;
        self.record_outcome(syncs, true);
        result
    }

    async fn finish(&mut self, id: &str) -> Result<()> {
        self.vault.game_exited(id)?;
        self.capture_exit()?;
        if self.control.cancellation().is_cancelled() {
            self.vault.revoke()?;
            self.control.show(CloudPhase::Pending, None)?;
            return Ok(());
        }
        self.run(false).await
    }

    /// Cloud maintenance of this game: a two-way sync without a launch. The
    /// caller holds the idle guard, so no supervisor or game owns the saves.
    pub(crate) async fn maintain(&mut self, alive: &SessionAlive<'_>) -> Result<()> {
        let syncs = self.vault.syncs();
        self.problem = None;
        let result = self.maintain_inner(alive).await;
        if let Err(error) = &result
            && self.problem.is_none()
        {
            self.problem = Some((ProblemKind::Failed, error.to_string()));
        }
        self.record_outcome(syncs, false);
        result
    }

    async fn maintain_inner(&mut self, alive: &SessionAlive<'_>) -> Result<()> {
        self.recover_dead_owner(alive)?;
        // An uninstalled game only finishes its remote work; its archived
        // saves are never projected into a missing installation.
        if !self.context.absent()? && !self.context.installation_removed {
            let had_work = has_pending_work(&self.vault, Some(&self.context));
            let bindings = store::bindings(&self.context, &self.vault, None, true)?;
            self.vault.activate(&bindings)?;
            self.capture_exit()?;
            // Saves unchanged since the last sync are compared, not queued: a
            // failed check must not report them as waiting to be sent.
            let scope = self.context.config.scope(&self.context.account_id);
            let journal = cloud_vault::peek(&self.context.config.root, scope)?;
            if !had_work && journal.baseline && !journal.local_changes {
                self.context.pending = None;
                self.context.save()?;
            }
        }
        self.run(false).await
    }

    /// Account revocation abandoned the launch: its authorization ends now.
    pub fn revoked(&mut self) -> Result<()> {
        Ok(self.vault.revoke()?)
    }

    /// No game ran, or its exec was never confirmed: the exit save is only
    /// captured, and stays pending.
    pub fn aborted(&mut self, id: &str) -> Result<()> {
        self.vault.game_exited(id)?;
        self.capture_exit()?;
        self.control.show(CloudPhase::Pending, None)
    }

    /// Only the matching session's actual exit may clear its game guard. The
    /// caller holds Epic's owner lock, so `alive` can answer for it.
    fn recover_dead_owner(&mut self, alive: &SessionAlive<'_>) -> Result<()> {
        let previous = self.vault.game_session().map(str::to_owned);
        if let Some(id) = previous {
            if alive(&id)? {
                return Err(Error::Busy);
            }
            self.vault.game_exited(&id)?;
            self.vault.recover_local()?;
            self.capture_exit()?;
        } else {
            self.vault.recover_local()?;
            recover_pending(&self.vault, &mut self.context)?;
            if matches!(self.context.pending, Some(Pending::Capture)) {
                self.capture_exit()?;
            }
        }
        Ok(())
    }

    fn capture_exit(&mut self) -> Result<()> {
        capture_exit(&mut self.vault, &mut self.context)
    }

    async fn ask(
        &self,
        phase: CloudPhase,
        error: Option<String>,
        choices: Vec<Choice>,
    ) -> Result<Choice> {
        ask(&self.control, phase, error, choices).await
    }

    async fn run(&mut self, launching: bool) -> Result<()> {
        let mut budget = RetryBudget::new(Instant::now());
        let mut last = Error::Failed;
        let mut choice = None;
        loop {
            self.control.check()?;
            let remaining = match budget.poll(Instant::now()) {
                Retry::Wait(wait) => {
                    pause(&self.control, wait).await?;
                    continue;
                }
                Retry::Defer => {
                    if !launching {
                        self.control
                            .show(CloudPhase::Pending, Some(last.to_string()))?;
                        return Err(last);
                    }
                    let answer = deferred_choice(&self.control, &last, retryable(&last)).await;
                    self.problem = Some((ProblemKind::Failed, last.to_string()));
                    match answer? {
                        Choice::Retry => {
                            budget = RetryBudget::new(Instant::now());
                            continue;
                        }
                        _ => {
                            self.capture_offline()?;
                            return self.control.show(CloudPhase::Pending, None);
                        }
                    }
                }
                Retry::Attempt { remaining } => remaining,
            };
            self.control.show(
                if launching {
                    CloudPhase::Checking
                } else {
                    CloudPhase::Syncing
                },
                None,
            )?;
            let result = tokio::time::timeout(remaining, self.cycle(launching, choice.take()))
                .await
                .unwrap_or(Err(Error::Failed));
            match result {
                Ok(()) => return Ok(()),
                Err(error)
                    if self.shown.is_some()
                        && matches!(error, Error::Epic(EpicError::CloudConflict)) =>
                {
                    let result = self
                        .ask(
                            CloudPhase::Conflict,
                            Some(error.to_string()),
                            vec![Choice::Local, Choice::Cloud, Choice::Cancel],
                        )
                        .await;
                    match result {
                        Ok(answer @ (Choice::Local | Choice::Cloud)) => {
                            self.mark(cloud_status::answered);
                            choice = Some(answer);
                            budget = RetryBudget::new(Instant::now());
                        }
                        _ => {
                            self.problem = Some((ProblemKind::Conflict, error.to_string()));
                            return Err(Error::Cancelled);
                        }
                    }
                }
                Err(
                    error @ (Error::Cancelled
                    | Error::Epic(EpicError::Cancelled | EpicError::Unauthorized)),
                ) => return Err(error),
                Err(error) => {
                    budget.failed(Instant::now(), !retryable(&error));
                    last = error;
                }
            }
        }
    }

    fn capture_offline(&mut self) -> Result<()> {
        // Ownership failures and an uninstall fence cannot be bypassed.
        if self.context.installation_removed {
            return Err(Error::Recovery);
        }
        let bindings = store::bindings(&self.context, &self.vault, None, true)?;
        self.vault.activate(&bindings)?;
        self.capture_exit()
    }

    async fn cycle(&mut self, launching: bool, choice: Option<Choice>) -> Result<()> {
        self.control.check()?;
        self.vault.recover_local()?;
        recover_pending(&self.vault, &mut self.context)?;
        if self.api.account_id().await? != self.context.account_id {
            return Err(Error::Session);
        }
        let full = self
            .api
            .cloud_download(self.cloud_game(), self.control.cancellation())
            .await?;
        let remote = stage_remote(&self.vault, &self.context.rules, &full)?;
        self.control.check()?;
        // A prior request may have committed. Never replay its journal. A fresh
        // verified complete view admits a new comparison and a new operation ID.
        if let Some(pending) = self.vault.pending().cloned() {
            let path = Context::path(&self.context.config, &self.context.account_id)?
                .parent()
                .ok_or(Error::Storage)?
                .join(format!("epic-upload-{}.json", pending.id));
            if !cloud_vault::journal::regular(&path)? {
                cloud_vault::journal::write(&path, &pending)?;
            }
            self.vault.recompare_pending(
                &pending.id,
                self.vault.authorize(self.context.account_id.as_str())?,
                &remote,
            )?;
        }
        let retrying = self.context.pending.is_some();
        let absent = self.context.absent()?;
        if absent && (!self.context.installation_removed || launching) {
            return Err(Error::Recovery);
        }
        let bindings = store::bindings(&self.context, &self.vault, Some(&remote.files), !absent)?;
        let local = if launching {
            self.vault.activate(&bindings)?;
            let local = capture_set(
                &mut self.vault,
                self.context.installation_removed,
                &bindings,
            )?;
            if retrying {
                self.context.pending = Some(Pending::Upload {
                    desired: local.clone(),
                });
                self.context.save()?;
            }
            local
        } else {
            self.context
                .pending
                .as_ref()
                .and_then(Pending::files)
                .cloned()
                .or_else(|| self.vault.local().cloned())
                .ok_or(Error::Recovery)?
        };
        let decision = decide(
            &mut self.vault,
            &mut self.context,
            &mut self.shown,
            &local,
            &remote,
            choice,
        )?;
        self.control.check()?;
        match decision {
            Decision::Converged => {
                if launching && self.context.installation_removed {
                    self.vault.restore_local(&bindings)?;
                }
                self.vault.confirm_equal(&local, &remote)?;
            }
            Decision::Download => {
                self.context.pending = Some(Pending::Applying {
                    desired: local.clone(),
                    remote: remote.clone(),
                    project: !absent,
                });
                self.context.save()?;
                if absent {
                    // A Cloud choice resolves remote work only. Preserve the
                    // archived local version and force a choice on reinstall.
                    self.vault.confirm_equal(&remote.files, &remote)?;
                } else {
                    if !launching && self.vault.capture(&bindings)? != local {
                        return Err(cloud_vault::Error::Stale.into());
                    }
                    self.vault.apply_download(
                        &bindings,
                        &remote,
                        &remote,
                        choice == Some(Choice::Cloud) || self.context.installation_removed,
                    )?;
                }
            }
            Decision::KeepLocal => {
                if launching && self.context.installation_removed {
                    self.vault.restore_local(&bindings)?;
                }
                if !launching || retrying {
                    self.upload(full, &local, &remote).await?;
                }
            }
            Decision::Conflict => unreachable!(),
        }
        self.shown = None;
        if launching || !absent {
            self.context.installation_removed = false;
        }
        if decision != Decision::KeepLocal || !launching || retrying {
            self.context.pending = None;
        }
        self.context.save()?;
        self.control.show(
            if launching {
                CloudPhase::Ready
            } else {
                CloudPhase::Synced
            },
            None,
        )
    }

    async fn upload(
        &mut self,
        full: CloudSnapshot,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<()> {
        self.context.pending = Some(Pending::Upload {
            desired: local.clone(),
        });
        self.context.save()?;
        // Retained remote bytes move into the upload set; the snapshot's copy
        // would otherwise stay live alongside the packaged one.
        let revision = full.revision;
        let files = upload_files(&self.vault, &self.context.rules, full.files, local)?;
        self.control.check()?;
        let authorization = self.vault.authorize(self.context.account_id.as_str())?;
        let id = self
            .vault
            .queue_upload(local, remote, "epic-egs-cloud", authorization)?;
        let permit = self.vault.begin_upload(&id, authorization, remote)?;
        let verified = self
            .api
            .cloud_upload(
                self.cloud_game(),
                &self.context.rules,
                &files,
                &revision,
                self.control.cancellation(),
            )
            .await?;
        self.control.check()?;
        let readback = stage_remote(&self.vault, &self.context.rules, &verified)?;
        self.vault.complete_upload(&permit, true, &readback)?;
        Ok(())
    }

    fn cloud_game(&self) -> CloudGame<'_> {
        CloudGame {
            namespace: &self.context.config.namespace,
            catalog_item_id: &self.context.config.catalog_item_id,
            app_name: &self.context.config.app_name,
        }
    }
}

fn decide(
    vault: &mut Vault,
    context: &mut Context,
    shown: &mut Option<(RemoteView, Manifest)>,
    local: &Manifest,
    remote: &RemoteView,
    choice: Option<Choice>,
) -> Result<Decision> {
    if let Some(choice) = choice {
        if shown
            .as_ref()
            .is_none_or(|(shown, files)| shown != remote || files != local)
        {
            return conflict(context, shown, local, remote);
        }
        *shown = None;
        return match choice {
            Choice::Local => {
                vault.choose_local(remote, remote)?;
                Ok(Decision::KeepLocal)
            }
            Choice::Cloud => Ok(Decision::Download),
            _ => Err(Error::Cancelled),
        };
    }
    *shown = None;
    let decision = if context.installation_removed && local != &remote.files {
        Decision::Conflict
    } else {
        vault.decision(local, remote)?
    };
    if decision == Decision::Conflict {
        return conflict(context, shown, local, remote);
    }
    Ok(decision)
}

fn conflict(
    context: &mut Context,
    shown: &mut Option<(RemoteView, Manifest)>,
    local: &Manifest,
    remote: &RemoteView,
) -> Result<Decision> {
    *shown = Some((remote.clone(), local.clone()));
    context.pending = Some(Pending::Upload {
        desired: local.clone(),
    });
    context.save()?;
    Err(EpicError::CloudConflict.into())
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

fn retryable(error: &Error) -> bool {
    matches!(
        error,
        Error::Failed
            | Error::Epic(
                EpicError::Network
                    | EpicError::CloudConflict
                    | EpicError::Http(408 | 429 | 500..=599)
            )
            | Error::Vault(cloud_vault::Error::Stale)
    )
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
        let path = entry.path().join("epic-context.json");
        store::safe(&path)?;
        let Some(context): Option<Context> = cloud_vault::journal::read(&path)? else {
            continue;
        };
        context.validate(&context.config, account)?;
        if context.config.root != root || Context::path(&context.config, account)? != path {
            return Err(Error::Storage);
        }
        // Vault work outlives the context marker: an uncertain upload or an
        // interrupted replacement still needs this game's next sync.
        let vault = cloud_vault::peek(root, context.config.scope(account))?;
        if context.pending.is_some() || vault.pending.is_some() || vault.applying {
            configs.push(context.config);
        }
    }
    Ok(configs)
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
    let journal = context
        .as_ref()
        .map_or_else(cloud_status::Journal::default, |context| {
            cloud_status::Journal {
                pending: matches!(
                    context.pending,
                    Some(Pending::Upload { .. } | Pending::Capture)
                ),
                uncertain: false,
                applying: matches!(context.pending, Some(Pending::Applying { .. })),
                installation_removed: context.installation_removed,
            }
        });
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
    channel: Arc<MaintenanceChannel>,
    idle: &IdleGuard,
) -> Result<()> {
    maintain_with(config, api, channel, &|id| idle.session_alive(id)).await
}

pub(crate) async fn maintain_with<R: CloudRemote>(
    config: Configuration,
    api: Arc<R>,
    channel: Arc<dyn CloudChannel>,
    alive: &SessionAlive<'_>,
) -> Result<()> {
    let account = api.account_id().await?;
    match Session::new(config, account, api, channel).await? {
        Some(mut session) => session.maintain(alive).await,
        // Unsupported rules were reported, and recorded as unavailable.
        None => Ok(()),
    }
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

fn capture_exit(vault: &mut Vault, context: &mut Context) -> Result<()> {
    let removed = context.installation_removed;
    let mut bindings = store::bindings(context, vault, None, !removed)?;
    if removed {
        bindings.retain(|_, file| file.path.parent().is_some_and(Path::is_dir));
    }
    let desired = capture_set(vault, removed, &bindings)?;
    context.pending = Some(Pending::Upload { desired });
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

fn recover_pending(vault: &Vault, context: &mut Context) -> Result<()> {
    if let Some(Pending::Applying {
        desired,
        remote,
        project,
    }) = context.pending.clone()
    {
        let completed =
            vault.baseline() == Some(&remote) && (!project || vault.local() == Some(&remote.files));
        context.pending = if completed {
            None
        } else {
            Some(Pending::Upload { desired })
        };
        context.save()?;
    }
    Ok(())
}

/// Pace a retry, but let cancellation cut the wait short.
async fn pause(control: &Arc<dyn CloudChannel>, wait: Duration) -> Result<()> {
    tokio::select! {
        _ = tokio::time::sleep(wait) => Ok(()),
        _ = control.cancellation().cancelled() => Err(Error::Cancelled),
    }
}

/// The prompt shown once a retry budget is spent. Returns only `Retry` or
/// `LaunchAnyway`; anything else — including a revoked account — is a cancel.
async fn deferred_choice(
    control: &Arc<dyn CloudChannel>,
    last: &Error,
    allow_launch_anyway: bool,
) -> Result<Choice> {
    let mut choices = vec![Choice::Retry, Choice::Cancel];
    if allow_launch_anyway {
        choices.push(Choice::LaunchAnyway);
    }
    match ask(control, CloudPhase::Failed, Some(last.to_string()), choices).await? {
        choice @ (Choice::Retry | Choice::LaunchAnyway) => Ok(choice),
        _ => Err(Error::Cancelled),
    }
}

/// The operator prompt always crosses to a blocking thread; the channel
/// parks until the UI answers, revokes the account or cancels.
async fn ask(
    control: &Arc<dyn CloudChannel>,
    phase: CloudPhase,
    error: Option<String>,
    choices: Vec<Choice>,
) -> Result<Choice> {
    let owner = control.clone();
    tokio::task::spawn_blocking(move || owner.question(phase, error, choices))
        .await
        .map_err(|_| Error::Session)?
}

async fn configuration_rules(
    config: &Configuration,
    saved: Option<&Context>,
    api: &impl CloudRemote,
    control: &Arc<dyn CloudChannel>,
) -> Result<(Option<CloudRules>, bool)> {
    let mut budget = RetryBudget::new(Instant::now());
    let mut last = Error::Failed;
    loop {
        control.check()?;
        let remaining = match budget.poll(Instant::now()) {
            Retry::Wait(wait) => {
                pause(control, wait).await?;
                continue;
            }
            Retry::Defer => {
                // Cloud maintenance asks only about conflicts.
                if !control.launching() {
                    return Err(last);
                }
                let offer_launch = retryable(&last) && saved.is_some();
                match deferred_choice(control, &last, offer_launch).await? {
                    Choice::Retry => {
                        budget = RetryBudget::new(Instant::now());
                        continue;
                    }
                    _ => {
                        let context = saved.ok_or(Error::Recovery)?;
                        context.validate(config, &context.account_id)?;
                        if api.account_id().await? != context.account_id {
                            return Err(Error::Session);
                        }
                        return Ok((Some(context.rules.clone()), true));
                    }
                }
            }
            Retry::Attempt { remaining } => remaining,
        };
        let result = tokio::select! {
            _ = control.cancellation().cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout(remaining, api.cloud_configuration(
                &config.namespace, &config.catalog_item_id, &config.app_name, &config.platform,
            )) => result.map_err(|_| Error::Failed).and_then(|result| result.map_err(Error::from)),
        };
        match result {
            Ok(rules) => return Ok((rules, false)),
            Err(error) if retryable(&error) => {
                budget.failed(Instant::now(), false);
                last = error;
            }
            Err(error) => return Err(error),
        }
    }
}

fn has_pending_work(vault: &Vault, context: Option<&Context>) -> bool {
    context.is_some_and(|context| context.pending.is_some()) || vault.pending().is_some()
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
