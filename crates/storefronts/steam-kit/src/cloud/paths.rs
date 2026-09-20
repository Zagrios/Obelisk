use super::{CloudError, CloudFile, CloudMetadata};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// Resolves and discovers Auto-Cloud files. This is not a RemoteStorage cache
/// adapter. Unsupported metadata cannot produce an empty set.
pub struct MacCloudPaths {
    rules: Vec<Rule>,
}

struct Rule {
    root: String,
    source: String,
    destination: PathBuf,
    recursive: bool,
    pattern: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalCloudFile {
    pub name: String,
    pub path: PathBuf,
    pub platforms: u32,
}

fn text<'a>(node: &'a CloudMetadata, key: &str) -> Result<&'a str, CloudError> {
    node.field(key)
        .and_then(CloudMetadata::text)
        .ok_or(CloudError::Invalid("Auto-Cloud field"))
}

fn fields(node: &CloudMetadata, allowed: &[&str]) -> Result<(), CloudError> {
    let entries = node
        .entries()
        .ok_or(CloudError::Invalid("Auto-Cloud object"))?;
    let mut seen = std::collections::BTreeSet::new();
    for (key, _) in entries {
        if !allowed.iter().any(|a| key.eq_ignore_ascii_case(a))
            || !seen.insert(key.to_ascii_lowercase())
        {
            return Err(CloudError::Invalid(
                "unsupported or duplicate Auto-Cloud field",
            ));
        }
    }
    Ok(())
}

fn relative(value: &str, allow_empty: bool) -> Result<String, CloudError> {
    if value.chars().any(char::is_control) || value.contains([':', '%', '{', '}', '*', '?']) {
        return Err(CloudError::Invalid("Auto-Cloud path"));
    }
    let value = value.replace('\\', "/");
    if value.starts_with('/') || value.split('/').any(|p| p == "..") {
        return Err(CloudError::Invalid("Auto-Cloud traversal"));
    }
    let value = value
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect::<Vec<_>>()
        .join("/");
    if value.is_empty() && !allow_empty {
        return Err(CloudError::Invalid("empty Auto-Cloud filename"));
    }
    Ok(value)
}

fn substitute(value: &str, steam_id: u64) -> String {
    value
        .replace("{64BitSteamID}", &steam_id.to_string())
        .replace("{Steam3AccountID}", &(steam_id as u32).to_string())
}

fn root(name: &str, home: &Path, install: &Path) -> Result<PathBuf, CloudError> {
    match name {
        "MacHome" => Ok(home.to_owned()),
        "MacAppSupport" => Ok(home.join("Library/Application Support")),
        "MacDocuments" => Ok(home.join("Documents")),
        "GameInstall" => Ok(install.to_owned()),
        _ => Err(CloudError::Invalid("unsupported macOS Auto-Cloud root")),
    }
}

