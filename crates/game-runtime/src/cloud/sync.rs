//! One Cloud sync, whatever the storefront: before a launch, after the game
//! exits, or as Cloud maintenance. This module owns the order of the steps,
//! the retries and Cloud questions, the choice between local and Cloud saves
//! and the work left pending. A storefront plugs in as a `Storefront`: its
//! Cloud rules, where each save lives, a verified remote view and publication.
use super::{Choice, Control, Phase, Question, Stop};
use crate::cloud_status::{self, ProblemKind, UnavailableReason};
use crate::supervisor::client::SessionAlive;
use cloud_vault::{
    AccountIdentity, Bindings, Decision, Manifest, RemoteView, Retry, RetryBudget, StoreScope,
    Vault,
};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc, time::Instant};

#[cfg(test)]
pub(crate) mod tests;

/// How a failed step affects a Cloud sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Another attempt may succeed: the network, a changing remote, an upload
    /// whose outcome is unknown.
    Transient,
    /// Local and Cloud saves both changed since the last sync.
    Conflict,
    /// The storefront's Cloud cannot sync this game for now. Playing without
    /// it leaves local saves safe.
    Unavailable,
    /// Nothing may go on without a sync: storage, ownership, recovery.
    Blocking,
    /// The account's session was refused or revoked.
    Denied,
    Cancelled,
}
impl Kind {
    fn retryable(self) -> bool {
        matches!(self, Self::Transient | Self::Conflict)
    }
    fn launch_anyway(self) -> bool {
        matches!(self, Self::Transient | Self::Unavailable)
    }
}

/// A storefront's failures, as the sync classifies and reports them.
pub(crate) trait Failure:
    From<Stop> + From<cloud_vault::Error> + std::fmt::Debug + Send + 'static
{
    fn kind(&self) -> Kind;
    /// Localization key of the failure.
    fn key(&self) -> String;
    /// An attempt ran out of time.
    fn timed_out() -> Self;
    fn conflict() -> Self;
    /// The installation or durable state a sync needs is missing.
    fn recovery() -> Self;
    /// The storefront has no Cloud rules Forge can follow for a game whose
    /// saves Forge already holds.
    fn unsupported() -> Self;
}

/// The work a Cloud sync leaves durable for a later sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", deny_unknown_fields)]
pub(crate) enum Pending {
    /// A game was admitted; its exit save is not captured yet.
    Capture,
    /// An exit save, or local saves the player kept, wait to be sent.
    Upload { desired: Manifest },
    /// A chosen Cloud version is being applied.
    Applying {
        desired: Manifest,
        remote: RemoteView,
        /// Whether the remote set is written into the installation.
        project: bool,
    },
}
impl Pending {
    pub(crate) fn files(&self) -> Option<&Manifest> {
        match self {
            Self::Capture => None,
            Self::Upload { desired } | Self::Applying { desired, .. } => Some(desired),
        }
    }
}

/// A storefront's durable Cloud facts about one game.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Work {
    pub pending: Option<Pending>,
    /// An uninstall may have removed part of the saves. This records removal
    /// intent, not proof that the installation is absent.
    pub installation_removed: bool,
}
impl Work {
    /// What a game's Cloud indicator reads from this work.
    pub(crate) fn journal(work: Option<&Self>, uncertain: bool) -> cloud_status::Journal {
        let pending = work.and_then(|work| work.pending.as_ref());
        cloud_status::Journal {
            pending: matches!(pending, Some(Pending::Capture | Pending::Upload { .. })),
            uncertain,
            applying: matches!(pending, Some(Pending::Applying { .. })),
            installation_removed: work.is_some_and(|work| work.installation_removed),
        }
    }
}

/// Whether the storefront's Cloud can sync this game now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Available,
    Unavailable(UnavailableReason),
}

/// What a storefront adds to a Cloud sync. Steps that talk to the storefront
/// take the attempt's `Link`, which the sync drops after each attempt.
#[allow(async_fn_in_trait)]
pub(crate) trait Storefront {
    type Failure: Failure;
    type Link;

