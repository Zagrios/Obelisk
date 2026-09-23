pub(crate) mod disk;
mod projection;

use crate::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs::File, path::PathBuf, time::Duration};

pub use projection::{Bindings, SelectedFile};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingUpload {
    pub id: String,
    pub scope: StoreScope,
    pub adapter: String,
    pub desired: Manifest,
    pub against: RemoteView,
    /// Persisted before handing any mutation to an adapter. A crash or transport
    /// error cannot turn a possibly committed upload into an unsent upload.
    pub may_have_committed: bool,
}

/// Contains account identity and an authorization generation, never a token.
/// The authenticated adapter remains responsible for binding its actual session
/// to its account identity. UI detachment must not call revoke().
#[derive(Clone, Copy, Debug)]
pub struct Authorization {
    account_key: [u8; 32],
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct UploadPermit {
    pub operation_id: String,
    pub scope: StoreScope,
    pub adapter: String,
    pub desired: Manifest,
    authorization: Authorization,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    scope: StoreScope,
    baseline: Option<RemoteView>,
    intent: Option<LocalIntent>,
    /// Latest local set for this account, including when another account owns
    /// the physical paths. This is synchronization state, not a version list.
    local: Option<Manifest>,
    pending: Option<PendingUpload>,
    applying: Option<projection::Apply>,
    game: Option<String>,
    /// Last Cloud sync, in Unix seconds. Display only: it never takes part in
    /// a comparison or decision, since timestamps never pick a winner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    synced_at: Option<u64>,
    /// Set by the transition that records a sync, never persisted.
    #[serde(skip)]
    stamped: bool,
}

impl State {
    fn validate(&self, scope: &StoreScope) -> Result<()> {
        if self.schema != 2 || !same_store(&self.scope, scope) {
            return Err(Error::Invalid);
        }
        self.scope.validate()?;
        if let Some(local) = &self.local {
            local.validate()?;
        }
        if let Some(base) = &self.baseline {
            base.validate()?;
        }
        if let Some(intent) = &self.intent {
            intent.against.validate()?;
        }
        if let Some(pending) = &self.pending {
            if !same_store(&pending.scope, scope) || uuid::Uuid::parse_str(&pending.id).is_err() {
                return Err(Error::Invalid);
            }
            pending.scope.validate()?;
            pending.desired.validate()?;
            pending.against.validate()?;
        }
        if let Some(applying) = &self.applying {
            applying.validate(scope)?;
        }
        if let Some(game) = &self.game {
            uuid::Uuid::parse_str(game).map_err(|_| Error::Invalid)?;
        }
        Ok(())
    }
}

/// An exclusive owner of one storefront's part of the archive. Each storefront
/// has its own lock, which serializes that storefront's accounts and Cloud App
/// IDs. Physical save paths shared across storefronts are serialized by the
/// separate `owners.lock`, held only while ownership changes. Production
/// lifecycle wiring must retain this owner until finalization ends.
pub struct Vault {
    root: PathBuf,
    directory: PathBuf,
    scope: StoreScope,
    state: State,
    preferences: Preferences,
    poisoned: bool,
    owners_wait: Duration,
    clock: fn() -> u64,
    syncs: u64,
    _lock: File,
}

/// What one scope's journal says about its Cloud saves, read without taking
/// any lock. It can be one write behind a vault that is open elsewhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub baseline: bool,
    pub synced_at: Option<u64>,
    /// A game session's guard is set. Only its supervisor can tell whether
    /// that game still runs.
    pub game_session: bool,
    pub applying: bool,
    pub pending: Option<UploadState>,
    /// Local saves differ from the last confirmed Cloud version, or a local
    /// choice still waits to be sent.
    pub local_changes: bool,
    /// The scope of an unfinished account switch anywhere in the archive.
    pub switching: Option<StoreScope>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadState {
    Queued,
    MayHaveCommitted,
}

/// Never opens a `Vault`: that would take the storefront lock and create
/// directories.
pub fn peek(root: &std::path::Path, scope: impl Into<StoreScope>) -> Result<Summary> {
    let scope = scope.into();
    let state = disk::read::<State>(&scope.directory(root)?.join("state.json"))?;
    let switching = projection::switching(root)?;
    let Some(state) = state else {
        return Ok(Summary {
            switching,
            ..Summary::default()
        });
    };
    state.validate(&scope)?;
    Ok(Summary {
        baseline: state.baseline.is_some(),
        synced_at: state.synced_at,
        game_session: state.game.is_some(),
        applying: state.applying.is_some(),
        pending: state.pending.as_ref().map(|pending| {
            if pending.may_have_committed {
                UploadState::MayHaveCommitted
            } else {
                UploadState::Queued
            }
        }),
        local_changes: state.baseline.as_ref().is_some_and(|base| {
            state.intent.is_some()
                || state
                    .local
                    .as_ref()
                    .is_some_and(|local| local != &base.files)
        }),
        switching,
    })
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Vault {
    pub fn open(root: &std::path::Path, scope: impl Into<StoreScope>) -> Result<Self> {
        let root = disk::root(root)?;
        let scope = scope.into();
        scope.validate()?;
        let lock = disk::store_lock(&root, &scope)?;
        Self::open_locked(root, scope, lock)
    }

    // The caller already holds this scope's storefront lock. File::try_clone
    // retains the same lock ownership while another account's complete set is
    // archived.
    fn open_locked(root: PathBuf, scope: StoreScope, lock: File) -> Result<Self> {
        scope.validate()?;
        for account in scope.account_directories(&root) {
            disk::directory(&account)?;
        }
        let directory = disk::directory(&scope.directory(&root)?)?;
        disk::directory(&directory.join("blobs"))?;
        let state = match disk::read::<State>(&directory.join("state.json"))? {
            Some(state) => state,
            None => State {
                schema: 2,
                scope: scope.clone(),
                baseline: None,
                intent: None,
                local: None,
                pending: None,
                applying: None,
                game: None,
                synced_at: None,
                stamped: false,
            },
        };
        state.validate(&scope)?;
        Ok(Self {
            root,
            directory,
            scope,
            state,
            preferences: Preferences::default(),
            poisoned: false,
            owners_wait: disk::OWNERS_WAIT,
            clock: unix_now,
            syncs: 0,
            _lock: lock,
        })
    }

    fn save(&mut self, mut next: State) -> Result<()> {
        self.healthy()?;
        if let Err(error) = disk::write(&self.directory.join("state.json"), &next) {
            // Rename may already have committed before parent fsync failed.
            // Do not make another decision from the old in-memory state.
            self.poisoned = true;
            return Err(error);
        }
        if std::mem::take(&mut next.stamped) {
            self.syncs += 1;
        }
        self.state = next;
        Ok(())
    }

    fn healthy(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Recovery)
        } else {
            Ok(())
        }
    }

