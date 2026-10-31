//! Epic game ownership and EGS Cloud orchestration. The UI only observes and
//! answers revision-bound choices; a detached process owns the entire session.
mod client;
pub mod content;
pub(crate) mod launch;
mod session;
mod social_overlay;
mod windows;

// A Game session's phase is `Phase`; its Cloud sync's phase is `CloudPhase`.
use crate::cloud::{Control, Phase as CloudPhase, record};
use crate::runtimes::Runtime;
use crate::supervisor::{self, Record, Target};
pub use crate::supervisor::{FailureReason, Phase};
pub use client::start;
pub use launch::serve;
use obelisk_core::{Platform, transfer::CancelToken};
use serde::{Deserialize, Serialize};
pub use session::{archive, maintain};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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
    /// What readying or starting the game failed with, shown as every
    /// storefront shows it.
    #[error("{0}")]
    Launch(crate::launch::Error),
    /// The other storefront's copy of this game uses the same save paths.
    #[error("backend.cloud.sharedSavesInUse")]
    SharedSaves,
    #[error("backend.runtime.rosettaUnavailable")]
    RosettaUnavailable,
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
supervisor::session_error_conversions!(Error);
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
    #[serde(serialize_with = "epic_kit::client::write_platform")]
    pub platform: Platform,
    pub language: String,
    /// The game's name in the store, which the Linux engine shows in the
    /// menu bar, the Dock and Cmd-Tab.
    #[serde(default)]
    pub title: String,
    /// Set for a Windows build, which runs in Wine: its saves live in this
    /// prefix.
    #[serde(default)]
    pub prefix: Option<PathBuf>,
}
impl Configuration {
    /// The game as Obelisk's profiles and logs name it.
    pub(crate) fn profile_game(&self) -> crate::profile::Game<'_> {
        crate::profile::Game::Epic {
            namespace: &self.namespace,
            app_name: &self.app_name,
        }
    }
    /// The game as Wine starts a Windows build.
    pub(crate) fn game(&self) -> crate::wine::Game<'_> {
        crate::wine::Game {
            install: &self.install,
            title: &self.title,
        }
    }
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
        let prefix = match (self.platform, &self.prefix) {
            (Platform::Mac, None) => None,
            (Platform::Windows, Some(prefix)) => Some(prefix),
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
        .any(|path| !supervisor::plain_absolute(path))
            || self.catalog_item_id.is_empty()
            || self.catalog_item_id.len() > 256
            || self.language.len() > 128
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }
}

impl record::Configuration for Configuration {
    fn root(&self) -> &Path {
        &self.root
    }
    fn install(&self) -> &Path {
        &self.install
    }
    fn prefix(&self) -> Option<&Path> {
        self.prefix.as_deref()
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
    type Control = supervisor::NoControl;
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
    /// Built in Obelisk's process: a supervisor starts without any environment.
    #[serde(default)]
    environment: std::collections::HashMap<String, String>,
    /// How a Windows build runs.
    #[serde(default)]
    wine: Option<crate::wine::Config>,
}

/// What an Epic installation names of its game: its catalog item, in its
/// namespace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub namespace: String,
    /// The catalog item the build belongs to. Older receipt files lack it;
    /// the installation's own receipt records it.
    #[serde(default)]
    pub catalog_item_id: String,
    pub app_name: String,
}

impl crate::sessions::Storefront for Epic {
    type Game = Store;
    const BEGINS: bool = true;
    const INVALID_SESSION: &'static str = "backend.epic.cloud.invalidSession";
    const SESSION_FAILED: &'static str = "backend.epic.cloud.failed";
    const RUNTIMES: &'static [Runtime] = &[];
    fn game(store: &Store) -> Store {
        store.clone()
    }
}

impl crate::installations::Storefront for Epic {
    type Product = Product;
    /// Nothing joins a game through this storefront.
    type Join = std::convert::Infallible;
    const PLATFORM: obelisk_core::platform::Spelling = epic_kit::client::PLATFORM;
    const RECEIPTS_UNAVAILABLE: &'static str = "model.epic.receiptsUnavailable";
    const RECEIPTS_NOT_SAVED: &'static str = "model.epic.receiptsNotSaved";
    const GAMES_DIRECTORY: &'static str = "Epic";
    const QUEUE: crate::queue::Storefront = crate::queue::Storefront::Epic;
    fn product_game(product: &Product) -> Store {
        Store {
            namespace: product.namespace.clone(),
            app_name: product.app_name.clone(),
        }
    }
    fn profile_game(product: &Product) -> crate::profile::Game<'_> {
        crate::profile::Game::Epic {
            namespace: &product.namespace,
            app_name: &product.app_name,
        }
    }
    fn receipt_id(game: &Store, platform: Platform) -> String {
        format!(
            "{}:{}:{}",
            game.namespace,
            game.app_name,
            Self::PLATFORM.name(platform)
        )
    }
    fn transfer_name(game: &Store) -> String {
        game.app_name.clone()
    }
    fn complete(receipt: &mut crate::installations::Receipt<Epic>) -> bool {
        if !receipt.product.catalog_item_id.is_empty() {
            return false;
        }
        let directory = std::path::Path::new(&receipt.directory);
        match epic_kit::client::installed_catalog_item(directory) {
            Ok(catalog_item_id) => {
                receipt.product.catalog_item_id = catalog_item_id;
                true
            }
            Err(_) => false,
        }
    }
}
