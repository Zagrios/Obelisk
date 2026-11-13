//! Account-scoped GOG requests. Token refresh is serialized, while reads
//! release the session lock so disconnect can invalidate them promptly.
use std::{future::Future, sync::Arc};

use obelisk_core::{
    oauth_session::OAuthSession,
    platform::{Platform, Spelling},
    transfer::CdnClient,
};
use serde::Serializer;

use crate::{
    GogError,
    auth::{self, Api, Credentials, Session},
};

mod download;
mod gameplay;
mod launch;
pub use crate::library::{Game, GameDetails, GameInfo};
pub use download::{Installation, TransferProgress, TransferState, installation_directory_name};
pub use gameplay::Achievement;
pub use launch::{InstalledLaunch, Prerequisite, installed_launch};

/// How Obelisk names GOG's two desktop builds in its receipts, folders and
/// ids; the content system's "osx" stays inside `content_platform`.
pub const PLATFORM: Spelling = Spelling::new("macos", "windows");

/// Writes a platform as Obelisk spells GOG's builds.
pub fn write_platform<S: Serializer>(
    platform: &Platform,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    PLATFORM.serialize(*platform, serializer)
}

/// GOG product ids are decimal numbers; they name URLs and folders.
pub fn valid_product_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// The content system's name for a build.
pub(crate) fn content_platform(platform: Platform) -> &'static str {
    match platform {
        Platform::Mac => "osx",
        Platform::Windows => "windows",
    }
}

pub struct Client {
    downloads: download::Downloads,
    tokens: TokenSource,
    /// How the Mac pick and every download reach GOG's CDN for chunks.
    cdn: CdnClient,
    avatar_url: Option<String>,
    /// Each Mac build's verdict, by product, build and language, and the
    /// depot items the last native one read, for its download.
    build_choice: download::BuildChoice,
}

impl Client {
    /// Signs in with the code the web sign-in returned.
    pub async fn sign_in(input: &str) -> Result<Self, GogError> {
        let code = auth::authorization_code(input)?;
        let api = Api::new()?;
        let session = api.authorize(&code).await?;
        Self::from_session(api, session).await
    }

    pub async fn restore(credentials: Credentials) -> Result<Self, GogError> {
        let api = Api::new()?;
        let session = api.refresh(&credentials).await?;
        Self::from_session(api, session).await
    }

    /// The session's account, named from its profile.
    async fn from_session(api: Api, mut session: Session) -> Result<Self, GogError> {
        let profile = api.profile(&session).await?;
        if !profile.username.is_empty() {
            session.credentials.username = profile.username;
        }
        Ok(Self {
            downloads: download::Downloads::default(),
            cdn: CdnClient::new(api.http.clone()),
            tokens: TokenSource::new(api, session),
            avatar_url: profile.avatar_url,
            build_choice: Default::default(),
        })
    }

    /// The account's avatar, as its profile shows it.
    pub fn avatar_url(&self) -> Option<String> {
        self.avatar_url.clone()
    }

    pub async fn credentials(&self) -> Result<Credentials, GogError> {
        self.tokens.session.credentials().await
    }

    /// Refresh before a long read so the caller can persist a rotated token
    /// before the read starts.
    pub async fn prepare(&self) -> Result<(), GogError> {
        self.tokens.session.access_token(None).await.map(drop)
    }

    /// Every game the account owns, with its artwork and platforms.
    pub async fn games(&self) -> Result<Vec<Game>, GogError> {
        let user_id = self.credentials().await?.user_id;
        let user_id = user_id.as_str();
        self.with_access_token(|token| async move {
            crate::library::games(&self.tokens.api, &token, user_id).await
        })
        .await
    }

    /// What a game's page adds to its library entry, with the depots of
    /// `language`, such as `fr-FR`: the build it shows before any download,
    /// the Mac build unless a download found that its game does not run
    /// natively, and its size. Only `platform` reads the build's files.
    pub async fn game_details(
        &self,
        product_id: &str,
        language: &str,
    ) -> Result<GameDetails, GogError> {
        if !valid_product_id(product_id) {
            return Err(GogError::Protocol);
        }
        let shown = self
            .with_access_token(|token| async move {
                // Re-raise so the funnel refreshes the token once.
                match self.shown_build(&token, product_id, language).await {
                    Err(GogError::Unauthorized) => Err(GogError::Unauthorized),
                    shown => Ok(shown.ok()),
                }
            })
            .await?;
        Ok(match shown {
            Some((platform, both_builds, size)) => GameDetails {
                platform: Some(platform),
                both_builds,
                installation_bytes: size,
            },
            None => GameDetails::default(),
        })
    }

    /// The revision of the credentials now, which grows with each rotation.
    pub fn credentials_revision(&self) -> u64 {
        self.tokens.session.credentials_revision()
    }

    pub async fn next_credentials_revision(&self, after_revision: u64) -> Option<u64> {
        self.tokens
            .session
            .next_credentials_revision(after_revision)
            .await
    }

    /// Synchronous cancellation crosses UniFFI even when a Swift task is
    /// suspended. This affects no running content transfer.
    pub fn shutdown_observers(&self) {
        self.tokens.session.stop_credentials_watch();
    }

    /// Forgets the session. GOG offers no revocation of a Galaxy token: it
    /// expires within the hour, and the refresh token is never used again.
    pub async fn disconnect(&self) -> Result<(), GogError> {
        self.shutdown_observers();
        self.cancel_download();
        self.tokens.session.discard().await;
        Ok(())
    }

    /// Runs `operation` with a current access token, once more when GOG
    /// rejects that token. A result computed for a session the account
    /// discarded meanwhile is never returned.
    pub(crate) async fn with_access_token<T, Fut>(
        &self,
        operation: impl Fn(String) -> Fut,
    ) -> Result<T, GogError>
    where
        Fut: Future<Output = Result<T, GogError>>,
    {
        self.tokens.session.with_access_token(operation).await
    }
}

/// The account's access token, renewed as it lapses, for work that outlives
/// one call, such as a Transfer's secure links.
#[derive(Clone)]
pub(crate) struct TokenSource {
    api: Arc<Api>,
    session: OAuthSession<Api>,
}

impl TokenSource {
    /// The source of the account `session` signed in.
    fn new(api: Api, session: Session) -> Self {
        Self {
            session: OAuthSession::new(api.clone(), session),
            api: Arc::new(api),
        }
    }

    /// As [`Client::with_access_token`], but its result is returned though
    /// the account discarded its session meanwhile: a Transfer tells its
    /// cancellation first, then checks the session itself.
    pub(crate) async fn with_access_token<T, Fut>(
        &self,
        operation: impl Fn(String) -> Fut,
    ) -> Result<T, GogError>
    where
        Fut: Future<Output = Result<T, GogError>>,
    {
        self.session.with_access_token_detached(operation).await
    }
}

#[cfg(test)]
mod tests;
