//! GOG's part of a web storefront Account session: how its kit signs in,
//! lists and names games, and what it tells of each.
use crate::{
    GameOffer,
    installations::Named,
    library_cache::{self, LibraryAchievement, PlaytimeUnit},
    web_account::{WebAccountCredentials, WebAccountError, WebGameDetails, WebStore},
};
use game_runtime::gog;
use gog_kit::{auth, client};
use serde_json::Value;
use std::collections::BTreeMap;

impl From<gog_kit::GogError> for WebAccountError {
    fn from(error: gog_kit::GogError) -> Self {
        match error {
            gog_kit::GogError::Network => Self::Network,
            gog_kit::GogError::Unauthorized => Self::Unauthorized,
            gog_kit::GogError::InvalidCode => Self::InvalidCode,
            gog_kit::GogError::Protocol => Self::InvalidResponse,
            gog_kit::GogError::Service { message } => Self::Service { message },
            error => Self::Service {
                message: error.to_string(),
            },
        }
    }
}

#[uniffi::export]
pub fn gog_login_url() -> String {
    auth::login_url()
}

/// Whether the sign-in window reached the page that carries GOG's code.
#[uniffi::export]
pub fn gog_is_login_redirect(url: String) -> bool {
    auth::is_redirect(&url)
}

#[uniffi::export]
pub fn gog_authorization_code(input: String) -> Result<String, WebAccountError> {
    auth::authorization_code(&input).map_err(Into::into)
}

impl From<auth::Credentials> for WebAccountCredentials {
    fn from(credentials: auth::Credentials) -> Self {
        Self {
            account_id: credentials.user_id,
            name: credentials.username,
            refresh_token: credentials.refresh_token,
        }
    }
}

impl From<WebAccountCredentials> for auth::Credentials {
    fn from(credentials: WebAccountCredentials) -> Self {
        Self {
            user_id: credentials.account_id,
            username: credentials.name,
            refresh_token: credentials.refresh_token,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct GogGame {
    pub product_id: String,
    pub title: String,
    /// The portrait cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// The wide artwork the hero and the game's page show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_image_url: Option<String>,
    pub windows: bool,
    pub macos: bool,
    /// In English: gamesdb has no other language.
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub developers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    /// Seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<u64>,
    #[serde(default)]
    pub genres: Vec<String>,
    /// The critics' rating, out of 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u32>,
    /// The languages the game speaks, such as `fr-FR`.
    #[serde(default)]
    pub languages: Vec<String>,
    /// When the game joined the account's library, in seconds since the
    /// Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[uniffi(default = None)]
    pub added_at: Option<u64>,
}

impl From<client::Game> for GogGame {
    fn from(game: client::Game) -> Self {
        let info = game.info;
        Self {
            product_id: game.product_id,
            title: game.title,
            image_url: game.image_url,
            header_image_url: game.header_image_url,
            windows: game.windows,
            macos: game.macos,
            summary: info.summary,
            developers: info.developers,
            publishers: info.publishers,
            release_date: info.release_date,
            genres: info.genres,
            rating: info.rating,
            languages: info.languages,
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
            unlocked_at: value.unlock_time,
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

impl WebStore for gog::Gog {
    type Client = client::Client;
    type Game = String;
    type Product = gog::Product;
    type Listed = client::Game;
    type Shown = GogGame;

    const CACHE: &'static str = library_cache::GOG;
    /// GOG counts minutes.
    const PLAYTIME: PlaytimeUnit = PlaytimeUnit::MINUTES;
    /// Achievement requests a library sends at once: GOG answers one game at
    /// a time.
    const ACHIEVEMENT_READS: usize = 6;

    fn named(name: &str) -> Option<String> {
        <Self as Named>::named(name)
    }

    fn product(offer: GameOffer) -> Option<gog::Product> {
        Some(<Self as Named>::offer(offer)?.product)
    }

    /// GOG tells playtimes and achievements by product.
    fn playtime_id(game: &String) -> &str {
        game
    }

    fn achievements_id(game: &String) -> &str {
        game
    }

    /// Beside the game: its library record and catalog data.
    fn shown(mut listed: client::Game) -> (GogGame, Value) {
        let metadata = std::mem::take(&mut listed.metadata);
        (listed.into(), metadata)
    }

    fn id(game: &GogGame) -> String {
        game.product_id.clone()
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

    /// GOG answers one game at a time: each of `games` is asked.
    async fn playtimes(
        client: &client::Client,
        games: &[String],
    ) -> Result<BTreeMap<String, u64>, WebAccountError> {
        Ok(client.playtimes(games).await?)
    }

    async fn achievements(
        client: &client::Client,
        id: &str,
        locale: &str,
    ) -> Result<Vec<LibraryAchievement>, WebAccountError> {
        let achievements = client.achievements(id, locale).await?;
        Ok(achievements.into_iter().map(Into::into).collect())
    }

    /// `language` names the depots, such as `fr-FR`.
    async fn details(
        client: &client::Client,
        product: &gog::Product,
        language: &str,
    ) -> Result<WebGameDetails, WebAccountError> {
        Ok(client
            .game_details(&product.product_id, language)
            .await?
            .into())
    }

    /// GOG offers no revocation of a Galaxy token: this only forgets the
    /// session.
    async fn disconnect(client: &client::Client) -> Result<(), WebAccountError> {
        Ok(client.disconnect().await?)
    }

    fn shutdown_observers(client: &client::Client) {
        client.shutdown_observers();
    }

    /// The account's avatar, as its GOG profile shows it.
    fn avatar_url(client: &client::Client) -> Option<String> {
        client.avatar_url()
    }
}
