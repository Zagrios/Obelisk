//! EGS launcher Cloud Saves rules. EOS Player Data Storage remains game-owned.
//!
//! Protocol references are recorded in docs/research-epic-cloud-saves.md and
//! docs/research-gamenative-epic-cloud.md. No third-party implementation is used.
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::EpicError;

pub(crate) mod codec;

/// Explicit in-memory safety limits, independent of the publisher's quota.
pub const MAX_FILES: usize = 10_000;
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

/// Catalog identity supplied by the caller and checked against the owned library
/// before a Cloud transaction. The account always comes from the current session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudGame<'a> {
    pub namespace: &'a str,
    pub catalog_item_id: &'a str,
    pub app_name: &'a str,
}

impl CloudGame<'_> {
    pub(crate) fn validate(self) -> Result<(), EpicError> {
        identity(self.namespace)?;
        identity(self.catalog_item_id)?;
        identity(self.app_name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudRules {
    /// The Windows build's save folder.
    pub cloud_save_folder: Option<String>,
    /// The Mac build's save folder.
    pub cloud_save_folder_mac: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// The build of a game that runs, and where its save folder's variables point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudPlatform {
    /// A Mac build: its saves are under the player's home.
    Mac { home: PathBuf },
    /// A Windows build running in Wine: its saves are under the user profile
    /// of its prefix.
    Windows { profile: PathBuf },
}

impl CloudPlatform {
    /// The catalog attribute naming this build's save folder.
    fn attribute(platform: &str) -> Option<&'static str> {
        match platform {
            "Mac" => Some("CloudSaveFolder_MAC"),
            "Windows" => Some("CloudSaveFolder"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedCloudRules {
    pub directory: PathBuf,
    include: Vec<String>,
    exclude: Vec<String>,
    anchor: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Fingerprint of the complete manifest inventory. Not an atomic CAS token.
    pub revision: String,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl CloudRules {
    /// The rules of the `platform` build ("Mac" or "Windows"), or `None` when
    /// the catalog names no save folder for it.
    pub(crate) fn from_attributes(
        attributes: &BTreeMap<String, serde_json::Value>,
        platform: &str,
    ) -> Result<Option<Self>, EpicError> {
        let attr = |key: &str| -> Result<Option<String>, EpicError> {
            attributes
                .get(key)
                .map(|value| {
                    value
                        .get("value")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| value.len() <= 16_384 && !value.contains('\0'))
                        .map(str::to_owned)
                        .ok_or(EpicError::Protocol)
                })
                .transpose()
        };
        let key = CloudPlatform::attribute(platform).ok_or(EpicError::UnsupportedContent)?;
        let Some(folder) = attr(key)? else {
            return Ok(None);
        };
        if folder.trim().is_empty() {
            return Err(EpicError::UnsupportedContent);
        }
        let rules = Self {
            cloud_save_folder: attr("CloudSaveFolder")?,
            cloud_save_folder_mac: attr("CloudSaveFolder_MAC")?,
            include: parse_filters(attr("CloudIncludeList")?)?,
            exclude: parse_filters(attr("CloudExcludeList")?)?,
        };
        rules.validate()?;
        Ok(Some(rules))
    }

    pub(crate) fn validate(&self) -> Result<(), EpicError> {
        let folders = [&self.cloud_save_folder, &self.cloud_save_folder_mac];
        if folders.iter().all(|folder| folder.is_none())
            || folders
                .iter()
                .filter_map(|folder| folder.as_ref())
                .any(|v| v.is_empty() || v.len() > 16_384 || v.contains('\0'))
        {
            return Err(EpicError::UnsupportedContent);
        }
        for filters in [&self.include, &self.exclude] {
            if filters.len() > 256 {
                return Err(EpicError::UnsupportedContent);
            }
            for filter in filters {
                validate_filter(filter)?;
            }
        }
        Ok(())
    }

    /// Resolves the save folder of `platform`'s build. On a Mac, AppData is
    /// deliberately unsupported: the launcher and Legendary disagree about its
    /// native Mac base directory. On Windows, variables follow the launcher's:
    /// AppData is the profile's AppData/Local, which paths such as
    /// `{AppData}/../LocalLow` leave within the profile.
    /// The caller must handle UnsupportedContent, never treat it as an empty tree.
    pub fn resolve(
        &self,
        platform: &CloudPlatform,
        install: &Path,
        account_id: &str,
        app_name: &str,
    ) -> Result<ResolvedCloudRules, EpicError> {
        self.validate()?;
        identity(account_id)?;
        identity(app_name)?;
        let install = canonical_or_missing(install)?;
        let (base, source, protected) = match platform {
            CloudPlatform::Mac { home } => {
                let home = home.canonicalize()?;
                if !home.is_dir() {
                    return Err(EpicError::UnsupportedContent);
                }
                let source = self
                    .cloud_save_folder_mac
                    .as_ref()
                    .ok_or(EpicError::UnsupportedContent)?
                    .replace('\\', "/");
                let source = source
                    .strip_prefix("~/")
                    .map(|rest| format!("{{Home}}/{rest}"))
                    .unwrap_or(source);
                let protected: &[&str] = &["Library", "Documents", "Library/Application Support"];
                (home, source, protected)
            }
            // A prefix that no wineboot created yet still resolves.
            CloudPlatform::Windows { profile } => {
                let source = self
                    .cloud_save_folder
                    .as_ref()
                    .ok_or(EpicError::UnsupportedContent)?
                    .replace('\\', "/");
                let protected: &[&str] = &[
                    "Documents",
                    "AppData",
                    "AppData/Local",
                    "AppData/LocalLow",
                    "AppData/Roaming",
                    "Saved Games",
                ];
                (canonical_or_missing(profile)?, source, protected)
            }
        };
        let lowercase = source.to_ascii_lowercase();
        let personal: &[&str] = match platform {
            CloudPlatform::Mac { .. } => {
                &["{home}/", "{userprofile}/", "{userdir}/", "{userlibrary}/"]
            }
            CloudPlatform::Windows { .. } => &[
                "{userprofile}/",
                "{userdir}/",
                "{appdata}/",
                "{usersavedgames}/",
            ],
        };
        let anchor = if lowercase.starts_with("{installdir}/") {
            &install
        } else if personal.iter().any(|prefix| lowercase.starts_with(prefix)) {
            &base
        } else {
            return Err(EpicError::UnsupportedContent);
        };
        let mut expanded = String::new();
        let mut rest = source.as_str();
        while let Some(index) = rest.find('{') {
            expanded.push_str(&rest[..index]);
            let end = rest[index..]
                .find('}')
                .ok_or(EpicError::UnsupportedContent)?
                + index;
            let folder = |relative: &str| base.join(relative).to_string_lossy().into_owned();
            let replacement = match (platform, rest[index + 1..end].to_ascii_lowercase().as_str()) {
                (CloudPlatform::Mac { .. }, "home") | (_, "userprofile") => folder(""),
                (CloudPlatform::Mac { .. }, "userlibrary") => folder("Library"),
                (_, "userdir") => folder("Documents"),
                (CloudPlatform::Windows { .. }, "appdata") => folder("AppData/Local"),
                (CloudPlatform::Windows { .. }, "usersavedgames") => folder("Saved Games"),
                (_, "installdir") => install.to_string_lossy().into_owned(),
                (_, "epicid") => account_id.to_owned(),
                (_, "appname") => app_name.to_owned(),
                _ => return Err(EpicError::UnsupportedContent),
            };
            let replacement = replacement.trim_end_matches('/').to_owned();
            expanded.push_str(&replacement);
            rest = &rest[end + 1..];
        }
        expanded.push_str(rest);
        if expanded.contains(['{', '}']) {
            return Err(EpicError::UnsupportedContent);
        }
        let mut directory = PathBuf::new();
        for component in Path::new(&expanded).components() {
            match component {
                Component::RootDir => directory.push("/"),
                Component::Normal(name) => directory.push(name),
                Component::CurDir => (),
                Component::ParentDir if directory.pop() => (),
                _ => return Err(EpicError::UnsafePath(expanded)),
            }
        }
        if !directory.starts_with(anchor)
            || directory == *anchor
            || protected
                .iter()
                .any(|folder| directory == base.join(folder))
        {
            return Err(EpicError::UnsafePath(directory.display().to_string()));
        }
        let resolved = ResolvedCloudRules {
            directory,
            anchor: anchor.clone(),
            include: self.include.clone(),
            exclude: self.exclude.clone(),
        };
        resolved.check_directory()?;
        Ok(resolved)
    }

    pub fn accepts(&self, relative: &str) -> Result<bool, EpicError> {
        self.filters()?.accepts(relative)
    }

    /// Validate the filter lists once, for callers that then match many names.
    /// `accepts` revalidates on every call, which is O(names x filters).
    pub fn filters(&self) -> Result<CloudFilters<'_>, EpicError> {
        self.validate()?;
        Ok(CloudFilters {
            include: &self.include,
            exclude: &self.exclude,
        })
    }
}

/// Validated include/exclude lists borrowed from a `CloudRules`.
pub struct CloudFilters<'a> {
    include: &'a [String],
    exclude: &'a [String],
}

impl CloudFilters<'_> {
    pub fn accepts(&self, relative: &str) -> Result<bool, EpicError> {
        accepts(relative, self.include, self.exclude)
    }
}

impl ResolvedCloudRules {
    pub fn accepts(&self, relative: &str) -> Result<bool, EpicError> {
        accepts(relative, &self.include, &self.exclude)
    }

    /// Revalidates every existing parent. Does not create directories or files.
    pub fn check_directory(&self) -> Result<(), EpicError> {
        if canonical_or_missing(&self.directory)? != self.directory {
            return Err(EpicError::UnsafePath(self.directory.display().to_string()));
        }
        let relative = self
            .directory
            .strip_prefix(&self.anchor)
            .map_err(|_| EpicError::UnsafePath(self.directory.display().to_string()))?;
        let mut current = self.anchor.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err(EpicError::UnsafePath(self.directory.display().to_string()));
            };
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
                _ => return Err(EpicError::UnsafePath(current.display().to_string())),
            }
        }
        Ok(())
    }
}

