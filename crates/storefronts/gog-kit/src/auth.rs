//! GOG Galaxy's OAuth protocol, as heroic-gogdl's `auth.py` and Heroic's
//! `user.ts` use it: a web sign-in returns a code to `embed.gog.com`, which
//! the Galaxy client's public credentials exchange for tokens.
use std::time::{Duration, Instant};

use obelisk_core::oauth_session::{OAuth, SessionError, Tokens};
use reqwest::{StatusCode, Url};
use serde::Deserialize;

use crate::{
    GogError,
    http::{bounded_json, check_status, send},
};

/// The Galaxy client's public credentials, not an account secret.
pub(crate) const CLIENT_ID: &str = "46899977096215655";
const CLIENT_SECRET: &str = "9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9";
const REDIRECT_URI: &str = "https://embed.gog.com/on_login_success?origin=client";
const AUTH: &str = "https://auth.gog.com";

// Deliberately no Debug: callers store this record in the system Keychain.
#[derive(Clone)]
pub struct Credentials {
    /// The Galaxy user id every account call names.
    pub user_id: String,
    pub username: String,
    pub refresh_token: String,
}

/// The page that signs the player in. GOG then redirects to `REDIRECT_URI`
/// with the code; the native login window intercepts it there.
pub fn login_url() -> String {
    let mut url = Url::parse(&format!("{AUTH}/auth")).expect("a valid URL");
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("layout", "galaxy");
    url.into()
}

/// Whether `url` is where GOG returns the code: its page itself never loads.
pub fn is_redirect(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("embed.gog.com")
            && url.port().is_none()
            && url.path() == "/on_login_success"
    })
}

/// Accepts the code itself or the redirect URL that carries it. This only
/// extracts a code; it never opens a URL provided by the caller.
pub fn authorization_code(input: &str) -> Result<String, GogError> {
    let input = input.trim();
    if input.len() > 32_768 {
        return Err(GogError::InvalidCode);
    }
    let candidate = if input.contains("://") {
        let url = Url::parse(input).map_err(|_| GogError::InvalidCode)?;
        if !is_redirect(input)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(GogError::InvalidCode);
        }
        let codes: Vec<_> = url
            .query_pairs()
            .filter(|(name, _)| name == "code")
            .collect();
        if codes.len() != 1 {
            return Err(GogError::InvalidCode);
        }
        codes[0].1.to_string()
    } else {
        input.to_owned()
    };
    if !valid_code(&candidate) {
        return Err(GogError::InvalidCode);
    }
    Ok(candidate)
}

fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 4096
        && code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// What GOG answers a sign-in or a renewal.
pub(crate) type Session = Tokens<Credentials>;

impl OAuth for Api {
    type Credentials = Credentials;
    type Error = GogError;

    async fn refresh(&self, credentials: &Credentials) -> Result<Session, GogError> {
        Api::refresh(self, credentials).await
    }
}

impl SessionError for GogError {
    fn unauthorized() -> Self {
        Self::Unauthorized
    }

    fn cancelled() -> Self {
        Self::Cancelled
    }

    fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    user_id: String,
    expires_in: u64,
}

/// The account's public profile, as Heroic's `user.ts` reads it.
#[derive(Deserialize)]
struct UserProfile {
    #[serde(default)]
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    avatar: Avatar,
}

#[derive(Default, Deserialize)]
struct Avatar {
    #[serde(default)]
    medium_2x: Option<String>,
}

/// The signed-in account's name and avatar.
pub(crate) struct Profile {
    pub username: String,
    pub avatar_url: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Api {
    pub http: reqwest::Client,
    pub auth: String,
    pub users: String,
    pub embed: String,
    pub library: String,
    pub gameplay: String,
    pub api: String,
    pub gamesdb: String,
    pub content: String,
    pub cdn: String,
}

impl Api {
    pub fn new() -> Result<Self, GogError> {
        Ok(Self {
            http: crate::http::client()?,
            auth: AUTH.into(),
            users: "https://users.gog.com".into(),
            embed: "https://embed.gog.com".into(),
            library: "https://galaxy-library.gog.com".into(),
            gameplay: "https://gameplay.gog.com".into(),
            api: "https://api.gog.com".into(),
            gamesdb: "https://gamesdb.gog.com".into(),
            content: "https://content-system.gog.com".into(),
            cdn: "https://gog-cdn-fastly.gog.com".into(),
        })
    }

    /// Signs in with the code the web sign-in returned.
    pub async fn authorize(&self, code: &str) -> Result<Session, GogError> {
        if !valid_code(code) {
            return Err(GogError::InvalidCode);
        }
        self.token(
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", REDIRECT_URI),
            ],
            GogError::InvalidCode,
        )
        .await
    }

