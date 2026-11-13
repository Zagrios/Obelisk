//! One account's Cloud saves between Game sessions, whatever the storefront:
//! the archive an uninstall takes before a game's files go, the removal it
//! records once they are gone, Cloud maintenance on the player's request, and
//! the Cloud indicators. A storefront plugs in as a `Storefront`: its Cloud
//! record, where each installation keeps its saves, the rules it gives now
//! and its Cloud sync. One that keeps no Cloud saves plugs in nothing.
use super::{
    Control,
    record::{self, Configuration as _, Record},
    sync::{self, Failure as _, Pending, Work},
};
use crate::{
    cloud_status,
    files::safe_storage_path,
    installations::{self, BoxFuture, Receipt, blocking},
    sessions::{self, Failure, failed},
    supervisor::{self, IdleGuard},
};
use cloud_vault::{AccountIdentity, StoreScope, Vault};
use std::{borrow::Borrow, collections::BTreeSet, fs, future::Future, path::Path, sync::Arc};

#[cfg(test)]
pub(crate) mod tests;

/// A game, as the account's library names it.
pub(crate) type Game<S> = <<S as Storefront>::Of as sessions::Storefront>::Game;
/// Where a game is installed and keeps its saves.
pub(crate) type Configuration<S> = <<S as Storefront>::Record as Record>::Configuration;
type Fail<S> = <<S as Storefront>::Record as Record>::Failure;

/// What a storefront adds to one account's Cloud saves between Game
/// sessions. Its failures are the keys the player sees.
pub(crate) trait Storefront: Send + Sync + 'static {
    /// The storefront, as its installations and Game sessions name it.
    type Of: installations::Storefront;
    /// Its Cloud record of one game, the one its Cloud sync holds.
    type Record: Record<Configuration: Send + Sync + 'static, Account: ToOwned<Owned: Send + 'static>>
        + Send
        + 'static;

    /// The Cloud archive.
    fn root(&self) -> &Path;
    /// The account, as its sign-in names it.
    fn account(&self) -> &<Self::Record as Record>::Account;
    /// Where the storefront records its Game sessions, and the helper that
    /// inspects them: what its idle guard takes.
    fn sessions(&self) -> (&Path, &Path);
    /// Where the Cloud archive keeps `game`'s saves for the account.
    fn scope(&self, game: &Game<Self>) -> StoreScope;
    /// The game `scope` names, when it is one of this storefront's.
    fn game(scope: StoreScope) -> Option<Game<Self>>;
    /// Where `installation` keeps its saves now.
    fn configuration(
        &self,
        installation: &Receipt<Self::Of>,
    ) -> impl Future<Output = Result<Configuration<Self>, Failure>> + Send;
    /// Before an uninstall: the rules the storefront gives now for the saves
    /// under `configuration`, for the account its session is signed in to.
    /// `None` when it gives none: nothing is locked.
    fn rules(
        &self,
        configuration: &Configuration<Self>,
    ) -> impl Future<Output = Result<Option<Archive<Self::Record>>, Failure>> + Send;
    /// What an uninstall fails with when the capture of its archive is lost
    /// with its thread.
    fn capture_lost() -> String;
    /// What maintenance of an uninstalled game syncs: the configuration its
    /// record `saved`, `None` without a record.
    fn recorded(&self, saved: Option<Configuration<Self>>) -> Result<Configuration<Self>, Failure>;
    /// Runs the Cloud sync of `configuration` as Cloud maintenance. `idle`
    /// proves that no launch of the storefront can start meanwhile.
    fn sync(
        &self,
        configuration: Configuration<Self>,
        control: Arc<Control>,
        idle: IdleGuard<Self::Of>,
    ) -> impl Future<Output = Result<(), Failure>> + Send;
}

/// What a storefront gives an uninstall archive: the game's vault, which
/// holds its storefront's lock, and the rules its saves follow now.
pub(crate) struct Archive<R> {
    pub vault: Vault,
    pub rules: Rules<R>,
}

/// Whether Obelisk follows the rules a storefront gives now, as its Cloud
/// sync decides it.
pub(crate) enum Rules<R> {
    /// The record the saves are captured under: the saved one or a new one,
    /// with these rules.
    Followed(R),
    /// The storefront keeps nothing of this build's saves under rules
    /// Obelisk follows. The record the game saved, if any, stays as it is.
    Unfollowed(Option<R>),
}

