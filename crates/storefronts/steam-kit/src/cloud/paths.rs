use super::{CloudError, CloudFile, CloudMetadata};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// Resolves and discovers Auto-Cloud files. This is not a RemoteStorage cache
/// adapter. Unsupported metadata cannot produce an empty set.
pub struct AutoCloudPaths {
    rules: Vec<Rule>,
    foreign: Vec<(String, String, bool)>,
    mask: u32,
}

/// The build of a game that runs, and where its Auto-Cloud roots live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudPlatform {
    /// A macOS build: its roots are under the player's home.
    Mac { home: PathBuf },
    /// A Windows build running in Wine: its roots are under the user profile
    /// of its prefix.
    Windows { profile: PathBuf },
}

impl CloudPlatform {
    /// This platform's bit in a Cloud file's platform mask.
    pub fn mask(&self) -> u32 {
        match self {
            Self::Mac { .. } => 2,
            Self::Windows { .. } => 1,
        }
    }

    /// The name root overrides compare against.
    fn os(&self) -> &'static str {
        match self {
            Self::Mac { .. } => "MacOS",
            Self::Windows { .. } => "Windows",
        }
    }

    /// Whether `name` is another platform's root. Steam skips a rule there,
    /// unless a root override moves it onto this platform.
    fn foreign_root(&self, name: &str) -> bool {
        let foreign: &[&str] = match self {
            Self::Mac { .. } => &["Win", "Linux"],
            Self::Windows { .. } => &["Mac", "Linux"],
        };
        foreign.iter().any(|prefix| name.starts_with(prefix))
    }

    fn base(&self) -> &Path {
        match self {
            Self::Mac { home } => home,
            Self::Windows { profile } => profile,
        }
    }

    fn root(&self, name: &str, install: &Path) -> Result<PathBuf, CloudError> {
        if canonical_root(name) == "GameInstall" {
            return Ok(install.to_owned());
        }
        let folder = match (self, name) {
            (Self::Mac { .. }, "MacHome") => "",
            (Self::Mac { .. }, "MacAppSupport") => "Library/Application Support",
            (Self::Mac { .. }, "MacDocuments") => "Documents",
            (Self::Windows { .. }, "WinMyDocuments") => "Documents",
            (Self::Windows { .. }, "WinAppDataLocal") => "AppData/Local",
            (Self::Windows { .. }, "WinAppDataLocalLow") => "AppData/LocalLow",
            (Self::Windows { .. }, "WinAppDataRoaming") => "AppData/Roaming",
            (Self::Windows { .. }, "WinSavedGames") => "Saved Games",
            _ => return Err(CloudError::Invalid("unsupported Auto-Cloud root")),
        };
        Ok(self.base().join(folder))
    }
}

fn canonical_root(name: &str) -> &str {
    if name.eq_ignore_ascii_case("GameInstall") {
        "GameInstall"
    } else {
        name
    }
}

// Publishers vary the case of an override's OS, as "macos".
fn canonical_os(name: &str) -> Option<&'static str> {
    ["MacOS", "Windows", "Linux"]
        .into_iter()
        .find(|os| name.eq_ignore_ascii_case(os))
}

fn rule_suffix(
    name: &str,
    root: &str,
    source: &str,
    recursive: bool,
) -> Result<Option<String>, CloudError> {
    Ok(rule_tail(name, root, source, recursive, false)?.map(|(_, suffix)| suffix))
}

/// Splits `name` under a rule into the rule's directory as `name` spells it and
/// the path below. With `any_case`, `name` may spell the directory in another
/// ASCII case.
fn rule_tail<'a>(
    name: &'a str,
    root: &str,
    source: &str,
    recursive: bool,
    any_case: bool,
) -> Result<Option<(&'a str, String)>, CloudError> {
    let Some(after_root) = name.strip_prefix(&format!("%{root}%")) else {
        return Ok(None);
    };
    let path = after_root.strip_prefix('/').unwrap_or(after_root);
    let (spelled, rest) = if source.is_empty() {
        ("", path)
    } else {
        let Some((spelled, rest)) = path.split_at_checked(source.len()) else {
            return Ok(None);
        };
        let same = if any_case {
            spelled.eq_ignore_ascii_case(source)
        } else {
            spelled == source
        };
        let Some(rest) = rest.strip_prefix('/') else {
            return Ok(None);
        };
        if !same {
            return Ok(None);
        }
        (spelled, rest)
    };
    let suffix = relative(rest, false)?;
    Ok((recursive || !suffix.contains('/')).then_some((spelled, suffix)))
}

