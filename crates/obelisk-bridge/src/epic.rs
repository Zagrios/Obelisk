//! Epic's account seam. Only refresh credentials cross into the macOS Keychain;
//! the account access token stays inside the Rust session.
use std::{collections::HashMap, sync::Arc};

use crate::{
    GamePlatform, LibraryCache,
    library_cache::{self, EpicLibrary, GameAchievements, LibraryAchievement},
};
use epic_kit::{auth, client};

/// Sandboxes whose achievements a library reads at once: each takes two
/// requests.
const ACHIEVEMENT_READS: usize = 4;

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct EpicGame {
    pub app_name: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_image_url: Option<String>,
    pub developer: String,
    pub windows: bool,
    pub macos: bool,
    /// When the game joined the account's library, in seconds since the
    /// Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[uniffi(default = None)]
    pub added_at: Option<u64>,
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
            added_at: game.added_at,
        }
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
            unlocked_at: value
                .unlock_date
                .as_deref()
                .filter(|_| value.unlocked)
                .and_then(obelisk_core::date::unix_seconds),
        }
    }
}

/// What a game's page adds to its library entry: the build it shows before
/// any download, and its size.
#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicGameDetails {
    /// Unknown when Epic could not tell.
    pub platform: Option<GamePlatform>,
    /// Unknown when Epic serves no manifest of the build.
    pub installation_bytes: Option<u64>,
}

impl From<client::GameDetails> for EpicGameDetails {
    fn from(details: client::GameDetails) -> Self {
        Self {
            platform: details.platform.map(Into::into),
            installation_bytes: details.installation_bytes,
        }
    }
}

/// How long the account played one game, as Epic records it.
#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicPlaytime {
    pub app_name: String,
    pub seconds: u64,
}