/// One account's Cloud saves between Game sessions, as its installations
/// reach them.
pub trait Saves<S: installations::Storefront>: Send + Sync {
    /// Before an uninstall removes `installation`'s files: archives its
    /// saves and raises the removal fence. A failure keeps the game.
    fn archive<'a>(&'a self, installation: &'a Receipt<S>) -> BoxFuture<'a, Result<(), Failure>>;
    /// Records the removal of `installation` for Cloud sync. Runs even after
    /// sign-out.
    fn removed<'a>(&'a self, installation: &'a Receipt<S>) -> BoxFuture<'a, Result<(), Failure>>;
    /// Cloud maintenance of `game`: syncs the saves of `installed`, or, for
    /// an uninstalled game, those its Cloud record names. `control` shows
    /// the sync's progress and asks the player.
    fn maintain<'a>(
        &'a self,
        game: &'a S::Game,
        installed: Option<&'a Receipt<S>>,
        control: Arc<Control>,
    ) -> BoxFuture<'a, Result<(), Failure>>;
    /// The Cloud indicators of `games`, and of every other game of the
    /// account with pending work. It blocks on file reads.
    fn indicators(
        &self,
        games: &[S::Game],
    ) -> Result<Vec<(S::Game, cloud_status::Indicator)>, Failure>;
}

/// The account's Cloud saves behind `store`, as its installations reach them.
pub(crate) fn of<S: Storefront>(store: &S) -> Box<dyn Saves<S::Of> + '_> {
    Box::new(Through(store))
}

/// A storefront's adapter, driven through this module.
struct Through<'s, S>(&'s S);

impl<S: Storefront> Saves<S::Of> for Through<'_, S> {
    fn archive<'a>(
        &'a self,
        installation: &'a Receipt<S::Of>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        let store = self.0;
        Box::pin(async move { archive(store, store.configuration(installation).await?).await })
    }
    fn removed<'a>(
        &'a self,
        installation: &'a Receipt<S::Of>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        let store = self.0;
        Box::pin(async move { removed(store, store.configuration(installation).await?).await })
    }
    fn maintain<'a>(
        &'a self,
        game: &'a Game<S>,
        installed: Option<&'a Receipt<S::Of>>,
        control: Arc<Control>,
    ) -> BoxFuture<'a, Result<(), Failure>> {
        let store = self.0;
        Box::pin(async move {
            let installed = match installed {
                Some(installation) => Some(store.configuration(installation).await?),
                None => None,
            };
            maintain(store, game, installed, control).await
        })
    }
    fn indicators(
        &self,
        games: &[Game<S>],
    ) -> Result<Vec<(Game<S>, cloud_status::Indicator)>, Failure> {
        indicators(self.0, games)
    }
}

/// A Cloud failure, as the player sees it.
fn shown(failure: impl sync::Failure) -> Failure {
    Failure::Failed(failure.key())
}

/// Runs record work off the async runtime: it reads and writes files.
async fn off_runtime<S: Storefront, T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Fail<S>> + Send + 'static,
) -> Result<T, Failure> {
    blocking(<S::Of as sessions::Storefront>::SESSION_FAILED, work)
        .await?
        .map_err(shown)
}

/// Before an uninstall removes the files under `configuration`: archives the
/// saves the storefront's rules name now and raises the removal fence.
pub(crate) async fn archive<S: Storefront>(
    store: &S,
    configuration: Configuration<S>,
) -> Result<(), Failure> {
    let Some(Archive { mut vault, rules }) = store.rules(&configuration).await? else {
        return Ok(());
    };
    // Off the async runtime: a capture rehashes every save.
    blocking(S::capture_lost(), move || match rules {
        Rules::Followed(record) => capture(&mut vault, record, configuration),
        Rules::Unfollowed(saved) => unfollowed(&vault, saved, &configuration),
    })
    .await?
    .map_err(shown)
}