struct Rule {
    root: String,
    source: String,
    destination: PathBuf,
    /// The root `destination` is under, kept only where a name may spell the
    /// rule's directory in another case: see `case_alias_suffix`.
    alias_base: Option<PathBuf>,
    recursive: bool,
    pattern: String,
    platforms: u32,
}

impl Rule {
    /// What `name` adds to this rule's directory when it spells that directory
    /// in another case, as a server's older files can. The two spellings are
    /// one directory only if the filesystem shows it; anything else is an
    /// error, never a guess.
    fn case_alias_suffix(&self, name: &str) -> Result<Option<String>, CloudError> {
        let Some(base) = &self.alias_base else {
            return Ok(None);
        };
        let Some((spelled, suffix)) =
            rule_tail(name, &self.root, &self.source, self.recursive, true)?
        else {
            return Ok(None);
        };
        let alias_path = base.join(spelled);
        if !plain_path(&alias_path)? || !plain_path(&self.destination)? {
            return Err(CloudError::Invalid("unproven Auto-Cloud directory alias"));
        }
        let alias = fs::metadata(&alias_path).map_err(|_| CloudError::Storage)?;
        let rule = fs::metadata(&self.destination).map_err(|_| CloudError::Storage)?;
        if !rule.is_dir() || (alias.dev(), alias.ino()) != (rule.dev(), rule.ino()) {
            return Err(CloudError::Invalid("ambiguous Auto-Cloud directory alias"));
        }
        Ok(Some(suffix))
    }
}

/// The platforms of a rule, by Steam's bits (4 is PS3, not Linux), whose lookup
/// of the name ignores case.
const PLATFORMS: [(&str, u32); 4] = [
    ("Windows", 1),
    ("MacOS", 2),
    ("Linux", 8),
    ("all", u32::MAX),
];

fn platforms(save: &CloudMetadata) -> Result<u32, CloudError> {
    let Some(platforms) = save.field("platforms") else {
        return Ok(u32::MAX);
    };
    let entries = platforms
        .entries()
        .ok_or(CloudError::Invalid("Auto-Cloud platforms"))?;
    let mut mask = 0;
    let mut keys = BTreeSet::new();
    for (key, value) in entries {
        if !keys.insert(key) {
            return Err(CloudError::Invalid("duplicate Auto-Cloud platform"));
        }
        mask |= value
            .text()
            .and_then(|text| {
                PLATFORMS
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(text))
            })
            .map(|&(_, bit)| bit)
            .ok_or(CloudError::Invalid("unsupported Auto-Cloud platform"))?;
    }
    if mask == 0 {
        return Err(CloudError::Invalid("empty Auto-Cloud platforms"));
    }
    Ok(mask)
}

