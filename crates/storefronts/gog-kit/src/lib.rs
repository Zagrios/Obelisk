//! GOG account authentication, owned libraries and verified Galaxy content
//! downloads, independently implemented from the endpoints documented by
//! heroic-gogdl and Heroic's GOG store manager.

pub mod auth;
pub mod client;
mod content;
mod http;
mod library;
mod manifest;

#[derive(Debug, thiserror::Error)]
pub enum GogError {
    #[error("backend.gog.network")]
    Network,
    #[error("backend.gog.unauthorized")]
    Unauthorized,
    #[error("backend.gog.invalid_code")]
    InvalidCode,
    #[error("backend.gog.invalid_response")]
    Protocol,
    #[error("backend.gog.cancelled")]
    Cancelled,
    #[error("backend.gog.storage")]
    Io(#[from] std::io::Error),
    #[error("backend.gog.download_in_progress")]
    AlreadyDownloading,
    #[error("backend.gog.unsafe_path")]
    UnsafePath(String),
    #[error("backend.gog.integrity")]
    Integrity,
    #[error("backend.gog.content_http")]
    Http(u16),
    /// GOG serves no build Obelisk can install for this platform.
    #[error("backend.gog.no_build")]
    NoBuild,
    #[error("{message}")]
    Service { message: String },
}

use obelisk_core::{
    build_choice::{NOT_NATIVE, Unavailable},
    install::InstallError,
    publish::PublishError,
    transfer::Cancelled,
};

impl From<Cancelled> for GogError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<Unavailable> for GogError {
    fn from(unavailable: Unavailable) -> Self {
        match unavailable {
            Unavailable::NoBuild => Self::NoBuild,
            Unavailable::NotNative => Self::Service {
                message: NOT_NATIVE.into(),
            },
        }
    }
}

impl From<InstallError> for GogError {
    fn from(error: InstallError) -> Self {
        match error {
            InstallError::Cancelled => Self::Cancelled,
            InstallError::UnsafePath(path) => Self::UnsafePath(path),
            InstallError::Io(error) => Self::Io(error),
        }
    }
}

impl From<PublishError> for GogError {
    fn from(error: PublishError) -> Self {
        match error {
            // Chunks are checked as they arrive; no whole file is compared.
            PublishError::Mismatch => Self::Integrity,
            PublishError::Install(error) => error.into(),
        }
    }
}