/// Captures the saves under the installation's `configuration`, then raises
/// the removal fence. It never creates work: an exit save or an upload still
/// waiting stays as it is. After a partial uninstall, or once the saves
/// moved to another Wine prefix, a save missing now keeps its archived
/// version.
fn capture<R: Record>(
    vault: &mut Vault,
    mut record: R,
    configuration: R::Configuration,
) -> Result<(), R::Failure> {
    let gone = record::absent(configuration.install())?;
    record::rebind(&mut record, configuration);
    let fenced = gone || record.work().installation_removed;
    let bindings = record.bindings(vault, None, false)?;
    record::capture(vault, bindings, fenced)?;
    // Persist before removal starts: recursive deletion can fail partway.
    record::fence(&mut record);
    record.save()
}

/// The storefront keeps nothing of this build's saves, so the uninstall has
/// nothing to capture and goes on. A game whose record is its other build's
/// still gets its removal fence before any file goes: its archived saves are
/// never sent as deleted. A record of this build holds saves the storefront
/// kept under earlier rules: what the build wrote since cannot be captured,
/// so the uninstall is refused and its files stay. Saves Obelisk holds
/// without a record never pass for a game without Cloud either.
fn unfollowed<R: Record>(
    vault: &Vault,
    saved: Option<R>,
    configuration: &R::Configuration,
) -> Result<(), R::Failure> {
    match saved {
        Some(mut record) if record::other_build(&record, configuration) => {
            record::fence(&mut record);
            record.save()
        }
        saved if record::holds_saves(vault, saved.is_some()) => Err(R::Failure::unsupported()),
        _ => Ok(()),
    }
}

/// Records a completed uninstall of the installation under `configuration`:
/// a save missing from now on is never read as deleted. Exit saves and
/// uploads still waiting stay valid.
pub(crate) async fn removed<S: Storefront>(
    store: &S,
    configuration: Configuration<S>,
) -> Result<(), Failure> {
    let account = store.account().to_owned();
    off_runtime::<S, _>(move || {
        let (_vault, record) = record::locked::<S::Record>(&configuration, account.borrow())?;
        let Some(mut record) = record else {
            return Ok(());
        };
        if !record::absent(configuration.install())? {
            return Err(Fail::<S>::recovery());
        }
        record::fence(&mut record);
        record.save()
    })
    .await
}

/// Cloud maintenance of `game`: a Cloud sync of the saves under `installed`,
/// or, for an uninstalled game, of those its record names. The storefront's
/// idle guard keeps every launch of it out until the sync ends.
pub(crate) async fn maintain<S: Storefront>(
    store: &S,
    game: &Game<S>,
    installed: Option<Configuration<S>>,
    control: Arc<Control>,
) -> Result<(), Failure> {
    let (sessions, helper) = store.sessions();
    let (sessions, helper) = (sessions.to_owned(), helper.to_owned());
    let lost = <S::Of as sessions::Storefront>::SESSION_FAILED;
    // Held until the sync ends: no supervisor or game owns the saves.
    let idle = blocking(lost, move || {
        supervisor::Client::<S::Of>::idle_guard(&sessions, &helper)
    })
    .await?
    .map_err(failed)?;
    let configuration = match installed {
        Some(configuration) => configuration,
        // An uninstalled game syncs what its record names.
        None => {
            let (root, scope) = (store.root().to_owned(), store.scope(game));
            let saved = off_runtime::<S, _>(move || saved::<S::Record>(&root, &scope)).await?;
            store.recorded(saved.map(|record| record.configuration().clone()))?
        }
    };
    store.sync(configuration, control, idle).await
}

/// The Cloud indicators of `games`, plus every other game of the account
/// with pending work, such as an uninstalled one. It blocks on file reads.
pub(crate) fn indicators<S: Storefront>(
    store: &S,
    games: &[Game<S>],
) -> Result<Vec<(Game<S>, cloud_status::Indicator)>, Failure> {
    let scopes: Vec<StoreScope> = games.iter().map(|game| store.scope(game)).collect();
    let account = S::Record::identity(store.account());
    let found = discovered::<S::Record>(store.root(), &account, &scopes).map_err(shown)?;
    Ok(found
        .into_iter()
        .filter_map(|(scope, indicator)| Some((S::game(scope)?, indicator)))
        .collect())
}

