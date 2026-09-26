//! Epic game ownership and EGS Cloud orchestration. The UI only observes and
//! answers revision-bound choices; a detached process owns the entire session.
mod client;
pub(crate) mod launch;
mod session;
mod windows;

// A Game session's phase is `Phase`; its Cloud sync's phase is `CloudPhase`.
use crate::cloud::{Control, Phase as CloudPhase};
use crate::supervisor::{self, Record, Target};
pub use crate::supervisor::{FailureReason, Phase};
pub use client::start;
pub use launch::serve;
use sailover_core::transfer::CancelToken;
use serde::{Deserialize, Serialize};
pub use session::{EpicIndicator, archive, indicators, maintain, removed};
use std::{path::PathBuf, sync::Arc};
pub use windows::prepare_windows;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("backend.epic.cloud.session")]
    Session,
    #[error("backend.epic.cloud.busy")]
    Busy,
    #[error("backend.epic.cloud.cancelled")]
    Cancelled,
    #[error("backend.epic.cloud.recovery")]
    Recovery,
    #[error("backend.epic.cloud.storage")]
    Storage,
    #[error("backend.epic.cloud.failed")]
    Failed,
    #[error("backend.epic.cloud.unsupported")]
    Unsupported,
    /// The other storefront's copy of this game uses the same save paths.
    #[error("backend.cloud.sharedSavesInUse")]
    SharedSaves,
    #[error("backend.runtime.rosettaUnavailable")]
    RosettaUnavailable,
    #[error("backend.runtime.wineUnavailable")]
    WineUnavailable,
    #[error("{0}")]
    Epic(#[from] epic_kit::EpicError),
    #[error("backend.epic.cloud.storage")]
    Vault(cloud_vault::Error),
    #[error("backend.epic.cloud.storage")]
    Io(#[from] std::io::Error),
}
/// Revocation also cancels: an Epic sync then ends like any cancellation.
impl From<crate::cloud::Stop> for Error {
    fn from(_: crate::cloud::Stop) -> Self {
        Self::Cancelled
    }
}
impl From<cloud_vault::Error> for Error {
    fn from(error: cloud_vault::Error) -> Self {
        match error {
            cloud_vault::Error::Busy => Self::Busy,
            cloud_vault::Error::OtherStoreBusy => Self::SharedSaves,
            error => Self::Vault(error),
        }
    }
}
impl From<supervisor::Error> for Error {
    fn from(error: supervisor::Error) -> Self {
        match error {
            supervisor::Error::Storage => Self::Storage,
            supervisor::Error::NewerSession | supervisor::Error::Busy => Self::Busy,
            supervisor::Error::Cancelled => Self::Cancelled,
            supervisor::Error::RosettaUnavailable => Self::RosettaUnavailable,
            supervisor::Error::InvalidSession
            | supervisor::Error::Unavailable
            | supervisor::Error::StartFailed => Self::Session,
        }
    }
}
impl From<crate::Error> for Error {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::RosettaUnavailable => Self::RosettaUnavailable,
            crate::Error::WineUnavailable => Self::WineUnavailable,
            crate::Error::IncompatibleArchitecture
            | crate::Error::InvalidApplication
            | crate::Error::NotExecutable => Self::Unsupported,
            _ => Self::Session,
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub root: PathBuf,
    pub home: PathBuf,
    pub install: PathBuf,
    pub session_directory: PathBuf,
    pub helper: PathBuf,
    pub namespace: String,
    pub catalog_item_id: String,
    pub app_name: String,
    pub platform: String,
    pub language: String,
    /// Set for a Windows build, which runs in Wine: its saves live in this
    /// prefix.
    #[serde(default)]
    pub prefix: Option<PathBuf>,
}
impl Configuration {
    fn scope(&self, account: &str) -> cloud_vault::EpicScope {
        cloud_vault::EpicScope {
            account_id: account.into(),
            namespace: self.namespace.clone(),
            app_name: self.app_name.clone(),
        }
    }
    /// The build whose saves this Cloud manages, and where they are.
    fn cloud_platform(&self) -> epic_kit::cloud::CloudPlatform {
        match &self.prefix {
            Some(prefix) => epic_kit::cloud::CloudPlatform::Windows {
                profile: crate::wine::profile(prefix),
            },
            None => epic_kit::cloud::CloudPlatform::Mac {
                home: self.home.clone(),
            },
        }
    }
    /// Whether `wine` runs this build in its own prefix, and only a Windows
    /// build.
    fn runs_in(&self, wine: Option<&crate::wine::Config>) -> bool {
        self.prefix.as_ref() == wine.map(|wine| &wine.prefix)
    }
    fn validate(&self, account: &str) -> Result<()> {
        cloud_vault::StoreScope::from(self.scope(account)).validate()?;
        // A Mac build runs natively; a Windows build in its prefix.
        let prefix = match (self.platform.as_str(), &self.prefix) {
            ("Mac", None) => None,
            ("Windows", Some(prefix)) => Some(prefix),
            _ => return Err(Error::Unsupported),
        };
        if [
            &self.root,
            &self.home,
            &self.install,
            &self.session_directory,
            &self.helper,
        ]
        .into_iter()
        .chain(prefix)
        .any(|p| {
            !p.is_absolute()
                || p.components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
        }) || self.catalog_item_id.is_empty()
            || self.catalog_item_id.len() > 256
            || self.language.len() > 128
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Epic;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    pub namespace: String,
    pub app_name: String,
}

pub type Snapshot = Record<Epic>;
pub type Client = supervisor::Client<Epic>;
pub type IdleGuard = supervisor::IdleGuard<Epic>;

impl supervisor::Provider for Epic {
    type Account = String;
    type Store = Store;
    type Error = Error;
    const SUPERVISE: &'static str = "epic-supervise";
    const CANCELLED: &'static [&'static str] =
        &["backend.epic.cloud.cancelled", "backend.epic.cancelled"];

    fn valid(record: &Record<Self>) -> bool {
        record.account_id.len() == 32
            && !record.store.app_name.is_empty()
            && record.socket.is_absolute()
    }

    fn cancelled(error: &Error) -> bool {
        matches!(
            error,
            Error::Cancelled | Error::Epic(epic_kit::EpicError::Cancelled)
        )
    }

    fn same_game(a: &Store, b: &Store) -> bool {
        a == b
    }

    /// Epic keeps reconnecting to a game that still runs. A retired session's
    /// Cloud work stays pending for its next sync.
    fn orphaned(record: &mut Record<Self>, owned: bool) {
        if owned {
            record.reconnecting = true;
            return;
        }
        if !matches!(
            record.cloud.phase,
            CloudPhase::Unsupported | CloudPhase::Disabled | CloudPhase::Synced
        ) {
            record.cloud.phase = CloudPhase::Pending;
        }
        record.cloud.allowed_choices.clear();
    }
}

// A one-use exchange code travels solely through the private handoff pipe.
// This request is never persisted, formatted with Debug, or put in argv.
#[derive(Serialize, Deserialize)]
struct Request {
    config: Configuration,
    id: String,
    account_id: String,
    exchange_code: String,
    /// Built in SailOver's process: a supervisor starts without any environment.
    #[serde(default)]
    environment: std::collections::HashMap<String, String>,
    /// How a Windows build runs.
    #[serde(default)]
    wine: Option<crate::wine::Config>,
}
