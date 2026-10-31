//! Epic game ownership and EGS Cloud orchestration. The UI only observes and
//! answers revision-bound choices; a detached process owns the entire session.
pub mod content;
pub(crate) mod launch;
mod session;
mod windows;

// A Game session's phase is `Phase`; its Cloud sync's phase is `CloudPhase`.
use crate::cloud::{Control, Phase as CloudPhase, record};
use crate::runtimes::Runtime;
use crate::supervisor::{self, Record, Target};
pub use crate::supervisor::{FailureReason, Phase};
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
crate::launch_shell::session_error_conversions!(Error);
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

/// A one-use exchange code travels solely through the private handoff pipe.
/// Never `Debug`.
#[derive(Serialize, Deserialize)]
pub(crate) struct ExchangeCode {
    pub(crate) exchange_code: String,
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

impl crate::launch_shell::Launchable for Epic {
    type Configuration = Configuration;
    type Credential = ExchangeCode;

    fn build(configuration: &Configuration) -> crate::launch_shell::Build<'_> {
        crate::launch_shell::Build {
            home: &configuration.home,
            install: &configuration.install,
            session_directory: &configuration.session_directory,
            helper: &configuration.helper,
            platform: configuration.platform,
            title: &configuration.title,
            prefix: configuration.prefix.as_deref(),
            game: crate::profile::Game::Epic {
                namespace: &configuration.namespace,
                app_name: &configuration.app_name,
            },
        }
    }

    fn store(configuration: &Configuration) -> Store {
        Store {
            namespace: configuration.namespace.clone(),
            app_name: configuration.app_name.clone(),
        }
    }

    fn check(configuration: &Configuration, account: &str) -> Result<()> {
        cloud_vault::StoreScope::from(configuration.scope(account)).validate()?;
        if !supervisor::plain_absolute(&configuration.root)
            || configuration.catalog_item_id.is_empty()
            || configuration.catalog_item_id.len() > 256
            || configuration.language.len() > 128
        {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    fn installed(configuration: &Configuration) -> Result<crate::launch_shell::Installed> {
        windows::installed(configuration)
    }
}
