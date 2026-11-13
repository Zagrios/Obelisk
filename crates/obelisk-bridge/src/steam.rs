//! Asynchronous, account-scoped Steam API for Swift. Secrets cross this seam only
//! for authentication and the macOS Keychain; they are never part of a catalog.
use crate::{
    GamePlatform, LibraryCache,
    installations::Named,
    library_cache::{self, GameAchievements, LibraryAchievement, SteamLibrary},
};
use game_runtime::{
    protocol::Credentials,
    steam::{self, account_session::AccountSession},
};
use std::{collections::HashMap, path::Path, sync::Arc};
use steam_kit::{artwork, auth, client};
use tokio::sync::Mutex;

/// Games whose achievements a library reads at once: each takes up to two
/// requests.
const ACHIEVEMENT_READS: usize = 4;

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

impl From<client::Achievement> for LibraryAchievement {
    fn from(value: client::Achievement) -> Self {
        Self {
            id: value.id,
            name: value.name,
            description: value.description,
            icon_url: value.icon_url,
            hidden: value.hidden,
            unlocked: value.unlocked,
            unlocked_at: (value.unlocked && value.unlock_time > 0)
                .then_some(u64::from(value.unlock_time)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
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
    /// Nil when PICS did not return the game.
    #[uniffi(default = None)]
    pub artwork: Option<SteamGameArtwork>,
}
/// A game's library artwork, each image's path under the game's store assets.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct SteamGameArtwork {
    /// The portrait capsule, 300 × 450.
    pub cover: Option<String>,
    /// The wide hero, 1920 × 620.
    pub hero: Option<String>,
    pub logo: Option<String>,
    /// The library header, or the store's when the game has no library one.
    pub header: Option<String>,
    /// The app whose artwork stands in for the game's missing images, such
    /// as Rust's for its staging branch.
    #[uniffi(default = None)]
    pub parent: Option<u32>,
    /// What the app is when not a full game, which its cover says.
    #[uniffi(default = None)]
    pub kind: Option<SteamAppKind>,
}
/// What the library lists beside full games, as Steam types it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SteamAppKind {
    Demo,
    /// A beta its name calls a playtest, as Steam's playtests are named.
    Playtest,
    /// Any other beta, such as an open beta or a game's beta branch.
    Beta,
}
impl From<artwork::AppKind> for SteamAppKind {
    fn from(kind: artwork::AppKind) -> Self {
        match kind {
            artwork::AppKind::Demo => Self::Demo,
            artwork::AppKind::Playtest => Self::Playtest,
            artwork::AppKind::Beta => Self::Beta,
        }
    }
}
impl From<artwork::LibraryArtwork> for SteamGameArtwork {
    fn from(
        artwork::LibraryArtwork {
            cover,
            hero,
            logo,
            header,
            parent,
            kind,
        }: artwork::LibraryArtwork,
    ) -> Self {
        Self {
            cover,
            hero,
            logo,
            header,
            parent,
            kind: kind.map(Into::into),
        }
    }
}
#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamGameDetails {
    pub app_id: u32,
    pub name: String,
    /// The build the game's page shows; none when the account can install
    /// neither.
    pub platform: Option<GamePlatform>,
    pub windows_bytes: u64,
    pub macos_bytes: u64,
}
#[derive(uniffi::Object)]
pub struct SteamSession {
    /// Shared with the account's installations for the whole sign-in.
    pub(crate) account: Arc<AccountSession>,
    /// Where the account's library is kept as it loads.
    library: Option<SteamLibrary>,
}

impl SteamSession {
    /// The account's CM connection of the moment.
    pub(crate) fn client(&self) -> Arc<client::Client> {
        self.account.current()
    }