    pub async fn refresh(&self, credentials: &Credentials) -> Result<Session, GogError> {
        if !valid_secret(&credentials.refresh_token) || credentials.user_id.is_empty() {
            return Err(GogError::Unauthorized);
        }
        let mut session = self
            .token(
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &credentials.refresh_token),
                ],
                GogError::Unauthorized,
            )
            .await?;
        if session.credentials.user_id != credentials.user_id {
            return Err(GogError::Unauthorized);
        }
        session
            .credentials
            .username
            .clone_from(&credentials.username);
        Ok(session)
    }

    /// The session `grant` opens, or `refused` when GOG refuses the grant.
    async fn token(&self, grant: &[(&str, &str)], refused: GogError) -> Result<Session, GogError> {
        let response = self
            .http
            .get(format!("{}/token", self.auth))
            .query(&[("client_id", CLIENT_ID), ("client_secret", CLIENT_SECRET)])
            .query(grant)
            .send()
            .await
            .map_err(|_| GogError::Network)?;
        let status = response.status();
        if matches!(status, StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED) {
            return Err(refused);
        }
        check_status(status)?;
        let response: TokenResponse = bounded_json(response, 1024 * 1024).await?;
        if !valid_secret(&response.access_token)
            || !valid_secret(&response.refresh_token)
            || !valid_user_id(&response.user_id)
            || response.expires_in == 0
        {
            return Err(GogError::Protocol);
        }
        let lifetime = Duration::from_secs(response.expires_in.saturating_sub(60).max(1));
        let expires_at = Instant::now()
            .checked_add(lifetime)
            .ok_or(GogError::Protocol)?;
        Ok(Session {
            access_token: response.access_token,
            credentials: Credentials {
                user_id: response.user_id,
                username: String::new(),
                refresh_token: response.refresh_token,
            },
            expires_at,
        })
    }

    /// The account's name and avatar, which the token response lacks.
    pub async fn profile(&self, session: &Session) -> Result<Profile, GogError> {
        let url = crate::library::endpoint(&self.users, &["users", &session.credentials.user_id])?;
        let profile: UserProfile = self.get(url, &session.access_token).await?;
        if !profile.id.is_empty() && profile.id != session.credentials.user_id {
            return Err(GogError::Unauthorized);
        }
        Ok(Profile {
            username: profile.username,
            avatar_url: profile
                .avatar
                .medium_2x
                .as_deref()
                .and_then(crate::http::safe_https_url),
        })
    }

    pub async fn get<T: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        access_token: &str,
    ) -> Result<T, GogError> {
        let response = send(self.http.get(url).bearer_auth(access_token)).await?;
        bounded_json(response, 16 * 1024 * 1024).await
    }

    /// A request that carries no account credential: public catalog data.
    pub async fn get_public<T: serde::de::DeserializeOwned>(
        &self,
        url: Url,
    ) -> Result<T, GogError> {
        bounded_json(send(self.http.get(url)).await?, 16 * 1024 * 1024).await
    }
}

fn valid_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16_384 && !value.chars().any(char::is_whitespace)
}

/// Galaxy user ids are decimal numbers.
pub fn valid_user_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 32 && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_input_accepts_gog_formats_without_accepting_other_urls() {
        for input in [
            "  abc_123-XYZ \n",
            "https://embed.gog.com/on_login_success?origin=client&code=abc_123-XYZ",
        ] {
            assert_eq!(authorization_code(input).unwrap(), "abc_123-XYZ");
        }
        for input in [
            "",
            "https://example.com/on_login_success?code=secret",
            "http://embed.gog.com/on_login_success?code=secret",
            "https://embed.gog.com/other?code=secret",
            "https://embed.gog.com/on_login_success?code=one&code=two",
            "https://embed.gog.com/on_login_success?origin=client",
            "some words",
            "code\r\ninjection",
        ] {
            assert!(
                matches!(authorization_code(input), Err(GogError::InvalidCode)),
                "{input}"
            );
        }
    }

    #[test]
    fn the_login_page_returns_to_the_galaxy_redirect() {
        let login = Url::parse(&login_url()).unwrap();
        assert_eq!(login.host_str(), Some("auth.gog.com"));
        assert_eq!(login.path(), "/auth");
        let query: Vec<_> = login.query_pairs().collect();
        assert!(query.contains(&("client_id".into(), CLIENT_ID.into())));
        assert!(query.contains(&("redirect_uri".into(), REDIRECT_URI.into())));
        assert!(query.contains(&("response_type".into(), "code".into())));
        assert!(is_redirect(REDIRECT_URI));
        assert!(!is_redirect("https://embed.gog.com/account"));
    }
}