    fn idle(&self) -> Result<()> {
        self.idle_with(None)
    }

    /// `guard` proves the caller already holds `owners.lock`, so a switch plan
    /// seen here cannot belong to a switch still running in another process.
    pub(super) fn idle_with(&self, guard: Option<&disk::OwnersGuard>) -> Result<()> {
        self.healthy()?;
        if self.state.applying.is_some() {
            return Err(Error::Recovery);
        }
        if self.state.game.is_some() {
            return Err(Error::Busy);
        }
        projection::check_switch(&self.root, guard, self.owners_wait)?;
        Ok(())
    }

    pub(super) fn owners_guard(&self) -> Result<disk::OwnersGuard> {
        disk::owners_lock(&self.root, self.owners_wait)
    }

    #[cfg(test)]
    pub(crate) fn set_owners_wait(&mut self, wait: Duration) {
        self.owners_wait = wait;
    }

    #[cfg(test)]
    pub(crate) fn set_clock(&mut self, clock: fn() -> u64) {
        self.clock = clock;
    }

    /// The only place a Last Cloud sync is recorded: a transition that has
    /// just confirmed local and Cloud saves identical.
    fn synced(&self, next: &mut State) {
        next.synced_at = Some((self.clock)());
        next.stamped = true;
    }

    pub fn synced_at(&self) -> Option<u64> {
        self.state.synced_at
    }

    /// How many Last Cloud syncs this vault recorded since it was opened. An
    /// adapter compares it across an operation; two syncs in the same second
    /// still differ here.
    pub fn syncs(&self) -> u64 {
        self.syncs
    }

