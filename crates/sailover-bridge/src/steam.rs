//! Asynchronous, account-scoped Steam API for Swift. Secrets cross this seam only
//! for authentication and the macOS Keychain; they are never part of a catalog.
use std::{path::Path, sync::Arc};
use steam_kit::{auth, client};
use tokio::sync::Mutex;

#[uniffi::export]
pub fn steam_language(locale: String) -> String {
    steam_kit::language::from_locale(&locale).into()
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SteamError {
    #[error("{reason}")]
    Failure { reason: String },
    #[error("backend.steam.friends.authenticationRequired")]
    AuthenticationRequired,
    #[error("backend.steam.content.cancelled")]
    Cancelled,
}
impl From<client::ClientConnectError> for SteamError {
    fn from(error: client::ClientConnectError) -> Self {
        match error {
            client::ClientConnectError::AuthenticationRequired(_) => Self::AuthenticationRequired,
            client::ClientConnectError::Other(error) => error.into(),
        }
    }
}
impl From<auth::AuthError> for SteamError {
    fn from(error: auth::AuthError) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}
impl From<client::ClientError> for SteamError {
    fn from(error: client::ClientError) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}
impl From<steam_kit::cloud::CloudError> for SteamError {
    fn from(error: steam_kit::cloud::CloudError) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

#[derive(Clone, uniffi::Record)]
pub struct SteamCredentials {
    pub account_name: String,
    pub steam_id: u64,
    pub refresh_token: String,
}

#[derive(Clone, Copy, Debug, uniffi::Enum)]
pub enum SteamGuardKind {
    None,
    EmailCode,
    DeviceCode,
    DeviceConfirmation,
    EmailConfirmation,
    MachineToken,
    Unknown,
}

impl From<auth::GuardType> for SteamGuardKind {
    fn from(kind: auth::GuardType) -> Self {
        match kind {
            auth::GuardType::None => Self::None,
            auth::GuardType::EmailCode => Self::EmailCode,
            auth::GuardType::DeviceCode => Self::DeviceCode,
            auth::GuardType::DeviceConfirmation => Self::DeviceConfirmation,
            auth::GuardType::EmailConfirmation => Self::EmailConfirmation,
            auth::GuardType::MachineToken => Self::MachineToken,
            auth::GuardType::Unknown => Self::Unknown,
        }
    }
}
impl From<SteamGuardKind> for auth::GuardType {
    fn from(kind: SteamGuardKind) -> Self {
        match kind {
            SteamGuardKind::None => Self::None,
            SteamGuardKind::EmailCode => Self::EmailCode,
            SteamGuardKind::DeviceCode => Self::DeviceCode,
            SteamGuardKind::DeviceConfirmation => Self::DeviceConfirmation,
            SteamGuardKind::EmailConfirmation => Self::EmailConfirmation,
            SteamGuardKind::MachineToken => Self::MachineToken,
            SteamGuardKind::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamGuardMethod {
    pub kind: SteamGuardKind,
    pub message: String,
}

// A QR challenge can approve an account session: deliberately no Debug.
#[derive(Clone, uniffi::Record)]
pub struct SteamChallenge {
    pub qr_url: Option<String>,
    pub confirmations: Vec<SteamGuardMethod>,
    pub poll_interval_ms: u32,
    pub remote_interaction: bool,
}

#[derive(uniffi::Object)]
pub struct SteamLogin {
    client: auth::AuthClient,
    session: Mutex<auth::AuthSession>,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamLogin {
    #[uniffi::constructor]
    pub async fn begin_qr() -> Result<Arc<Self>, SteamError> {
        let client = auth::AuthClient::new(client::http_client()?);
        let session = client.begin_qr().await?;
        Ok(Arc::new(Self {
            client,
            session: Mutex::new(session),
        }))
    }
    #[uniffi::constructor]
    pub async fn begin_credentials(
        account_name: String,
        password: String,
    ) -> Result<Arc<Self>, SteamError> {
        let client = auth::AuthClient::new(client::http_client()?);
        let session = client.begin_credentials(&account_name, &password).await?;
        Ok(Arc::new(Self {
            client,
            session: Mutex::new(session),
        }))
    }
    pub async fn challenge(&self) -> SteamChallenge {
        let session = self.session.lock().await;
        SteamChallenge {
            qr_url: session.challenge_url().map(str::to_owned),
            confirmations: session
                .allowed_confirmations()
                .iter()
                .map(|confirmation| SteamGuardMethod {
                    kind: confirmation.kind.into(),
                    message: confirmation.message.clone(),
                })
                .collect(),
            poll_interval_ms: session.polling_interval().as_millis().min(u32::MAX as u128) as u32,
            remote_interaction: session.had_remote_interaction(),
        }
    }
    pub async fn poll(&self) -> Result<Option<SteamCredentials>, SteamError> {
        let mut session = self.session.lock().await;
        Ok(self
            .client
            .poll(&mut session)
            .await?
            .map(|tokens| SteamCredentials {
                account_name: tokens.account_name,
                steam_id: tokens.steam_id,
                refresh_token: tokens.refresh_token,
            }))
    }
    pub async fn submit_guard(&self, code: String, kind: SteamGuardKind) -> Result<(), SteamError> {
        let session = self.session.lock().await;
        self.client
            .submit_guard(&session, &code, kind.into())
            .await
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamAchievement {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon_url: String,
    pub hidden: bool,
    pub unlocked: bool,
    pub unlock_time: u32,
}
impl From<client::Achievement> for SteamAchievement {
    fn from(value: client::Achievement) -> Self {
        Self {
            id: value.id,
            name: value.name,
            description: value.description,
            icon_url: value.icon_url,
            hidden: value.hidden,
            unlocked: value.unlocked,
            unlock_time: value.unlock_time,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamGame {
    pub app_id: u32,
    pub name: String,
    pub icon_hash: String,
    pub last_played: u64,
    pub playtime_minutes: u32,
}
impl From<client::OwnedGame> for SteamGame {
    fn from(game: client::OwnedGame) -> Self {
        Self {
            app_id: game.app_id,
            name: game.name,
            icon_hash: game.icon_hash,
            last_played: game.last_played,
            playtime_minutes: game.playtime_minutes,
        }
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamGamePlatforms {
    pub app_id: u32,
    pub platforms: Vec<String>,
}
impl From<client::GamePlatforms> for SteamGamePlatforms {
    fn from(game: client::GamePlatforms) -> Self {
        Self {
            app_id: game.app_id,
            platforms: game.platforms,
        }
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamGameDetails {
    pub app_id: u32,
    pub name: String,
    pub windows: bool,
    pub macos: bool,
    pub windows_bytes: u64,
    pub macos_bytes: u64,
}
#[derive(uniffi::Object)]
pub struct SteamSession {
    /// Shared with the account's installations.
    pub(crate) client: Arc<client::Client>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamProfile {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamSession {
    #[uniffi::constructor]
    pub async fn connect(credentials: SteamCredentials) -> Result<Arc<Self>, SteamError> {
        // Decode the routing identity before sending the token, then let Steam
        // validate its signature, expiry, revocation and account ownership.
        let token = auth::AuthTokens::restore(credentials.account_name, credentials.refresh_token)?;
        if token.steam_id != credentials.steam_id {
            return Err(auth::AuthError::Protocol.into());
        }
        Ok(Arc::new(Self {
            client: Arc::new(
                client::Client::connect(&token.account_name, token.steam_id, &token.refresh_token)
                    .await?,
            ),
        }))
    }
    pub async fn games(&self) -> Result<Vec<SteamGame>, SteamError> {
        Ok(self
            .client
            .games()
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }
    pub async fn achievements(
        &self,
        app_id: u32,
        language: String,
    ) -> Result<Vec<SteamAchievement>, SteamError> {
        Ok(self
            .client
            .achievements(app_id, &language)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }
    /// Suspends on the CM watch channel; no content-query mutex is held.
    pub async fn next_friends(&self, after_revision: u64) -> crate::SteamFriendsSnapshot {
        self.client.next_friends(after_revision).await.into()
    }

    /// Also wakes UniFFI futures, whose Swift wrappers do not cancel Rust awaits.
    pub fn shutdown(&self) {
        self.client.shutdown();
    }
    pub async fn profile(&self) -> Result<SteamProfile, SteamError> {
        let profile = self.client.profile().await?;
        Ok(SteamProfile {
            display_name: profile.display_name,
            avatar_url: profile.avatar_url,
        })
    }
    /// Explicit diagnostic operation using this already-authenticated session.
    /// It downloads a verified backup; no Cloud mutation or game launch occurs.
    pub async fn cloud_backup(&self, app_id: u32, directory: String) -> Result<String, SteamError> {
        Ok(self
            .client
            .cloud_backup(app_id, Path::new(&directory))
            .await?)
    }
    /// Returns only Cloud preferences and source revision, never roaming data.
    pub async fn cloud_preferences(&self, app_id: u32) -> Result<String, SteamError> {
        let preferences = self.client.cloud_preferences(app_id).await?;
        serde_json::to_string(&preferences).map_err(|_| SteamError::Failure {
            reason: "Invalid Cloud preference report".into(),
        })
    }
    /// Declared application platforms in batches; an empty platform list means unknown.
    pub async fn platforms(
        &self,
        app_ids: Vec<u32>,
    ) -> Result<Vec<SteamGamePlatforms>, SteamError> {
        Ok(self
            .client
            .platforms(&app_ids)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }
    pub async fn details(
        &self,
        app_id: u32,
        language: String,
    ) -> Result<SteamGameDetails, SteamError> {
        let details = self.client.details(app_id, &language).await?;
        Ok(SteamGameDetails {
            app_id: details.app_id,
            name: details.name,
            windows: details.windows,
            macos: details.macos,
            windows_bytes: details.windows_bytes,
            macos_bytes: details.macos_bytes,
        })
    }
}

// Gate the entire export: UniFFI otherwise generates Release wrappers for
// methods that Rust removes through their individual cfg attributes.
#[cfg(debug_assertions)]
#[uniffi::export(async_runtime = "tokio")]
impl SteamSession {
    pub async fn cloud_interruption_probe(
        &self,
        app_id: u32,
        directory: String,
    ) -> Result<String, SteamError> {
        Ok(self
            .client
            .cloud_interruption_probe(app_id, Path::new(&directory))
            .await?)
    }

    pub async fn cloud_sdk_candidates(&self) -> Result<String, SteamError> {
        Ok(self.client.cloud_sdk_candidates().await?)
    }
    pub async fn cloud_autocloud_metadata(&self) -> Result<String, SteamError> {
        Ok(self.client.cloud_autocloud_metadata().await?)
    }
    pub async fn cloud_transfer_probe(
        &self,
        app_id: u32,
        directory: String,
    ) -> Result<String, SteamError> {
        Ok(self
            .client
            .cloud_transfer_probe(app_id, Path::new(&directory))
            .await?)
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamNews {
    pub id: String,
    pub title: String,
    pub url: String,
    pub excerpt: String,
    pub date: u64,
    pub source: String,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn steam_news(app_id: u32) -> Result<Vec<SteamNews>, SteamError> {
    Ok(steam_kit::news::for_app(app_id)
        .await?
        .into_iter()
        .map(|item| SteamNews {
            id: item.id,
            title: item.title,
            url: item.url,
            excerpt: item.excerpt,
            date: item.date,
            source: item.source,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_with_metadata_network_bytes_stays_verification_across_bridge() {
        let mut progress = client::TransferProgress::new("1");
        progress.state = client::TransferState::Verifying;
        progress.completed_bytes = 522_734_781;
        progress.total_bytes = 522_734_781;
        progress.network_bytes = 253;
        progress.current_file = "game.exe".into();
        let mapped = crate::GameTransfer::from(progress);
        assert!(matches!(mapped.state, crate::TransferState::Verifying));
        assert_eq!(mapped.game, "1");
        assert_eq!(mapped.completed_bytes, 522_734_781);
        assert_eq!(mapped.network_bytes, 253);
        assert_eq!(mapped.content_bytes, 0);
    }
}