impl MacCloudPaths {
    pub fn new(
        metadata: &CloudMetadata,
        steam_id: u64,
        home: &Path,
        install: &Path,
    ) -> Result<Self, CloudError> {
        let mut keys = std::collections::BTreeSet::new();
        for (key, _) in metadata
            .entries()
            .ok_or(CloudError::Invalid("Auto-Cloud metadata"))?
        {
            if !keys.insert(key.to_ascii_lowercase()) {
                return Err(CloudError::Invalid("duplicate Auto-Cloud metadata field"));
            }
        }
        if steam_id >> 32 != 0x0110_0001
            || steam_id as u32 == 0
            || !home.is_absolute()
            || !install.is_absolute()
        {
            return Err(CloudError::Invalid("Auto-Cloud identity or roots"));
        }
        // Redirects need an adapter that resolves and authenticates the target
        // Cloud App ID. Never silently use the logical game's store instead.
        if metadata
            .field("appidredirect")
            .is_some_and(|v| v.text() != Some("0"))
        {
            return Err(CloudError::Invalid("unresolved Cloud App ID redirect"));
        }
        let saves = metadata
            .field("savefiles")
            .and_then(CloudMetadata::entries)
            .ok_or(CloudError::Invalid("missing Auto-Cloud rules"))?;
        let overrides = match metadata.field("rootoverrides") {
            Some(value) => value
                .entries()
                .ok_or(CloudError::Invalid("Auto-Cloud overrides"))?,
            None => &[],
        };
        let mut rules = Vec::new();
        for (_, save) in saves {
            fields(save, &["root", "path", "pattern", "recursive", "platforms"])?;
            // Platform-specific rule syntax is preserved but not guessed here.
            if save.field("platforms").is_some() {
                return Err(CloudError::Invalid("unsupported Auto-Cloud platform rule"));
            }
            let source_root = text(save, "root")?;
            let source_path = text(save, "path")?;
            let pattern = text(save, "pattern")?;
            if pattern.is_empty()
                || pattern.len() > 255
                || pattern.contains(['/', '\\', '[', ']', ';', '{', '}'])
                || pattern.chars().any(char::is_control)
            {
                return Err(CloudError::Invalid("unsupported Auto-Cloud wildcard"));
            }
            let recursive = match text(save, "recursive")? {
                "0" => false,
                "1" => true,
                _ => return Err(CloudError::Invalid("Auto-Cloud recursion")),
            };
            let mut local_root = source_root;
            let mut local_path = source_path.to_owned();
            let mut matched = false;
            for (_, override_) in overrides {
                if text(override_, "root")? != source_root {
                    continue;
                }
                let applies = match (text(override_, "os")?, text(override_, "oscompare")?) {
                    ("MacOS", "=") => true,
                    ("Windows" | "Linux", "=") => false,
                    _ => return Err(CloudError::Invalid("unsupported Auto-Cloud OS comparison")),
                };
                if !applies {
                    continue;
                }
                fields(
                    override_,
                    &["root", "os", "oscompare", "useinstead", "pathtransforms"],
                )?;
                if matched {
                    return Err(CloudError::Invalid("ambiguous Auto-Cloud override"));
                }
                matched = true;
                local_root = text(override_, "useinstead")?;
                if let Some(transforms) = override_.field("pathtransforms") {
                    for (_, transform) in transforms
                        .entries()
                        .ok_or(CloudError::Invalid("Auto-Cloud transforms"))?
                    {
                        fields(transform, &["find", "replace"])?;
                        let find = text(transform, "find")?;
                        let replacement = text(transform, "replace")?;
                        // Only a complete subdirectory or component prefix is
                        // supported until other transform forms are validated.
                        if find.is_empty()
                            || !(local_path == find
                                || local_path
                                    .strip_prefix(find)
                                    .is_some_and(|s| s.starts_with('/')))
                        {
                            return Err(CloudError::Invalid(
                                "unsupported Auto-Cloud path transform",
                            ));
                        }
                        local_path = format!("{replacement}{}", &local_path[find.len()..]);
                    }
                }
            }
            // Valve serializes some override replacements with a leading slash.
            // It is relative to the new root, never an absolute filesystem path.
            let source = relative(&substitute(source_path, steam_id), true)?;
            let local = relative(
                substitute(&local_path, steam_id).trim_start_matches('/'),
                true,
            )?;
            rules.push(Rule {
                root: source_root.to_owned(),
                source,
                destination: root(local_root, home, install)?.join(local),
                recursive,
                pattern: pattern.to_owned(),
            });
        }
        if rules.is_empty() {
            return Err(CloudError::Invalid("empty Auto-Cloud rules"));
        }
        Ok(Self { rules })
    }

    /// Existing files can outlive a publisher's current wildcard. Keep their
    /// remote identities and apply them under the declared directory; wildcard
    /// changes do not authorize deletion of historical backups.
    pub fn existing_file(&self, file: &CloudFile) -> Result<Option<PathBuf>, CloudError> {
        if file.persist_state != 0 {
            return Ok(None);
        }
        self.destination(&file.name, file.platforms)
    }

    /// The local path a remote name projects to, or `None` when the file is not
    /// published for macOS. A name no rule maps is an error, not a skip.
    pub fn destination(&self, name: &str, platforms: u32) -> Result<Option<PathBuf>, CloudError> {
        if platforms & 2 == 0 {
            return Ok(None);
        }
        self.path_for_name(name)?
            .map(Some)
            .ok_or(CloudError::Invalid("unmapped Auto-Cloud file"))
    }