    pub fn baseline(&self) -> Option<&RemoteView> {
        self.state.baseline.as_ref()
    }

    pub fn game_session(&self) -> Option<&str> {
        self.state.game.as_deref()
    }

    pub fn local(&self) -> Option<&Manifest> {
        self.state.local.as_ref()
    }

    pub fn pending(&self) -> Option<&PendingUpload> {
        self.state.pending.as_ref()
    }

    pub fn needs_recovery(&self) -> bool {
        self.poisoned
            || self.state.applying.is_some()
            || projection::check_switch(&self.root, None, self.owners_wait).is_err()
    }

    pub fn set_preferences(&mut self, preferences: Preferences) {
        self.preferences = preferences;
    }

    fn transfers_enabled(&self) -> Result<()> {
        self.healthy()?;
        if self.preferences.enabled() {
            Ok(())
        } else {
            Err(Error::Preferences)
        }
    }

    /// Retains pending uploads and blocks all capture, projection and upload
    /// admission until the matching game exits.
    pub fn game_started(&mut self, session: &str) -> Result<()> {
        self.idle()?;
        uuid::Uuid::parse_str(session).map_err(|_| Error::Invalid)?;
        let mut next = self.state.clone();
        next.game = Some(session.into());
        self.save(next)
    }

    /// Only the supervisor that observed actual process exit may release this
    /// guard. UI closure, sign-out and loss of IPC do not imply game exit.
    pub fn game_exited(&mut self, session: &str) -> Result<()> {
        if self.state.game.as_deref() != Some(session) {
            return Err(Error::Stale);
        }
        let mut next = self.state.clone();
        next.game = None;
        self.save(next)
    }

    pub fn decision(&self, local: &Manifest, remote: &RemoteView) -> Result<Decision> {
        self.decision_with(None, local, remote)
    }

    pub(super) fn decision_with(
        &self,
        guard: Option<&disk::OwnersGuard>,
        local: &Manifest,
        remote: &RemoteView,
    ) -> Result<Decision> {
        self.idle_with(guard)?;
        local.validate()?;
        remote.validate()?;
        Ok(reconcile(
            self.state.baseline.as_ref(),
            local,
            remote,
            self.state.intent.as_ref(),
        ))
    }

    pub fn confirm_equal(&mut self, local: &Manifest, remote: &RemoteView) -> Result<()> {
        self.idle()?;
        remote.validate()?;
        if local != &remote.files || self.state.pending.is_some() {
            return Err(Error::Stale);
        }
        self.verify(local)?;
        let mut next = self.state.clone();
        next.baseline = Some(remote.clone());
        next.intent = None;
        // A Cloud choice for an uninstalled game confirms the remote set while
        // the archived local version differs. That is not a sync.
        if self.state.local.as_ref() == Some(&remote.files) {
            self.synced(&mut next);
        }
        self.save(next)
    }

    pub fn choose_local(&mut self, shown: &RemoteView, current: &RemoteView) -> Result<()> {
        self.idle()?;
        current.validate()?;
        if shown != current || self.state.pending.is_some() {
            return Err(Error::Stale);
        }
        let mut next = self.state.clone();
        next.intent = Some(LocalIntent {
            against: current.clone(),
        });
        self.save(next)
    }

    pub fn authorize(&self, account: impl Into<AccountIdentity>) -> Result<Authorization> {
        self.healthy()?;
        let account = account.into();
        if account != self.scope.account() {
            return Err(Error::Authentication);
        }
        Ok(Authorization {
            account_key: account.permit_key(),
            generation: self.generation()?,
        })
    }

    fn check_authorization(&self, authorization: Authorization) -> Result<()> {
        self.healthy()?;
        if authorization.account_key != self.scope.account().permit_key()
            || authorization.generation != self.generation()?
        {
            return Err(Error::Authentication);
        }
        Ok(())
    }