// Publisher paths are root-relative even when serialized with one leading '/'.
// Remote filenames still use the stricter relative() validation below.
fn publisher_path(value: &str, steam_id: u64) -> Result<String, CloudError> {
    let value = substitute(value, steam_id).replace('\\', "/");
    if value.starts_with("//") {
        return Err(CloudError::Invalid("Auto-Cloud network path"));
    }
    relative(value.strip_prefix('/').unwrap_or(&value), true)
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

pub(super) fn relative(value: &str, allow_empty: bool) -> Result<String, CloudError> {
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

// Publishers vary the case of a placeholder, as {Steam3AccountId}.
fn substitute(value: &str, steam_id: u64) -> String {
    let mut value = value.to_owned();
    for (placeholder, id) in [
        ("{64bitsteamid}", steam_id.to_string()),
        ("{steam3accountid}", (steam_id as u32).to_string()),
    ] {
        // ASCII lowercasing keeps byte offsets.
        while let Some(start) = value.to_ascii_lowercase().find(placeholder) {
            value.replace_range(start..start + placeholder.len(), &id);
        }
    }
    value
}

impl AutoCloudPaths {
    /// The paths of a macOS build whose roots are under `home`.
    pub fn mac(
        metadata: &CloudMetadata,
        steam_id: u64,
        home: &Path,
        install: &Path,
    ) -> Result<Self, CloudError> {
        Self::new(
            metadata,
            steam_id,
            &CloudPlatform::Mac { home: home.into() },
            install,
        )
    }

    pub fn new(
        metadata: &CloudMetadata,
        steam_id: u64,
        platform: &CloudPlatform,
        install: &Path,
    ) -> Result<Self, CloudError> {
        let mask = platform.mask();
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
            || !platform.base().is_absolute()
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
        let mut foreign = Vec::new();
        for (_, save) in saves {
            fields(save, &["root", "path", "pattern", "recursive", "platforms"])?;
            let platforms = platforms(save)?;
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
            let recursive = match save.field("recursive").map(CloudMetadata::text) {
                None | Some(Some("0")) => false,
                Some(Some("1")) => true,
                _ => return Err(CloudError::Invalid("Auto-Cloud recursion")),
            };
            if platforms & mask == 0 {
                foreign.push((
                    canonical_root(source_root).to_owned(),
                    publisher_path(source_path, steam_id)?,
                    recursive,
                ));
                continue;
            }
            let mut local_root = source_root;
            let mut local_path = source_path.to_owned();
            let mut matched = false;
            for (_, override_) in overrides {
                if text(override_, "root")? != source_root {
                    continue;
                }
                let applies = match (
                    canonical_os(text(override_, "os")?),
                    text(override_, "oscompare")?,
                ) {
                    (Some(os), "=") => os == platform.os(),
                    _ => return Err(CloudError::Invalid("unsupported Auto-Cloud OS comparison")),
                };
                if !applies {
                    continue;
                }
                fields(
                    override_,
                    &[
                        "root",
                        "os",
                        "oscompare",
                        "useinstead",
                        "pathtransforms",
                        "addpath",
                    ],
                )?;
                if matched {
                    return Err(CloudError::Invalid("ambiguous Auto-Cloud override"));
                }
                matched = true;
                local_root = text(override_, "useinstead")?;
                if let Some(addpath) = override_.field("addpath") {
                    let prefix = publisher_path(
                        addpath
                            .text()
                            .ok_or(CloudError::Invalid("Auto-Cloud added path"))?,
                        steam_id,
                    )?;
                    if !prefix.is_empty() {
                        if override_.field("pathtransforms").is_some() {
                            return Err(CloudError::Invalid("ambiguous Auto-Cloud path transform"));
                        }
                        local_path = format!("{prefix}/{}", publisher_path(&local_path, steam_id)?);
                    }
                }
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
            if !matched && platform.foreign_root(source_root) {
                foreign.push((
                    canonical_root(source_root).to_owned(),
                    publisher_path(source_path, steam_id)?,
                    recursive,
                ));
                continue;
            }
            // Valve serializes some override replacements with a leading slash.
            // It is relative to the new root, never an absolute filesystem path.
            let source = publisher_path(source_path, steam_id)?;
            let local = publisher_path(&local_path, steam_id)?;
            let base = platform.root(local_root, install)?;
            // A server's older names can spell the directory of a rule in
            // another case. Only a Windows build whose rule no override moved
            // reads them as that directory, once the filesystem proves it.
            let alias_base = (!matched && matches!(platform, CloudPlatform::Windows { .. }))
                .then(|| base.clone());
            rules.push(Rule {
                root: canonical_root(source_root).to_owned(),
                source,
                destination: base.join(local),
                alias_base,
                recursive,
                pattern: pattern.to_owned(),
                platforms,
            });
        }
        if rules.is_empty() {
            return Err(if foreign.is_empty() {
                CloudError::Invalid("empty Auto-Cloud rules")
            } else {
                CloudError::OtherPlatforms
            });
        }
        Ok(Self {
            rules,
            foreign,
            mask,
        })
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
    /// published for this platform. A name no rule maps is an error, not a skip.
    pub fn destination(&self, name: &str, platforms: u32) -> Result<Option<PathBuf>, CloudError> {
        if platforms & self.mask == 0 {
            return Ok(None);
        }
        if let Some(path) = self.path_for_name(name)? {
            return Ok(Some(path));
        }
        // Older files may carry an all-platform mask even though the current
        // publisher rule explicitly confines their namespace to another OS.
        if self.is_foreign(name)? {
            return Ok(None);
        }
        Err(CloudError::Invalid("unmapped Auto-Cloud file"))
    }

    /// Whether a rule for another platform spells `name`.
    fn is_foreign(&self, name: &str) -> Result<bool, CloudError> {
        for (root, source, recursive) in &self.foreign {
            if rule_suffix(name, root, source, *recursive)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn path_for_name(&self, name: &str) -> Result<Option<PathBuf>, CloudError> {
        let mut found = BTreeSet::new();
        for rule in &self.rules {
            if let Some(suffix) = rule_suffix(name, &rule.root, &rule.source, rule.recursive)? {
                found.insert(rule.destination.join(suffix));
            }
        }
        // A name that a rule spells exactly, or that another platform's rule
        // does, is never read as a case alias.
        if found.is_empty() && !self.is_foreign(name)? {
            for rule in &self.rules {
                if let Some(suffix) = rule.case_alias_suffix(name)? {
                    found.insert(rule.destination.join(suffix));
                }
            }
        }
        if found.len() > 1 {
            return Err(CloudError::Invalid("ambiguous Auto-Cloud destination"));
        }
        Ok(found.pop_first())
    }

    /// Includes known files even after deletion or a publisher wildcard change.
    /// The caller can therefore distinguish a missing known save from an empty
    /// discovery. Scan only while the supervisor has established game quiescence.
    pub fn local_files(&self, known: &[CloudFile]) -> Result<Vec<LocalCloudFile>, CloudError> {
        let mut selected = BTreeMap::<PathBuf, LocalCloudFile>::new();
        let mut excluded = BTreeSet::new();
        for file in known {
            let path = if file.persist_state != 0 && file.platforms & self.mask != 0 {
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
                // Foreign-platform identities are outside the managed set.
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
                        if !known_paths.contains(&path)
                            && (previous.name != remote || previous.platforms != rule.platforms)
                        {
                            return Err(CloudError::Invalid("ambiguous Auto-Cloud source"));
                        }
                        continue;
                    }
                    selected.insert(
                        path.clone(),
                        LocalCloudFile {
                            name: remote,
                            path,
                            platforms: rule.platforms,
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

pub(super) fn plain_path(path: &Path) -> Result<bool, CloudError> {
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
    // Game-install rules of one Windows build: (path, pattern, platform).
    fn windows_install_rules(rules: &[(&str, &str, &str)], install: &Path) -> AutoCloudPaths {
        let entries = rules
            .iter()
            .enumerate()
            .map(|(index, (path, pattern, platform))| {
                let rule = object(vec![
                    ("path", value(path)),
                    ("pattern", value(pattern)),
                    ("platforms", object(vec![("1", value(platform))])),
                    ("root", value("gameinstall")),
                ]);
                (index.to_string(), rule)
            })
            .collect::<Vec<_>>();
        let metadata = object(vec![("savefiles", CloudMetadata::Object(entries))]);
        let platform = CloudPlatform::Windows {
            profile: install.to_owned(),
        };
        AutoCloudPaths::new(&metadata, 0x011000010000002a, &platform, install).unwrap()
    }
    #[test]
    fn a_case_alias_is_read_as_the_rules_directory_when_the_filesystem_shows_one_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir(root.join("Id1")).unwrap();
        fs::write(root.join("Id1/config.cfg"), b"fixture").unwrap();
        // Only a case-insensitive volume, macOS's default, reaches `Id1` as `id1`.
        if !root.join("id1").is_dir() {
            return;
        }
        let resolver = windows_install_rules(&[("Id1", "config.cfg", "all")], &root);
        let historical = file("%GameInstall%id1/config.cfg");
        assert_eq!(
            resolver.existing_file(&historical).unwrap(),
            Some(root.join("Id1/config.cfg"))
        );
        let selected = resolver
            .local_files(std::slice::from_ref(&historical))
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, historical.name);
        assert!(
            resolver
                .local_files(&[historical, file("%GameInstall%Id1/config.cfg")])
                .is_err()
        );
    }
    #[test]
    fn a_case_alias_of_a_directory_that_is_not_there_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let resolver = windows_install_rules(&[("Id1", "config.cfg", "all")], &root);
        assert!(
            resolver
                .existing_file(&file("%GameInstall%id1/config.cfg"))
                .is_err()
        );
    }
    #[test]
    fn a_case_alias_is_only_read_for_a_windows_rule_no_override_moved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir(root.join("Id1")).unwrap();
        let rules = object(vec![(
            "0",
            object(vec![
                ("path", value("Id1")),
                ("pattern", value("*.sav")),
                ("root", value("gameinstall")),
            ]),
        )]);
        let unmoved = object(vec![("savefiles", rules.clone())]);
        let moved = object(vec![
            ("savefiles", rules),
            (
                "rootoverrides",
                object(vec![(
                    "0",
                    object(vec![
                        ("root", value("gameinstall")),
                        ("os", value("Windows")),
                        ("oscompare", value("=")),
                        ("useinstead", value("gameinstall")),
                    ]),
                )]),
            ),
        ]);
        let windows = CloudPlatform::Windows {
            profile: root.clone(),
        };
        let mac = AutoCloudPaths::mac(&unmoved, 0x011000010000002a, &root, &root).unwrap();
        let overridden = AutoCloudPaths::new(&moved, 0x011000010000002a, &windows, &root).unwrap();
        let historical = file("%GameInstall%id1/a.sav");
        assert!(mac.existing_file(&historical).is_err());
        assert!(overridden.existing_file(&historical).is_err());
    }
    #[test]
    fn rules_that_differ_only_by_case_keep_the_names_each_spells() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir(root.join("Id1")).unwrap();
        let resolver = windows_install_rules(
            &[("Id1", "*.sav", "Windows"), ("id1", "*.cfg", "Windows")],
            &root,
        );
        assert_eq!(
            resolver
                .existing_file(&file("%GameInstall%Id1/a.sav"))
                .unwrap(),
            Some(root.join("Id1/a.sav"))
        );
        assert_eq!(
            resolver
                .existing_file(&file("%GameInstall%id1/config.cfg"))
                .unwrap(),
            Some(root.join("id1/config.cfg"))
        );
    }
    #[test]
    fn a_name_in_another_platforms_namespace_is_not_read_as_a_case_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir(root.join("Saves")).unwrap();
        let resolver = windows_install_rules(
            &[("Saves", "*.sav", "Windows"), ("saves", "*.sav", "Linux")],
            &root,
        );
        assert_eq!(
            resolver
                .existing_file(&file("%GameInstall%saves/a.sav"))
                .unwrap(),
            None
        );
    }
    #[test]
    fn quake_all_platform_rules_discover_windows_files() {
        // Public app 2310 UFS metadata, using synthetic paths and files only.
        let metadata = object(vec![(
            "savefiles",
            object(vec![
                (
                    "1",
                    object(vec![
                        ("path", value("Id1")),
                        ("pattern", value("*.sav")),
                        ("platforms", object(vec![("1", value("all"))])),
                        ("root", value("gameinstall")),
                    ]),
                ),
                (
                    "2",
                    object(vec![
                        ("path", value("Id1")),
                        ("pattern", value("config.cfg")),
                        ("platforms", object(vec![("1", value("all"))])),
                        ("root", value("gameinstall")),
                    ]),
                ),
                (
                    "3",
                    object(vec![
                        ("path", value("Nightdive Studios/Quake")),
                        ("pattern", value("*.sav")),
                        ("recursive", value("1")),
                        ("root", value("WinSavedGames")),
                    ]),
                ),
            ]),
        )]);
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let install = base.join("game");
        let profile = base.join("profile");
        fs::create_dir_all(install.join("Id1")).unwrap();
        fs::create_dir_all(profile.join("Saved Games/Nightdive Studios/Quake/nested")).unwrap();
        fs::write(install.join("Id1/config.cfg"), b"synthetic").unwrap();
        fs::write(install.join("Id1/test.sav"), b"synthetic").unwrap();
        fs::write(
            profile.join("Saved Games/Nightdive Studios/Quake/nested/test.sav"),
            b"synthetic",
        )
        .unwrap();
        let resolver = AutoCloudPaths::new(
            &metadata,
            0x011000010000002a,
            &CloudPlatform::Windows { profile },
            &install,
        )
        .unwrap();
        let files = resolver.local_files(&[]).unwrap();
        let mut names = files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            [
                "%GameInstall%Id1/config.cfg",
                "%GameInstall%Id1/test.sav",
                "%WinSavedGames%Nightdive Studios/Quake/nested/test.sav",
            ]
        );
        assert!(files.iter().all(|f| f.platforms == u32::MAX));
    }
    #[test]
    fn authenticated_publisher_fixtures_map_mac_paths() {
        for (json, remote, local) in [
            (
                include_str!("fixtures/1145360.json"),
                "%WinMyDocuments%Saved Games/Hades/Profile1.sav",
                "Library/Application Support/Supergiant Games/Hades/Profile1.sav",
            ),
            (
                include_str!("fixtures/774171.json"),
                "%MacHome%Steam/MuseDash/slot.sav",
                "Steam/MuseDash/slot.sav",
            ),
            (
                include_str!("fixtures/1809540.json"),
                "%WinAppDataLocalLow%RedCandleGames/NineSols/slot.dat",
                "Library/Application Support/RedCandleGames/NineSols/slot.dat",
            ),
            (
                include_str!("fixtures/1062160.json"),
                "%WinAppDataLocalLow%Dry Cactus/Poly Bridge 2/SaveSlots/slot.dat",
                "Library/Application Support/Dry Cactus/Poly Bridge 2/SaveSlots/slot.dat",
            ),
            (
                include_str!("fixtures/225260.json"),
                "%MacAppSupport%Doublefine/BrutalLegend/76561197960265770/slot.sav",
                "Library/Application Support/Doublefine/BrutalLegend/76561197960265770/slot.sav",
            ),
            (
                include_str!("fixtures/2239150.json"),
                "%WinAppDataLocalLow%Grizzly Games/Thronefall/slot.sav",
                "Library/Application Support/Grizzly Games/Thronefall/slot.sav",
            ),
        ] {
            let metadata = serde_json::from_str(json).unwrap();
            let home = Path::new("/Users/fixture");
            let resolver = AutoCloudPaths::mac(
                &metadata,
                0x011000010000002a,
                home,
                Path::new("/Games/fixture"),
            )
            .unwrap();
            assert_eq!(
                resolver.destination(remote, 2).unwrap(),
                Some(home.join(local))
            );
        }
    }

    #[test]
    fn authenticated_publisher_fixtures_map_windows_paths() {
        for (json, remote, local) in [
            (
                include_str!("fixtures/239030.json"),
                "%WinAppDataRoaming%3909/PapersPlease/slot.sav",
                "AppData/Roaming/3909/PapersPlease/slot.sav",
            ),
            (
                include_str!("fixtures/1888930.json"),
                "%WinSavedGames%The Last of Us Part I/users/42/savedata/slot",
                "Saved Games/The Last of Us Part I/users/42/savedata/slot",
            ),
        ] {
            let metadata = serde_json::from_str(json).unwrap();
            let profile = Path::new("/Prefix/drive_c/users/crossover");
            let resolver = AutoCloudPaths::new(
                &metadata,
                0x011000010000002a,
                &CloudPlatform::Windows {
                    profile: profile.into(),
                },
                Path::new("/Games/fixture"),
            )
            .unwrap();
            assert_eq!(
                resolver.destination(remote, 1).unwrap(),
                Some(profile.join(local))
            );
        }
    }

    #[test]
    fn lowercase_gameinstall_uses_canonical_remote_root_and_addpath() {
        for (json, remote, destination) in [
            (
                include_str!("fixtures/504230.json"),
                "%GameInstall%Saves/0.celeste",
                "/Users/fixture/Library/Application Support/Celeste/Saves/0.celeste",
            ),
            (
                include_str!("fixtures/1585220.json"),
                "%GameInstall%save.json",
                "/Games/fixture/save.json",
            ),
            (
                include_str!("fixtures/200710.json"),
                "%WinMyDocuments%my games/runic games/torchlight 2/save/76561197960265770/slot.svb",
                "/Games/fixture/my games/runic games/torchlight 2/save/76561197960265770/slot.svb",
            ),
        ] {
            let metadata = serde_json::from_str(json).unwrap();
            let resolver = AutoCloudPaths::mac(
                &metadata,
                0x011000010000002a,
                Path::new("/Users/fixture"),
                Path::new("/Games/fixture"),
            )
            .unwrap();
            assert_eq!(
                resolver.destination(remote, 2).unwrap(),
                Some(PathBuf::from(destination))
            );
        }
    }

    #[test]
    fn a_windows_build_finds_its_saves_in_its_prefix_and_ignores_mac_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("drive_c/users/crossover");
        let saves = profile.join("AppData/LocalLow/Studio/Game/42/nested");
        fs::create_dir_all(&saves).unwrap();
        fs::write(saves.join("slot.sav"), b"windows").unwrap();
        let platform = CloudPlatform::Windows {
            profile: profile.clone(),
        };
        let resolver =
            AutoCloudPaths::new(&metadata(), 0x011000010000002a, &platform, temp.path()).unwrap();

        assert_eq!(
            resolver
                .destination("%WinAppDataLocalLow%Studio/Game/42/slot.sav", u32::MAX)
                .unwrap(),
            Some(profile.join("AppData/LocalLow/Studio/Game/42/slot.sav"))
        );
        let selected = resolver.local_files(&[]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].name,
            "%WinAppDataLocalLow%Studio/Game/42/nested/slot.sav"
        );
        assert_eq!(selected[0].path, saves.join("slot.sav"));
    }

    #[test]
    fn rules_for_other_platforms_leave_a_build_without_cloud_saves() {
        // KIBORG: an all-platform Windows root and a Linux-only rule.
        let metadata = serde_json::from_str(include_str!("fixtures/2405060.json")).unwrap();
        let install = Path::new("/Games/fixture");
        assert!(matches!(
            AutoCloudPaths::mac(
                &metadata,
                0x011000010000002a,
                Path::new("/Users/fixture"),
                install
            ),
            Err(CloudError::OtherPlatforms)
        ));

        let profile = PathBuf::from("/prefix/drive_c/users/crossover");
        let platform = CloudPlatform::Windows {
            profile: profile.clone(),
        };
        let resolver =
            AutoCloudPaths::new(&metadata, 0x011000010000002a, &platform, install).unwrap();
        assert_eq!(
            resolver
                .destination("%WinAppDataLocal%KIBORG/Saved/SaveGames/slot.sav", u32::MAX)
                .unwrap(),
            Some(profile.join("AppData/Local/KIBORG/Saved/SaveGames/slot.sav"))
        );
        assert_eq!(
            resolver
                .destination(
                    "%LinuxXdgConfigHome%Epic/KIBORG/Saved/SaveGames/slot.sav",
                    4
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn a_windows_build_keeps_only_windows_files_of_platform_specific_rules() {
        let metadata = serde_json::from_str(include_str!("fixtures/774171.json")).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let profile = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("drive_c/users/crossover");
        let platform = CloudPlatform::Windows {
            profile: profile.clone(),
        };
        let resolver =
            AutoCloudPaths::new(&metadata, 0x011000010000002a, &platform, temp.path()).unwrap();

        let mut windows = file("%WinAppDataLocal%Steam/MuseDash/windows.sav");
        windows.platforms = 1;
        assert_eq!(
            resolver.existing_file(&windows).unwrap(),
            Some(profile.join("AppData/Local/Steam/MuseDash/windows.sav"))
        );
        let mut mac = file("%MacHome%Steam/MuseDash/slot.sav");
        mac.platforms = 2;
        assert_eq!(resolver.existing_file(&mac).unwrap(), None);
    }

    #[test]
    fn platform_specific_rules_discover_only_mac_files_with_mac_mask() {
        let metadata = serde_json::from_str(include_str!("fixtures/774171.json")).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        fs::create_dir_all(home.join("Steam/MuseDash/nested")).unwrap();
        fs::write(home.join("Steam/MuseDash/slot.sav"), b"mac").unwrap();
        fs::write(home.join("Steam/MuseDash/nested/slot.sav"), b"ignored").unwrap();
        let resolver = AutoCloudPaths::mac(&metadata, 0x011000010000002a, &home, &home).unwrap();
        let selected = resolver.local_files(&[]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].platforms, 2);
        assert_eq!(selected[0].name, "%MacHome%Steam/MuseDash/slot.sav");
        let foreign = file("%WinAppDataLocal%Steam/MuseDash/windows.sav");
        assert_eq!(resolver.existing_file(&foreign).unwrap(), None);
        assert_eq!(resolver.local_files(&[foreign]).unwrap(), selected);
        assert!(resolver.destination("%Unknown%slot.sav", u32::MAX).is_err());
    }

    #[test]
    fn publisher_path_normalization_never_allows_traversal_or_network_paths() {
        for bad in [
            "/../escape",
            "\\\\server\\share",
            "//server/share",
            "C:\\save",
            "/{unknown}/save",
            "a/../../save",
        ] {
            assert!(publisher_path(bad, 0x011000010000002a).is_err(), "{bad}");
        }
        assert_eq!(
            publisher_path("/saves/", 0x011000010000002a).unwrap(),
            "saves"
        );
        assert!(relative("/saves/file", false).is_err());
    }

    #[test]
    fn invalid_platform_lists_and_foreign_only_rules_never_become_empty_saves() {
        for list in [
            value("MacOS"),
            object(vec![]),
            object(vec![("1", value("Unknown"))]),
            object(vec![("1", value("MacOS")), ("1", value("Windows"))]),
            object(vec![("1", value("Windows"))]),
        ] {
            let mut metadata = metadata();
            let CloudMetadata::Object(top) = &mut metadata else {
                unreachable!()
            };
            let CloudMetadata::Object(saves) = &mut top[0].1 else {
                unreachable!()
            };
            let CloudMetadata::Object(rule) = &mut saves[0].1 else {
                unreachable!()
            };
            rule.push(("platforms".into(), list));
            assert!(
                AutoCloudPaths::mac(
                    &metadata,
                    0x011000010000002a,
                    Path::new("/Users/fixture"),
                    Path::new("/Games/fixture")
                )
                .is_err()
            );
        }
    }
    #[test]
    fn platforms_are_steams_bits_whatever_the_case_of_their_names() {
        let mask = |names: &[&str]| {
            let list = CloudMetadata::Object(
                names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| (index.to_string(), value(name)))
                    .collect(),
            );
            platforms(&object(vec![("platforms", list)]))
        };
        assert_eq!(mask(&["Windows"]).unwrap(), 1);
        assert_eq!(mask(&["MacOS"]).unwrap(), 2);
        // 4 is PS3, which a rule's Linux must not claim.
        assert_eq!(mask(&["Linux"]).unwrap(), 8);
        assert_eq!(mask(&["Windows", "Linux"]).unwrap(), 9);
        assert_eq!(mask(&["windows", "MACOS", "linux"]).unwrap(), 11);
        assert_eq!(mask(&["all"]).unwrap(), u32::MAX);
        assert_eq!(mask(&["All"]).unwrap(), u32::MAX);
        assert_eq!(platforms(&object(vec![])).unwrap(), u32::MAX);
        assert!(mask(&["PS3"]).is_err());
    }
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
        let resolver = AutoCloudPaths::mac(&metadata(), 0x011000010000002a, &home, &home).unwrap();
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
        let resolver = AutoCloudPaths::mac(&metadata(), 0x011000010000002a, &home, &home).unwrap();
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
        let resolver = AutoCloudPaths::mac(&metadata(), 0x011000010000002a, &home, &home).unwrap();
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
        let resolver = AutoCloudPaths::mac(&metadata(), 0x011000010000002a, &home, &home).unwrap();
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
        let resolver = AutoCloudPaths::mac(&metadata(), 0x011000010000002a, &home, &home).unwrap();
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
        let resolver = AutoCloudPaths::mac(
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
            AutoCloudPaths::mac(
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
            let resolver = AutoCloudPaths::mac(
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