    fn root(&self) -> &Path;
    fn scope(&self) -> StoreScope;
    fn account(&self) -> AccountIdentity;
    /// Reads the durable state under the vault's lock.
    fn load(&mut self) -> Result<(), Self::Failure>;
    /// `None` until the game's Cloud rules were known once.
    fn work(&self) -> Option<Work>;
    fn save(&mut self, work: Work) -> Result<(), Self::Failure>;
    fn installation_absent(&self) -> Result<bool, Self::Failure>;
    /// Where each save lives: the local set, and `remote`'s names. `create`
    /// makes their directories inside an installation that exists.
    fn bindings(
        &mut self,
        vault: &Vault,
        remote: Option<&Manifest>,
        create: bool,
    ) -> Result<Bindings, Self::Failure>;
    /// A launch may go on without a sync: it knows where the saves live.
    fn offline(&self) -> bool;
    /// Before garbage collection: every manifest its durable and in-memory
    /// state references. Staged bytes it does not list may be deleted.
    /// `None` defers the collection, when that state cannot be read.
    fn retained(&mut self) -> Option<Vec<Manifest>>;

    async fn connect(&mut self, control: &Arc<Control>) -> Result<Self::Link, Self::Failure>;
    /// Reads the game's Cloud rules and whether Cloud is on. `adopt` accepts
    /// rules that changed: the local side is captured under the new ones.
    async fn rules(
        &mut self,
        link: &mut Self::Link,
        vault: &mut Vault,
        adopt: bool,
    ) -> Result<Availability, Self::Failure>;
    /// Settles an earlier upload whose outcome was unknown. True once that
    /// outcome is established; false leaves only a fresh comparison.
    async fn settle(&mut self, link: &mut Self::Link, vault: &Vault)
    -> Result<bool, Self::Failure>;
    /// The earlier upload is reconciled: forget what settling it needed.
    fn reconciled(&mut self) -> Result<(), Self::Failure>;
    /// A verified complete view of the game's Cloud saves.
    async fn view(
        &mut self,
        link: &mut Self::Link,
        vault: &mut Vault,
        control: &Control,
    ) -> Result<RemoteView, Self::Failure>;
    /// Sends `local` over `remote`, then completes the vault's upload with a
    /// verified readback.
    async fn publish(
        &mut self,
        link: &mut Self::Link,
        vault: &mut Vault,
        control: &Control,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<(), Self::Failure>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Launch,
    Exit,
    /// `installed` compares today's saves; otherwise the archived ones.
    Maintenance {
        installed: bool,
    },
}
impl Purpose {
    /// Today's saves are captured under the rules just read.
    fn fresh(self) -> bool {
        matches!(self, Self::Launch | Self::Maintenance { installed: true })
    }
}

/// One game's Cloud sync for one account.
pub(crate) struct CloudSync<S: Storefront> {
    store: S,
    vault: Vault,
    control: Arc<Control>,
    /// The Game session whose game guard this sync takes.
    session_id: String,
    shown: Option<(RemoteView, Manifest)>,
    /// How the current operation ended without a sync, for the status file.
    problem: Option<(ProblemKind, String)>,
}

type Outcome<S> = Result<(), <S as Storefront>::Failure>;

impl<S: Storefront> CloudSync<S> {
    pub(crate) fn open(
        mut store: S,
        control: Arc<Control>,
        session_id: String,
    ) -> Result<Self, S::Failure> {
        let vault = Vault::open(store.root(), store.scope())?;
        store.load()?;
        Ok(Self {
            store,
            vault,
            control,
            session_id,
            shown: None,
            problem: None,
        })
    }

    pub(crate) fn store(&self) -> &S {
        &self.store
    }

    /// The launch check. `alive` answers for a previous session whose game
    /// guard remains; the caller holds the storefront's owner lock.
    pub(crate) async fn prepare(&mut self, alive: &SessionAlive<'_>) -> Outcome<S> {
        self.operation(async |sync| {
            sync.recover_dead_owner(alive)?;
            sync.run(Purpose::Launch).await
        })
        .await
        .inspect_err(|failure| {
            if failure.kind() != Kind::Cancelled {
                self.control.show(Phase::Failed, Some(failure.key()));
            }
        })
    }