    /// `app_ids`' achievements in `language`: the library's for a game not
    /// played since they were read, unless `fresh`, else Steam's, a few
    /// games at a time, which the library then keeps. A game Steam did not
    /// answer for is left out; when none could be told, the first failure.
    async fn read_achievements(
        &self,
        app_ids: Vec<u32>,
        language: &str,
        fresh: bool,
    ) -> Result<Vec<GameAchievements>, SteamError> {
        let client = &self.client();
        library_cache::achievements(
            self.library.as_ref().map(SteamLibrary::achievements),
            app_ids,
            language,
            fresh,
            ACHIEVEMENT_READS,
            |app_id| async move {
                client
                    .achievements(app_id, language)
                    .await
                    .map_err(SteamError::from)
            },
        )
        .await
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamProfile {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[uniffi::export(async_runtime = "tokio")]
impl SteamSession {
    /// Keeps every library it loads in `library`, and shows the account to
    /// its friends as `status` says.
    #[uniffi::constructor]
    pub async fn connect(
        credentials: SteamCredentials,
        library: Option<Arc<LibraryCache>>,
        status: crate::OnlineStatus,
    ) -> Result<Arc<Self>, SteamError> {
        // Decode the routing identity before sending the token, then let Steam
        // validate its signature, expiry, revocation and account ownership.
        let token = auth::AuthTokens::restore(credentials.account_name, credentials.refresh_token)?;
        if token.steam_id != credentials.steam_id {
            return Err(auth::AuthError::Protocol.into());
        }
        let library = library.map(|library| library.steam(token.steam_id));
        let account = AccountSession::connect(
            Credentials {
                steam_id: token.steam_id,
                account_name: token.account_name,
                refresh_token: token.refresh_token,
            },
            status.into(),
        )
        .await?;
        Ok(Arc::new(Self { account, library }))
    }
    pub async fn games(&self) -> Result<Vec<SteamGame>, SteamError> {
        let games = self.client().games().await?;
        Ok(match &self.library {
            Some(library) => library.games(games),
            None => games.into_iter().map(Into::into).collect(),
        })
    }
    /// The game's achievements in `language`, such as the game page's: the
    /// library's while the game was not played since they were read, unless
    /// `fresh`, else Steam's, which the library then keeps.
    pub async fn achievements(
        &self,
        app_id: u32,
        language: String,
        fresh: bool,
    ) -> Result<Vec<LibraryAchievement>, SteamError> {
        let read = self
            .read_achievements(vec![app_id], &language, fresh)
            .await?;
        Ok(read
            .into_iter()
            .next()
            .map(|game| game.achievements)
            .unwrap_or_default())
    }
    /// The share of each of `app_ids`' achievements the account unlocked, by
    /// app ID, as `achievements` reads them: none for a game without
    /// achievements or that Steam did not answer for. It fails only when no
    /// game could be told.
    pub async fn achievement_shares(
        &self,
        app_ids: Vec<u32>,
        language: String,
    ) -> Result<HashMap<u32, f64>, SteamError> {
        let read = self.read_achievements(app_ids, &language, false).await?;
        Ok(library_cache::unlocked_shares(read)
            .filter_map(|(id, share)| Some((<steam::Steam as Named>::named(&id)?, share)))
            .collect())
    }
    /// When the account got each game its licenses grant, in seconds since
    /// the Unix epoch, by app ID. It expands every license through PICS.
    pub async fn acquisitions(&self) -> Result<HashMap<u32, u64>, SteamError> {
        let acquired = self.client().acquisitions().await?;
        if let Some(library) = &self.library {
            library.acquisitions(&acquired);
        }
        Ok(acquired
            .into_iter()
            .map(|(app_id, at)| (app_id, u64::from(at)))
            .collect())
    }
    /// The session's state after `after_revision`: its friends and how it
    /// stands with Steam, across its CM connections. No content-query mutex
    /// is held.
    pub async fn next_friends(&self, after_revision: u64) -> crate::SteamFriendsSnapshot {
        self.account.next(after_revision).await.into()
    }

    /// Cookies that sign Steam's sites in as this session's account.
    pub async fn web_cookies(&self) -> Result<Vec<SteamWebCookie>, SteamError> {
        Ok(self
            .client()
            .web_cookies()
            .await?
            .into_iter()
            .map(|cookie| SteamWebCookie {
                domain: cookie.domain.into(),
                name: cookie.name.into(),
                value: cookie.value,
                http_only: cookie.http_only,
            })
            .collect())
    }

    /// Stops reconnecting and ends the CM connection. Also wakes UniFFI
    /// futures, whose Swift wrappers do not cancel Rust awaits.
    pub fn shutdown(&self) {
        self.account.close();
    }
    /// Replaces the CM connection, live or not, and connects again, as on the
    /// player's retry or the Mac's wake. Nothing once Steam asked the account
    /// to sign in again.
    pub fn reconnect(&self) {
        self.account.reconnect();
    }
    pub async fn profile(&self) -> Result<SteamProfile, SteamError> {
        let profile = self.client().profile().await?;
        Ok(SteamProfile {
            display_name: profile.display_name,
            avatar_url: profile.avatar_url,
        })
    }
    /// Explicit diagnostic operation using this already-authenticated session.
    /// It downloads a verified backup; no Cloud mutation or game launch occurs.
    pub async fn cloud_backup(&self, app_id: u32, directory: String) -> Result<String, SteamError> {
        Ok(self
            .client()
            .cloud_backup(app_id, Path::new(&directory))
            .await?)
    }
    /// Returns only Cloud preferences and source revision, never roaming data.
    pub async fn cloud_preferences(&self, app_id: u32) -> Result<String, SteamError> {
        let preferences = self.client().cloud_preferences(app_id).await?;
        serde_json::to_string(&preferences).map_err(|_| SteamError::Failure {
            reason: "Invalid Cloud preference report".into(),
        })
    }
    /// Declared application platforms in batches; an empty platform list means
    /// unknown. Each game's library artwork comes with them, in `language`.
    pub async fn platforms(
        &self,
        app_ids: Vec<u32>,
        language: String,
    ) -> Result<Vec<SteamGamePlatforms>, SteamError> {
        let games = self.client().platforms(&app_ids).await?;
        let platforms = games
            .iter()
            .map(|game| SteamGamePlatforms {
                app_id: game.app_id,
                platforms: game.platforms.clone(),
                artwork: game
                    .pics
                    .as_ref()
                    .map(|pics| artwork::library_artwork(pics, &language).into()),
            })
            .collect();
        if let Some(library) = &self.library {
            library.platforms(games);
        }
        Ok(platforms)
    }
    pub async fn details(
        &self,
        app_id: u32,
        language: String,
    ) -> Result<SteamGameDetails, SteamError> {
        let details = self.client().details(app_id, &language).await?;
        Ok(SteamGameDetails {
            app_id: details.app_id,
            name: details.name,
            platform: details.platform.map(Into::into),
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
            .client()
            .cloud_interruption_probe(app_id, Path::new(&directory))
            .await?)
    }

    pub async fn cloud_sdk_candidates(&self) -> Result<String, SteamError> {
        Ok(self.client().cloud_sdk_candidates().await?)
    }
    pub async fn cloud_autocloud_metadata(&self) -> Result<String, SteamError> {
        Ok(self.client().cloud_autocloud_metadata().await?)
    }
    pub async fn cloud_transfer_probe(
        &self,
        app_id: u32,
        directory: String,
    ) -> Result<String, SteamError> {
        Ok(self
            .client()
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

// A login cookie signs Steam's sites in: deliberately no Debug.
#[derive(Clone, uniffi::Record)]
pub struct SteamWebCookie {
    pub domain: String,
    pub name: String,
    pub value: String,
    pub http_only: bool,
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