    pub fn revoke(&mut self) -> Result<()> {
        self.healthy()?;
        let generation = self.generation()?.checked_add(1).ok_or(Error::Invalid)?;
        // Account-wide fencing also invalidates leases for other Cloud App IDs.
        if let Err(error) = disk::write(&self.authorization_path(), &generation) {
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }

    fn authorization_path(&self) -> PathBuf {
        self.directory
            .parent()
            .expect("account directory")
            .join("authorization.json")
    }

    fn generation(&self) -> Result<u64> {
        Ok(disk::read(&self.authorization_path())?.unwrap_or(0))
    }

    pub fn queue_upload(
        &mut self,
        desired: &Manifest,
        remote: &RemoteView,
        adapter: &str,
        authorization: Authorization,
    ) -> Result<String> {
        self.transfers_enabled()?;
        self.check_authorization(authorization)?;
        if adapter.is_empty() || adapter.len() > 128 || self.state.pending.is_some() {
            return Err(Error::Invalid);
        }
        if self.decision(desired, remote)? != Decision::KeepLocal {
            return Err(Error::Stale);
        }
        self.verify(desired)?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut next = self.state.clone();
        next.pending = Some(PendingUpload {
            id: id.clone(),
            scope: self.scope.clone(),
            adapter: adapter.into(),
            desired: desired.clone(),
            against: remote.clone(),
            may_have_committed: false,
        });
        self.save(next)?;
        Ok(id)
    }

    pub fn begin_upload(
        &mut self,
        id: &str,
        authorization: Authorization,
        current: &RemoteView,
    ) -> Result<UploadPermit> {
        self.transfers_enabled()?;
        self.idle()?;
        self.check_authorization(authorization)?;
        current.validate()?;
        let pending = self.state.pending.as_ref().ok_or(Error::Stale)?;
        if pending.id != id || pending.may_have_committed {
            return Err(Error::Recovery);
        }
        if &pending.against != current {
            return Err(Error::Stale);
        }
        self.verify(&pending.desired)?;
        let permit = UploadPermit {
            operation_id: pending.id.clone(),
            scope: pending.scope.clone(),
            adapter: pending.adapter.clone(),
            desired: pending.desired.clone(),
            authorization,
        };
        let mut next = self.state.clone();
        next.pending.as_mut().unwrap().may_have_committed = true;
        self.save(next)?;
        Ok(permit)
    }

    /// Requires both the adapter's positive server acknowledgement and a fresh,
    /// complete remote readback. Native admission or an idle flag is insufficient.
    pub fn complete_upload(
        &mut self,
        permit: &UploadPermit,
        server_confirmed: bool,
        readback: &RemoteView,
    ) -> Result<()> {
        self.idle()?;
        self.check_authorization(permit.authorization)?;
        readback.validate()?;
        let pending = self.state.pending.as_ref().ok_or(Error::Stale)?;
        if pending.id != permit.operation_id
            || !same_store(&permit.scope, &self.scope)
            || pending.adapter != permit.adapter
            || pending.desired != permit.desired
            || !pending.may_have_committed
            || !server_confirmed
            || pending.desired != readback.files
        {
            return Err(Error::Stale);
        }
        let mut next = self.state.clone();
        next.baseline = Some(readback.clone());
        next.intent = None;
        next.pending = None;
        self.synced(&mut next);
        self.save(next)
    }

    /// An adapter may settle an ambiguous request only after it has established
    /// that the old transaction can no longer commit. Readback alone cannot
    /// establish this when a server-side batch is still open.
    pub fn reconcile_pending(
        &mut self,
        id: &str,
        authorization: Authorization,
        transaction_settled: bool,
        readback: &RemoteView,
    ) -> Result<()> {
        self.idle()?;
        self.check_authorization(authorization)?;
        readback.validate()?;
        let pending = self.state.pending.as_ref().ok_or(Error::Stale)?;
        if pending.id != id || !transaction_settled {
            return Err(Error::Recovery);
        }
        let mut next = self.state.clone();
        if pending.desired == readback.files {
            next.baseline = Some(readback.clone());
            next.intent = None;
            next.pending = None;
            self.synced(&mut next);
        } else {
            next.pending.as_mut().unwrap().may_have_committed = false;
        }
        self.save(next)
    }

    /// Retire a failed attempt after a fresh, complete verified observation.
    /// The next attempt compares against the existing baseline and local choice;
    /// this does not acknowledge or cancel the old server operation. The caller
    /// must stop the old local worker before adopting this retry policy.
    pub fn recompare_pending(
        &mut self,
        id: &str,
        authorization: Authorization,
        observed: &RemoteView,
    ) -> Result<()> {
        self.idle()?;
        self.transfers_enabled()?;
        self.check_authorization(authorization)?;
        observed.validate()?;
        self.verify(&observed.files)?;
        let pending = self.state.pending.as_ref().ok_or(Error::Stale)?;
        if pending.id != id {
            return Err(Error::Stale);
        }
        self.verify(&pending.desired)?;
        let mut next = self.state.clone();
        next.pending = None;
        self.save(next)
    }

    /// Resolve a deferred conflict only after any prior transaction is settled.
    /// Choosing Cloud clears the upload; the local snapshot stays intact until
    /// the caller stages and applies the chosen remote set.
    pub fn resolve_pending_conflict(
        &mut self,
        id: &str,
        shown: &RemoteView,
        current: &RemoteView,
        keep_local: bool,
    ) -> Result<()> {
        self.idle()?;
        current.validate()?;
        let pending = self.state.pending.as_ref().ok_or(Error::Stale)?;
        if pending.id != id || shown != current {
            return Err(Error::Stale);
        }
        if pending.may_have_committed {
            return Err(Error::Recovery);
        }
        let desired = pending.desired.clone();
        self.verify(&desired)?;
        let mut next = self.state.clone();
        if keep_local {
            next.pending.as_mut().unwrap().against = current.clone();
            next.intent = Some(LocalIntent {
                against: current.clone(),
            });
        } else {
            next.pending = None;
        }
        self.save(next)
    }

    /// Call only at an operation boundary, after newly staged content has been
    /// referenced by synchronization state. Pending data, the latest local set
    /// and the confirmed base survive collection. The owner must also supply
    /// manifests referenced by its durable recovery/conflict state. No live
    /// save path is removed.
    pub fn collect_garbage(&mut self, retained: &[&Manifest]) -> Result<usize> {
        self.idle()?;
        for files in retained {
            files.validate()?;
        }
        let mut keep = std::collections::BTreeSet::new();
        let mut retain = |files: &Manifest| {
            keep.extend(files.0.values().map(|file| file.hash.clone()));
        };
        if let Some(local) = &self.state.local {
            retain(local);
        }
        if let Some(base) = &self.state.baseline {
            retain(&base.files);
        }
        if let Some(intent) = &self.state.intent {
            retain(&intent.against.files);
        }
        if let Some(pending) = &self.state.pending {
            retain(&pending.desired);
            retain(&pending.against.files);
        }
        for files in retained {
            retain(files);
        }
        let directory = self.directory.join("blobs");
        let mut removed = 0;
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(hash) = ContentHash::try_from(name) else {
                continue;
            };
            if !keep.contains(&hash) && disk::regular(&entry.path())? {
                std::fs::remove_file(entry.path())?;
                removed += 1;
            }
        }
        File::open(directory)?.sync_all()?;
        Ok(removed)
    }

