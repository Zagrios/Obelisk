use super::{store::Context, *};
use crate::{
    cloud_status::{self, ProblemKind, UnavailableReason},
    protocol::Credentials,
    supervisor::client::SessionAlive,
};
use cloud_vault::{Decision, Manifest, Preferences, RemoteView, Retry, RetryBudget, Vault};
use steam_kit::{
    cloud::{CloudListing, CloudPreferencesCache, CloudUpload, UploadTransaction},
    cm::SteamConnection,
};

#[cfg(test)]
mod cycle_tests;
#[cfg(test)]
mod fake;
#[cfg(test)]
use crate::supervisor::client::no_other_session;
mod remote;
pub(crate) use remote::SteamCm;
pub(crate) use remote::{CloudConnection, CloudRemote, RemoteSource};

pub(crate) struct Session<R: CloudRemote = SteamCm> {
    config: Config,
    credentials: Credentials,
    session_id: String,
    control: Arc<Control>,
    remote: R,
    preferences: CloudPreferencesCache,
    downloaded: Manifest,
    vault: Vault,
    context: Option<Context>,
    shown: Option<(RemoteView, Manifest)>,
    /// How the current operation ended without a sync, for the status file.
    problem: Option<(ProblemKind, Failure)>,
}
impl Session {
    pub fn new(
        config: Config,
        credentials: Credentials,
        session_id: String,
        control: Arc<Control>,
    ) -> Result<Self, crate::Error> {
        Self::open(config, credentials, session_id, control).map_err(Into::into)
    }
    fn open(
        config: Config,
        credentials: Credentials,
        session_id: String,
        control: Arc<Control>,
    ) -> Result<Self, Failure> {
        Self::open_with(SteamCm::new()?, config, credentials, session_id, control)
    }
}