    /// Inside the admission lock: the game is about to run.
    pub(crate) fn started(&mut self) -> Outcome<S> {
        self.control.check()?;
        let Some(mut work) = self.store.work() else {
            return Ok(());
        };
        // A persisted marker schedules the exit capture even if the
        // supervisor disappears while the game runs.
        work.pending = Some(Pending::Capture);
        self.store.save(work)?;
        self.vault.game_started(&self.session_id)?;
        Ok(())
    }

    /// Settles the exit save. `confirmed` is false when exec was never
    /// confirmed: the save is then only captured and kept pending. Never
    /// fails: its result lives in the Cloud status.
    pub(crate) async fn finished(&mut self, confirmed: bool) {
        let syncs = self.vault.syncs();
        self.problem = None;
        if let Err(failure) = self.finish(confirmed).await {
            // An unanswered conflict still reports why the save waits.
            let reason = match &self.problem {
                Some((ProblemKind::Conflict, key)) => key.clone(),
                _ => failure.key(),
            };
            self.control.show(Phase::Pending, Some(reason));
        }
        self.record_outcome(syncs, true);
        self.collect_garbage();
    }

    /// A launch abandoned before spawn releases its Cloud work.
    pub(crate) fn aborted(&mut self) {
        if self.guard_held() {
            let _ = self.vault.game_exited(&self.session_id);
            if self.capture_exit().is_ok() {
                self.control.show(Phase::Pending, None);
            }
        }
        // Account revocation abandoned this launch: its authorization ends now.
        if self.control.revoked() {
            let _ = self.vault.revoke();
        }
    }

    /// Cloud maintenance: a two-way sync without a launch. The caller holds
    /// the idle guard, so no supervisor or game owns the saves.
    pub(crate) async fn maintain(&mut self, alive: &SessionAlive<'_>) -> Outcome<S> {
        self.operation(async |sync| {
            sync.recover_dead_owner(alive)?;
            let installed = !sync.store.installation_absent()?;
            sync.run(Purpose::Maintenance { installed }).await
        })
        .await
    }

    /// A launch check or Cloud maintenance, with its outcome recorded.
    async fn operation(&mut self, body: impl AsyncFnOnce(&mut Self) -> Outcome<S>) -> Outcome<S> {
        let syncs = self.vault.syncs();
        self.problem = None;
        self.collect_garbage();
        let result = body(self).await;
        if let Err(failure) = &result
            && self.problem.is_none()
        {
            self.problem = Some((ProblemKind::Failed, failure.key()));
        }
        self.record_outcome(syncs, false);
        self.collect_garbage();
        result
    }

    async fn finish(&mut self, confirmed: bool) -> Outcome<S> {
        if self.guard_held() {
            self.vault.game_exited(&self.session_id)?;
        }
        if self.store.work().is_none() {
            return Ok(());
        }
        self.capture_exit()?;
        if !confirmed {
            self.control.show(Phase::Pending, None);
            return Ok(());
        }
        if self.control.revoked() {
            self.vault.revoke()?;
            self.control.show(Phase::Pending, None);
            return Ok(());
        }
        self.collect_garbage();
        let result = self.run(Purpose::Exit).await;
        if result.is_err() && self.control.revoked() {
            let _ = self.vault.revoke();
        }
        result
    }

    fn guard_held(&self) -> bool {
        self.vault.game_session() == Some(self.session_id.as_str())
    }

    fn mark(&self, change: impl FnOnce(&mut cloud_status::Record)) {
        // Display state only: a failed write must not fail the sync itself.
        if let Err(error) = cloud_status::update(self.store.root(), &self.store.scope(), change) {
            eprintln!("Cloud status not recorded: {error}");
        }
    }

    fn has_pending_work(&self) -> bool {
        self.store.work().is_some_and(|work| work.pending.is_some())
            || self.vault.pending().is_some()
    }

    /// Forge holds this game's saves, so its Cloud cannot silently vanish.
    fn saved_state(&self) -> bool {
        self.store.work().is_some()
            || self.vault.pending().is_some()
            || self.vault.game_session().is_some()
            || self.vault.local().is_some()
            || self.vault.baseline().is_some()
    }