    /// Cache a verified file before exposing it in a complete manifest. Remote
    /// names are never joined to an on-disk path in this archive.
    pub fn stage(&self, bytes: &[u8], platforms: u32) -> Result<FileVersion> {
        let file = FileVersion {
            hash: ContentHash::of(bytes),
            size: bytes.len() as u64,
            platforms,
        };
        disk::blob(&self.directory, &file, bytes)?;
        Ok(file)
    }

    pub fn read_content(&self, file: &FileVersion) -> Result<Vec<u8>> {
        disk::content(&self.directory, file)
    }

    fn verify(&self, files: &Manifest) -> Result<()> {
        files.validate()?;
        let mut verified = std::collections::BTreeSet::new();
        for file in files.0.values() {
            // Several save names can reference the same archived bytes. Check
            // each content identity once in this transition, never across them.
            if verified.insert((&file.hash, file.size)) {
                self.read_content(file)?;
            }
        }
        Ok(())
    }

    pub fn record_local(&mut self, files: &Manifest) -> Result<()> {
        self.record_local_with(None, files)
    }

    pub(super) fn record_local_with(
        &mut self,
        guard: Option<&disk::OwnersGuard>,
        files: &Manifest,
    ) -> Result<()> {
        self.idle_with(guard)?;
        self.verify(files)?;
        let mut next = self.state.clone();
        next.local = Some(files.clone());
        self.save(next)
    }
}

