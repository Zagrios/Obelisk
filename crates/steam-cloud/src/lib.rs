//! Forge-owned Cloud decisions and durable save storage. No Valve ABI or network
//! calls belong here. An adapter must supply complete, verified remote views.
mod model;
mod reconcile;
mod retry;
mod vault;

pub use model::*;
pub use reconcile::*;
pub use retry::*;
pub use vault::*;

/// Durable JSON journal primitives shared with adapters that keep their own
/// small state files beside the archive.
pub mod journal {
    pub use crate::vault::disk::{read, regular, write};
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid Cloud state")]
    Invalid,
    #[error("Cloud storage is already in use")]
    Busy,
    #[error("Cloud content failed verification")]
    Integrity,
    #[error("Cloud state changed since the decision")]
    Stale,
    #[error("Cloud work requires authentication by its original account")]
    Authentication,
    #[error("Cloud preferences are disabled or unavailable")]
    Preferences,
    #[error("Cloud operation requires recovery")]
    Recovery,
    #[error("Cloud storage I/O failed")]
    Io(#[from] std::io::Error),
    #[error("Invalid Cloud journal")]
    Journal(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
