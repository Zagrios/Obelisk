//! Account-scoped GOG requests. Token refresh is serialized, while reads
//! release the session lock so disconnect can invalidate them promptly.
use std::{future::Future, sync::Arc, time::Instant};

use obelisk_core::{
    platform::{Platform, Spelling},
    watch::Revisions,
};
use serde::Serializer;
use tokio::sync::Mutex;

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
            tokens: TokenSource {
                api: Arc::new(api),
                session: Arc::new(Mutex::new(Some(session))),
                credential_events: Revisions::new(),
            },
            avatar_url: profile.avatar_url,
            build_choice: Default::default(),
        })
    }

    /// The account's avatar, as its profile shows it.
    pub fn avatar_url(&self) -> Option<String> {
        self.avatar_url.clone()
    }

    pub async fn credentials(&self) -> Result<Credentials, GogError> {
        self.tokens
            .session
            .lock()
            .await
            .as_ref()
            .map(|session| session.credentials.clone())
            .ok_or(GogError::Unauthorized)
    }

    /// Refresh before a long read so the caller can persist a rotated token
    /// before the read starts.
    pub async fn prepare(&self) -> Result<(), GogError> {
        self.tokens.access_token(None).await.map(drop)
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
            Some((platform, size)) => GameDetails {
                platform: Some(platform),
                installation_bytes: size,
            },
            None => GameDetails::default(),
        })
    }

    /// The revision of the credentials now, which grows with each rotation.
    pub fn credentials_revision(&self) -> u64 {
        self.tokens.credential_events.current()
    }

    pub async fn next_credentials_revision(&self, after_revision: u64) -> Option<u64> {
        self.tokens.credential_events.next(after_revision).await
    }

    /// Synchronous cancellation crosses UniFFI even when a Swift task is
    /// suspended. This affects no running content transfer.
    pub fn shutdown_observers(&self) {
        self.tokens.credential_events.stop();
    }

    /// Forgets the session. GOG offers no revocation of a Galaxy token: it
    /// expires within the hour, and the refresh token is never used again.
    pub async fn disconnect(&self) -> Result<(), GogError> {
        self.shutdown_observers();
        self.cancel_download();
        self.tokens.session.lock().await.take();
        Ok(())
    }

    /// As [`TokenSource::with_access_token`], but a result computed for a
    /// session the account discarded meanwhile is never returned.
    pub(crate) async fn with_access_token<T, Fut>(
        &self,
        operation: impl Fn(String) -> Fut,
    ) -> Result<T, GogError>
    where
        Fut: Future<Output = Result<T, GogError>>,
    {
        let result = self.tokens.with_access_token(operation).await;
        if self.tokens.session.lock().await.is_none() {
            return Err(GogError::Unauthorized);
        }
        result
    }
}

/// The account's access token, renewed as it lapses, for work that outlives
/// one call, such as a Transfer's secure links.
#[derive(Clone)]
pub(crate) struct TokenSource {
    api: Arc<Api>,
    session: Arc<Mutex<Option<Session>>>,
    credential_events: Revisions,
}

impl TokenSource {
    /// Runs `operation` with a current access token. When GOG rejects that
    /// token, it is refreshed once and the operation runs again.
    pub(crate) async fn with_access_token<T, Fut>(
        &self,
        operation: impl Fn(String) -> Fut,
    ) -> Result<T, GogError>
    where
        Fut: Future<Output = Result<T, GogError>>,
    {
        let token = self.access_token(None).await?;
        match operation(token.clone()).await {
            Err(GogError::Unauthorized) => operation(self.access_token(Some(&token)).await?).await,
            result => result,
        }
    }

    /// A current access token, renewed when it lapsed or GOG `rejected` it.
    pub(crate) async fn access_token(&self, rejected: Option<&str>) -> Result<String, GogError> {
        // The renewing task owns the refresh, not the caller awaiting it: a
        // dropped caller never loses a token GOG already rotated.
        let tokens = self.clone();
        let rejected = rejected.map(str::to_owned);
        tokio::spawn(async move {
            let mut guard = tokens.session.lock().await;
            let session = guard.as_mut().ok_or(GogError::Unauthorized)?;
            if session.expires_at <= Instant::now()
                || rejected.as_deref() == Some(&session.access_token)
            {
                *session = tokens.api.refresh(&session.credentials).await?;
                tokens.credential_events.advance();
            }
            Ok(session.access_token.clone())
        })
        .await
        .map_err(|_| GogError::Cancelled)?
    }
}
