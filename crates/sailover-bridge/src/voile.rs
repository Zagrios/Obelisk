//! The Voile runtime Windows games run in, which SailOver installs on the first
//! Windows launch. Cancellation crosses UniFFI explicitly.
use crate::SailOverLocations;
use std::{path::Path, sync::Arc};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum VoileError {
    #[error("the runtime install was cancelled")]
    Cancelled,
    #[error("{reason}")]
    Failure { reason: String },
}

impl From<game_runtime::voile::Failure> for VoileError {
    fn from(failure: game_runtime::voile::Failure) -> Self {
        match failure {
            game_runtime::voile::Failure::Cancelled => Self::Cancelled,
            failure => Self::Failure {
                reason: failure.to_string(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct VoileProgress {
    pub received_bytes: u64,
    pub total_bytes: u64,
}

/// The runtime Windows games run in, or nil until it is installed.
#[uniffi::export]
pub fn voile_runtime(locations: SailOverLocations) -> Option<String> {
    game_runtime::voile::runtime(Path::new(&locations.voile))
        .map(|runtime| runtime.to_string_lossy().into_owned())
}

/// One install of the pinned Voile release.
#[derive(uniffi::Object)]
pub struct VoileInstall {
    install: game_runtime::voile::Install,
}

#[uniffi::export(async_runtime = "tokio")]
impl VoileInstall {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            install: game_runtime::voile::Install::new(),
        })
    }

    pub fn cancel(&self) {
        self.install.cancel();
    }

    pub fn progress(&self) -> VoileProgress {
        let (received_bytes, total_bytes) = self.install.progress();
        VoileProgress {
            received_bytes,
            total_bytes,
        }
    }

    /// Installs the release unless a runtime is already in place, and returns
    /// the runtime. `archive`, a local copy of the release, replaces the
    /// download in development.
    pub async fn run(
        &self,
        locations: SailOverLocations,
        archive: Option<String>,
    ) -> Result<String, VoileError> {
        let runtime = self
            .install
            .run(
                Path::new(&locations.voile),
                archive.as_deref().map(Path::new),
            )
            .await?;
        Ok(runtime.to_string_lossy().into_owned())
    }
}
