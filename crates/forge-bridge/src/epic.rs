//! Epic's account seam. Only refresh credentials cross into the macOS Keychain;
//! the account access token stays inside the Rust session.
use std::{path::Path, sync::Arc};

use epic_kit::{auth, client};

use crate::{LaunchTarget, TransferState};

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum EpicError {
    #[error("backend.epic.network")]
    Network,
    #[error("backend.epic.unauthorized")]
    Unauthorized,
    #[error("backend.epic.invalid_code")]
    InvalidCode,
    #[error("backend.epic.invalid_response")]
    InvalidResponse,
    #[error("{message}")]
    Service { message: String },
}

impl From<epic_kit::EpicError> for EpicError {
    fn from(error: epic_kit::EpicError) -> Self {
        match error {
            epic_kit::EpicError::Network => Self::Network,
            epic_kit::EpicError::Unauthorized => Self::Unauthorized,
            epic_kit::EpicError::InvalidCode => Self::InvalidCode,
            epic_kit::EpicError::Protocol => Self::InvalidResponse,
            epic_kit::EpicError::Service { message } => Self::Service { message },
            error => Self::Service {
                message: error.to_string(),
            },
        }
    }
}

#[uniffi::export]
pub fn epic_login_url() -> String {
    auth::login_url()
}

#[uniffi::export]
pub fn epic_authorization_code(input: String) -> Result<String, EpicError> {
    auth::authorization_code(&input).map_err(Into::into)
}

/// The directory name the Epic engine installs into, so the host removes only
/// directories it created without re-deriving the hashing scheme.
#[uniffi::export]
pub fn epic_installation_directory_name(
    namespace: String,
    catalog_item_id: String,
    app_name: String,
    platform: String,
) -> String {
    client::installation_directory_name(&namespace, &catalog_item_id, &app_name, &platform)
}

#[derive(Clone, uniffi::Record)]
pub struct EpicCredentials {
    pub account_id: String,
    pub display_name: String,
    pub refresh_token: String,
}

impl From<auth::Credentials> for EpicCredentials {
    fn from(credentials: auth::Credentials) -> Self {
        Self {
            account_id: credentials.account_id,
            display_name: credentials.display_name,
            refresh_token: credentials.refresh_token,
        }
    }
}

