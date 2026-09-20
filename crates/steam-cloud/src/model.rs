use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