    fn path_for_name(&self, name: &str) -> Result<Option<PathBuf>, CloudError> {
        let mut selected = None;
        for rule in &self.rules {
            let Some(after_root) = name.strip_prefix(&format!("%{}%", rule.root)) else {
                continue;
            };
            let path = after_root.strip_prefix('/').unwrap_or(after_root);
            let suffix = if rule.source.is_empty() {
                path
            } else {
                let Some(suffix) = path.strip_prefix(&format!("{}/", rule.source)) else {
                    continue;
                };
                suffix
            };
            let suffix = relative(suffix, false)?;
            if !rule.recursive && suffix.contains('/') {
                continue;
            }
            let destination = rule.destination.join(suffix);
            if selected
                .as_ref()
                .is_some_and(|previous| previous != &destination)
            {
                return Err(CloudError::Invalid("ambiguous Auto-Cloud destination"));
            }
            selected = Some(destination);
        }
        Ok(selected)
    }

    /// Includes known files even after deletion or a publisher wildcard change.
    /// The caller can therefore distinguish a missing known save from an empty
    /// discovery. Scan only while the supervisor has established game quiescence.
    pub fn local_files(&self, known: &[CloudFile]) -> Result<Vec<LocalCloudFile>, CloudError> {
        let mut selected = BTreeMap::<PathBuf, LocalCloudFile>::new();
        let mut excluded = BTreeSet::new();
        for file in known {
            let path = if file.persist_state != 0 && file.platforms & 2 != 0 {
                // A recreated save keeps its deleted remote identity, including
                // its platform mask and historical wildcard exceptions.
                self.path_for_name(&file.name)?
            } else {
                self.existing_file(file)?
            };
            if let Some(path) = path {
                let entry = LocalCloudFile {
                    name: file.name.clone(),
                    path: path.clone(),
                    platforms: file.platforms,
                };
                if selected
                    .insert(path, entry.clone())
                    .is_some_and(|previous| previous != entry)
                {
                    return Err(CloudError::Invalid("aliased Auto-Cloud identity"));
                }
            } else if let Some(path) = self.path_for_name(&file.name)? {
                // Foreign-platform identities are outside the managed Mac set.
                // Ignore matching local files instead of reclassifying them.
                excluded.insert(path);
            }
        }
        let known_paths: BTreeSet<_> = selected.keys().cloned().collect();
        let mut visited = 0;
        for rule in &self.rules {
            let mut pending = vec![(rule.destination.clone(), 0)];
            while let Some((directory, depth)) = pending.pop() {
                if depth > 64 {
                    return Err(CloudError::Invalid("Auto-Cloud directory depth"));
                }
                if !plain_path(&directory)? {
                    continue;
                }
                if !fs::symlink_metadata(&directory)
                    .map_err(|_| CloudError::Storage)?
                    .is_dir()
                {
                    return Err(CloudError::Storage);
                }
                for entry in fs::read_dir(&directory).map_err(|_| CloudError::Storage)? {
                    visited += 1;
                    if visited > 100_000 {
                        return Err(CloudError::Invalid("Auto-Cloud scan size"));
                    }
                    let entry = entry.map_err(|_| CloudError::Storage)?;
                    let path = entry.path();
                    if excluded.contains(&path) && !known_paths.contains(&path) {
                        continue;
                    }
                    let metadata = fs::symlink_metadata(&path).map_err(|_| CloudError::Storage)?;
                    // A matching symlink or recursive directory alias must never
                    // turn into an absent file and cause a remote deletion.
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| CloudError::Invalid("Auto-Cloud filename encoding"))?;
                    if metadata.file_type().is_symlink() {
                        if rule.recursive
                            || wildcard(&rule.pattern, &name)
                            || selected.contains_key(&path)
                        {
                            return Err(CloudError::Invalid("Auto-Cloud symlink"));
                        }
                        continue;
                    }
                    if metadata.is_dir() {
                        if rule.recursive {
                            pending.push((path, depth + 1));
                        }
                        continue;
                    }
                    if !wildcard(&rule.pattern, &name) {
                        continue;
                    }
                    if !metadata.is_file() || metadata.nlink() != 1 {
                        return Err(CloudError::Invalid("Auto-Cloud file alias"));
                    }
                    let suffix = path
                        .strip_prefix(&rule.destination)
                        .map_err(|_| CloudError::Storage)?
                        .to_str()
                        .ok_or(CloudError::Invalid("Auto-Cloud filename encoding"))?;
                    let suffix = relative(suffix, false)?;
                    let separator = if rule.source.is_empty() { "" } else { "/" };
                    let remote = format!("%{}%{}{separator}{suffix}", rule.root, rule.source);
                    if let Some(previous) = selected.get(&path) {
                        if !known_paths.contains(&path) && previous.name != remote {
                            return Err(CloudError::Invalid("ambiguous Auto-Cloud source"));
                        }
                        continue;
                    }
                    selected.insert(
                        path.clone(),
                        LocalCloudFile {
                            name: remote,
                            path,
                            platforms: u32::MAX,
                        },
                    );
                }
            }
        }
        let mut names = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for file in selected.values() {
            if !names.insert(&file.name) {
                return Err(CloudError::Invalid("ambiguous Auto-Cloud remote name"));
            }
            if plain_path(&file.path)? {
                let metadata = fs::symlink_metadata(&file.path).map_err(|_| CloudError::Storage)?;
                if !metadata.is_file()
                    || metadata.nlink() != 1
                    || !identities.insert((metadata.dev(), metadata.ino()))
                {
                    return Err(CloudError::Invalid("Auto-Cloud file alias"));
                }
            }
        }
        Ok(selected.into_values().collect())
    }
}