impl<R: CloudRemote> Session<R> {
    pub fn native_storage(&self) -> Result<Option<crate::NativeStorage>, crate::Error> {
        self.context
            .as_ref()
            .map(|context| context.prepare_sdk())
            .transpose()
            .map(Option::flatten)
            .map_err(Into::into)
    }
    fn open_with(
        remote: R,
        config: Config,
        credentials: Credentials,
        session_id: String,
        control: Arc<Control>,
    ) -> Result<Self, Failure> {
        store::directory(&config, credentials.steam_id)?;
        let vault = Vault::open(&config.root, scope(&config, credentials.steam_id))?;
        let context = Context::load(&config, credentials.steam_id)?;
        Ok(Self {
            config,
            credentials,
            session_id,
            control,
            remote,
            preferences: CloudPreferencesCache::default(),
            downloaded: Manifest::default(),
            vault,
            context,
            shown: None,
            problem: None,
        })
    }
    fn mark(&self, change: impl FnOnce(&mut cloud_status::Record)) {
        let scope = scope(&self.config, self.credentials.steam_id).into();
        // Display state only: a failed write must not fail the sync itself.
        if let Err(error) = cloud_status::update(&self.config.root, &scope, change) {
            eprintln!("Steam Cloud status not recorded: {error}");
        }
    }
    fn has_pending_work(&self) -> bool {
        self.context
            .as_ref()
            .is_some_and(|context| context.needs_work)
            || self.vault.pending().is_some()
    }
    /// A sync clears the last problem. Otherwise a failed operation leaves a
    /// problem, unless pending work already shows it or `exit` says the game
    /// just exited; an unanswered conflict is recorded either way.
    fn record_outcome(&mut self, syncs: u64, exit: bool) {
        let problem = self.problem.take();
        if self.vault.syncs() != syncs {
            self.mark(cloud_status::synced);
        } else if let Some((kind, failure)) = problem
            && (kind == ProblemKind::Conflict || !exit && !self.has_pending_work())
        {
            self.mark(cloud_status::problem(kind, failure.key()));
        }
    }
    /// Clear a prior owner's durable game guard once its session is known
    /// dead, and roll forward any interrupted local transaction. The caller
    /// holds Steam's owner lock, so `alive` can answer for that session.
    fn recover_dead_owner(&mut self, alive: &SessionAlive<'_>) -> Result<(), Failure> {
        if let Some(old) = self.vault.game_session().map(str::to_owned) {
            if alive(&old).map_err(|_| Failure::Recovery)? {
                return Err(Failure::Busy);
            }
            self.vault.game_exited(&old)?;
        }
        self.vault.recover_local()?;
        Ok(())
    }
    pub async fn prepare(&mut self, alive: &SessionAlive<'_>) -> Result<(), crate::Error> {
        let syncs = self.vault.syncs();
        self.problem = None;
        let result = self.prepare_inner(alive).await;
        if matches!(result, Err(crate::Error::Cloud(Failure::Authentication))) {
            self.control
                .status(Phase::Failed, Some(Failure::Authentication));
        }
        self.record_outcome(syncs, false);
        result
    }
    async fn prepare_inner(&mut self, alive: &SessionAlive<'_>) -> Result<(), crate::Error> {
        // The supervisor holds its process-owner lock and has checked the
        // previous game identity before admitting this launch.
        self.recover_dead_owner(alive)?;
        Ok(self.compare(false).await?)
    }
    /// Compares local and Cloud saves until they agree, the player decides or
    /// the retry budget runs out. `force` is Cloud maintenance: it syncs even
    /// without earlier pending work, and a failure ends it instead of asking.
    async fn compare(&mut self, force: bool) -> Result<(), Failure> {
        self.collect_garbage();
        let mut choice = None;
        let mut budget = RetryBudget::new(Instant::now());
        let mut last = Failure::Network;
        loop {
            let remaining = match budget.poll(Instant::now()) {
                Retry::Wait(wait) => {
                    self.control.wait(wait).await?;
                    continue;
                }
                Retry::Defer => {
                    let conflict = matches!(last, Failure::Conflict);
                    if force && !conflict {
                        self.problem = Some((ProblemKind::Failed, last));
                        return Err(last);
                    }
                    let answer = match self.control.ask(last).await {
                        Ok(answer) => answer,
                        Err(error) => {
                            let kind = if conflict {
                                ProblemKind::Conflict
                            } else {
                                ProblemKind::Failed
                            };
                            self.problem = Some((kind, last));
                            return Err(error);
                        }
                    };
                    if conflict {
                        self.mark(cloud_status::answered);
                    }
                    match answer {
                        Choice::Retry => choice = None,
                        Choice::Local => choice = Some(Choice::Local),
                        Choice::Cloud => choice = Some(Choice::Cloud),
                        Choice::LaunchAnyway => {
                            self.problem = Some((ProblemKind::Failed, last));
                            // Offline launch still checks account ownership and
                            // preserves any captured work for the next sync.
                            self.capture_without_sync()?;
                            self.control.status(Phase::Pending, Some(last));
                            return Ok(());
                        }
                        Choice::Cancel => return Err(Failure::Cancelled),
                    }
                    budget = RetryBudget::new(Instant::now());
                    continue;
                }
                Retry::Attempt { remaining } => remaining,
            };
            self.control.status(Phase::Checking, None);
            let control = self.control.clone();
            let result = control
                .guarded(remaining, self.preflight(choice.take(), force))
                .await;
            match result {
                Ok(()) => return Ok(()),
                Err(failure @ (Failure::Cancelled | Failure::Authentication)) => {
                    self.problem = Some((ProblemKind::Failed, failure));
                    return Err(failure);
                }
                Err(failure) => {
                    last = failure;
                    budget.failed(Instant::now(), !failure.retryable());
                }
            }
        }
    }
    fn context(&self) -> Result<&Context, Failure> {
        self.context.as_ref().ok_or(Failure::Storage)
    }
    fn context_mut(&mut self) -> Result<&mut Context, Failure> {
        self.context.as_mut().ok_or(Failure::Storage)
    }
    /// Unknown preferences never authorize a transfer, and an explicitly
    /// disabled one is not an error. Returns false when Cloud is off.
    async fn preferences_enabled(&mut self, cm: &mut R::Connection) -> Result<bool, Failure> {
        let preferences = cm.preferences(&mut self.preferences).await?;
        self.vault.set_preferences(Preferences {
            account_enabled: preferences.account_enabled,
            game_enabled: preferences.game_enabled,
        });
        if preferences.account_enabled == Some(false) || preferences.game_enabled == Some(false) {
            self.mark(cloud_status::unavailable(UnavailableReason::Disabled));
            return Ok(false);
        }
        if preferences.account_enabled != Some(true) || preferences.game_enabled != Some(true) {
            return Err(Failure::Preferences);
        }
        self.mark(cloud_status::available);
        Ok(true)
    }
    async fn connect(&self) -> Result<R::Connection, Failure> {
        self.control.check()?;
        let cm = self
            .remote
            .connect(&self.credentials, self.config.app_id, self.control.clone())
            .await?;
        self.control.check()?;
        Ok(cm)
    }
    async fn preflight(&mut self, choice: Option<Choice>, force: bool) -> Result<(), Failure> {
        let mut cm = self.connect().await?;
        let Some(metadata) = cm.cloud_metadata().await? else {
            self.context = None;
            self.mark(cloud_status::unavailable(UnavailableReason::NoCloud));
            self.control.status(Phase::Disabled, None);
            return Ok(());
        };
        if metadata
            .field("appidredirect")
            .is_some_and(|v| v.text() != Some("0"))
        {
            self.mark(cloud_status::unavailable(UnavailableReason::Unsupported));
            return Err(Failure::Unsupported);
        }
        if metadata.field("savefiles").is_some() {
            steam_kit::cloud::MacCloudPaths::new(
                &metadata,
                self.credentials.steam_id,
                &self.config.home,
                &self.config.install,
            )?;
        }
        if let Some(context) = &mut self.context {
            context.config = self.config.clone();
            context.metadata = metadata;
        } else {
            self.context = Some(Context::new(
                &self.config,
                self.credentials.steam_id,
                metadata,
            ));
        }
        // Learn the server's exact remote identities before adopting local
        // files. Optional root separators must not create archive aliases.
        let listing = cm.listing().await?;
        self.context_mut()?.listing = listing.clone();
        let retrying = force
            || self.context.as_ref().is_some_and(|c| c.needs_work)
            || self.vault.pending().is_some();
        if retrying {
            // Preserve today's local bytes before retiring earlier work.
            self.capture_without_sync()?;
        } else {
            // Ownership must be established before transfer, but a normal
            // launch only needs its snapshot after the remote view is ready.
            self.activate_before_launch()?;
            self.context()?.save()?;
        }
        if !self.preferences_enabled(&mut cm).await? {
            if !retrying {
                self.capture_without_sync()?;
            }
            self.control.status(Phase::Disabled, None);
            return Ok(());
        }
        self.recover_previous(&mut cm).await?;
        let listing = cm.listing().await?;
        let remote = self.read_remote(&mut cm, &listing).await?;
        self.set_listing(listing, remote.clone())?;
        let local = self.capture_before_launch()?.ok_or(Failure::Storage)?;
        if retrying {
            // A failed previous exit is synchronized before this launch using
            // today's local files, not an older queued upload plan.
            let context = self.context_mut()?;
            context.desired = Some(local.clone());
            context.needs_work = true;
            context.save()?;
        }
        let decision = self.reconcile_launch(&local, &remote, choice)?;
        if retrying {
            if decision == Decision::KeepLocal {
                let listing = self.context()?.listing.clone();
                self.upload_local(&mut cm, listing, &local, &remote).await?;
            }
            self.completed()?;
        }
        self.collect_garbage();
        if !force {
            self.control.status(Phase::Ready, None);
        }
        Ok(())
    }
    fn reconcile_launch(
        &mut self,
        local: &Manifest,
        remote: &RemoteView,
        choice: Option<Choice>,
    ) -> Result<Decision, Failure> {
        let removed = self.context()?.installation_removed;
        let decision = if let Some(choice) = choice {
            if self
                .shown
                .as_ref()
                .is_none_or(|(shown_remote, shown_local)| {
                    shown_remote != remote || shown_local != local
                })
            {
                return Err(Failure::Changed);
            }
            if choice == Choice::Local {
                self.vault.choose_local(remote, remote)?;
                Decision::KeepLocal
            } else {
                self.apply(remote, true)?;
                Decision::Download
            }
        } else {
            // A reinstall can remove GameInstall saves without any user save
            // deletion. Never infer upload intent from that missing projection,
            // or restore an entire archive over unrelated save roots.
            if removed && local != &remote.files {
                self.shown = Some((remote.clone(), local.clone()));
                return Err(Failure::Conflict);
            }
            let decision = self.vault.decision(local, remote)?;
            match decision {
                Decision::Converged => self.vault.confirm_equal(local, remote)?,
                Decision::KeepLocal => {} // The caller resumes any earlier failed exit.
                Decision::Download => self.apply(remote, false)?,
                Decision::Conflict => {
                    self.shown = Some((remote.clone(), local.clone()));
                    return Err(Failure::Conflict);
                }
            }
            decision
        };
        if removed {
            let context = self.context_mut()?;
            context.installation_removed = false;
            context.save()?;
        }
        Ok(decision)
    }
    fn activate_before_launch(&mut self) -> Result<(), Failure> {
        let Some(context) = &mut self.context else {
            return Ok(());
        };
        context.refresh_bindings(self.vault.local())?;
        context.ensure_parents()?;
        self.vault.activate(&context.bindings)?;
        Ok(())
    }
    fn capture_before_launch(&mut self) -> Result<Option<Manifest>, Failure> {
        self.activate_before_launch()?;
        let Some(context) = &mut self.context else {
            return Ok(None);
        };
        let local = self.vault.capture(&context.bindings)?;
        context.save()?;
        Ok(Some(local))
    }
    fn capture_without_sync(&mut self) -> Result<(), Failure> {
        if self
            .context
            .as_ref()
            .is_some_and(|context| context.installation_removed)
        {
            // Ownership still applies offline, but missing reinstall files do
            // not replace the archive before a fresh Cloud comparison.
            self.activate_before_launch()?;
            self.context()?.save()
        } else {
            self.capture_before_launch().map(|_| ())
        }
    }
    fn set_listing(&mut self, listing: CloudListing, remote: RemoteView) -> Result<(), Failure> {
        // Split borrow: the context is updated while `self.vault` is read.
        let context = self.context.as_mut().ok_or(Failure::Storage)?;
        context.listing = listing;
        context.remote = remote;
        context.refresh_bindings(self.vault.local())?;
        context.save()
    }
    fn apply(&mut self, remote: &RemoteView, chosen: bool) -> Result<(), Failure> {
        // Split borrow: the context is updated while `self.vault` is used.
        let context = self.context.as_mut().ok_or(Failure::Storage)?;
        context.ensure_parents()?;
        self.vault
            .apply_download(&context.bindings, remote, remote, chosen)?;
        context.desired = None;
        context.needs_work = false;
        context.save()
    }
    async fn recover_previous(&mut self, cm: &mut R::Connection) -> Result<(), Failure> {
        let Some(pending) = self.vault.pending().cloned() else {
            return Ok(());
        };
        let mut settled = !pending.may_have_committed;
        if pending.may_have_committed {
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
            settled = recovery_is_settled(result, transaction.is_settled())?;
        }
        // An unavailable/partial view never clears pending work. On an unknown
        // outcome, this is a new observation, not proof of server cancellation.
        let listing = cm.listing().await?;
        let current = self.read_remote(cm, &listing).await?;
        self.reconcile_previous(&current, settled)
    }
    fn reconcile_previous(&mut self, current: &RemoteView, settled: bool) -> Result<(), Failure> {
        self.control.check()?;
        let pending = self.vault.pending().ok_or(Failure::Storage)?.clone();
        let authorization = self.vault.authorize(self.credentials.steam_id)?;
        if settled {
            self.vault
                .reconcile_pending(&pending.id, authorization, true, current)?;
            if self.vault.pending().is_some() {
                self.vault
                    .resolve_pending_conflict(&pending.id, current, current, false)?;
            }
        } else {
            // Keep the old confirmed baseline and conflict choice. Never mark
            // the unanswered transaction Acknowledged/Verified or replay it.
            self.vault
                .recompare_pending(&pending.id, authorization, current)?;
        }
        let context = self.context_mut()?;
        context.transaction = None;
        context.save()
    }
    pub fn started(&mut self) -> Result<(), crate::Error> {
        self.control.check()?;
        if let Some(context) = &mut self.context {
            // A disabled/offline launch is allowed after reinstall. Keep the
            // removal fence until a fresh comparison authorizes synchronization.
            context.needs_work = true;
            context.desired = None;
            context.save()?;
            self.vault
                .game_started(&self.session_id)
                .map_err(Failure::from)?;
        }
        Ok(())
    }
    fn game_guard_held(&self) -> bool {
        self.vault.game_session() == Some(self.session_id.as_str())
    }
    /// The exit save an uninstalled game's maintenance sends. Without one, an
    /// upload still in the vault supplies it.
    fn recover_exit_snapshot(&mut self) -> Result<(), Failure> {
        let queued = self.vault.pending().map(|pending| pending.desired.clone());
        if let Some(context) = &mut self.context
            && context.desired.is_none()
        {
            // There was no completed exit snapshot before removal. A new
            // launch must compare the projection; maintenance cannot infer
            // save deletions from a partially removed installation.
            context.desired = Some(queued.ok_or(Failure::Changed)?);
            context.needs_work = true;
            context.save()?;
        }
        Ok(())
    }
    /// Cloud maintenance of this game: a two-way sync without a launch. The
    /// caller holds the idle guard, so no supervisor or game owns the saves.
    pub(crate) async fn maintain(&mut self, alive: &SessionAlive<'_>) -> Result<(), Failure> {
        let syncs = self.vault.syncs();
        self.problem = None;
        let result = self.maintain_inner(alive).await;
        if let Err(failure) = result
            && self.problem.is_none()
        {
            self.problem = Some((ProblemKind::Failed, failure));
        }
        self.record_outcome(syncs, false);
        result
    }
    async fn maintain_inner(&mut self, alive: &SessionAlive<'_>) -> Result<(), Failure> {
        self.recover_dead_owner(alive)?;
        let uninstalled = match &self.context {
            Some(context) => context.removed_installation_absent()?,
            None => false,
        };
        if !uninstalled {
            return self.compare(true).await;
        }
        // Preflight needs the installation. The exit save is sent as captured.
        self.recover_exit_snapshot()?;
        if !self.has_pending_work() {
            self.control.status(Phase::Synced, None);
            return Ok(());
        }
        self.finalize().await
    }
    pub fn aborted(&mut self) {
        if self.game_guard_held() {
            let _ = self.vault.game_exited(&self.session_id);
        }
        // Account revocation abandoned this launch: its authorization ends now.
        if self.control.revoked() {
            let _ = self.vault.revoke();
        }
    }
    /// `confirmed` is false when exec was never confirmed: the exit save is
    /// captured and kept pending, never uploaded.
    pub async fn finished(&mut self, confirmed: bool) {
        let syncs = self.vault.syncs();
        self.problem = None;
        self.finish(confirmed).await;
        // Exit failures show through the pending work they leave.
        self.record_outcome(syncs, true);
    }
    async fn finish(&mut self, confirmed: bool) {
        let capture = (|| -> Result<(), Failure> {
            if self.game_guard_held() {
                self.vault.game_exited(&self.session_id)?;
            }
            let Some(context) = &mut self.context else {
                return Ok(());
            };
            context.refresh_bindings(self.vault.local())?;
            context.ensure_parents()?;
            context.desired = Some(self.vault.capture(&context.bindings)?);
            context.needs_work = true;
            context.save()
        })();
        if let Err(error) = capture {
            self.control.status(Phase::Pending, Some(error));
            return;
        }
        if self.context.is_none() {
            return;
        }
        if !confirmed {
            self.control.status(Phase::Pending, None);
            return;
        }
        if let Err(error) = self.finalize().await {
            // An unanswered conflict still reports why the save waits.
            let reason = match self.problem {
                Some((ProblemKind::Conflict, _)) => Failure::Conflict,
                _ => error,
            };
            self.control.status(Phase::Pending, Some(reason));
        }
    }
    /// Sends the captured exit save, asking live on a conflict. Returns the
    /// failure that leaves it pending, or `Cancelled` for an unanswered conflict.
    async fn finalize(&mut self) -> Result<(), Failure> {
        self.collect_garbage();
        let mut choice = None;
        let mut approved = None;
        let mut budget = RetryBudget::new(Instant::now());
        let mut last = Failure::Network;
        loop {
            match budget.poll(Instant::now()) {
                Retry::Defer if matches!(last, Failure::Conflict) => {
                    // Ask now, as at launch. An unanswered conflict leaves the
                    // exit save pending until the next launch or maintenance.
                    match self.control.ask(last).await {
                        Ok(answer) => {
                            self.mark(cloud_status::answered);
                            choice = Some(answer);
                            approved = self.context.as_ref().map(|c| c.remote.clone());
                            budget = RetryBudget::new(Instant::now());
                        }
                        Err(error) => {
                            self.problem = Some((ProblemKind::Conflict, last));
                            last = error;
                            break;
                        }
                    }
                }
                Retry::Defer => break,
                Retry::Wait(wait) => {
                    if let Err(error) = self.control.wait(wait).await {
                        last = error;
                        break;
                    }
                }
                Retry::Attempt { remaining } => {
                    self.control.status(Phase::Syncing, None);
                    let control = self.control.clone();
                    match control
                        .guarded(remaining, self.upload(choice, approved.as_ref()))
                        .await
                    {
                        Ok(()) => {
                            return Ok(());
                        }
                        Err(error) => {
                            last = error;
                            budget.failed(Instant::now(), !error.retryable());
                        }
                    }
                }
            }
        }
        if self.control.revoked.load(Ordering::Acquire) {
            let _ = self.vault.revoke();
        }
        self.collect_garbage();
        Err(last)
    }
    async fn upload(
        &mut self,
        choice: Option<Choice>,
        approved: Option<&RemoteView>,
    ) -> Result<(), Failure> {
        let mut cm = self.connect().await?;
        let metadata = cm.cloud_metadata().await?.ok_or(Failure::Unsupported)?;
        let context = self.context()?;
        if metadata != context.metadata {
            // An uninstalled game's exit save can only be sent under the rules
            // it was captured with. A reinstall compares it under the new ones.
            return Err(if context.installation_removed {
                Failure::RulesChanged
            } else {
                Failure::Unsupported
            });
        }
        if !self.preferences_enabled(&mut cm).await? {
            // Cloud is off in Steam, so this exit save is never sent. It stays
            // in the archive as the local snapshot, but not as pending work.
            let context = self.context_mut()?;
            context.needs_work = false;
            context.desired = None;
            context.save()?;
            self.control.status(Phase::Disabled, None);
            return Ok(());
        }
        self.recover_previous(&mut cm).await?;
        let listing = cm.listing().await?;
        let remote = self.read_remote(&mut cm, &listing).await?;
        // A resume operates on immutable captured bytes, including after the
        // installation has been removed. It does not scan unrelated games.
        let local = self.context()?.desired.clone().ok_or(Failure::Storage)?;
        self.set_listing(listing.clone(), remote.clone())?;
        if let Some(choice) = choice {
            if approved != Some(&remote) {
                return Err(Failure::Conflict);
            }
            match choice {
                Choice::Local => {
                    self.vault.choose_local(&remote, &remote)?;
                }
                Choice::Cloud => {
                    return self.choose_cloud_after_exit(&local, &remote);
                }
                _ => return Err(Failure::Cancelled),
            }
        }
        self.check_removed_upload(&local, &remote, choice)?;
        match self.vault.decision(&local, &remote)? {
            Decision::Converged => {
                self.vault.confirm_equal(&local, &remote)?;
                self.completed()?;
                return Ok(());
            }
            Decision::Conflict => return Err(Failure::Conflict),
            Decision::Download => {
                // Never replace files that have been edited since the captured
                // exit version, or recreate an uninstalled game's directory.
                if !self.config.install.is_dir() {
                    return Err(Failure::Conflict);
                }
                // Split borrow: the context is read while `self.vault` is used.
                let bindings = &self.context.as_ref().ok_or(Failure::Storage)?.bindings;
                if self.vault.capture(bindings)? != local {
                    return Err(Failure::Conflict);
                }
                self.apply(&remote, false)?;
                self.control.status(Phase::Synced, None);
                return Ok(());
            }
            Decision::KeepLocal => {}
        }
        self.upload_local(&mut cm, listing, &local, &remote).await?;
        self.completed()
    }
    fn check_removed_upload(
        &self,
        local: &Manifest,
        remote: &RemoteView,
        choice: Option<Choice>,
    ) -> Result<(), Failure> {
        if self.context()?.installation_removed && choice.is_none() && local != &remote.files {
            // A game can run with Cloud disabled after reinstall. Its exit
            // snapshot is real, but missing files still require a fresh choice.
            return Err(Failure::Conflict);
        }
        Ok(())
    }
    async fn upload_local(
        &mut self,
        cm: &mut R::Connection,
        listing: CloudListing,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<(), Failure> {
        let mut uploads = Vec::new();
        for (name, file) in &local.0 {
            if remote.files.0.get(name) != Some(file) {
                uploads.push(CloudUpload {
                    name: name.clone(),
                    bytes: self.vault.read_content(file)?,
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
            listing,
            uploads,
            deletes,
        )?;
        let authorization = self.vault.authorize(self.credentials.steam_id)?;
        let id = self
            .vault
            .queue_upload(local, remote, "steam-cm-autocloud", authorization)?;
        let context = self.context_mut()?;
        context.transaction = Some(transaction.directory().to_path_buf());
        context.save()?;
        let permit = self.vault.begin_upload(&id, authorization, remote)?;
        let paths = self.context()?.paths()?;
        let mut files = Manifest::default();
        let listing = cm
            .upload_verified(&mut transaction, |file, bytes| {
                if paths.existing_file(file)?.is_some() {
                    let version = self
                        .vault
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
        self.control.check()?;
        self.vault.complete_upload(&permit, true, &verified)?;
        self.set_listing(listing, verified)
    }
    fn choose_cloud_after_exit(
        &mut self,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<(), Failure> {
        self.control.check()?;
        if self.context()?.removed_installation_absent()? {
            // upload() must first recompare any old attempt against a fresh
            // remote view. A choice cannot silently skip that recovery step.
            if let Some(pending) = self.vault.pending().cloned() {
                if pending.may_have_committed {
                    return Err(Failure::Uncertain);
                }
                self.vault
                    .resolve_pending_conflict(&pending.id, remote, remote, false)?;
            }
            self.vault.record_local(local)?;
            self.vault.confirm_equal(&remote.files, remote)?;
            // Keep the removal marker: outside-install saves were deliberately
            // not replaced, and reinstall still needs its own fresh comparison.
        } else {
            self.apply(remote, true)?;
        }
        self.completed()
    }
    fn completed(&mut self) -> Result<(), Failure> {
        let context = self.context_mut()?;
        context.needs_work = false;
        context.desired = None;
        context.transaction = None;
        context.save()?;
        self.collect_garbage();
        self.control.status(Phase::Synced, None);
        Ok(())
    }

    fn collect_garbage(&mut self) {
        // Call only between operations: staged bytes still in local variables
        // must first be recorded in the Vault or durable Context.
        self.downloaded = Manifest::default();
        // A failed context save can leave newer in-memory values alongside an
        // older durable decision. Keep both until a later save commits.
        let saved = match Context::load(&self.config, self.credentials.steam_id) {
            Ok(saved) => saved,
            Err(error) => {
                eprintln!("Steam Cloud cache cleanup deferred: {error}");
                return;
            }
        };
        let mut retained = Vec::new();
        for context in saved.iter().chain(self.context.iter()) {
            retained.push(&context.remote.files);
            retained.extend(context.desired.iter());
        }
        if let Some((remote, local)) = &self.shown {
            retained.extend([&remote.files, local]);
        }
        if let Err(error) = self.vault.collect_garbage(&retained) {
            // Housekeeping failure must not turn an acknowledged sync into a
            // failed transfer. The next operation boundary will try again.
            eprintln!("Steam Cloud cache cleanup deferred: {error}");
        }
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
    let mut session = Session::open_with(
        remote,
        config,
        credentials.clone(),
        uuid::Uuid::new_v4().to_string(),
        control,
    )?;
    session.maintain(alive).await
}

#[cfg(test)]
mod removal_tests {
    use super::*;
    use cloud_vault::{Bindings, SelectedFile};
    use std::fs;
    use steam_kit::cloud::CloudMetadata;

    const ACCOUNT: u64 = 0x0110_0001_0000_002a;

    fn installed_fixture() -> (tempfile::TempDir, Session, RemoteView) {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let config = Config {
            root: base.join("cloud"),
            home: base.join("home"),
            install: base.join("game"),
            app_id: 42,
        };
        for path in [&config.home, &config.install] {
            crate::files::private_directory(path).unwrap();
        }
        fs::write(config.install.join("slot.sav"), b"archived progress").unwrap();
        let metadata = CloudMetadata::Object(vec![(
            "savefiles".into(),
            CloudMetadata::Object(vec![(
                "0".into(),
                CloudMetadata::Object(
                    [
                        ("root", "GameInstall"),
                        ("path", ""),
                        ("pattern", "*.sav"),
                        ("recursive", "0"),
                    ]
                    .into_iter()
                    .map(|(key, value)| (key.into(), CloudMetadata::Value(value.into())))
                    .collect(),
                ),
            )]),
        )]);
        let mut session = Session::open(
            config.clone(),
            Credentials {
                steam_id: ACCOUNT,
                account_name: "offline fixture".into(),
                refresh_token: "unused".into(),
            },
            uuid::Uuid::new_v4().to_string(),
            Arc::new(Control::new(ACCOUNT, |_| {})),
        )
        .unwrap();
        session.context = Some(Context::new(&config, ACCOUNT, metadata));
        let files = session.capture_before_launch().unwrap().unwrap();
        let remote = RemoteView {
            revision: "1".into(),
            files,
        };
        session.vault.confirm_equal(&remote.files, &remote).unwrap();
        session.vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        let context = session.context_mut().unwrap();
        context.remote = remote.clone();
        context.installation_removed = true;
        context.save().unwrap();
        (temporary, session, remote)
    }

    pub(super) fn fixture() -> (tempfile::TempDir, Session, RemoteView) {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let config = Config {
            root: base.join("cloud"),
            home: base.join("home"),
            install: base.join("game"),
            app_id: 42,
        };
        for path in [&config.root, &config.home, &config.install] {
            crate::files::private_directory(path).unwrap();
        }
        let mut session = Session::open(
            config.clone(),
            Credentials {
                steam_id: ACCOUNT,
                account_name: "offline fixture".into(),
                refresh_token: "unused".into(),
            },
            uuid::Uuid::new_v4().to_string(),
            Arc::new(Control::new(ACCOUNT, |_| {})),
        )
        .unwrap();
        let bindings: Bindings = [
            ("install", config.install.join("slot.sav")),
            ("outside", config.home.join("outside.sav")),
        ]
        .into_iter()
        .map(|(name, path)| {
            fs::write(&path, name.as_bytes()).unwrap();
            (name.into(), SelectedFile { path, platforms: 7 })
        })
        .collect();
        let files = session.vault.capture(&bindings).unwrap();
        let remote = RemoteView {
            revision: "1".into(),
            files,
        };
        session.vault.confirm_equal(&remote.files, &remote).unwrap();
        session.vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        let mut context = Context::new(&config, ACCOUNT, CloudMetadata::Object(Vec::new()));
        context.listing.change_number = 1;
        context.remote = remote.clone();
        context.bindings = bindings;
        context.installation_removed = true;
        context.save().unwrap();
        session.context = Some(context);
        (temporary, session, remote)
    }

    #[test]
    fn missing_reinstalled_save_requires_fresh_choice_and_preserves_other_roots() {
        let (_temporary, mut session, remote) = fixture();
        let outside = session.config.home.join("outside.sav");
        fs::remove_file(session.config.install.join("slot.sav")).unwrap();
        fs::write(&outside, b"new outside progress").unwrap();
        let local = session
            .vault
            .capture(&session.context.as_ref().unwrap().bindings)
            .unwrap();
        // Normal reconciliation would upload the absent install save as a deletion.
        assert_eq!(
            session.vault.decision(&local, &remote).unwrap(),
            Decision::KeepLocal
        );
        assert!(matches!(
            session.reconcile_launch(&local, &remote, None),
            Err(Failure::Conflict)
        ));
        assert_eq!(fs::read(&outside).unwrap(), b"new outside progress");
        assert!(!session.config.install.join("slot.sav").exists());
        let mut newer = remote.clone();
        newer.revision = "2".into();
        assert!(matches!(
            session.reconcile_launch(&local, &newer, Some(Choice::Local)),
            Err(Failure::Changed)
        ));
        assert!(session.context.as_ref().unwrap().installation_removed);

        session
            .reconcile_launch(&local, &remote, Some(Choice::Local))
            .unwrap();

        assert!(
            !Context::load(&session.config, ACCOUNT)
                .unwrap()
                .unwrap()
                .installation_removed
        );
        assert!(session.vault.pending().is_none());
        assert_eq!(
            session.vault.decision(&local, &remote).unwrap(),
            Decision::KeepLocal
        );
        assert_eq!(fs::read(&outside).unwrap(), b"new outside progress");
        assert!(!session.config.install.join("slot.sav").exists());
    }

    #[test]
    fn disabled_or_offline_reinstall_can_start_without_clearing_removal_protection() {
        for phase in [Phase::Disabled, Phase::Pending] {
            let (_temporary, mut session, before) = installed_fixture();
            fs::remove_file(session.config.install.join("slot.sav")).unwrap();
            session.control.status(phase, None);

            session.capture_without_sync().unwrap();
            session.started().unwrap();

            assert!(session.game_guard_held());
            assert_eq!(session.vault.local(), Some(&before.files));
            let context = Context::load(&session.config, ACCOUNT).unwrap().unwrap();
            assert!(context.installation_removed);
            assert!(context.needs_work);
            assert!(context.desired.is_none());
            session.aborted();
            let local = session.capture_before_launch().unwrap().unwrap();
            assert!(local.0.is_empty());
            assert!(matches!(
                session.check_removed_upload(&local, &before, None),
                Err(Failure::Conflict)
            ));
            assert!(
                session
                    .check_removed_upload(&local, &before, Some(Choice::Local))
                    .is_ok()
            );
        }
    }

    #[tokio::test]
    async fn preparation_authentication_failure_is_not_reported_as_cancellation() {
        let (_temporary, mut session, _) = fixture();
        session.control.revoke(ACCOUNT).unwrap();

        assert!(matches!(
            session.prepare(&no_other_session).await,
            Err(crate::Error::Cloud(Failure::Authentication))
        ));
        let status = session.control.state.lock().unwrap().status.clone();
        assert_eq!(status.phase, Phase::Failed);
        assert_eq!(status.error.as_deref(), Some(Failure::Authentication.key()));
    }

    #[test]
    fn interrupted_uninstall_cannot_capture_missing_files_as_cloud_deletions() {
        let (_temporary, mut session, before) = installed_fixture();
        fs::remove_file(session.config.install.join("slot.sav")).unwrap();
        session.context_mut().unwrap().needs_work = true;
        session.context().unwrap().save().unwrap();

        assert!(matches!(
            session.recover_exit_snapshot(),
            Err(Failure::Changed)
        ));

        assert_eq!(session.vault.local(), Some(&before.files));
        let context = Context::load(&session.config, ACCOUNT).unwrap().unwrap();
        assert!(context.needs_work);
        assert!(context.desired.is_none());
    }

    #[test]
    fn equal_reinstall_and_explicit_cloud_choice_clear_the_marker() {
        for cloud_choice in [false, true] {
            let (_temporary, mut session, remote) = fixture();
            if cloud_choice {
                fs::remove_file(session.config.install.join("slot.sav")).unwrap();
            }
            let local = session
                .vault
                .capture(&session.context.as_ref().unwrap().bindings)
                .unwrap();
            let choice = if cloud_choice {
                assert!(matches!(
                    session.reconcile_launch(&local, &remote, None),
                    Err(Failure::Conflict)
                ));
                Some(Choice::Cloud)
            } else {
                None
            };

            session.reconcile_launch(&local, &remote, choice).unwrap();

            assert!(
                !Context::load(&session.config, ACCOUNT)
                    .unwrap()
                    .unwrap()
                    .installation_removed
            );
            assert_eq!(
                fs::read(session.config.install.join("slot.sav")).unwrap(),
                b"install"
            );
            assert!(session.vault.pending().is_none());
        }
    }

    #[test]
    fn uninstalled_cloud_choice_keeps_local_snapshot_and_remote_baseline_without_projection() {
        let (_temporary, mut session, before) = fixture();
        let outside = session.config.home.join("outside.sav");
        fs::write(session.config.install.join("slot.sav"), b"exit version").unwrap();
        let local = session
            .vault
            .capture(&session.context.as_ref().unwrap().bindings)
            .unwrap();
        let authorization = session.vault.authorize(ACCOUNT).unwrap();
        session
            .vault
            .queue_upload(&local, &before, "test-exit", authorization)
            .unwrap();
        let remote = RemoteView {
            revision: "2".into(),
            files: Manifest(
                [
                    (
                        "install".into(),
                        session.vault.stage(b"remote install", 7).unwrap(),
                    ),
                    (
                        "outside".into(),
                        session.vault.stage(b"remote outside", 7).unwrap(),
                    ),
                ]
                .into(),
            ),
        };
        let context = session.context.as_mut().unwrap();
        context.remote = remote.clone();
        context.desired = Some(local.clone());
        context.needs_work = true;
        context.save().unwrap();
        let old_fingerprint = context.fingerprint();
        fs::remove_dir_all(&session.config.install).unwrap();
        fs::write(&outside, b"outside progress after exit").unwrap();

        session.choose_cloud_after_exit(&local, &remote).unwrap();

        assert!(!session.config.install.exists());
        assert_eq!(fs::read(&outside).unwrap(), b"outside progress after exit");
        assert!(session.vault.pending().is_none());
        assert_eq!(session.vault.baseline(), Some(&remote));
        assert_eq!(session.vault.local(), Some(&local));
        assert_eq!(
            session.vault.read_content(&local.0["install"]).unwrap(),
            b"exit version"
        );
        assert_eq!(
            session
                .vault
                .read_content(&remote.files.0["install"])
                .unwrap(),
            b"remote install"
        );
        let saved =
            Context::load_app(&session.config.root, ACCOUNT, session.config.app_id).unwrap();
        assert!(saved.installation_removed);
        assert!(!saved.needs_work);
        assert!(saved.desired.is_none());
        assert!(saved.transaction.is_none());
        assert_ne!(saved.fingerprint(), old_fingerprint);
        assert_eq!(
            session.control.state.lock().unwrap().status.phase,
            Phase::Synced
        );
    }

    #[test]
    fn missing_install_without_removal_marker_does_not_discard_saved_work() {
        let (_temporary, mut session, remote) = fixture();
        let context = session.context.as_mut().unwrap();
        context.installation_removed = false;
        context.desired = Some(remote.files.clone());
        context.needs_work = true;
        context.save().unwrap();
        let before_fingerprint = context.fingerprint();
        fs::remove_dir_all(&session.config.install).unwrap();

        assert!(matches!(
            session.choose_cloud_after_exit(&remote.files, &remote),
            Err(Failure::Conflict)
        ));

        let saved =
            Context::load_app(&session.config.root, ACCOUNT, session.config.app_id).unwrap();
        assert!(saved.needs_work);
        assert_eq!(saved.desired, Some(remote.files.clone()));
        assert_eq!(saved.fingerprint(), before_fingerprint);
        assert!(!session.config.install.exists());
    }

    #[test]
    fn uninstalled_cloud_choice_cannot_discard_an_uncertain_upload() {
        let (_temporary, mut session, remote) = fixture();
        fs::write(session.config.install.join("slot.sav"), b"uncertain exit").unwrap();
        let local = session
            .vault
            .capture(&session.context.as_ref().unwrap().bindings)
            .unwrap();
        let authorization = session.vault.authorize(ACCOUNT).unwrap();
        let id = session
            .vault
            .queue_upload(&local, &remote, "test-exit", authorization)
            .unwrap();
        session
            .vault
            .begin_upload(&id, authorization, &remote)
            .unwrap();
        let context = session.context.as_mut().unwrap();
        context.desired = Some(local.clone());
        context.needs_work = true;
        context.save().unwrap();
        fs::remove_dir_all(&session.config.install).unwrap();

        assert!(matches!(
            session.choose_cloud_after_exit(&local, &remote),
            Err(Failure::Uncertain)
        ));

        assert_eq!(session.vault.pending().unwrap().id, id);
        assert!(session.vault.pending().unwrap().may_have_committed);
        assert_eq!(session.context.as_ref().unwrap().desired, Some(local));
        assert!(!session.config.install.exists());
    }

    #[test]
    fn loading_by_app_rejects_a_context_copied_into_another_account_or_app() {
        let (_temporary, session, remote) = fixture();
        let root = session.config.root.clone();
        let app_id = session.config.app_id;
        drop(session);

        let context = Context::load_app(&root, ACCOUNT, app_id).unwrap();

        assert_eq!(context.remote, remote);
        for (account, requested_app) in [(ACCOUNT + 1, app_id), (ACCOUNT, app_id + 1)] {
            let copied = root
                .join(account.to_string())
                .join(requested_app.to_string())
                .join("context.json");
            store::write(&copied, &context).unwrap();
            assert!(matches!(
                Context::load_app(&root, account, requested_app),
                Err(Failure::Storage)
            ));
        }
    }

    #[test]
    fn cleanup_preserves_durable_exit_and_conflict_files_and_completed_sync_prunes_orphans() {
        let (_temporary, mut session, baseline) = fixture();
        let desired = Manifest(
            [(
                "install".into(),
                session.vault.stage(b"pending exit", 7).unwrap(),
            )]
            .into(),
        );
        let conflict = RemoteView {
            revision: "2".into(),
            files: Manifest(
                [(
                    "install".into(),
                    session.vault.stage(b"deck conflict", 7).unwrap(),
                )]
                .into(),
            ),
        };
        let orphan = session.vault.stage(b"obsolete attempt", 7).unwrap();
        let context = session.context.as_mut().unwrap();
        context.needs_work = true;
        context.desired = Some(desired.clone());
        context.remote = conflict.clone();
        context.save().unwrap();
        let fingerprint = context.fingerprint();
        let config = session.config.clone();
        let credentials = session.credentials.clone();
        let control = session.control.clone();
        drop(session);
        let mut session = Session::open(
            config,
            credentials,
            uuid::Uuid::new_v4().to_string(),
            control,
        )
        .unwrap();
        session.collect_garbage();
        assert_eq!(
            session.vault.read_content(&desired.0["install"]).unwrap(),
            b"pending exit"
        );
        assert_eq!(
            session
                .vault
                .read_content(&conflict.files.0["install"])
                .unwrap(),
            b"deck conflict"
        );
        assert_eq!(
            session
                .vault
                .read_content(&baseline.files.0["install"])
                .unwrap(),
            b"install"
        );
        assert!(session.vault.read_content(&orphan).is_err());
        assert_eq!(session.context().unwrap().fingerprint(), fingerprint);
        // Successful finalization durably drops the old exit reference first.
        session.completed().unwrap();
        assert!(session.vault.read_content(&desired.0["install"]).is_err());
        assert!(
            session
                .vault
                .read_content(&conflict.files.0["install"])
                .is_ok()
        );
        assert_eq!(
            fs::read(session.config.install.join("slot.sav")).unwrap(),
            b"install"
        );
    }

    #[test]
    fn cleanup_never_runs_through_an_active_game_guard() {
        let (_temporary, mut session, _) = fixture();
        let orphan = session.vault.stage(b"staged while guarded", 7).unwrap();
        session.vault.game_started(&session.session_id).unwrap();
        session.collect_garbage();
        assert!(session.vault.read_content(&orphan).is_ok());
        session.vault.game_exited(&session.session_id).unwrap();
        session.completed().unwrap();
        assert!(session.vault.read_content(&orphan).is_err());
    }

    #[test]
    fn cleanup_keeps_the_last_durable_context_when_memory_has_advanced() {
        let (_temporary, mut session, baseline) = fixture();
        let desired = Manifest(
            [(
                "install".into(),
                session.vault.stage(b"durable exit", 7).unwrap(),
            )]
            .into(),
        );
        let remote = RemoteView {
            revision: "2".into(),
            files: Manifest(
                [(
                    "install".into(),
                    session.vault.stage(b"durable conflict", 7).unwrap(),
                )]
                .into(),
            ),
        };
        let context = session.context_mut().unwrap();
        context.desired = Some(desired.clone());
        context.remote = remote.clone();
        context.save().unwrap();
        // As after an unsuccessful context write, disk still owns these bytes.
        context.desired = None;
        context.remote = baseline;
        session.collect_garbage();
        assert!(session.vault.read_content(&desired.0["install"]).is_ok());
        assert!(
            session
                .vault
                .read_content(&remote.files.0["install"])
                .is_ok()
        );
        session.completed().unwrap();
        assert!(session.vault.read_content(&desired.0["install"]).is_err());
        assert!(
            session
                .vault
                .read_content(&remote.files.0["install"])
                .is_err()
        );
    }

    #[test]
    fn lost_receipts_reopen_for_a_fresh_attempt_without_rewriting_the_old_transaction() {
        use steam_kit::cloud::{CloudError, UploadPhase};
        for (phase, unanswered) in [
            (UploadPhase::Beginning, true),
            (UploadPhase::Open { batch_id: 7 }, true),
            (UploadPhase::Interrupted { batch_id: Some(7) }, true),
            (UploadPhase::Closed { batch_id: 7 }, true),
            (UploadPhase::Completing { batch_id: 7 }, false),
        ] {
            let (_temporary, mut session, before) = fixture();
            fs::write(session.config.install.join("slot.sav"), b"new progress").unwrap();
            let local = session
                .vault
                .capture(&session.context.as_ref().unwrap().bindings)
                .unwrap();
            let auth = session.vault.authorize(ACCOUNT).unwrap();
            let old_id = session
                .vault
                .queue_upload(&local, &before, "steam-cm-autocloud", auth)
                .unwrap();
            let transaction = UploadTransaction::create(
                &store::directory(&session.config, ACCOUNT).unwrap(),
                session.context.as_ref().unwrap().listing.clone(),
                vec![CloudUpload {
                    name: "install".into(),
                    bytes: b"new progress".to_vec(),
                    timestamp: 1,
                    platforms: 7,
                }],
                vec![],
            )
            .unwrap();
            let directory = transaction.directory().to_path_buf();
            drop(transaction);
            let journal = directory.join("state.json");
            let mut state: serde_json::Value =
                serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
            state["phase"] = serde_json::to_value(phase).unwrap();
            state["unanswered_mutation"] = unanswered.into();
            let old_bytes = serde_json::to_vec(&state).unwrap();
            fs::write(&journal, &old_bytes).unwrap();
            let context = session.context.as_mut().unwrap();
            context.installation_removed = false;
            context.desired = Some(local.clone());
            context.needs_work = true;
            context.transaction = Some(directory.clone());
            context.save().unwrap();
            session.vault.begin_upload(&old_id, auth, &before).unwrap();
            let config = session.config.clone();
            let credentials = session.credentials.clone();
            let control = session.control.clone();
            drop(session);
            crate::tests::wait_for_lock_release(&config.root.join("owner.lock"));

            let mut session = Session::open(
                config,
                credentials,
                uuid::Uuid::new_v4().to_string(),
                control,
            )
            .unwrap();
            session.vault.set_preferences(Preferences {
                account_enabled: Some(true),
                game_enabled: Some(true),
            });
            let transaction = UploadTransaction::open(&directory).unwrap();
            assert!(!transaction.is_settled());
            let settled =
                recovery_is_settled(Err(CloudError::Uncertain), transaction.is_settled()).unwrap();
            session.reconcile_previous(&before, settled).unwrap();
            assert_eq!(
                session.reconcile_launch(&local, &before, None).unwrap(),
                Decision::KeepLocal
            );
            assert_eq!(session.vault.baseline(), Some(&before));
            assert!(session.vault.pending().is_none());
            let saved = Context::load(&session.config, ACCOUNT).unwrap().unwrap();
            assert!(saved.transaction.is_none());
            assert!(saved.needs_work);
            assert_eq!(saved.desired, Some(local.clone()));
            assert_eq!(fs::read(&journal).unwrap(), old_bytes);

            let fresh_id = session
                .vault
                .queue_upload(
                    &local,
                    &before,
                    "steam-cm-autocloud",
                    session.vault.authorize(ACCOUNT).unwrap(),
                )
                .unwrap();
            assert_ne!(fresh_id, old_id);
        }
    }

    #[test]
    fn receipt_recovery_keeps_hard_errors_and_allows_a_fresh_read_after_timeout() {
        use steam_kit::cloud::CloudError;
        for error in [
            CloudError::Uncertain,
            CloudError::Network,
            CloudError::Timeout,
            CloudError::Changed,
        ] {
            assert!(!recovery_is_settled(Err(error), false).unwrap());
        }
        for error in [
            CloudError::Revoked,
            CloudError::Storage,
            CloudError::Integrity,
            CloudError::Invalid("fixture"),
        ] {
            assert!(recovery_is_settled(Err(error), false).is_err());
        }
        assert!(recovery_is_settled(Ok(()), true).unwrap());
        assert!(Failure::Uncertain.retryable());
        assert!(!Failure::Conflict.retryable());
        assert!(!Failure::Authentication.retryable());
    }

    #[test]
    fn retry_accepts_already_uploaded_content_and_does_not_reupload_a_cloud_choice() {
        for already_uploaded in [true, false] {
            let (_temporary, mut session, before) = fixture();
            fs::write(session.config.install.join("slot.sav"), b"local progress").unwrap();
            let local = session
                .vault
                .capture(&session.context.as_ref().unwrap().bindings)
                .unwrap();
            let auth = session.vault.authorize(ACCOUNT).unwrap();
            let id = session
                .vault
                .queue_upload(&local, &before, "fixture", auth)
                .unwrap();
            session.vault.begin_upload(&id, auth, &before).unwrap();
            session.context.as_mut().unwrap().installation_removed = false;
            let mut remote = RemoteView {
                revision: "2".into(),
                files: local.clone(),
            };
            if !already_uploaded {
                remote.files.0.insert(
                    "install".into(),
                    session.vault.stage(b"deck progress", 7).unwrap(),
                );
            }
            session.reconcile_previous(&remote, false).unwrap();
            if already_uploaded {
                assert_eq!(
                    session.reconcile_launch(&local, &remote, None).unwrap(),
                    Decision::Converged
                );
            } else {
                assert!(matches!(
                    session.reconcile_launch(&local, &remote, None),
                    Err(Failure::Conflict)
                ));
                assert_eq!(
                    fs::read(session.config.install.join("slot.sav")).unwrap(),
                    b"local progress"
                );
                assert_eq!(
                    session
                        .reconcile_launch(&local, &remote, Some(Choice::Cloud))
                        .unwrap(),
                    Decision::Download
                );
                assert_eq!(
                    fs::read(session.config.install.join("slot.sav")).unwrap(),
                    b"deck progress"
                );
            }
            assert_eq!(session.vault.baseline(), Some(&remote));
            assert!(session.vault.pending().is_none());
        }
    }
}