#[derive(uniffi::Object)]
pub struct EpicSession {
    /// Shared with the Cloud maintenance this session starts.
    pub(crate) client: Arc<client::Client>,
    pub(crate) persistence: Arc<crate::credentials::Persistence>,
    /// Where the account's library is kept as it loads.
    library: Option<EpicLibrary>,
}
impl EpicSession {
    async fn with(
        client: client::Client,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Arc<Self>, EpicError> {
        let library = match library {
            Some(library) => Some(library.epic(&client.credentials().await?.account_id)),
            None => None,
        };
        Ok(Arc::new(Self {
            client: Arc::new(client),
            persistence: Default::default(),
            library,
        }))
    }

    /// `namespaces`' achievements in `locale`: the library's for a game not
    /// played since they were read, unless `fresh`, else Epic's, a few
    /// sandboxes at a time, which the library then keeps. A sandbox Epic did
    /// not answer for is left out; when none could be told, the first
    /// failure.
    async fn read_achievements(
        &self,
        namespaces: Vec<String>,
        locale: &str,
        fresh: bool,
    ) -> Result<Vec<GameAchievements>, EpicError> {
        let client = &self.client;
        library_cache::achievements(
            self.library.as_ref().map(|library| library.achievements()),
            namespaces,
            locale,
            fresh,
            ACHIEVEMENT_READS,
            |namespace| async move {
                client
                    .achievements(&namespace, locale)
                    .await
                    .map_err(EpicError::from)
            },
        )
        .await
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl EpicSession {
    /// Keeps every library it loads in `library`.
    #[uniffi::constructor]
    pub async fn sign_in(
        authorization_code: String,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Arc<Self>, EpicError> {
        Self::with(client::Client::sign_in(&authorization_code).await?, library).await
    }

    /// Keeps every library it loads in `library`.
    #[uniffi::constructor]
    pub async fn restore(
        credentials: EpicCredentials,
        library: Option<Arc<LibraryCache>>,
    ) -> Result<Arc<Self>, EpicError> {
        Self::with(client::Client::restore(credentials.into()).await?, library).await
    }

    pub async fn credentials(&self) -> Result<EpicCredentials, EpicError> {
        Ok(self.client.credentials().await?.into())
    }

    /// Shows the account to its friends as `status` says: from the next
    /// friends connection on, and at once on a live one.
    pub fn set_status(&self, status: crate::OnlineStatus) {
        self.client.set_status(status.into());
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

    /// Saves the account's credentials through `credentials` now and after
    /// every rotation, one save at a time and in order, until the session's
    /// observers stop. Every account call relies on it.
    pub async fn persist(&self, credentials: Arc<dyn crate::CredentialSave>) {
        crate::credentials::persist(self.client.clone(), self.persistence.clone(), credentials);
    }

    pub async fn games(&self) -> Result<Vec<EpicGame>, EpicError> {
        let games = self.client.games().await?;
        Ok(match &self.library {
            Some(library) => library.games(games),
            None => games.into_iter().map(Into::into).collect(),
        })
    }

    /// The game's achievements in `locale`, such as the game page's: the
    /// library's while the game was not played since they were read, unless
    /// `fresh`, else Epic's, which the library then keeps. Epic tells them by
    /// sandbox, which several games may share.
    pub async fn achievements(
        &self,
        namespace: String,
        locale: String,
        fresh: bool,
    ) -> Result<Vec<LibraryAchievement>, EpicError> {
        let read = self
            .read_achievements(vec![namespace], &locale, fresh)
            .await?;
        Ok(read
            .into_iter()
            .next()
            .map(|game| game.achievements)
            .unwrap_or_default())
    }

    /// The share of each of `namespaces`' achievements the account unlocked,
    /// by namespace, as `achievements` reads them: none for a sandbox without
    /// achievements or that Epic did not answer for. It fails only when no
    /// sandbox could be told.
    pub async fn achievement_shares(
        &self,
        namespaces: Vec<String>,
        locale: String,
    ) -> Result<HashMap<String, f64>, EpicError> {
        let read = self.read_achievements(namespaces, &locale, false).await?;
        Ok(library_cache::unlocked_shares(read).collect())
    }

    /// Every game the account played, in one request.
    pub async fn playtimes(&self) -> Result<Vec<EpicPlaytime>, EpicError> {
        let playtimes = self.client.playtimes().await?;
        if let Some(library) = &self.library {
            library.playtimes(&playtimes);
        }
        Ok(playtimes
            .into_iter()
            .map(|(app_name, seconds)| EpicPlaytime { app_name, seconds })
            .collect())
    }

    pub async fn disconnect(&self) -> Result<(), EpicError> {
        self.client.disconnect().await.map_err(Into::into)
    }

    /// Opens Epic's site signed in as the account, then `redirect_url`, an
    /// Epic page. The address holds a single-use code: never keep or log it.
    pub async fn web_sign_in_url(&self, redirect_url: String) -> Result<String, EpicError> {
        Ok(self.client.web_sign_in_url(&redirect_url).await?)
    }

    pub async fn game_details(
        &self,
        namespace: String,
        catalog_item_id: String,
        app_name: String,
    ) -> Result<EpicGameDetails, EpicError> {
        Ok(self
            .client
            .game_details(&namespace, &catalog_item_id, &app_name)
            .await?
            .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlock_date_reads_with_or_without_fractions_and_only_once_unlocked() {
        let achievement = |date: Option<&str>, unlocked| {
            LibraryAchievement::from(client::Achievement {
                id: "test".into(),
                name: "Title".into(),
                description: String::new(),
                icon_url: String::new(),
                hidden: false,
                unlocked,
                unlock_date: date.map(String::from),
            })
            .unlocked_at
        };
        assert_eq!(
            achievement(Some("2022-09-21T12:00:00Z"), true),
            Some(1_663_761_600)
        );
        assert_eq!(
            achievement(Some("2022-09-21T12:00:00.000Z"), true),
            Some(1_663_761_600)
        );
        assert_eq!(achievement(Some("not-a-date"), true), None);
        assert_eq!(achievement(None, true), None);
        assert_eq!(achievement(Some("2022-09-21T12:00:00Z"), false), None);
    }
}
