//! Lossless PICS Cloud metadata. Unknown roots, overrides and fields remain
//! available to the Cloud rule resolver; missing metadata is not an empty rule
//! set and malformed shapes are not silently converted into one.
use crate::kv::Kv;
pub(crate) mod downloads;
mod paths;
mod sdk;
pub use sdk::SdkCloudPaths;
pub(crate) mod preferences;
mod read;
pub(crate) mod upload;
pub(crate) mod wire;
pub use paths::{AutoCloudPaths, CloudPlatform, LocalCloudFile};
pub use preferences::{CloudPreferences, CloudPreferencesCache};
pub use read::*;
pub(crate) use read::{download, listing};
pub use upload::{CloudUpload, UploadPhase, UploadTransaction};

/// Run one Cloud operation under a hard deadline. Every request already has its
/// own transport timeout; this bounds the operation as a whole.
pub(crate) async fn deadline<T>(
    seconds: u64,
    operation: impl Future<Output = Result<T, CloudError>>,
) -> Result<T, CloudError> {
    tokio::time::timeout(std::time::Duration::from_secs(seconds), operation)
        .await
        .map_err(|_| CloudError::Timeout)?
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CloudMetadata {
    Value(String),
    Object(Vec<(String, CloudMetadata)>),
}

impl CloudMetadata {
    pub fn entries(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(entries) => Some(entries),
            Self::Value(_) => None,
        }
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Value(value) => Some(value),
            Self::Object(_) => None,
        }
    }

    /// Duplicate names are ambiguous. Callers may inspect entries for an
    /// unsupported-state diagnostic, but cannot silently choose the first.
    pub fn field(&self, name: &str) -> Option<&Self> {
        let mut matches = self
            .entries()?
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name));
        let (_, value) = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(value)
        }
    }
}

impl From<&Kv> for CloudMetadata {
    fn from(value: &Kv) -> Self {
        match value {
            Kv::Value(text) => Self::Value(text.clone()),
            Kv::Object(entries) => Self::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::from(value)))
                    .collect(),
            ),
        }
    }
}
