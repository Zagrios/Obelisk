//! Epic's part of a web storefront Account session: how its kit signs in,
//! lists and names games, and what it tells of each.
use crate::{
    GameOffer,
    installations::Named,
    library_cache::{self, LibraryAchievement, PlaytimeUnit},
    web_account::{WebAccountCredentials, WebAccountError, WebGameDetails, WebStore},
};
use epic_kit::{auth, client};
use game_runtime::epic;
use serde_json::Value;
use std::collections::BTreeMap;

impl From<epic_kit::EpicError> for WebAccountError {
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
pub fn epic_authorization_code(input: String) -> Result<String, WebAccountError> {
    auth::authorization_code(&input).map_err(Into::into)
}

impl From<auth::Credentials> for WebAccountCredentials {
    fn from(credentials: auth::Credentials) -> Self {
        Self {
            account_id: credentials.account_id,
            name: credentials.display_name,
            refresh_token: credentials.refresh_token,
        }
    }
}

impl From<WebAccountCredentials> for auth::Credentials {
    fn from(credentials: WebAccountCredentials) -> Self {
        Self {
            account_id: credentials.account_id,
            display_name: credentials.name,
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

impl From<client::GameDetails> for WebGameDetails {
    fn from(details: client::GameDetails) -> Self {
        Self {
            platform: details.platform.map(Into::into),
            installation_bytes: details.installation_bytes,
        }
    }
}

impl WebStore for epic::Epic {
    type Client = client::Client;
    type Game = epic::Store;
    type Product = epic::Product;
    type Listed = client::Game;
    type Shown = EpicGame;

    const CACHE: &'static str = library_cache::EPIC;
    /// Epic counts seconds.
    const PLAYTIME: PlaytimeUnit = PlaytimeUnit::SECONDS;
    /// Sandboxes whose achievements a library reads at once: each takes two
    /// requests.
    const ACHIEVEMENT_READS: usize = 4;

    fn named(name: &str) -> Option<epic::Store> {
        <Self as Named>::named(name)
    }

    fn product(offer: GameOffer) -> Option<epic::Product> {
        Some(<Self as Named>::offer(offer)?.product)
    }

    /// Epic tells playtimes by app name, whatever the sandbox.
    fn playtime_id(game: &epic::Store) -> &str {
        &game.app_name
    }

    /// Epic tells achievements by sandbox, which several games may share.
    fn achievements_id(game: &epic::Store) -> &str {
        &game.namespace
    }

    /// Beside the game: its catalog items, library records and assets.
    fn shown(mut listed: client::Game) -> (EpicGame, Value) {
        let metadata = std::mem::take(&mut listed.metadata);
        (listed.into(), metadata)
    }

    fn id(game: &EpicGame) -> String {
        <Self as Named>::name(&epic::Store {
            namespace: game.namespace.clone(),
            app_name: game.app_name.clone(),
        })
    }

    async fn sign_in(code: &str) -> Result<client::Client, WebAccountError> {
        Ok(client::Client::sign_in(code).await?)
    }

    async fn restore(
        credentials: WebAccountCredentials,
    ) -> Result<client::Client, WebAccountError> {
        Ok(client::Client::restore(credentials.into()).await?)
    }

    async fn credentials(
        client: &client::Client,
    ) -> Result<WebAccountCredentials, WebAccountError> {
        Ok(client.credentials().await?.into())
    }

    async fn games(client: &client::Client) -> Result<Vec<client::Game>, WebAccountError> {
        Ok(client.games().await?)
    }

    /// Every game the account played, in one request.
    async fn playtimes(
        client: &client::Client,
        _: &[epic::Store],
    ) -> Result<BTreeMap<String, u64>, WebAccountError> {
        Ok(client.playtimes().await?)
    }

    async fn achievements(
        client: &client::Client,
        id: &str,
        locale: &str,
    ) -> Result<Vec<LibraryAchievement>, WebAccountError> {
        let achievements = client.achievements(id, locale).await?;
        Ok(achievements.into_iter().map(Into::into).collect())
    }

    /// Epic's builds have no language.
    async fn details(
        client: &client::Client,
        product: &epic::Product,
        _language: &str,
    ) -> Result<WebGameDetails, WebAccountError> {
        Ok(client
            .game_details(
                &product.namespace,
                &product.catalog_item_id,
                &product.app_name,
            )
            .await?
            .into())
    }

    async fn disconnect(client: &client::Client) -> Result<(), WebAccountError> {
        Ok(client.disconnect().await?)
    }

    fn shutdown_observers(client: &client::Client) {
        client.shutdown_observers();
    }

    /// Each address spends a new exchange code of the session.
    async fn web_sign_in_url(
        client: &client::Client,
        redirect: &str,
    ) -> Result<Option<String>, WebAccountError> {
        Ok(Some(client.web_sign_in_url(redirect).await?))
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