// An uninstall can remove the installation while its Cloud recovery records
// remain valid. Canonicalize only the existing parent, preserving a checked,
// missing suffix rather than turning this condition into an empty save tree.
fn canonical_or_missing(path: &Path) -> Result<PathBuf, EpicError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(EpicError::UnsafePath(path.display().to_string()));
    }
    let mut current = path;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let mut resolved = current.canonicalize()?;
                for name in missing.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.file_name().ok_or(EpicError::UnsupportedContent)?);
                current = current.parent().ok_or(EpicError::UnsupportedContent)?;
            }
            Err(error) => return Err(error.into()),
            _ => return Err(EpicError::UnsafePath(path.display().to_string())),
        }
    }
}

pub(crate) fn identity(value: &str) -> Result<(), EpicError> {
    if value.is_empty()
        || value.len() > 256
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(EpicError::Protocol);
    }
    Ok(())
}

pub(crate) fn relative_path(value: &str) -> Result<(), EpicError> {
    if value.is_empty()
        || value.len() > 4096
        || value.contains(['\\', ':', '\0'])
        || value.chars().any(char::is_control)
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | "..") || part.ends_with([' ', '.']))
    {
        return Err(EpicError::UnsafePath(value.into()));
    }
    Ok(())
}

/// Comparison only: preserve publisher filenames on the wire. Normalization
/// and case expansion conservatively reject paths that may alias on macOS.
pub(crate) fn path_identity(value: &str) -> String {
    value
        .nfc()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
        .nfc()
        .collect()
}