    /// A sync clears the last problem. Otherwise a failed operation leaves a
    /// problem, unless pending work already shows it or `exit` says the game
    /// just exited; an unanswered conflict is recorded either way.
    fn record_outcome(&mut self, syncs: u64, exit: bool) {
        let problem = self.problem.take();
        if self.vault.syncs() != syncs {
            self.mark(cloud_status::synced);
        } else if let Some((kind, key)) = problem
            && (kind == ProblemKind::Conflict || !exit && !self.has_pending_work())
        {
            self.mark(cloud_status::problem(kind, &key));
        }
    }

    /// Only the matching session's actual exit may clear its game guard.
    fn recover_dead_owner(&mut self, alive: &SessionAlive<'_>) -> Outcome<S> {
        if let Some(old) = self.vault.game_session().map(str::to_owned) {
            if alive(&old).map_err(|_| S::Failure::recovery())? {
                return Err(cloud_vault::Error::Busy.into());
            }
            self.vault.game_exited(&old)?;
            self.vault.recover_local()?;
            if self.store.work().is_some() {
                self.capture_exit()?;
            }
            return Ok(());
        }
        self.vault.recover_local()?;
        self.recover_applying()?;
        if self
            .store
            .work()
            .is_some_and(|work| matches!(work.pending, Some(Pending::Capture)))
        {
            self.capture_exit()?;
        }
        Ok(())
    }

    /// An interrupted Cloud choice either completed or leaves its local
    /// version to send again.
    fn recover_applying(&mut self) -> Outcome<S> {
        let Some(mut work) = self.store.work() else {
            return Ok(());
        };
        if let Some(Pending::Applying {
            desired,
            remote,
            project,
        }) = work.pending.clone()
        {
            let completed = self.vault.baseline() == Some(&remote)
                && (!project || self.vault.local() == Some(&remote.files));
            work.pending = (!completed).then_some(Pending::Upload { desired });
            self.store.save(work)?;
        }
        Ok(())
    }

    /// Captures the saves the game left as pending work. After an uninstall,
    /// files already removed keep their archived version and no directory is
    /// recreated.
    fn capture_exit(&mut self) -> Outcome<S> {
        let mut work = self.store.work().ok_or_else(S::Failure::recovery)?;
        let removed = work.installation_removed;
        let mut bindings = self.store.bindings(&self.vault, None, !removed)?;
        let desired = if removed {
            bindings.retain(|_, file| file.path.parent().is_some_and(Path::is_dir));
            self.vault.capture_preserving_missing(&bindings)?
        } else {
            self.vault.capture(&bindings)?
        };
        work.pending = Some(Pending::Upload { desired });
        self.store.save(work)
    }

    /// Launch anyway: ownership still applies offline, and the archive holds
    /// today's saves. Files an uninstall removed keep their archived version.
    fn capture_offline(&mut self) -> Outcome<S> {
        let Some(work) = self.store.work() else {
            return Ok(());
        };
        let bindings = self.store.bindings(&self.vault, None, true)?;
        self.vault.activate(&bindings)?;
        if !work.installation_removed {
            self.vault.capture(&bindings)?;
        }
        Ok(())
    }

