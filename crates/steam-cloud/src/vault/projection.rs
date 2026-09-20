use super::*;
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path},
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
    scope: Scope,
    bindings: Bindings,
    before: Manifest,
    after: Manifest,
    effect: Effect,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Effect {
    Download { remote: RemoteView },
}

impl Apply {
    pub(super) fn validate(&self, scope: Scope) -> Result<()> {
        self.scope.validate()?;
        if !same_store(self.scope, scope)
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
        let Effect::Download { remote } = &self.effect;
        remote.validate()
    }
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Owners {
    files: BTreeMap<PathBuf, Scope>,
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
    scope: Scope,
    bindings: Bindings,
    before: BTreeMap<String, SwitchBefore>,
    after: Manifest,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SwitchBefore {
    owner: Option<Scope>,
    file: Option<FileVersion>,
}

pub(super) fn check_switch(root: &Path) -> Result<()> {
    if disk::read::<Owners>(&root.join("owners.json"))?
        .is_some_and(|owners| owners.switching.is_some())
    {
        return Err(Error::Recovery);
    }
    Ok(())
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

    fn claim(&self, bindings: &Bindings) -> Result<Bindings> {
        let normalized = self.normalize(bindings)?;
        let path = self.root.join("owners.json");
        let mut owners: Owners = disk::read(&path)?.unwrap_or_default();
        if owners.switching.is_some() {
            return Err(Error::Recovery);
        }
        for (name, file) in &normalized {
            for (claimed, owner) in &owners.files {
                if !same_store(*owner, self.scope)
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
            owners.files.insert(file.path.clone(), self.scope);
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
        if self.recover_switch(stop_after)? {
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
                && !same_store(*owner, self.scope)
            {
                displaced.insert((owner.steam_id, owner.cloud_app_id), *owner);
            }
        }
        if displaced.is_empty() {
            self.claim(&normalized)?;
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
                if !same_store(*owner, *scope) {
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
            old_bindings.insert(*key, files);
        }
        let mut archived = BTreeMap::new();
        for (key, scope) in &displaced {
            let mut old = Vault::open_locked(self.root.clone(), *scope, self._lock.try_clone()?)?;
            old.idle()?;
            let bindings = old.claim(&old_bindings[key])?;
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
            old.record_local(&files)?;
            archived.insert(*key, files);
        }
        let mut changes = Bindings::new();
        let mut before = BTreeMap::new();
        for (name, file) in &normalized {
            let owner = owners.files.get(&file.path).copied();
            let foreign = owner.filter(|owner| !same_store(*owner, self.scope));
            if foreign.is_none() && desired.is_none() {
                continue; // No account snapshot: leave own/unowned physical files alone.
            }
            let previous = if let Some(owner) = foreign {
                let metadata = owners
                    .metadata
                    .get(&file.path)
                    .ok_or(Error::Authentication)?;
                archived[&(owner.steam_id, owner.cloud_app_id)]
                    .0
                    .get(&metadata.name)
                    .cloned()
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
            scope: self.scope,
            bindings: changes,
            before,
            after: desired.unwrap_or_default(),
        });
        self.save_owners(&owners)?;
        self.recover_switch(stop_after)
    }

    fn save_owners(&mut self, owners: &Owners) -> Result<()> {
        if let Err(error) = disk::write(&self.root.join("owners.json"), owners) {
            self.poisoned = true;
            return Err(error);
        }
        Ok(())
    }

    fn recover_switch(&mut self, stop_after: Option<usize>) -> Result<bool> {
        let mut owners: Owners = disk::read(&self.root.join("owners.json"))?.unwrap_or_default();
        let Some(plan) = owners.switching.clone() else {
            return Ok(false);
        };
        if !same_store(plan.scope, self.scope) || self.state.applying.is_some() {
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
            let owner_matches = match (owners.files.get(&selected.path), before.owner) {
                (Some(current), Some(previous)) => same_store(*current, previous),
                (None, None) => true,
                _ => false,
            };
            if !owner_matches {
                return Err(Error::Stale);
            }
            if let Some(file) = &before.file {
                let owner = before.owner.unwrap_or(self.scope);
                let old = Vault::open_locked(self.root.clone(), owner, self._lock.try_clone()?)?;
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
            owners.files.insert(file.path.clone(), self.scope);
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
        self.save_owners(&owners)?;
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
                        .prefix(".forge-cloud-names-")
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
        self.capture_with_missing(bindings, false)
    }

    /// A retry of a partial uninstall must retain already removed files in the
    /// account snapshot while still capturing surviving files' current bytes.
    /// This does not create upload intent or project files back into the game.
    pub fn capture_preserving_missing(&mut self, bindings: &Bindings) -> Result<Manifest> {
        self.capture_with_missing(bindings, true)
    }

    fn capture_with_missing(
        &mut self,
        bindings: &Bindings,
        preserve_missing: bool,
    ) -> Result<Manifest> {
        self.idle()?;
        let bindings = self.claim(bindings)?;
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
        self.record_local(&files)?;
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
        self.transfers_enabled()?;
        self.idle()?;
        current.validate()?;
        if shown != current {
            return Err(Error::Stale);
        }
        let local = self.capture(bindings)?;
        if !choose_cloud && self.decision(&local, current)? != Decision::Download {
            return Err(Error::Stale);
        }
        self.prepare_checked_apply(
            bindings,
            current.files.clone(),
            Effect::Download {
                remote: current.clone(),
            },
            Some(&local),
        )?;
        self.recover_local()
    }

    #[cfg(test)]
    fn prepare_apply(
        &mut self,
        bindings: &Bindings,
        after: Manifest,
        effect: Effect,
    ) -> Result<()> {
        self.prepare_checked_apply(bindings, after, effect, None)
    }

    fn prepare_checked_apply(
        &mut self,
        bindings: &Bindings,
        after: Manifest,
        effect: Effect,
        expected_before: Option<&Manifest>,
    ) -> Result<()> {
        self.idle()?;
        if self.state.pending.is_some() {
            return Err(Error::Busy);
        }
        self.verify(&after)?;
        let bindings = self.claim(bindings)?;
        if after.0.keys().any(|name| !bindings.contains_key(name)) {
            return Err(Error::Invalid);
        }
        let before = self.capture(&bindings)?;
        if expected_before.is_some_and(|expected| expected != &before) {
            return Err(Error::Stale);
        }
        let mut next = self.state.clone();
        // Both preimage and intent are durable before the first replacement.
        next.applying = Some(Apply {
            scope: self.scope,
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
        self.healthy()?;
        if self.state.game.is_some() {
            return Err(Error::Busy);
        }
        self.recover_switch(stop_after)?;
        let Some(plan) = self.state.applying.clone() else {
            return Ok(());
        };
        if !same_store(plan.scope, self.scope) {
            return Err(Error::Invalid);
        }
        plan.before.validate()?;
        self.verify(&plan.after)?;
        self.verify(&plan.before)?;
        let bindings = self.claim(&plan.bindings)?;
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
        let Effect::Download { remote } = plan.effect;
        next.baseline = Some(remote);
        next.intent = None;
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
        .prefix(".forge-cloud-owner-")
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