fn parse_filters(value: Option<String>) -> Result<Vec<String>, EpicError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_empty() {
        return Ok(Vec::new());
    }
    let patterns: Vec<_> = value
        .split(',')
        .map(|part| part.trim().replace('\\', "/"))
        .collect();
    for pattern in &patterns {
        validate_filter(pattern)?;
    }
    Ok(patterns)
}

fn validate_filter(pattern: &str) -> Result<(), EpicError> {
    if pattern.is_empty()
        || pattern.len() > 1024
        || pattern.contains(['[', ']', '{', '}', ':', '\\', '\0'])
    {
        return Err(EpicError::UnsupportedContent);
    }
    relative_path(pattern.strip_suffix('/').unwrap_or(pattern))
}

fn accepts(relative: &str, include: &[String], exclude: &[String]) -> Result<bool, EpicError> {
    relative_path(relative)?;
    let matches = |pattern: &String| {
        (pattern.ends_with('/') && relative.starts_with(pattern))
            || relative.ends_with(pattern)
            || wildcard(pattern, relative)
    };
    Ok((include.is_empty() || include.iter().any(matches)) && !exclude.iter().any(matches))
}

// Bounded, linear-space glob matching. '*' includes directory separators, as in
// the EGS reference's fnmatch behavior; publisher exclusion wins over inclusion.
fn wildcard(pattern: &str, text: &str) -> bool {
    let text: Vec<_> = text.chars().collect();
    let mut states = vec![false; text.len() + 1];
    let mut next = vec![false; text.len() + 1];
    states[0] = true;
    for ch in pattern.chars() {
        // Every slot is written before it is read again, so the buffer carries
        // nothing over from the previous character.
        for index in 0..states.len() {
            next[index] = if ch == '*' {
                states[index] || index > 0 && next[index - 1]
            } else {
                index > 0 && states[index - 1] && (ch == '?' || ch == text[index - 1])
            };
        }
        std::mem::swap(&mut states, &mut next);
    }
    states[text.len()]
}

#[cfg(test)]
mod tests;