    /// Compares until both sides agree, the player decides or the retries run
    /// out. Only a launch asks about failures; a conflict is asked always.
    async fn run(&mut self, purpose: Purpose) -> Outcome<S> {
        let mut budget = RetryBudget::new(Instant::now());
        let mut last = None;
        let mut choice = None;
        loop {
            self.control.check()?;
            let remaining = match budget.poll(Instant::now()) {
                Retry::Wait(wait) => {
                    self.control.wait(wait).await?;
                    continue;
                }
                Retry::Defer => {
                    let last = last.take().unwrap_or_else(S::Failure::timed_out);
                    if purpose != Purpose::Launch {
                        self.problem = Some((ProblemKind::Failed, last.key()));
                        return Err(last);
                    }
                    match self.ask_after_failure(&last).await? {
                        Choice::Retry => {
                            budget = RetryBudget::new(Instant::now());
                            continue;
                        }
                        _ => {
                            // Launch anyway: preserve today's saves for the next sync.
                            // Rules the Cloud cannot follow find no saves to preserve.
                            self.problem = Some((ProblemKind::Failed, last.key()));
                            match self.capture_offline() {
                                Err(failure) if failure.kind() == Kind::Unavailable => {}
                                captured => captured?,
                            }
                            self.control.show(Phase::Pending, Some(last.key()));
                            return Ok(());
                        }
                    }
                }
                Retry::Attempt { remaining } => remaining,
            };
            self.control.show(
                if purpose == Purpose::Launch {
                    Phase::Checking
                } else {
                    Phase::Syncing
                },
                None,
            );
            let control = self.control.clone();
            let attempt = self.attempt(purpose, choice.take());
            let result = match tokio::time::timeout(remaining, control.interruptible(attempt)).await
            {
                Ok(Ok(result)) => result,
                Ok(Err(stop)) => Err(stop.into()),
                Err(_) => Err(S::Failure::timed_out()),
            };
            let failure = match result {
                Ok(()) => return Ok(()),
                Err(failure) => failure,
            };
            match failure.kind() {
                Kind::Conflict if self.shown.is_some() => {
                    let answer = self
                        .control
                        .ask(Question::Conflict, Some(failure.key()))
                        .await;
                    match answer {
                        Ok(answer @ (Choice::Local | Choice::Cloud)) => {
                            self.mark(cloud_status::answered);
                            choice = Some(answer);
                            budget = RetryBudget::new(Instant::now());
                        }
                        answer => {
                            self.problem = Some((ProblemKind::Conflict, failure.key()));
                            return Err(match answer {
                                Err(stop) => stop.into(),
                                Ok(_) => Stop::Cancelled.into(),
                            });
                        }
                    }
                }
                Kind::Denied | Kind::Cancelled => {
                    self.problem = Some((ProblemKind::Failed, failure.key()));
                    return Err(failure);
                }
                kind => {
                    budget.failed(Instant::now(), !kind.retryable());
                    last = Some(failure);
                }
            }
        }
    }

    /// Retry or Launch anyway, once a launch check's retries are spent.
    async fn ask_after_failure(&mut self, last: &S::Failure) -> Result<Choice, S::Failure> {
        let question = Question::Failed {
            launch_anyway: last.kind().launch_anyway() && self.store.offline(),
        };
        match self.control.ask(question, Some(last.key())).await {
            Ok(choice @ (Choice::Retry | Choice::LaunchAnyway)) => Ok(choice),
            answer => {
                self.problem = Some((ProblemKind::Failed, last.key()));
                Err(match answer {
                    Err(stop) => stop.into(),
                    Ok(_) => Stop::Cancelled.into(),
                })
            }
        }
    }

    async fn attempt(&mut self, purpose: Purpose, choice: Option<Choice>) -> Outcome<S> {
        self.control.check()?;
        self.vault.recover_local()?;
        self.recover_applying()?;
        let mut link = self.store.connect(&self.control).await?;
        match self
            .store
            .rules(&mut link, &mut self.vault, purpose.fresh())
            .await?
        {
            Availability::Available => self.mark(cloud_status::available),
            Availability::Unavailable(reason) => return self.unavailable(purpose, reason),
        }
        // Read before the vault retires an earlier upload below.
        let retrying = self.has_pending_work();
        let captured = if purpose.fresh() {
            None
        } else {
            Some(self.captured_local(purpose)?)
        };
        // A prior request may have committed. Never replay it: a fresh
        // verified complete view admits a new comparison.
        let previous = match self.vault.pending() {
            Some(_) => Some(self.store.settle(&mut link, &self.vault).await?),
            None => None,
        };
        let remote = self
            .store
            .view(&mut link, &mut self.vault, &self.control)
            .await?;
        if let Some(settled) = previous {
            self.reconcile_previous(&remote, settled)?;
        }
        match captured {
            None => {
                self.compare_fresh(&mut link, purpose, &remote, retrying, choice)
                    .await?
            }
            Some(local) => {
                self.compare_captured(&mut link, &remote, local, choice)
                    .await?
            }
        }
        self.control.show(
            if purpose == Purpose::Launch {
                Phase::Ready
            } else {
                Phase::Synced
            },
            None,
        );
        Ok(())
    }

