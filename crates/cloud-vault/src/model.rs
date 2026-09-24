use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub steam_id: u64,
    pub app_id: u32,
    pub cloud_app_id: u32,
}

impl Scope {
    pub fn validate(self) -> Result<()> {
        // Public-universe individual desktop account with a nonzero AccountID.
        if self.steam_id >> 32 != 0x0110_0001
            || self.steam_id as u32 == 0
            || self.app_id == 0
            || self.cloud_app_id == 0
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

/// Epic's account and game identity remains distinct from Steam's numeric IDs.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpicScope {
    pub account_id: String,
    pub namespace: String,
    pub app_name: String,
}

/// The directory under a vault root holding every Epic account.
const EPIC_STORE_DIRECTORY: &str = "epic";

/// Epic account IDs are 32 lowercase hex digits. Exposed on its own so callers
/// that know only an account never have to invent a scope to reach the rule.
pub fn epic_account_id(value: &str) -> Result<()> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

/// Where one Epic account's game journals live. The layout has a single owner;
/// callers derive it from here rather than re-spelling it.
pub fn epic_account_directory(root: &Path, account: &str) -> Result<PathBuf> {
    epic_account_id(account)?;
    Ok(root.join(EPIC_STORE_DIRECTORY).join(account))
}

impl EpicScope {
    pub fn validate(&self) -> Result<()> {
        epic_account_id(&self.account_id)?;
        if [&self.namespace, &self.app_name].into_iter().any(|value| {
            value.is_empty()
                || value.len() > 256
                || !value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
                || matches!(value.as_str(), "." | "..")
        }) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

/// Untagged Steam serialization preserves existing schema-2 journals exactly.
/// Epic uses disjoint named fields, including its unmodified external identity.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StoreScope {
    Steam(Scope),
    Epic(EpicScope),
}

impl From<Scope> for StoreScope {
    fn from(scope: Scope) -> Self {
        Self::Steam(scope)
    }
}

impl From<EpicScope> for StoreScope {
    fn from(scope: EpicScope) -> Self {
        Self::Epic(scope)
    }
}

impl StoreScope {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Steam(scope) => scope.validate(),
            Self::Epic(scope) => scope.validate(),
        }
    }

    /// Location for adapter journals beside the vault's state and blobs.
    /// This only derives a path; opening a Vault validates and locks its storage.
    pub fn directory(&self, root: &Path) -> Result<PathBuf> {
        self.validate()?;
        if !root.is_absolute() {
            return Err(Error::Invalid);
        }
        let account = self.account_directory(root);
        Ok(match self {
            Self::Steam(scope) => account.join(scope.cloud_app_id.to_string()),
            Self::Epic(scope) => {
                // Hash only the filesystem component, never the game identity.
                // Length-delimited JSON preserves tuple boundaries and case.
                let key = serde_json::to_vec(&(&scope.namespace, &scope.app_name))?;
                account.join(ContentHash::of(&key).as_str())
            }
        })
    }

    /// Every directory from `root` down to this scope's account directory, in
    /// creation order. Epic namespaces its accounts one level below Steam's.
    pub(crate) fn account_directories(&self, root: &Path) -> Vec<PathBuf> {
        match self {
            Self::Steam(_) => vec![self.account_directory(root)],
            Self::Epic(_) => vec![
                root.join(EPIC_STORE_DIRECTORY),
                self.account_directory(root),
            ],
        }
    }

    /// Where this storefront's archive lock lives.
    pub(crate) fn store_directory(&self, root: &Path) -> PathBuf {
        match self {
            Self::Steam(_) => root.to_path_buf(),
            Self::Epic(_) => root.join(EPIC_STORE_DIRECTORY),
        }
    }

    pub(crate) fn same_storefront(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Steam(_), Self::Steam(_)) | (Self::Epic(_), Self::Epic(_))
        )
    }

    pub(crate) fn account_directory(&self, root: &Path) -> PathBuf {
        match self {
            Self::Steam(scope) => root.join(scope.steam_id.to_string()),
            Self::Epic(scope) => root.join(EPIC_STORE_DIRECTORY).join(&scope.account_id),
        }
    }

    pub(crate) fn account(&self) -> AccountIdentity {
        match self {
            Self::Steam(scope) => AccountIdentity::Steam(scope.steam_id),
            Self::Epic(scope) => AccountIdentity::Epic(scope.account_id.clone()),
        }
    }

    pub(crate) fn store_key(&self) -> Self {
        match self {
            Self::Steam(scope) => Self::Steam(Scope {
                app_id: scope.cloud_app_id,
                ..*scope
            }),
            Self::Epic(_) => self.clone(),
        }
    }
}

/// The adapter must supply the identity of its actual authenticated account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccountIdentity {
    Steam(u64),
    Epic(String),
}

impl From<u64> for AccountIdentity {
    fn from(account: u64) -> Self {
        Self::Steam(account)
    }
}

impl From<&str> for AccountIdentity {
    fn from(account: &str) -> Self {
        Self::Epic(account.into())
    }
}

impl AccountIdentity {
    // A private permit fingerprint keeps Authorization Copy. It is never an
    // external account identifier or a persisted store identity.
    pub(crate) fn permit_key(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        match self {
            Self::Steam(account) => {
                hash.update(b"steam\0");
                hash.update(account.to_le_bytes());
            }
            Self::Epic(account) => {
                hash.update(b"epic\0");
                hash.update(account.as_bytes());
            }
        }
        hash.finalize().into()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash(String);

impl ContentHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ContentHash {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
}

impl From<ContentHash> for String {
    fn from(hash: ContentHash) -> Self {
        hash.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersion {
    pub hash: ContentHash,
    pub size: u64,
    pub platforms: u32,
}

/// A complete set. Absence means deletion. A CM delta must never be passed here
/// without first applying it to its known complete base. Names are remote keys,
/// not filesystem paths; only a separately validated resolver may map them.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Manifest(pub BTreeMap<String, FileVersion>);

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        Self::validate_names(self.0.keys().map(String::as_str))
    }

    /// The remote-name rules shared by manifests and the bindings that map them.
    pub fn validate_names<'a>(names: impl ExactSizeIterator<Item = &'a str>) -> Result<()> {
        if names.len() > 100_000 {
            return Err(Error::Invalid);
        }
        for name in names {
            if name.is_empty() || name.len() > 4096 || name.chars().any(char::is_control) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }

    pub fn id(&self) -> ContentHash {
        // BTreeMap ordering and fixed field order make the persisted identity
        // independent of enumeration order and timestamps.
        ContentHash::of(&serde_json::to_vec(self).expect("manifest serialization"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteView {
    /// Opaque adapter cursor. Content is also compared when fencing decisions.
    pub revision: String,
    pub files: Manifest,
}

impl RemoteView {
    pub fn validate(&self) -> Result<()> {
        if self.revision.is_empty() || self.revision.len() > 1024 {
            return Err(Error::Invalid);
        }
        self.files.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalIntent {
    /// Remote view observed when the user chose local during a conflict. This
    /// does not claim that those local bytes have ever reached Steam.
    pub against: RemoteView,
}

/// Unknown preferences never grant permission to transfer. An authenticated
/// adapter refreshes both settings when it opens a coordinator.
#[derive(Clone, Copy, Debug, Default)]
pub struct Preferences {
    pub account_enabled: Option<bool>,
    pub game_enabled: Option<bool>,
}

impl Preferences {
    pub fn enabled(self) -> bool {
        self.account_enabled == Some(true) && self.game_enabled == Some(true)
    }
}
