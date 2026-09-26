use super::*;
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedFile {
    /// An absolute path supplied by the save-rule resolver, not by a remote
    /// filename. Its parent must already exist. No recursive discovery here.
    pub path: PathBuf,
    pub platforms: u32,
}

pub type Bindings = BTreeMap<String, SelectedFile>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Apply {
    scope: StoreScope,
    bindings: Bindings,
    before: Manifest,
    after: Manifest,
    effect: Effect,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Effect {
    Download { remote: RemoteView },
    Local,
}

impl Apply {
    pub(super) fn validate(&self, scope: &StoreScope) -> Result<()> {
        self.scope.validate()?;
        if !same_store(&self.scope, scope)
            || self.bindings.values().any(|file| {
                !file.path.is_absolute()
                    || file
                        .path
                        .components()
                        .any(|part| matches!(part, Component::ParentDir))
            })
            || self
                .before
                .0
                .keys()
                .chain(self.after.0.keys())
                .any(|name| !self.bindings.contains_key(name))
        {
            return Err(Error::Invalid);
        }
        self.before.validate()?;
        self.after.validate()?;
        match &self.effect {
            Effect::Download { remote } => remote.validate(),
            Effect::Local => Ok(()),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Owners {
    files: BTreeMap<PathBuf, StoreScope>,
    metadata: BTreeMap<PathBuf, OwnerMetadata>,
    switching: Option<Switch>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerMetadata {
    name: String,
    platforms: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Switch {
    scope: StoreScope,
    bindings: Bindings,
    before: BTreeMap<String, SwitchBefore>,
    after: Manifest,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SwitchBefore {
    owner: Option<StoreScope>,
    file: Option<FileVersion>,
}

/// The scope whose account switch has not finished. Reads without any lock,
/// so a plan seen here may belong to a switch that is still running.
pub(super) fn switching(root: &Path) -> Result<Option<StoreScope>> {
    Ok(disk::read::<Owners>(&root.join("owners.json"))?
        .and_then(|owners| owners.switching)
        .map(|plan| plan.scope))
}

pub(super) fn check_switch(
    root: &Path,
    guard: Option<&disk::OwnersGuard>,
    wait: Duration,
) -> Result<()> {
    if switching(root)?.is_none() {
        return Ok(());
    }
    // Without the owners lock, the plan may belong to another storefront's
    // switch that is still running. Only a plan that outlives it needs recovery.
    if guard.is_none() {
        let _guard = disk::owners_lock(root, wait)?;
        if switching(root)?.is_none() {
            return Ok(());
        }
    }
    Err(Error::Recovery)
}

fn canonical_file(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error::Invalid);
    }
    // Resolve only the trusted platform prefix (/var -> /private/var, /tmp ->
    // /private/tmp). A symlink anywhere beneath it, including a leaf, is refused.
    let mut checked = PathBuf::from("/");
    let mut count = 0;
    for component in path.components() {
        if let Component::Normal(part) = component {
            checked.push(part);
            count += 1;
            if fs::symlink_metadata(&checked).is_ok_and(|m| m.file_type().is_symlink()) {
                if count == 1 && [Path::new("/tmp"), Path::new("/var")].contains(&checked.as_path())
                {
                    checked = checked.canonicalize()?;
                } else {
                    return Err(Error::Invalid);
                }
            }
        }
    }
    disk::regular(&checked)?;
    let parent = checked.parent().ok_or(Error::Invalid)?.canonicalize()?;
    Ok(parent.join(checked.file_name().ok_or(Error::Invalid)?))
}

impl Vault {
    fn normalize(&self, bindings: &Bindings) -> Result<Bindings> {
        Manifest::validate_names(bindings.keys().map(String::as_str))?;
        let mut paths = BTreeSet::new();
        let mut inodes = BTreeSet::new();
        let mut normalized = BTreeMap::new();
        for (name, file) in bindings {
            let path = canonical_file(&file.path)?;
            if path.starts_with(&self.root) {
                return Err(Error::Invalid);
            }
            if !paths.insert(path.clone()) {
                return Err(Error::Invalid);
            }
            if let Ok(metadata) = fs::metadata(&path)
                && (!inodes.insert((metadata.dev(), metadata.ino())) || metadata.nlink() != 1)
            {
                return Err(Error::Invalid);
            }
            normalized.insert(
                name.clone(),
                SelectedFile {
                    path,
                    platforms: file.platforms,
                },
            );
        }
        // Fail conservatively on case variants, including missing destinations.
        // Unicode normalization aliases are checked using the destination FS by
        // the staging collision check below before any real file is replaced.
        let mut folded = BTreeSet::new();
        for file in normalized.values() {
            if !folded.insert(file.path.to_string_lossy().to_lowercase()) {
                return Err(Error::Invalid);
            }
        }
        Ok(normalized)
    }

    fn claim(&self, _guard: &disk::OwnersGuard, bindings: &Bindings) -> Result<Bindings> {
        let normalized = self.normalize(bindings)?;
        let path = self.root.join("owners.json");
        let mut owners: Owners = disk::read(&path)?.unwrap_or_default();
        if owners.switching.is_some() {
            return Err(Error::Recovery);
        }
        for (name, file) in &normalized {
            for (claimed, owner) in &owners.files {
                if !same_store(owner, &self.scope)
                    && (claimed == &file.path
                        || same_existing_file(claimed, &file.path)
                        || names_alias(claimed, &file.path)?)
                {
                    return Err(Error::Authentication);
                }
            }
            if owners
                .metadata
                .get(&file.path)
                .is_some_and(|metadata| metadata.name != *name)
            {
                return Err(Error::Invalid);
            }
            owners.files.insert(file.path.clone(), self.scope.clone());
            owners.metadata.insert(
                file.path.clone(),
                OwnerMetadata {
                    name: name.clone(),
                    platforms: file.platforms,
                },
            );
        }
        disk::write(&path, &owners)?;
        Ok(normalized)
    }

    /// Archive displaced owners' complete physical sets, then switch only the
    /// selected paths. Foreign bytes stay exclusively in their original vault.
    pub fn activate(&mut self, bindings: &Bindings) -> Result<bool> {
        self.activate_after(bindings, None)
    }

    fn activate_after(&mut self, bindings: &Bindings, stop_after: Option<usize>) -> Result<bool> {
        self.healthy()?;
        if self.state.game.is_some() {
            return Err(Error::Busy);
        }
        if self.state.applying.is_some() {
            return Err(Error::Recovery);
        }
        // The owners lock covers the whole transaction, from the displaced
        // owners read below to the published result of the switch.
        let guard = self.owners_guard()?;
        let mut foreign = None;
        if self.recover_switch(&guard, &mut foreign, stop_after)? {
            return Ok(true);
        }
        let normalized = self.normalize(bindings)?;
        let owners: Owners = disk::read(&self.root.join("owners.json"))?.unwrap_or_default();
        let mut displaced = BTreeMap::new();
        for file in normalized.values() {
            for (claimed, owner) in &owners.files {
                owner.validate()?;
                if claimed != &file.path
                    && (same_existing_file(claimed, &file.path)
                        || names_alias(claimed, &file.path)?)
                {
                    // Reassignment needs one unambiguous persisted path/name.
                    return Err(Error::Authentication);
                }
            }
            if let Some(owner) = owners.files.get(&file.path)
                && !same_store(owner, &self.scope)
            {
                displaced.insert(owner.store_key(), owner.clone());
            }
        }
        if displaced.is_empty() {
            self.claim(&guard, &normalized)?;
            return Ok(false);
        }
        let desired = self.state.local.clone();
        if let Some(desired) = &desired {
            self.verify(desired)?;
            if desired.0.keys().any(|name| !normalized.contains_key(name)) {
                return Err(Error::Invalid);
            }
        }
        // Check every displaced owner's mappings before writing any journal or
        // deleting anything. Archiving a selected subset would invent deletions.
        let mut old_bindings = BTreeMap::new();
        for (key, scope) in &displaced {
            let mut files = Bindings::new();
            for (path, owner) in &owners.files {
                if !same_store(owner, scope) {
                    continue;
                }
                let metadata = owners.metadata.get(path).ok_or(Error::Authentication)?;
                if files
                    .insert(
                        metadata.name.clone(),
                        SelectedFile {
                            path: path.clone(),
                            platforms: metadata.platforms,
                        },
                    )
                    .is_some()
                {
                    return Err(Error::Invalid);
                }
            }
            old_bindings.insert(key.clone(), files);
        }
        let mut archived = BTreeMap::new();
        for (key, scope) in &displaced {
            let lock = self.store_lock_for(scope, &mut foreign)?;
            let mut old = Vault::open_locked(self.root.clone(), scope.clone(), lock)?;
            old.idle_with(Some(&guard))?;
            let bindings = old.claim(&guard, &old_bindings[key])?;
            // Earlier partial switches may have unprojected other old saves.
            // Preserve those archived entries and refresh every path this old
            // scope still owns, including explicit physical deletions.
            let mut files = old.state.local.clone().unwrap_or_default();
            for (name, selected) in bindings {
                if let Some(bytes) = read_selected(&selected.path)? {
                    files.0.insert(name, old.stage(&bytes, selected.platforms)?);
                } else {
                    files.0.remove(&name);
                }
            }
            old.record_local_with(Some(&guard), &files)?;
            archived.insert(key.clone(), files);
        }
        let mut changes = Bindings::new();
        let mut before = BTreeMap::new();
        for (name, file) in &normalized {
            let owner = owners.files.get(&file.path).cloned();
            let foreign = owner
                .as_ref()
                .filter(|owner| !same_store(owner, &self.scope));
            if foreign.is_none() && desired.is_none() {
                continue; // No account snapshot: leave own/unowned physical files alone.
            }
            let previous = if let Some(owner) = foreign {
                let metadata = owners
                    .metadata
                    .get(&file.path)
                    .ok_or(Error::Authentication)?;
                archived[&owner.store_key()].0.get(&metadata.name).cloned()
            } else {
                let next = desired.as_ref().and_then(|files| files.0.get(name));
                if let Some(bytes) = read_selected(&file.path)? {
                    // Do not replace an unarchived own/unowned edit with an older
                    // version merely because another selected path changed owner.
                    if !next.is_some_and(|version| {
                        version.size == bytes.len() as u64
                            && version.hash == ContentHash::of(&bytes)
                    }) {
                        return Err(Error::Stale);
                    }
                    next.cloned()
                } else {
                    None
                }
            };
            changes.insert(name.clone(), file.clone());
            before.insert(
                name.clone(),
                SwitchBefore {
                    owner,
                    file: previous,
                },
            );
        }
        // Old capture updated name metadata, but never reassigned a path.
        let mut owners: Owners =
            disk::read(&self.root.join("owners.json"))?.ok_or(Error::Invalid)?;
        owners.switching = Some(Switch {
            scope: self.scope.clone(),
            bindings: changes,
            before,
            after: desired.unwrap_or_default(),
        });
        self.save_owners(&guard, &owners)?;
        self.recover_switch(&guard, &mut foreign, stop_after)
    }

    /// The storefront lock that authorizes opening `owner`'s vault. The same
    /// storefront shares this vault's lock. The other storefront's lock is
    /// tried once without waiting, then kept until the ownership change ends.
    fn store_lock_for(&self, owner: &StoreScope, foreign: &mut Option<File>) -> Result<File> {
        if owner.same_storefront(&self.scope) {
            return Ok(self._lock.try_clone()?);
        }
        let held = match foreign {
            Some(held) => held,
            None => foreign.insert(disk::store_lock(&self.root, owner).map_err(
                |error| match error {
                    Error::Busy => Error::OtherStoreBusy,
                    error => error,
                },
            )?),
        };
        Ok(held.try_clone()?)
    }

    fn save_owners(&mut self, _guard: &disk::OwnersGuard, owners: &Owners) -> Result<()> {
        if let Err(error) = disk::write(&self.root.join("owners.json"), owners) {
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }

    fn recover_switch(
        &mut self,
        guard: &disk::OwnersGuard,
        foreign: &mut Option<File>,
        stop_after: Option<usize>,
    ) -> Result<bool> {
        let mut owners: Owners = disk::read(&self.root.join("owners.json"))?.unwrap_or_default();
        let Some(plan) = owners.switching.clone() else {
            return Ok(false);
        };
        if !same_store(&plan.scope, &self.scope) || self.state.applying.is_some() {
            return Err(Error::Recovery);
        }
        plan.scope.validate()?;
        if self.state.game.is_some() {
            return Err(Error::Busy);
        }
        let bindings = self.normalize(&plan.bindings)?;
        if bindings != plan.bindings
            || plan.before.len() != bindings.len()
            || plan.after.0.keys().any(|name| !bindings.contains_key(name))
        {
            return Err(Error::Invalid);
        }
        self.verify(&plan.after)?;
        // Ownership of every destination must still match the recorded plan,
        // and each displaced owner's blob must still be readable, before any
        // file is staged. These checks read only; they change nothing.
        for (name, selected) in &bindings {
            let before = plan.before.get(name).ok_or(Error::Invalid)?;
            let owner_matches = match (owners.files.get(&selected.path), before.owner.as_ref()) {
                (Some(current), Some(previous)) => same_store(current, previous),
                (None, None) => true,
                _ => false,
            };
            if !owner_matches {
                return Err(Error::Stale);
            }
            if let Some(file) = &before.file {
                let owner = before.owner.clone().unwrap_or_else(|| self.scope.clone());
                let lock = self.store_lock_for(&owner, foreign)?;
                let old = Vault::open_locked(self.root.clone(), owner, lock)?;
                old.read_content(file)?;
            }
        }
        self.project(
            &bindings,
            |name| {
                plan.before
                    .get(name)
                    .and_then(|before| before.file.as_ref())
            },
            &plan.after,
            stop_after,
        )?;
        for (name, file) in bindings {
            owners.files.insert(file.path.clone(), self.scope.clone());
            owners.metadata.insert(
                file.path,
                OwnerMetadata {
                    name,
                    platforms: file.platforms,
                },
            );
        }
        // One atomic ownership publication releases the recovery fence only
        // after every replacement/removal is durable. Pending uploads stay put.
        owners.switching = None;
        self.save_owners(guard, &owners)?;
        Ok(true)
    }

    /// Stages the entire set, on each destination volume, before changing any
    /// save, then commits it entry by entry. These private staged files are
    /// disposable; durable blobs drive recovery. `stop_after` injects a crash
    /// at a chosen commit index so tests can replay an interrupted projection.
    fn project<'a>(
        &self,
        bindings: &Bindings,
        before: impl Fn(&str) -> Option<&'a FileVersion>,
        after: &Manifest,
        stop_after: Option<usize>,
    ) -> Result<()> {
        let mut staged = BTreeMap::new();
        let mut collision_dirs = BTreeMap::new();
        for (name, selected) in bindings {
            verify_destination(&selected.path, before(name), after.0.get(name))?;
            let parent = selected.path.parent().ok_or(Error::Invalid)?;
            let probe = match collision_dirs.entry(parent.to_path_buf()) {
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                    tempfile::Builder::new()
                        .prefix(".sailover-cloud-names-")
                        .tempdir_in(parent)?,
                ),
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            };
            // create_new asks the actual filesystem about case/normalization
            // equality, including aliases of paths that do not yet exist.
            File::create_new(
                probe
                    .path()
                    .join(selected.path.file_name().ok_or(Error::Invalid)?),
            )
            .map_err(|_| Error::Invalid)?;
            if let Some(file) = after.0.get(name) {
                let mut temp = tempfile::NamedTempFile::new_in(parent)?;
                temp.write_all(&self.read_content(file)?)?;
                temp.as_file().sync_all()?;
                staged.insert(name.clone(), temp);
            }
        }
        for (index, (name, selected)) in bindings.iter().enumerate() {
            if stop_after == Some(index) {
                return Err(Error::Recovery);
            }
            canonical_file(&selected.path)?;
            verify_destination(&selected.path, before(name), after.0.get(name))?;
            if let Some(temp) = staged.remove(name) {
                temp.persist(&selected.path).map_err(|error| error.error)?;
                disk::sync_parent(&selected.path)?;
            } else if disk::regular(&selected.path)? {
                fs::remove_file(&selected.path)?;
                disk::sync_parent(&selected.path)?;
            }
        }
        if stop_after == Some(bindings.len()) {
            return Err(Error::Recovery);
        }
        Ok(())
    }

    pub fn capture(&mut self, bindings: &Bindings) -> Result<Manifest> {
        let guard = self.owners_guard()?;
        self.capture_with_missing(&guard, bindings, false)
    }

    /// A retry of a partial uninstall must retain already removed files in the
    /// account snapshot while still capturing surviving files' current bytes.
    /// This does not create upload intent or project files back into the game.
    pub fn capture_preserving_missing(&mut self, bindings: &Bindings) -> Result<Manifest> {
        let guard = self.owners_guard()?;
        self.capture_with_missing(&guard, bindings, true)
    }

    /// Reproject this account's current snapshot after installation removal.
    /// This restores local ownership, not a Cloud download: the confirmed remote
    /// baseline and any explicit local choice remain unchanged.
    pub fn restore_local(&mut self, bindings: &Bindings) -> Result<()> {
        let guard = self.owners_guard()?;
        self.prepare_local_restore(&guard, bindings)?;
        self.recover_with(&guard, None)
    }

    fn prepare_local_restore(
        &mut self,
        guard: &disk::OwnersGuard,
        bindings: &Bindings,
    ) -> Result<()> {
        self.idle_with(Some(guard))?;
        if self.state.pending.is_some() {
            return Err(Error::Busy);
        }
        let after = self.state.local.clone().ok_or(Error::Recovery)?;
        self.verify(&after)?;
        let bindings = self.claim(guard, bindings)?;
        if after.0.keys().any(|name| !bindings.contains_key(name)) {
            return Err(Error::Invalid);
        }
        // Preserve the intended snapshot while archiving the current preimage.
        // Ordinary capture would publish that preimage as the account snapshot
        // before the durable restoration plan existed.
        let mut before = Manifest::default();
        for (name, file) in &bindings {
            if let Some(bytes) = read_selected(&file.path)? {
                before
                    .0
                    .insert(name.clone(), self.stage(&bytes, file.platforms)?);
            }
        }
        let mut next = self.state.clone();
        next.applying = Some(Apply {
            scope: self.scope.clone(),
            bindings,
            before,
            after,
            effect: Effect::Local,
        });
        self.save(next)
    }

    fn capture_with_missing(
        &mut self,
        guard: &disk::OwnersGuard,
        bindings: &Bindings,
        preserve_missing: bool,
    ) -> Result<Manifest> {
        self.idle_with(Some(guard))?;
        let bindings = self.claim(guard, bindings)?;
        let mut files = if preserve_missing {
            self.state.local.clone().unwrap_or_default()
        } else {
            Manifest::default()
        };
        for (name, file) in bindings {
            if let Some(bytes) = read_selected(&file.path)? {
                files.0.insert(name, self.stage(&bytes, file.platforms)?);
            }
        }
        self.record_local_with(Some(guard), &files)?;
        Ok(files)
    }

    /// Called after a complete download has been staged and hash checked. A
    /// conflict choice must carry the exact view shown and a fresh remote view.
    pub fn apply_download(
        &mut self,
        bindings: &Bindings,
        shown: &RemoteView,
        current: &RemoteView,
        choose_cloud: bool,
    ) -> Result<()> {
        let guard = self.owners_guard()?;
        self.transfers_enabled()?;
        self.idle_with(Some(&guard))?;
        current.validate()?;
        if shown != current {
            return Err(Error::Stale);
        }
        let local = self.capture_with_missing(&guard, bindings, false)?;
        if !choose_cloud && self.decision_with(Some(&guard), &local, current)? != Decision::Download
        {
            return Err(Error::Stale);
        }
        self.prepare_checked_apply(
            &guard,
            bindings,
            current.files.clone(),
            Effect::Download {
                remote: current.clone(),
            },
            Some(&local),
        )?;
        self.recover_with(&guard, None)
    }

    #[cfg(test)]
    fn prepare_apply(
        &mut self,
        bindings: &Bindings,
        after: Manifest,
        effect: Effect,
    ) -> Result<()> {
        let guard = self.owners_guard()?;
        self.prepare_checked_apply(&guard, bindings, after, effect, None)
    }

    fn prepare_checked_apply(
        &mut self,
        guard: &disk::OwnersGuard,
        bindings: &Bindings,
        after: Manifest,
        effect: Effect,
        expected_before: Option<&Manifest>,
    ) -> Result<()> {
        self.idle_with(Some(guard))?;
        if self.state.pending.is_some() {
            return Err(Error::Busy);
        }
        self.verify(&after)?;
        let bindings = self.claim(guard, bindings)?;
        if after.0.keys().any(|name| !bindings.contains_key(name)) {
            return Err(Error::Invalid);
        }
        let before = self.capture_with_missing(guard, &bindings, false)?;
        if expected_before.is_some_and(|expected| expected != &before) {
            return Err(Error::Stale);
        }
        let mut next = self.state.clone();
        // Both preimage and intent are durable before the first replacement.
        next.applying = Some(Apply {
            scope: self.scope.clone(),
            bindings,
            before,
            after,
            effect,
        });
        self.save(next)
    }

    /// Roll forward an interrupted local transaction. Each destination must
    /// still match either its preimage or its intended result. An external edit
    /// leaves the journal intact and blocks launch instead of losing that edit.
    pub fn recover_local(&mut self) -> Result<()> {
        self.recover_after(None)
    }

    fn recover_after(&mut self, stop_after: Option<usize>) -> Result<()> {
        let guard = self.owners_guard()?;
        self.recover_with(&guard, stop_after)
    }

    fn recover_with(&mut self, guard: &disk::OwnersGuard, stop_after: Option<usize>) -> Result<()> {
        self.healthy()?;
        if self.state.game.is_some() {
            return Err(Error::Busy);
        }
        self.recover_switch(guard, &mut None, stop_after)?;
        let Some(plan) = self.state.applying.clone() else {
            return Ok(());
        };
        if !same_store(&plan.scope, &self.scope) {
            return Err(Error::Invalid);
        }
        plan.before.validate()?;
        self.verify(&plan.after)?;
        self.verify(&plan.before)?;
        let bindings = self.claim(guard, &plan.bindings)?;
        if plan
            .before
            .0
            .keys()
            .chain(plan.after.0.keys())
            .any(|name| !bindings.contains_key(name))
        {
            return Err(Error::Invalid);
        }
        self.project(
            &bindings,
            |name| plan.before.0.get(name),
            &plan.after,
            stop_after,
        )?;
        let mut next = self.state.clone();
        if let Effect::Download { remote } = plan.effect {
            next.baseline = Some(remote);
            next.intent = None;
            self.synced(&mut next);
        }
        next.local = Some(plan.after);
        next.applying = None;
        self.save(next)
    }
}

fn same_existing_file(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

fn names_alias(a: &Path, b: &Path) -> Result<bool> {
    if a.parent() != b.parent() {
        return Ok(false);
    }
    let probe = tempfile::Builder::new()
        .prefix(".sailover-cloud-owner-")
        .tempdir_in(b.parent().ok_or(Error::Invalid)?)?;
    File::create_new(probe.path().join(a.file_name().ok_or(Error::Invalid)?))?;
    match File::create_new(probe.path().join(b.file_name().ok_or(Error::Invalid)?)) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(true),
        Err(e) => Err(e.into()),
    }
}

fn read_selected(path: &Path) -> Result<Option<Vec<u8>>> {
    if !disk::regular(path)? {
        return Ok(None);
    }
    let before = fs::metadata(path)?;
    if before.len() > disk::MAX_FILE {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(disk::MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    let after = fs::metadata(path)?;
    if bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
    {
        return Err(Error::Stale);
    }
    Ok(Some(bytes))
}

fn verify_destination(
    path: &Path,
    before: Option<&FileVersion>,
    after: Option<&FileVersion>,
) -> Result<()> {
    let actual = read_selected(path)?.map(|bytes| FileVersion {
        hash: ContentHash::of(&bytes),
        size: bytes.len() as u64,
        platforms: 0,
    });
    let matches = |version: Option<&FileVersion>| match (&actual, version) {
        (Some(actual), Some(version)) => actual.size == version.size && actual.hash == version.hash,
        (None, None) => true,
        _ => false,
    };
    if matches(before) || matches(after) {
        Ok(())
    } else {
        Err(Error::Stale)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod activation_tests {
    use super::*;

    fn scope(account: u64) -> Scope {
        Scope {
            steam_id: 0x0110_0001_0000_0000 | account,
            app_id: 10,
            cloud_app_id: 42,
        }
    }

    fn bindings(root: &Path) -> Bindings {
        ["one", "two", "deleted"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    SelectedFile {
                        path: root.join(name),
                        platforms: 7,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn switching_archives_the_whole_old_owner_and_keeps_its_pending_upload() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let bindings = bindings(saves.path());
        fs::write(saves.path().join("one"), b"A one").unwrap();
        fs::write(saves.path().join("two"), b"A two").unwrap();
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        first.set_preferences(Preferences {
            account_enabled: Some(true),
            game_enabled: Some(true),
        });
        let original = first.capture(&bindings).unwrap();
        let remote = RemoteView {
            revision: "1".into(),
            files: Manifest::default(),
        };
        let auth = first.authorize(scope(1).steam_id).unwrap();
        let pending = first
            .queue_upload(&original, &remote, "test", auth)
            .unwrap();
        drop(first);
        fs::write(
            saves.path().join("two"),
            b"A changed outside selected subset",
        )
        .unwrap();
        let subset = Bindings::from([("one".into(), bindings["one"].clone())]);
        let mut second = Vault::open(root.path(), scope(2)).unwrap();
        assert!(second.activate(&subset).unwrap());
        assert!(!saves.path().join("one").exists());
        assert_eq!(
            fs::read(saves.path().join("two")).unwrap(),
            b"A changed outside selected subset"
        );
        assert!(second.local().is_none());
        assert_eq!(
            fs::read_dir(second.directory.join("blobs"))
                .unwrap()
                .count(),
            0
        );
        assert!(second.capture(&subset).unwrap().0.is_empty());
        assert!(matches!(
            Vault::open(root.path(), scope(1)),
            Err(Error::Busy)
        ));
        drop(second);
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        assert_eq!(first.pending().unwrap().id, pending);
        assert_eq!(first.pending().unwrap().desired, original);
        let archived = first.local().unwrap().clone();
        assert_eq!(archived.0.len(), 2);
        assert_eq!(
            first.read_content(&archived.0["two"]).unwrap(),
            b"A changed outside selected subset"
        );
        assert!(first.activate(&bindings).unwrap());
        assert_eq!(first.capture(&bindings).unwrap(), archived);
        assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"A one");
        assert_eq!(first.pending().unwrap().id, pending);
    }

    #[test]
    fn every_switch_boundary_recovers_without_adopting_foreign_bytes() {
        for interrupted_after in 0..=3 {
            let root = tempfile::tempdir().unwrap();
            let saves = tempfile::tempdir().unwrap();
            let bindings = bindings(saves.path());
            for file in bindings.values() {
                fs::write(&file.path, b"A private").unwrap();
            }
            let mut first = Vault::open(root.path(), scope(1)).unwrap();
            let original = first.capture(&bindings).unwrap();
            drop(first);
            let mut second = Vault::open(root.path(), scope(2)).unwrap();
            let desired = Manifest(BTreeMap::from([
                ("one".into(), second.stage(b"B one", 7).unwrap()),
                ("two".into(), second.stage(b"B two", 7).unwrap()),
            ]));
            second.record_local(&desired).unwrap();
            assert!(matches!(
                second.activate_after(&bindings, Some(interrupted_after)),
                Err(Error::Recovery)
            ));
            drop(second);
            let mut first = Vault::open(root.path(), scope(1)).unwrap();
            assert!(first.needs_recovery());
            assert!(matches!(first.capture(&bindings), Err(Error::Recovery)));
            assert!(matches!(first.recover_local(), Err(Error::Recovery)));
            assert_eq!(first.local(), Some(&original));
            drop(first);
            let mut second = Vault::open(root.path(), scope(2)).unwrap();
            assert!(second.needs_recovery());
            assert!(matches!(
                second.game_started(&uuid::Uuid::new_v4().to_string()),
                Err(Error::Recovery)
            ));
            second.recover_local().unwrap();
            assert!(!second.needs_recovery());
            assert_eq!(second.capture(&bindings).unwrap(), desired);
            assert!(
                !second
                    .directory
                    .join("blobs")
                    .join(ContentHash::of(b"A private").as_str())
                    .exists()
            );
            assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"B one");
            assert!(!saves.path().join("deleted").exists());
            assert!(!second.activate(&bindings).unwrap());
        }
    }

    #[test]
    fn recovery_keeps_external_edits_and_the_global_fence() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let bindings = bindings(saves.path());
        fs::write(saves.path().join("two"), b"A private").unwrap();
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        first.capture(&bindings).unwrap();
        drop(first);
        let mut second = Vault::open(root.path(), scope(2)).unwrap();
        assert!(matches!(
            second.activate_after(&bindings, Some(1)),
            Err(Error::Recovery)
        ));
        fs::write(saves.path().join("two"), b"external edit").unwrap();
        drop(second);
        let mut second = Vault::open(root.path(), scope(2)).unwrap();
        assert!(matches!(second.recover_local(), Err(Error::Stale)));
        assert_eq!(
            fs::read(saves.path().join("two")).unwrap(),
            b"external edit"
        );
        assert!(second.needs_recovery());
        assert!(second.local().is_none());
    }

    #[test]
    fn consecutive_partial_switches_preserve_previously_unprojected_old_saves() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let bindings = bindings(saves.path());
        for (name, file) in &bindings {
            fs::write(&file.path, name.as_bytes()).unwrap();
        }
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        let complete = first.capture(&bindings).unwrap();
        drop(first);
        for (account, name) in [(2, "one"), (3, "two")] {
            let subset = Bindings::from([(name.into(), bindings[name].clone())]);
            let mut other = Vault::open(root.path(), scope(account)).unwrap();
            assert!(other.activate(&subset).unwrap());
            fs::write(&bindings[name].path, b"other account").unwrap();
            other.capture(&subset).unwrap();
        }
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        assert_eq!(first.local(), Some(&complete));
        assert!(first.activate(&bindings).unwrap());
        assert_eq!(first.capture(&bindings).unwrap(), complete);
    }

    fn epic(account: &str, app_name: &str) -> EpicScope {
        EpicScope {
            account_id: account.into(),
            namespace: "namespace".into(),
            app_name: app_name.into(),
        }
    }

    const EPIC_A: &str = "0123456789abcdef0123456789abcdef";
    const EPIC_B: &str = "abcdef0123456789abcdef0123456789";

    fn hold_owners(root: &Path) -> File {
        let file = File::create(root.join("owners.lock")).unwrap();
        file.try_lock().unwrap();
        file
    }

    #[test]
    fn epic_accounts_and_games_share_one_epic_store_lock() {
        let root = tempfile::tempdir().unwrap();
        let held = Vault::open(root.path(), epic(EPIC_A, "game")).unwrap();
        for other in [epic(EPIC_B, "game"), epic(EPIC_A, "other-game")] {
            assert!(matches!(Vault::open(root.path(), other), Err(Error::Busy)));
        }
        assert!(Vault::open(root.path(), scope(1)).is_ok());
        drop(held);
        assert!(Vault::open(root.path(), epic(EPIC_B, "game")).is_ok());
    }

    #[test]
    fn ownership_changes_wait_for_the_owners_lock_up_to_the_limit() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let bindings = bindings(saves.path());
        fs::write(saves.path().join("one"), b"A one").unwrap();
        let mut vault = Vault::open(root.path(), scope(1)).unwrap();
        vault.set_owners_wait(std::time::Duration::from_millis(100));
        let held = hold_owners(root.path());
        let started = std::time::Instant::now();
        assert!(matches!(vault.capture(&bindings), Err(Error::Busy)));
        assert!(started.elapsed() >= std::time::Duration::from_millis(100));
        assert!(!root.path().join("owners.json").exists());

        vault.set_owners_wait(std::time::Duration::from_secs(5));
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            drop(held);
        });
        let captured = vault.capture(&bindings).unwrap();
        release.join().unwrap();
        assert_eq!(captured.0.len(), 1);
    }

    #[test]
    fn a_guarded_switch_check_never_waits_for_its_own_guard() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let bindings = bindings(saves.path());
        fs::write(saves.path().join("one"), b"A one").unwrap();
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        first.capture(&bindings).unwrap();
        drop(first);
        let mut second = Vault::open(root.path(), scope(2)).unwrap();
        assert!(matches!(
            second.activate_after(&bindings, Some(0)),
            Err(Error::Recovery)
        ));
        second.set_owners_wait(std::time::Duration::from_secs(5));
        let guard = second.owners_guard().unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(
            second.idle_with(Some(&guard)),
            Err(Error::Recovery)
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn an_interrupted_epic_switch_fences_steam_until_epic_recovers() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let shared = bindings(saves.path());
        let steam_only = Bindings::from([(
            "steam".into(),
            SelectedFile {
                path: saves.path().join("steam"),
                platforms: 7,
            },
        )]);
        fs::write(saves.path().join("one"), b"Steam one").unwrap();
        fs::write(saves.path().join("steam"), b"Steam only").unwrap();
        let mut steam = Vault::open(root.path(), scope(1)).unwrap();
        steam.capture(&shared).unwrap();
        drop(steam);
        let mut epic_vault = Vault::open(root.path(), epic(EPIC_A, "game")).unwrap();
        let desired = Manifest(BTreeMap::from([(
            "one".into(),
            epic_vault.stage(b"Epic one", 7).unwrap(),
        )]));
        epic_vault.record_local(&desired).unwrap();
        assert!(matches!(
            epic_vault.activate_after(&shared, Some(0)),
            Err(Error::Recovery)
        ));

        let mut steam = Vault::open(root.path(), scope(1)).unwrap();
        assert!(steam.needs_recovery());
        assert!(matches!(steam.capture(&steam_only), Err(Error::Recovery)));
        assert!(matches!(steam.recover_local(), Err(Error::Recovery)));
        // Epic reads the displaced Steam blobs, so Steam must be closed.
        drop(steam);
        epic_vault.recover_local().unwrap();
        assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"Epic one");

        let mut steam = Vault::open(root.path(), scope(1)).unwrap();
        assert!(!steam.needs_recovery());
        assert_eq!(steam.capture(&steam_only).unwrap().0.len(), 1);
    }

    #[test]
    fn peeking_reads_the_journal_and_the_switch_while_every_lock_is_held() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let bindings = bindings(saves.path());
        fs::write(saves.path().join("one"), b"A one").unwrap();
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        let base = first.capture(&bindings).unwrap();
        first.set_clock(|| 1_234);
        first
            .confirm_equal(
                &base,
                &RemoteView {
                    revision: "1".into(),
                    files: base.clone(),
                },
            )
            .unwrap();
        fs::write(saves.path().join("one"), b"A edited").unwrap();
        first.capture(&bindings).unwrap();
        drop(first);
        let mut playing = Vault::open(root.path(), scope(3)).unwrap();
        playing
            .game_started(&uuid::Uuid::new_v4().to_string())
            .unwrap();
        drop(playing);
        let mut second = Vault::open(root.path(), scope(2)).unwrap();
        assert!(matches!(
            second.activate_after(&bindings, Some(0)),
            Err(Error::Recovery)
        ));
        let _owners = hold_owners(root.path());

        let summary = peek(root.path(), scope(1)).unwrap();

        assert_eq!(
            summary,
            Summary {
                baseline: true,
                synced_at: Some(1_234),
                game_session: false,
                applying: false,
                pending: None,
                local_changes: true,
                switching: Some(scope(2).into()),
            }
        );
        assert!(peek(root.path(), scope(3)).unwrap().game_session);
        let absent = peek(root.path(), scope(4)).unwrap();
        assert_eq!(absent.switching, Some(scope(2).into()));
        assert!(!absent.baseline && absent.synced_at.is_none());
        assert!(
            !StoreScope::from(scope(4))
                .directory(root.path())
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn concurrent_steam_and_epic_captures_keep_every_ownership_record() {
        const FILES: usize = 25;
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let workers = [StoreScope::from(scope(1)), epic(EPIC_A, "game").into()].map(|store| {
            let root = root.path().to_path_buf();
            let saves = saves.path().to_path_buf();
            std::thread::spawn(move || {
                let prefix = match store {
                    StoreScope::Steam(_) => "steam",
                    StoreScope::Epic(_) => "epic",
                };
                let mut vault = Vault::open(&root, store).unwrap();
                let mut owned = Bindings::new();
                for index in 0..FILES {
                    let name = format!("{prefix}-{index}");
                    let path = saves.join(&name);
                    fs::write(&path, name.as_bytes()).unwrap();
                    owned.insert(name, SelectedFile { path, platforms: 7 });
                    vault.capture(&owned).unwrap();
                }
            })
        });
        for worker in workers {
            worker.join().unwrap();
        }
        let owners: Owners = disk::read(&root.path().join("owners.json"))
            .unwrap()
            .unwrap();
        assert_eq!(owners.files.len(), FILES * 2);
        assert_eq!(owners.metadata.len(), FILES * 2);
        for (path, owner) in &owners.files {
            let steam = path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("steam");
            assert_eq!(matches!(owner, StoreScope::Steam(_)), steam);
        }
    }

    #[test]
    fn incomplete_ownership_is_rejected_without_reassigning_saves() {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let bindings = bindings(saves.path());
        fs::write(saves.path().join("one"), b"A private").unwrap();
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        first.capture(&bindings).unwrap();
        let owners: Owners = disk::read(&root.path().join("owners.json"))
            .unwrap()
            .unwrap();
        disk::write(
            &root.path().join("owners.json"),
            &serde_json::json!({ "files": owners.files }),
        )
        .unwrap();
        drop(first);
        let mut second = Vault::open(root.path(), scope(2)).unwrap();
        assert!(matches!(second.activate(&bindings), Err(Error::Journal(_))));
        assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"A private");
        assert!(second.local().is_none());
        assert!(second.needs_recovery());
        drop(second);
        let mut first = Vault::open(root.path(), scope(1)).unwrap();
        assert!(matches!(first.capture(&bindings), Err(Error::Journal(_))));
        assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"A private");
    }
}