    fn unavailable(&mut self, purpose: Purpose, reason: UnavailableReason) -> Outcome<S> {
        self.mark(cloud_status::unavailable(reason));
        if reason == UnavailableReason::Disabled {
            if purpose.fresh() {
                self.capture_offline()?;
            } else if let Some(mut work) = self.store.work() {
                // Cloud is off, so this save is never sent. It stays in the
                // archive as the local snapshot, but not as pending work.
                work.pending = None;
                self.store.save(work)?;
            }
            self.control.show(Phase::Disabled, None);
            return Ok(());
        }
        // Pending work must not pass for a game without Cloud.
        if self.saved_state() {
            return Err(S::Failure::unsupported());
        }
        self.control.show(Phase::Unsupported, None);
        Ok(())
    }

    fn reconcile_previous(&mut self, current: &RemoteView, settled: bool) -> Outcome<S> {
        self.control.check()?;
        if self.vault.pending().is_none() {
            return Err(S::Failure::recovery());
        }
        self.vault
            .settle_pending(self.store.account(), current, settled)?;
        self.store.reconciled()
    }

    /// Today's saves against the Cloud. A launch sends them only to finish
    /// earlier work; maintenance syncs both ways.
    async fn compare_fresh(
        &mut self,
        link: &mut S::Link,
        purpose: Purpose,
        remote: &RemoteView,
        retrying: bool,
        choice: Option<Choice>,
    ) -> Outcome<S> {
        let mut work = self.store.work().ok_or_else(S::Failure::recovery)?;
        if self.store.installation_absent()? {
            return Err(S::Failure::recovery());
        }
        let removed = work.installation_removed;
        let bindings = self
            .store
            .bindings(&self.vault, Some(&remote.files), true)?;
        self.vault.activate(&bindings)?;
        // After a reinstall, missing files keep their archived version: an
        // uninstall never deletes Cloud saves.
        let local = if removed {
            self.vault.capture_preserving_missing(&bindings)?
        } else {
            self.vault.capture(&bindings)?
        };
        let maintenance = purpose != Purpose::Launch;
        // Saves unchanged since the last sync are compared, not queued: a
        // failed check must not report them as waiting to be sent.
        let queued = retrying
            || maintenance && self.vault.baseline().map(|base| &base.files) != Some(&local);
        if queued {
            work.pending = Some(Pending::Upload {
                desired: local.clone(),
            });
            self.store.save(work.clone())?;
        }
        let decision = self.decide(&mut work, &local, remote, choice)?;
        self.control.check()?;
        match decision {
            Decision::Converged => {
                if removed {
                    self.vault.restore_local(&bindings)?;
                }
                self.vault.confirm_equal(&local, remote)?;
            }
            Decision::Download => {
                work.pending = Some(Pending::Applying {
                    desired: local.clone(),
                    remote: remote.clone(),
                    project: true,
                });
                self.store.save(work.clone())?;
                let chosen = choice == Some(Choice::Cloud) || removed;
                self.vault
                    .apply_download(&bindings, remote, remote, chosen)?;
            }
            Decision::KeepLocal => {
                if removed {
                    self.vault.restore_local(&bindings)?;
                }
                if queued {
                    self.send(link, &mut work, &local, remote).await?;
                }
            }
            Decision::Conflict => unreachable!("decide reports conflicts"),
        }
        self.shown = None;
        self.store.save(Work {
            pending: None,
            installation_removed: false,
        })
    }

    /// The captured saves an exit or an uninstalled game's maintenance sends.
    fn captured_local(&self, purpose: Purpose) -> Result<Manifest, S::Failure> {
        let work = self.store.work().ok_or_else(S::Failure::recovery)?;
        match work.pending.as_ref().and_then(Pending::files) {
            Some(local) => Ok(local.clone()),
            None if purpose == Purpose::Exit => Err(S::Failure::recovery()),
            // Without an exit save, an upload still in the vault supplies it,
            // or else the archive.
            None => self
                .vault
                .pending()
                .map(|pending| pending.desired.clone())
                .or_else(|| self.vault.local().cloned())
                .ok_or_else(S::Failure::recovery),
        }
    }

