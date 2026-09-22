use super::*;
use epic_kit::{
    EpicError,
    client::Client as EpicClient,
    cloud::{CloudGame, CloudRules, ResolvedCloudRules, Snapshot as CloudSnapshot},
};
use std::{collections::BTreeMap, path::Path};
use steam_cloud::{
    Bindings, Decision, Manifest, Preferences, RemoteView, Retry, RetryBudget, Vault,
};
mod store;
use store::{Context, Pending};
#[cfg(test)]
mod tests;

pub(super) struct Session {
    context: Context,
    vault: Vault,
    api: Arc<EpicClient>,
    control: Arc<Control>,
    shown: Option<(RemoteView, Manifest)>,
    offline: bool,
}

impl Session {
    pub async fn new(
        config: Configuration,
        account: String,
        api: Arc<EpicClient>,
        control: Arc<Control>,
    ) -> Result<Option<Self>> {
        config.validate(&account)?;
        store::safe(&config.root)?;
        if api.credentials().await?.account_id != account {
            return Err(Error::Session);
        }
        control.check()?;
        let mut vault = Vault::open(&config.root, config.scope(&account))?;
        let saved = Context::load(&config, &account)?;
        let (rules, offline) = configuration_rules(&config, saved.as_ref(), &api, &control).await?;
        control.check()?;
        let Some(rules) = rules else {
            if has_saved_state(&vault, saved.as_ref()) {
                return Err(Error::Unsupported);
            }
            control.phase(CloudPhase::Unsupported)?;
            return Ok(None);
        };
        if !supports_rules(&config, &account, &rules, &vault, saved.as_ref())? {
            control.phase(CloudPhase::Unsupported)?;
            return Ok(None);
        }
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
        }))
    }

    pub async fn prepare(&mut self) -> Result<()> {
        self.recover_dead_owner()?;
        if self.offline {
            self.capture_offline()?;
            return self.control.phase(CloudPhase::Pending);
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
        self.vault.game_exited(id)?;
        self.capture_exit()?;
        if self.control.cancel.is_cancelled() {
            self.vault.revoke()?;
            self.control.phase(CloudPhase::Pending)?;
            return Ok(());
        }
        self.run(false).await
    }

    /// The caller proved that no child was accepted by the failed spawn.
    pub fn aborted(&mut self, id: &str) -> Result<()> {
        self.vault.game_exited(id)?;
        self.capture_exit()?;
        self.control.phase(CloudPhase::Pending)
    }

    pub async fn resume(&mut self) -> Result<()> {
        self.recover_dead_owner()?;
        if self.offline {
            return self.control.phase(CloudPhase::Pending);
        }
        if self.context.pending.is_none() && self.vault.pending().is_none() {
            return self.control.phase(CloudPhase::Ready);
        }
        self.run(false).await
    }

    fn recover_dead_owner(&mut self) -> Result<()> {
        let previous = self.vault.game_session().map(str::to_owned);
        if let Some(id) = previous {
            let config = &self.context.config;
            let snapshot =
                read_snapshot(&config.session_directory, Some(&id))?.ok_or(Error::Recovery)?;
            if snapshot.account_id != self.context.account_id
                || snapshot.namespace != config.namespace
                || snapshot.app_name != config.app_name
            {
                return Err(Error::Recovery);
            }
            if client::inspect(&config.helper, snapshot.supervisor.pid)?.as_ref()
                == Some(&snapshot.supervisor)
                || owns_child(&snapshot, |pid| client::inspect(&config.helper, pid))?
            {
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
                        self.control.update(|s| {
                            s.cloud_phase = CloudPhase::Pending;
                            s.error = Some(last.to_string());
                            s.allowed_choices.clear();
                        })?;
                        return Err(last);
                    }
                    match deferred_choice(&self.control, &last, retryable(&last)).await? {
                        Choice::Retry => {
                            budget = RetryBudget::new(Instant::now());
                            continue;
                        }
                        _ => {
                            self.capture_offline()?;
                            return self.control.phase(CloudPhase::Pending);
                        }
                    }
                }
                Retry::Attempt { remaining } => remaining,
            };
            self.control.phase(if launching {
                CloudPhase::Checking
            } else {
                CloudPhase::Syncing
            })?;
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
                            choice = Some(answer);
                            budget = RetryBudget::new(Instant::now());
                        }
                        _ => return Err(Error::Cancelled),
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
        if self.api.credentials().await?.account_id != self.context.account_id {
            return Err(Error::Session);
        }
        let full = self
            .api
            .cloud_download(self.cloud_game(), &self.control.cancel)
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
            if !steam_cloud::journal::regular(&path)? {
                steam_cloud::journal::write(&path, &pending)?;
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
                        return Err(steam_cloud::Error::Stale.into());
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
        self.control.phase(if launching {
            CloudPhase::Ready
        } else {
            CloudPhase::Synced
        })
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
                &self.control.cancel,
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
            | Error::Vault(steam_cloud::Error::Stale)
    )
}

pub fn pending(root: &Path, account: &str) -> Result<Vec<Configuration>> {
    store::safe(root)?;
    let directory = steam_cloud::epic_account_directory(root, account)?;
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
        let Some(context): Option<Context> = steam_cloud::journal::read(&path)? else {
            continue;
        };
        context.validate(&context.config, account)?;
        if context.config.root != root || Context::path(&context.config, account)? != path {
            return Err(Error::Storage);
        }
        if context.pending.is_some() {
            configs.push(context.config);
        }
    }
    Ok(configs)
}

pub async fn archive(config: Configuration, api: &EpicClient) -> Result<()> {
    if config.platform == "Windows" {
        return Ok(());
    }
    let account = api.credentials().await?.account_id;
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
async fn pause(control: &Arc<Control>, wait: Duration) -> Result<()> {
    tokio::select! {
        _ = tokio::time::sleep(wait) => Ok(()),
        _ = control.cancel.cancelled() => Err(Error::Cancelled),
    }
}

/// The prompt shown once a retry budget is spent. Returns only `Retry` or
/// `LaunchAnyway`; anything else — including a revoked account — is a cancel.
async fn deferred_choice(
    control: &Arc<Control>,
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

/// The operator prompt always crosses to a blocking thread; `Control::ask`
/// parks until the UI answers, revokes the account or cancels.
async fn ask(
    control: &Arc<Control>,
    phase: CloudPhase,
    error: Option<String>,
    choices: Vec<Choice>,
) -> Result<Choice> {
    let owner = control.clone();
    tokio::task::spawn_blocking(move || owner.ask(phase, error, choices))
        .await
        .map_err(|_| Error::Session)?
}

async fn configuration_rules(
    config: &Configuration,
    saved: Option<&Context>,
    api: &EpicClient,
    control: &Arc<Control>,
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
                let offer_launch = retryable(&last)
                    && saved.is_some()
                    && control.snapshot().phase == Phase::Preparing;
                match deferred_choice(control, &last, offer_launch).await? {
                    Choice::Retry => {
                        budget = RetryBudget::new(Instant::now());
                        continue;
                    }
                    _ => {
                        let context = saved.ok_or(Error::Recovery)?;
                        context.validate(config, &context.account_id)?;
                        if api.credentials().await?.account_id != context.account_id {
                            return Err(Error::Session);
                        }
                        return Ok((Some(context.rules.clone()), true));
                    }
                }
            }
            Retry::Attempt { remaining } => remaining,
        };
        let result = tokio::select! {
            _ = control.cancel.cancelled() => return Err(Error::Cancelled),
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
