//! Auto-Cloud lifecycle owned by the detached game supervisor. The UI sends
//! decisions, never file transfers. Credentials exist only in process memory.
mod paths;
mod session;
mod store;
pub(crate) fn safe_storage_path(path: &std::path::Path) -> Result<(), crate::Error> {
    store::safe(path).map_err(Into::into)
}
pub use session::maintain;
pub(crate) use session::{Session, open};
pub use store::{archive, archive_with_rules, indicators, removed};

use crate::cloud::{Control, Stop};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub root: PathBuf,
    pub home: PathBuf,
    pub install: PathBuf,
    pub app_id: u32,
}

/// What Cloud maintenance syncs: an installed game, or an uninstalled game
/// whose saved context still holds pending work.
#[derive(Clone, Debug)]
pub enum MaintenanceTarget {
    Installed(Config),
    Uninstalled { root: PathBuf, app_id: u32 },
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("{}", self.key())]
pub enum Failure {
    Network,
    Authentication,
    Preferences,
    Unsupported,
    Conflict,
    Storage,
    Ownership,
    Recovery,
    Uncertain,
    Cancelled,
    Changed,
    Busy,
    Quota,
    /// The other storefront's copy of this game uses the same save paths.
    SharedSaves,
    /// An uninstalled game's Cloud rules changed since its exit save.
    RulesChanged,
}
impl Failure {
    pub fn key(self) -> &'static str {
        match self {
            Self::Network => "backend.steam.cloud.network",
            Self::Authentication => "backend.steam.cloud.authentication",
            Self::Preferences => "backend.steam.cloud.preferences",
            Self::Unsupported => "backend.steam.cloud.unsupported",
            Self::Conflict => "backend.steam.cloud.conflict",
            Self::Storage => "backend.steam.cloud.storage",
            Self::Ownership => "backend.steam.cloud.ownership",
            Self::Recovery => "backend.steam.cloud.recovery",
            Self::Uncertain => "backend.steam.cloud.uncertain",
            Self::Cancelled => "backend.steam.runtime.cancelled",
            Self::Changed => "backend.steam.cloud.changed",
            Self::Busy => "backend.steam.runtime.sessionBusy",
            Self::Quota => "backend.steam.cloud.quota",
            Self::SharedSaves => "backend.cloud.sharedSavesInUse",
            Self::RulesChanged => "backend.steam.cloud.rulesChanged",
        }
    }
}
impl From<cloud_vault::Error> for Failure {
    fn from(e: cloud_vault::Error) -> Self {
        use cloud_vault::Error;
        match e {
            Error::Busy => Self::Busy,
            Error::OtherStoreBusy => Self::SharedSaves,
            Error::Authentication => Self::Ownership,
            Error::Preferences => Self::Preferences,
            Error::Recovery => Self::Recovery,
            Error::Stale => Self::Changed,
            Error::Invalid | Error::Integrity | Error::Io(_) | Error::Journal(_) => Self::Storage,
        }
    }
}
impl From<steam_kit::cm::CmError> for Failure {
    fn from(e: steam_kit::cm::CmError) -> Self {
        use steam_kit::cm::CmError;
        match e {
            e if e.authentication_required() => Self::Authentication,
            // EResult::LimitExceeded and EResult::InsufficientPrivilege.
            CmError::Rejected {
                result: 25 | 84, ..
            } => Self::Quota,
            CmError::Protocol(_) | CmError::MissingApp(_) => Self::Unsupported,
            CmError::Rejected { .. }
            | CmError::Disconnected
            | CmError::Timeout
            | CmError::NoContentServer
            | CmError::Directory(_) => Self::Network,
        }
    }
}
impl From<steam_kit::cloud::CloudError> for Failure {
    fn from(e: steam_kit::cloud::CloudError) -> Self {
        use steam_kit::cloud::CloudError;
        match e {
            CloudError::Cm(e) => e.into(),
            CloudError::Network | CloudError::Timeout => Self::Network,
            CloudError::Changed => Self::Changed,
            CloudError::Revoked => Self::Authentication,
            CloudError::Uncertain | CloudError::NotCommitted => Self::Uncertain,
            CloudError::Storage | CloudError::Integrity => Self::Storage,
            CloudError::Invalid(_)
            | CloudError::Delta
            | CloudError::UnsupportedFormat { .. }
            | CloudError::Host(_) => Self::Unsupported,
        }
    }
}
impl From<Stop> for Failure {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Cancelled => Self::Cancelled,
            Stop::Revoked => Self::Authentication,
        }
    }
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
impl From<serde_json::Error> for Failure {
    fn from(_: serde_json::Error) -> Self {
        Self::Storage
    }
}
impl From<Failure> for crate::Error {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Cancelled => Self::Cancelled,
            Failure::Busy => Self::SessionBusy,
            failure => Self::Cloud(failure),
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn scope(config: &Config, account: u64) -> cloud_vault::Scope {
    cloud_vault::Scope {
        steam_id: account,
        app_id: config.app_id,
        cloud_app_id: config.app_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::sync::{Failure as _, Kind};

    #[test]
    fn a_launch_goes_on_without_sync_only_when_local_saves_stay_safe() {
        for (failure, kind) in [
            (Failure::Network, Kind::Transient),
            (Failure::Changed, Kind::Transient),
            (Failure::Uncertain, Kind::Transient),
            (Failure::Conflict, Kind::Conflict),
            (Failure::Preferences, Kind::Unavailable),
            (Failure::Unsupported, Kind::Unavailable),
            (Failure::Quota, Kind::Unavailable),
            (Failure::Authentication, Kind::Denied),
            (Failure::Cancelled, Kind::Cancelled),
            (Failure::Storage, Kind::Blocking),
            (Failure::Ownership, Kind::Blocking),
            (Failure::Recovery, Kind::Blocking),
            (Failure::Busy, Kind::Blocking),
            (Failure::SharedSaves, Kind::Blocking),
            (Failure::RulesChanged, Kind::Blocking),
        ] {
            assert_eq!(failure.kind(), kind, "{failure:?}");
        }
    }
}