fn plain_path(path: &Path) -> Result<bool, CloudError> {
    let mut exists = true;
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CloudError::Invalid("Auto-Cloud symlink"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => exists = false,
            Err(_) => return Err(CloudError::Storage),
        }
    }
    Ok(exists)
}

// Linear-space matching of the supported Steam filename '*' and '?' patterns.
// Case is preserved, as are the destination filesystem's own alias semantics.
fn wildcard(pattern: &str, name: &str) -> bool {
    let name: Vec<_> = name.chars().collect();
    let mut previous = vec![false; name.len() + 1];
    previous[0] = true;
    for token in pattern.chars() {
        let mut next = vec![false; name.len() + 1];
        if token == '*' {
            next[0] = previous[0];
        }
        for index in 0..name.len() {
            next[index + 1] = if token == '*' {
                previous[index + 1] || next[index]
            } else {
                previous[index] && (token == '?' || token == name[index])
            };
        }
        previous = next;
    }
    previous[name.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value(s: &str) -> CloudMetadata {
        CloudMetadata::Value(s.into())
    }
    fn object(entries: Vec<(&str, CloudMetadata)>) -> CloudMetadata {
        CloudMetadata::Object(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }
    fn metadata() -> CloudMetadata {
        object(vec![
            (
                "savefiles",
                object(vec![(
                    "0",
                    object(vec![
                        ("root", value("WinAppDataLocalLow")),
                        ("path", value("Studio/Game/{Steam3AccountID}")),
                        ("pattern", value("*.sav")),
                        ("recursive", value("1")),
                    ]),
                )]),
            ),
            (
                "rootoverrides",
                object(vec![(
                    "0",
                    object(vec![
                        ("root", value("WinAppDataLocalLow")),
                        ("os", value("MacOS")),
                        ("oscompare", value("=")),
                        ("useinstead", value("MacHome")),
                        (
                            "pathtransforms",
                            object(vec![(
                                "0",
                                object(vec![
                                    ("find", value("Studio/Game/{Steam3AccountID}")),
                                    (
                                        "replace",
                                        value(
                                            "/Library/Application Support/Studio.Game/{Steam3AccountID}",
                                        ),
                                    ),
                                ]),
                            )]),
                        ),
                    ]),
                )]),
            ),
        ])
    }
    fn file(name: &str) -> CloudFile {
        CloudFile {
            name: name.into(),
            sha1: "00".repeat(20),
            size: 1,
            timestamp: 0,
            persist_state: 0,
            platforms: u32::MAX,
            reupload_requested: false,
        }
    }

    #[test]
    fn local_discovery_selects_new_saves_and_keeps_missing_known_files_and_backups() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        let directory = home.join("Library/Application Support/Studio.Game/42");
        fs::create_dir_all(directory.join("nested")).unwrap();
        for name in [
            "new.sav",
            "existing.sav.bak",
            "ignored.txt",
            "nested/next.sav",
        ] {
            fs::write(directory.join(name), b"data").unwrap();
        }
        let resolver = MacCloudPaths::new(&metadata(), 0x011000010000002a, &home, &home).unwrap();
        let mut backup = file("%WinAppDataLocalLow%/Studio/Game/42/existing.sav.bak");
        backup.platforms = 7;
        let missing = file("%WinAppDataLocalLow%Studio/Game/42/deleted.sav");
        let selected = resolver
            .local_files(&[backup.clone(), missing.clone()])
            .unwrap();
        assert_eq!(selected.len(), 4);
        assert!(
            selected
                .iter()
                .any(|file| file.name == backup.name && file.platforms == 7)
        );
        assert!(
            selected
                .iter()
                .any(|file| file.name == missing.name && !file.path.exists())
        );
        assert!(
            selected
                .iter()
                .any(|file| file.name == "%WinAppDataLocalLow%Studio/Game/42/nested/next.sav")
        );
        assert!(
            !selected
                .iter()
                .any(|file| file.path.ends_with("ignored.txt"))
        );
        fs::remove_dir_all(&directory).unwrap();
        assert_eq!(resolver.local_files(&[missing]).unwrap().len(), 1);
    }

    #[test]
    fn discovery_refuses_directory_links_file_links_and_remote_aliases() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        let directory = home.join("Library/Application Support/Studio.Game/42");
        fs::create_dir_all(&directory).unwrap();
        let resolver = MacCloudPaths::new(&metadata(), 0x011000010000002a, &home, &home).unwrap();
        fs::write(directory.join("real.sav"), b"data").unwrap();
        std::os::unix::fs::symlink(directory.join("real.sav"), directory.join("alias.sav"))
            .unwrap();
        assert!(resolver.local_files(&[]).is_err());
        fs::remove_file(directory.join("alias.sav")).unwrap();
        fs::hard_link(directory.join("real.sav"), directory.join("alias.sav")).unwrap();
        assert!(resolver.local_files(&[]).is_err());
        fs::remove_file(directory.join("alias.sav")).unwrap();
        assert!(
            resolver
                .local_files(&[
                    file("%WinAppDataLocalLow%Studio/Game/42/real.sav"),
                    file("%WinAppDataLocalLow%/Studio/Game/42/real.sav")
                ])
                .is_err()
        );
        std::os::unix::fs::symlink(&directory, directory.join("loop")).unwrap();
        assert!(resolver.local_files(&[]).is_err());
    }

    #[test]
    fn local_discovery_never_reclassifies_an_existing_foreign_platform_file() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        let directory = home.join("Library/Application Support/Studio.Game/42");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("slot.sav"), b"local mac save").unwrap();
        let resolver = MacCloudPaths::new(&metadata(), 0x011000010000002a, &home, &home).unwrap();
        for prefix in ["%WinAppDataLocalLow%", "%WinAppDataLocalLow%/"] {
            let mut windows = file(&format!("{prefix}Studio/Game/42/slot.sav"));
            windows.platforms = 1;
            for state in [0, 2] {
                windows.persist_state = state;
                assert!(resolver.local_files(&[windows.clone()]).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn local_discovery_reuses_recreated_tombstone_identity_and_platforms() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        let directory = home.join("Library/Application Support/Studio.Game/42");
        fs::create_dir_all(&directory).unwrap();
        let resolver = MacCloudPaths::new(&metadata(), 0x011000010000002a, &home, &home).unwrap();
        for prefix in ["%WinAppDataLocalLow%", "%WinAppDataLocalLow%/"] {
            for name in ["slot.sav", "slot.sav.bak"] {
                let path = directory.join(name);
                fs::write(&path, b"recreated mac save").unwrap();
                let mut deleted = file(&format!("{prefix}Studio/Game/42/{name}"));
                deleted.persist_state = 2;
                deleted.platforms = 2;
                assert_eq!(
                    resolver.local_files(&[deleted.clone()]).unwrap(),
                    vec![LocalCloudFile {
                        name: deleted.name,
                        path: path.clone(),
                        platforms: 2,
                    }]
                );
                fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn foreign_alias_does_not_hide_a_known_mac_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        let directory = home.join("Library/Application Support/Studio.Game/42");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("slot.sav");
        fs::write(&path, b"mac save").unwrap();
        let resolver = MacCloudPaths::new(&metadata(), 0x011000010000002a, &home, &home).unwrap();
        let mut windows = file("%WinAppDataLocalLow%Studio/Game/42/slot.sav");
        windows.platforms = 1;
        let mut mac = file("%WinAppDataLocalLow%/Studio/Game/42/slot.sav");
        mac.platforms = 2;
        assert_eq!(
            resolver.local_files(&[windows, mac.clone()]).unwrap(),
            vec![LocalCloudFile {
                name: mac.name,
                path,
                platforms: 2,
            }]
        );
    }

    #[test]
    fn filename_patterns_preserve_case_and_unicode() {
        for name in ["slot.sav", "é.sav", ".sav"] {
            assert!(wildcard("*.sav", name));
        }
        assert!(!wildcard("*.sav", "slot.sav.bak"));
        assert!(!wildcard("*.sav", "slot.SAV"));
        assert!(wildcard("?.sav", "é.sav"));
        assert!(!wildcard("?.sav", "ab.sav"));
        assert!(wildcard("*", ""));
    }
    #[test]
    fn maps_canonical_cross_platform_names_without_losing_backups() {
        let resolver = MacCloudPaths::new(
            &metadata(),
            0x0110_0001_0000_002a,
            Path::new("/Users/example"),
            Path::new("/Games/example"),
        )
        .unwrap();
        for source in [
            "%WinAppDataLocalLow%Studio/Game/42/nested/slot.sav.bak",
            "%WinAppDataLocalLow%/Studio/Game/42/nested/slot.sav.bak",
        ] {
            assert_eq!(
                resolver.existing_file(&file(source)).unwrap().unwrap(),
                Path::new(
                    "/Users/example/Library/Application Support/Studio.Game/42/nested/slot.sav.bak"
                )
            );
        }
        for source in [
            "%WinAppDataLocalLow%Studio/Game/42/../../../escape",
            "%WinAppDataLocalLow%Studio/Game/420/file.sav",
            "%WinAppDataLocalLow%Studio/Game/99/file.sav",
            "api-file.sav",
        ] {
            assert!(resolver.existing_file(&file(source)).is_err());
        }
        let mut other = file("%WinAppDataLocalLow%Studio/Game/42/slot.sav");
        other.platforms = 1;
        assert!(resolver.existing_file(&other).unwrap().is_none());
    }
    #[test]
    fn rejects_unknown_or_ambiguous_rules() {
        let mut m = metadata();
        if let CloudMetadata::Object(ref mut e) = m {
            e.push(("appidredirect".into(), value("7")));
        }
        assert!(
            MacCloudPaths::new(
                &m,
                0x0110_0001_0000_002a,
                Path::new("/Users/example"),
                Path::new("/Games/example")
            )
            .is_err()
        );
        assert!(relative("../../save", false).is_err());
        assert!(relative("/absolute", false).is_err());
        assert!(relative("C:\\save", false).is_err());
        assert!(relative("{unknown}/save", false).is_err());
        assert_eq!(relative("./folder\\save", false).unwrap(), "folder/save");
    }

    #[test]
    fn native_roots_and_steam64_substitution_are_independent_of_the_game() {
        let id = 0x0110_0001_0000_002a;
        for (name, base) in [
            ("MacHome", "/Users/example"),
            (
                "MacAppSupport",
                "/Users/example/Library/Application Support",
            ),
            ("MacDocuments", "/Users/example/Documents"),
            ("GameInstall", "/Games/example"),
        ] {
            let m = object(vec![(
                "savefiles",
                object(vec![(
                    "0",
                    object(vec![
                        ("root", value(name)),
                        ("path", value("saves/{64BitSteamID}")),
                        ("pattern", value("*")),
                        ("recursive", value("0")),
                    ]),
                )]),
            )]);
            let resolver = MacCloudPaths::new(
                &m,
                id,
                Path::new("/Users/example"),
                Path::new("/Games/example"),
            )
            .unwrap();
            assert_eq!(
                resolver
                    .existing_file(&file(&format!("%{name}%saves/{id}/slot")))
                    .unwrap()
                    .unwrap(),
                Path::new(base).join(format!("saves/{id}/slot"))
            );
            assert!(
                resolver
                    .existing_file(&file(&format!("%{name}%saves/{id}/nested/slot")))
                    .is_err()
            );
        }
    }
}