impl From<EpicCredentials> for auth::Credentials {
    fn from(credentials: EpicCredentials) -> Self {
        Self {
            account_id: credentials.account_id,
            display_name: credentials.display_name,
            refresh_token: credentials.refresh_token,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicGame {
    pub app_name: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub title: String,
    pub description: String,
    pub image_url: Option<String>,
    pub header_image_url: Option<String>,
    pub developer: String,
    pub windows: bool,
    pub macos: bool,
}

impl From<client::Game> for EpicGame {
    fn from(game: client::Game) -> Self {
        Self {
            app_name: game.app_name,
            namespace: game.namespace,
            catalog_item_id: game.catalog_item_id,
            title: game.title,
            description: game.description,
            image_url: game.image_url,
            header_image_url: game.header_image_url,
            developer: game.developer,
            windows: game.windows,
            macos: game.macos,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicGameDetails {
    pub playtime_seconds: Option<u64>,
    pub installation_bytes: Option<u64>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicInstallation {
    pub app_name: String,
    pub directory: String,
    pub platform: String,
    pub total_bytes: u64,
}

// Launch arguments can contain a short-lived exchange code. Do not log plans.
#[derive(Clone, uniffi::Record)]
pub struct EpicLaunchPlan {
    pub app_name: String,
    pub platform: String,
    pub target: LaunchTarget,
    pub ownership_token: Option<Vec<u8>>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicTransfer {
    pub app_name: String,
    pub state: TransferState,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub network_bytes: u64,
    pub content_bytes: u64,
    pub current_file: String,
    pub message: String,
}

impl From<client::TransferProgress> for EpicTransfer {
    fn from(progress: client::TransferProgress) -> Self {
        Self {
            app_name: progress.app_name,
            state: progress.state.into(),
            completed_bytes: progress.completed_bytes,
            total_bytes: progress.total_bytes,
            network_bytes: progress.network_bytes,
            content_bytes: progress.content_bytes,
            current_file: progress.current_file,
            message: progress.message,
        }
    }
}

impl From<client::GameDetails> for EpicGameDetails {
    fn from(details: client::GameDetails) -> Self {
        Self {
            playtime_seconds: details.playtime_seconds,
            installation_bytes: details.installation_bytes,
        }
    }
}

#[derive(uniffi::Object)]
pub struct EpicSession {
    client: client::Client,
}

#[uniffi::export(async_runtime = "tokio")]
impl EpicSession {
    #[uniffi::constructor]
    pub async fn sign_in(authorization_code: String) -> Result<Arc<Self>, EpicError> {
        Ok(Arc::new(Self {
            client: client::Client::sign_in(&authorization_code).await?,
        }))
    }

    #[uniffi::constructor]
    pub async fn restore(credentials: EpicCredentials) -> Result<Arc<Self>, EpicError> {
        Ok(Arc::new(Self {
            client: client::Client::restore(credentials.into()).await?,
        }))
    }

    pub async fn credentials(&self) -> Result<EpicCredentials, EpicError> {
        Ok(self.client.credentials().await?.into())
    }

    pub async fn prepare(&self) -> Result<EpicCredentials, EpicError> {
        Ok(self.client.prepare().await?.into())
    }

    pub async fn start_friends(&self) {
        self.client.start_friends();
    }
    pub async fn reconnect_friends(&self) {
        self.client.reconnect_friends();
    }
    pub fn stop_friends(&self) {
        self.client.stop_friends();
    }
    pub fn shutdown_observers(&self) {
        self.client.shutdown_observers();
    }
    pub async fn next_friends(&self, after_revision: u64) -> crate::EpicFriendsSnapshot {
        self.client.next_friends(after_revision).await.into()
    }
    pub async fn next_credentials_revision(&self, after_revision: u64) -> Option<u64> {
        self.client.next_credentials_revision(after_revision).await
    }

    pub async fn games(&self) -> Result<Vec<EpicGame>, EpicError> {
        Ok(self
            .client
            .games()
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn disconnect(&self) -> Result<(), EpicError> {
        self.client.disconnect().await.map_err(Into::into)
    }

    pub async fn download(
        &self,
        namespace: String,
        catalog_item_id: String,
        app_name: String,
        platform: String,
        directory: String,
    ) -> Result<EpicInstallation, EpicError> {
        let installation = self
            .client
            .download(
                &namespace,
                &catalog_item_id,
                &app_name,
                &platform,
                Path::new(&directory),
            )
            .await?;
        Ok(EpicInstallation {
            app_name: installation.app_name,
            directory: installation.directory,
            platform: installation.platform,
            total_bytes: installation.total_bytes,
        })
    }

    pub fn download_progress(&self) -> Option<EpicTransfer> {
        self.client.progress().map(Into::into)
    }

    pub async fn launch_plan(
        &self,
        namespace: String,
        catalog_item_id: String,
        app_name: String,
        platform: String,
        directory: String,
        language: String,
    ) -> Result<EpicLaunchPlan, EpicError> {
        let plan = self
            .client
            .launch_plan(
                &namespace,
                &catalog_item_id,
                &app_name,
                &platform,
                Path::new(&directory),
                &language,
            )
            .await?;
        Ok(EpicLaunchPlan {
            app_name: plan.app_name,
            platform: plan.platform,
            target: plan.target.into(),
            ownership_token: plan.ownership_token,
        })
    }

    pub fn cancel_download(&self) {
        self.client.cancel_download();
    }

    pub async fn game_details(
        &self,
        namespace: String,
        catalog_item_id: String,
        app_name: String,
        platform: String,
    ) -> Result<EpicGameDetails, EpicError> {
        Ok(self
            .client
            .game_details(&namespace, &catalog_item_id, &app_name, &platform)
            .await?
            .into())
    }
}
