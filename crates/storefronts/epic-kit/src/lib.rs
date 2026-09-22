//! Epic account authentication, owned libraries and verified BuildPatch downloads.

pub mod auth;
pub mod client;
pub mod cloud;
mod content;
mod content_manifest;
mod details;
pub mod friends;
mod manifest;
mod social;
mod stomp;

#[derive(Debug, thiserror::Error)]
pub enum EpicError {
    #[error("backend.epic.network")]
    Network,
    #[error("backend.epic.unauthorized")]
    Unauthorized,
    #[error("backend.epic.invalid_code")]
    InvalidCode,
    #[error("backend.epic.invalid_response")]
    Protocol,
    #[error("backend.epic.cancelled")]
    Cancelled,
    #[error("backend.epic.storage")]
    Io(#[from] std::io::Error),
    #[error("backend.epic.download_in_progress")]
    AlreadyDownloading,
    #[error("backend.epic.unsafe_path")]
    UnsafePath(String),
    #[error("backend.epic.integrity")]
    Integrity,
    #[error("backend.epic.cloud_conflict")]
    CloudConflict,
    #[error("backend.epic.content_http")]
    Http(u16),
    #[error("backend.epic.unsupported_content")]
    UnsupportedContent,
    #[error("{message}")]
    Service { message: String },
}

use forge_core::{
    install::InstallError,
    storefront::{StorefrontCapabilities, StorefrontDescriptor, StorefrontId},
    transfer::Cancelled,
};

impl From<Cancelled> for EpicError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<InstallError> for EpicError {
    fn from(error: InstallError) -> Self {
        match error {
            InstallError::Cancelled => Self::Cancelled,
            InstallError::UnsafePath(path) => Self::UnsafePath(path),
            InstallError::Io(error) => Self::Io(error),
        }
    }
}

pub fn descriptor() -> StorefrontDescriptor {
    StorefrontDescriptor {
        id: StorefrontId::Epic,
        display_name: "Epic Games",
        capabilities: StorefrontCapabilities {
            authentication: true,
            owned_library: true,
            downloads: true,
            local_import: false,
        },
    }
}