fn same_store(a: &StoreScope, b: &StoreScope) -> bool {
    match (a, b) {
        (StoreScope::Steam(a), StoreScope::Steam(b)) => {
            a.steam_id == b.steam_id && a.cloud_app_id == b.cloud_app_id
        }
        (StoreScope::Epic(a), StoreScope::Epic(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod sync_tests;

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            steam_id: 0x0110_0001_0000_0001,
            app_id: 10,
            cloud_app_id: 42,
        }
    }

    #[test]
    fn opening_accepts_only_the_current_archive_format_without_rewriting_old_state() {
        let root = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(root.path(), scope()).unwrap();
        vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        let local = Manifest(BTreeMap::from([(
            "save".into(),
            vault.stage(b"save", 7).unwrap(),
        )]));
        let remote = RemoteView {
            revision: "known".into(),
            files: Manifest::default(),
        };
        let authorization = vault.authorize(scope().steam_id).unwrap();
        vault
            .queue_upload(&local, &remote, "fixture", authorization)
            .unwrap();
        let path = vault.directory.join("state.json");
        let current = serde_json::to_value(&vault.state).unwrap();
        drop(vault);
        assert!(Vault::open(root.path(), scope()).is_ok());
        for case in 0..3 {
            let mut old = current.clone();
            match case {
                0 => old["schema"] = 1.into(),
                1 => old["history"] = serde_json::json!([]),
                2 => old["pending"]["status"] = "Queued".into(),
                _ => unreachable!(),
            }
            disk::write(&path, &old).unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(Vault::open(root.path(), scope()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }

    #[test]
    fn duplicate_blob_checks_still_reject_wrong_lengths_and_later_corruption() {
        let root = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(root.path(), scope()).unwrap();
        let file = vault.stage(b"save", 7).unwrap();
        let mut files = Manifest(BTreeMap::from([
            ("slot.sav".into(), file.clone()),
            ("backup.sav".into(), file.clone()),
        ]));
        vault.record_local(&files).unwrap();
        files.0.get_mut("slot.sav").unwrap().size += 1;
        assert!(matches!(vault.record_local(&files), Err(Error::Integrity)));
        files.0.get_mut("slot.sav").unwrap().size = file.size;
        std::fs::write(
            vault.directory.join("blobs").join(file.hash.as_str()),
            b"bad!",
        )
        .unwrap();
        assert!(matches!(vault.record_local(&files), Err(Error::Integrity)));
    }

    #[test]
    fn launch_without_sync_retains_pending_and_fences_transfers_until_matching_exit() {
        let root = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(root.path(), scope()).unwrap();
        vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        let files = Manifest(BTreeMap::from([(
            "save".into(),
            vault.stage(b"queued", 7).unwrap(),
        )]));
        let remote = RemoteView {
            revision: "1".into(),
            files: Manifest::default(),
        };
        let authorization = vault.authorize(scope().steam_id).unwrap();
        let pending = vault
            .queue_upload(&files, &remote, "test", authorization)
            .unwrap();
        let session = uuid::Uuid::new_v4().to_string();
        assert!(matches!(vault.game_started("invalid"), Err(Error::Invalid)));
        vault.game_started(&session).unwrap();
        assert!(matches!(
            vault.begin_upload(&pending, authorization, &remote),
            Err(Error::Busy)
        ));
        assert!(matches!(vault.capture(&Bindings::new()), Err(Error::Busy)));
        assert!(matches!(vault.game_started(&session), Err(Error::Busy)));
        assert!(matches!(
            vault.game_exited(&uuid::Uuid::new_v4().to_string()),
            Err(Error::Stale)
        ));
        drop(vault);
        let mut vault = Vault::open(root.path(), scope()).unwrap();
        vault.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        assert_eq!(vault.game_session(), Some(session.as_str()));
        assert_eq!(vault.pending().unwrap().id, pending);
        vault.game_exited(&session).unwrap();
        assert!(vault.begin_upload(&pending, authorization, &remote).is_ok());
    }
}