/// The Cloud indicators of `games`, plus every other game of `account` with
/// pending work. Nothing here takes a lock: a game or maintenance may be
/// writing these journals meanwhile.
fn discovered<R: Record>(
    root: &Path,
    account: &AccountIdentity,
    games: &[StoreScope],
) -> Result<Vec<(StoreScope, cloud_status::Indicator)>, R::Failure> {
    let mut wanted: BTreeSet<StoreScope> = games.iter().cloned().collect();
    wanted.extend(pending::<R>(root, account)?);
    let mut indicators = Vec::new();
    for scope in wanted {
        match indicator::<R>(root, &scope) {
            Ok(indicator) => indicators.push((scope, indicator)),
            Err(error) => eprintln!("Cloud indicator of {scope:?} unavailable: {}", error.key()),
        }
    }
    Ok(indicators)
}

fn indicator<R: Record>(
    root: &Path,
    scope: &StoreScope,
) -> Result<cloud_status::Indicator, R::Failure> {
    let vault = cloud_vault::peek(root, scope.clone())?;
    // No record: the game has no Cloud rules or was never checked.
    let record = saved::<R>(root, scope)?;
    let journal = journal(
        record.as_ref().map(R::work).as_ref(),
        record.as_ref().is_some_and(R::uncertain),
    );
    let status = cloud_status::read(root, scope)?;
    Ok(cloud_status::indicator(scope, &vault, &journal, &status))
}

/// What Cloud maintenance of an uninstalled game syncs: the record it left.
fn saved<R: Record>(root: &Path, scope: &StoreScope) -> Result<Option<R>, R::Failure> {
    safe_storage_path(root)?;
    read(root, &scope.directory(root)?)
}

/// The games of `account` whose Cloud work still waits. One unreadable record
/// never hides the others' work.
fn pending<R: Record>(
    root: &Path,
    account: &AccountIdentity,
) -> Result<Vec<StoreScope>, R::Failure> {
    safe_storage_path(root)?;
    let directory = account.directory(root)?;
    safe_storage_path(&directory)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut games = Vec::new();
    for entry in fs::read_dir(directory).map_err(cloud_vault::Error::from)? {
        let entry = entry.map_err(cloud_vault::Error::from)?;
        if !entry
            .file_type()
            .map_err(cloud_vault::Error::from)?
            .is_dir()
        {
            continue;
        }
        match waiting::<R>(root, &entry.path()) {
            Ok(Some(scope)) => games.push(scope),
            Ok(None) => {}
            Err(error) => eprintln!(
                "Cloud record of {} unreadable: {}",
                entry.file_name().to_string_lossy(),
                error.key()
            ),
        }
    }
    Ok(games)
}

/// The game whose record lives in `directory`, if its work still waits.
fn waiting<R: Record>(root: &Path, directory: &Path) -> Result<Option<StoreScope>, R::Failure> {
    let Some(record) = read::<R>(root, directory)? else {
        return Ok(None);
    };
    let scope = R::scope(record.configuration(), record.account())?;
    // Vault work outlives the record's: an uncertain upload or an
    // interrupted replacement still needs this game's next sync.
    let vault = cloud_vault::peek(root, scope.clone())?;
    Ok(
        (record.work().pending.is_some() || vault.pending.is_some() || vault.applying)
            .then_some(scope),
    )
}

/// A record read without the vault's lock, from the game directory it must
/// name: one copied from another account, game or archive is refused.
fn read<R: Record>(root: &Path, directory: &Path) -> Result<Option<R>, R::Failure> {
    let path = directory.join(R::FILE);
    safe_storage_path(&path)?;
    let Some(record) = cloud_vault::journal::read::<R>(&path)? else {
        return Ok(None);
    };
    record.check()?;
    if record.configuration().root() != root
        || R::scope(record.configuration(), record.account())?.directory(root)? != directory
    {
        return Err(cloud_vault::Error::Invalid.into());
    }
    Ok(Some(record))
}

/// What a game's Cloud indicator reads from its record's work.
fn journal(work: Option<&Work>, uncertain: bool) -> cloud_status::Journal {
    let pending = work.and_then(|work| work.pending.as_ref());
    cloud_status::Journal {
        pending: matches!(pending, Some(Pending::Capture | Pending::Upload { .. })),
        uncertain,
        applying: matches!(pending, Some(Pending::Applying { .. })),
        installation_removed: work.is_some_and(|work| work.installation_removed),
    }
}