    /// Captured saves against the Cloud: an exit save, or an uninstalled
    /// game's archive, whose installation is never recreated.
    async fn compare_captured(
        &mut self,
        link: &mut S::Link,
        remote: &RemoteView,
        local: Manifest,
        choice: Option<Choice>,
    ) -> Outcome<S> {
        let mut work = self.store.work().ok_or_else(S::Failure::recovery)?;
        let absent = self.store.installation_absent()?;
        let removed = work.installation_removed;
        if absent && !removed {
            return Err(S::Failure::recovery());
        }
        let bindings = self
            .store
            .bindings(&self.vault, Some(&remote.files), !absent)?;
        let decision = self.decide(&mut work, &local, remote, choice)?;
        self.control.check()?;
        match decision {
            Decision::Converged => self.vault.confirm_equal(&local, remote)?,
            Decision::Download => {
                work.pending = Some(Pending::Applying {
                    desired: local.clone(),
                    remote: remote.clone(),
                    project: !absent,
                });
                self.store.save(work.clone())?;
                if absent {
                    // A Cloud choice resolves remote work only. The archived
                    // local version stays, and a reinstall asks again.
                    self.vault.confirm_equal(&remote.files, remote)?;
                } else {
                    // Never replace files edited since the captured version.
                    if self.vault.capture(&bindings)? != local {
                        return Err(S::Failure::conflict());
                    }
                    let chosen = choice == Some(Choice::Cloud) || removed;
                    self.vault
                        .apply_download(&bindings, remote, remote, chosen)?;
                }
            }
            Decision::KeepLocal => self.send(link, &mut work, &local, remote).await?,
            Decision::Conflict => unreachable!("decide reports conflicts"),
        }
        self.shown = None;
        self.store.save(Work {
            pending: None,
            installation_removed: removed && absent,
        })
    }

    /// A choice applies only to the question it answered. Without one, the
    /// removal fence and the vault decide.
    fn decide(
        &mut self,
        work: &mut Work,
        local: &Manifest,
        remote: &RemoteView,
        choice: Option<Choice>,
    ) -> Result<Decision, S::Failure> {
        if let Some(choice) = choice {
            if self
                .shown
                .as_ref()
                .is_none_or(|(shown, files)| shown != remote || files != local)
            {
                return self.conflict(work, local, remote);
            }
            self.shown = None;
            return match choice {
                Choice::Local => {
                    self.vault.choose_local(remote, remote)?;
                    Ok(Decision::KeepLocal)
                }
                Choice::Cloud => Ok(Decision::Download),
                _ => Err(Stop::Cancelled.into()),
            };
        }
        self.shown = None;
        // A reinstall can lack saves nobody deleted: never infer upload intent
        // from them, or replace unrelated saves with an entire archive.
        let decision = if work.installation_removed && local != &remote.files {
            Decision::Conflict
        } else {
            self.vault.decision(local, remote)?
        };
        if decision == Decision::Conflict {
            return self.conflict(work, local, remote);
        }
        Ok(decision)
    }

    /// An unanswered conflict is unsettled work, durable before it is shown.
    fn conflict(
        &mut self,
        work: &mut Work,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<Decision, S::Failure> {
        self.shown = Some((remote.clone(), local.clone()));
        work.pending = Some(Pending::Upload {
            desired: local.clone(),
        });
        self.store.save(work.clone())?;
        Err(S::Failure::conflict())
    }

    async fn send(
        &mut self,
        link: &mut S::Link,
        work: &mut Work,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Outcome<S> {
        if work.pending.as_ref().and_then(Pending::files) != Some(local) {
            work.pending = Some(Pending::Upload {
                desired: local.clone(),
            });
            self.store.save(work.clone())?;
        }
        self.control.check()?;
        self.store
            .publish(link, &mut self.vault, &self.control, local, remote)
            .await
    }

    /// Only between operations: staged bytes must first be referenced by the
    /// vault or the storefront's durable state.
    fn collect_garbage(&mut self) {
        let Some(mut retained) = self.store.retained() else {
            return;
        };
        if let Some((remote, local)) = &self.shown {
            retained.extend([remote.files.clone(), local.clone()]);
        }
        let retained: Vec<&Manifest> = retained.iter().collect();
        if let Err(error) = self.vault.collect_garbage(&retained) {
            // Housekeeping failure must not turn an acknowledged sync into a
            // failed transfer. The next operation boundary tries again.
            eprintln!("Cloud cache cleanup deferred: {error}");
        }
    }
}
