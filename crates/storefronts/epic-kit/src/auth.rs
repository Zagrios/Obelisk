//! Launcher OAuth protocol, independently implemented from the endpoints
//! documented by Legendary's `legendary/api/egs.py`.
use std::time::{Duration, Instant};

use reqwest::{StatusCode, Url};
use serde::Deserialize;

use crate::EpicError;

const CLIENT_ID: &str = "34a02cf8f4414e29b15921876da36f9a";
// Public launcher client credentials, not an account secret.
const CLIENT_SECRET: &str = "daafbccc737745039dffe53d94fc76cf";
const USER_AGENT: &str =
    "UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit";

// Deliberately no Debug: callers store this record in the system Keychain.
#[derive(Clone)]
pub struct Credentials {
    pub account_id: String,
    pub display_name: String,
    pub refresh_token: String,
}

pub fn login_url() -> String {
    // Epic's launcher web flow redirects to localhost with an authorization
    // code. The native login window intercepts it before any localhost request.
    "https://www.epicgames.com/id/login?responseType=code".to_owned()
}

/// Accept the code itself, Epic's JSON response, or the final redirect URL.
/// This only extracts a code; it never opens a URL provided by the caller.
pub fn authorization_code(input: &str) -> Result<String, EpicError> {
    let input = input.trim();
    if input.len() > 32_768 {
        return Err(EpicError::InvalidCode);
    }
    let candidate = if input.starts_with('{') {
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|_| EpicError::InvalidCode)?;
        value["authorizationCode"]
            .as_str()
            .ok_or(EpicError::InvalidCode)?
            .to_owned()
    } else if input.contains("://") {
        let url = Url::parse(input).map_err(|_| EpicError::InvalidCode)?;
        if url.scheme() != "https"
            || !matches!(url.host_str(), Some("epicgames.com" | "www.epicgames.com"))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(EpicError::InvalidCode);
        }
        let codes: Vec<_> = url
            .query_pairs()
            .filter(|(name, _)| name == "code" || name == "authorizationCode")
            .collect();
        if codes.len() != 1 {
            return Err(EpicError::InvalidCode);
        }
        codes[0].1.to_string()
    } else {
        input.to_owned()
    };
    let candidate = candidate.trim();
    if candidate.is_empty()
        || candidate.len() > 2048
        || !candidate
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(EpicError::InvalidCode);
    }
    Ok(candidate.to_owned())
}

pub(crate) struct Session {
    pub credentials: Credentials,
    pub access_token: String,
    pub expires_at: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    account_id: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    expires_in: u64,
}

#[derive(Clone)]
pub(crate) struct Api {
    pub http: reqwest::Client,
    pub account: String,
    pub store: String,
    pub library: String,
    pub launcher: String,
    pub catalog: String,
    pub ecommerce: String,
    pub friends: String,
    pub presence: String,
    pub connect: String,
    pub cloud: String,
}

impl Api {
    pub fn new() -> Result<Self, EpicError> {
        Ok(Self {
            // Redirects are not needed for these APIs and must never carry an
            // OAuth credential to a different endpoint.
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(45))
                .build()
                .map_err(|_| EpicError::Network)?,
            store: "https://launcher.store.epicgames.com".into(),
            friends: "https://friends-public-service-prod.ol.epicgames.com".into(),
            presence: "https://api.epicgames.dev".into(),
            connect: "wss://connect.ol.epicgames.com/".into(),
            cloud: "https://datastorage-public-service-liveegs.live.use1a.on.epicgames.com".into(),
            account: "https://account-public-service-prod03.ol.epicgames.com".into(),
            library: "https://library-service.live.use1a.on.epicgames.com".into(),
            launcher: "https://launcher-public-service-prod06.ol.epicgames.com".into(),
            catalog: "https://catalog-public-service-prod06.ol.epicgames.com".into(),
            ecommerce: "https://ecommerceintegration-public-service-ecomprod02.ol.epicgames.com"
                .into(),
        })
    }

    pub async fn authorize(&self, code: &str) -> Result<Session, EpicError> {
        self.token("authorization_code", "code", code).await
    }

    pub async fn exchange(&self, code: &str, account: &str) -> Result<Session, EpicError> {
        if !valid_secret(code) || account.is_empty() {
            return Err(EpicError::InvalidCode);
        }
        let session = self.token("exchange_code", "exchange_code", code).await?;
        if session.credentials.account_id != account {
            let _ = self.revoke(&session.access_token).await;
            return Err(EpicError::Unauthorized);
        }
        Ok(session)
    }

    pub async fn refresh(&self, credentials: &Credentials) -> Result<Session, EpicError> {
        if !valid_secret(&credentials.refresh_token) || credentials.account_id.is_empty() {
            return Err(EpicError::Unauthorized);
        }
        let session = self
            .token("refresh_token", "refresh_token", &credentials.refresh_token)
            .await?;
        if session.credentials.account_id != credentials.account_id {
            let _ = self.revoke(&session.access_token).await;
            return Err(EpicError::Unauthorized);
        }
        Ok(session)
    }

    async fn token(&self, grant: &str, key: &str, secret: &str) -> Result<Session, EpicError> {
        let response = self
            .http
            .post(format!("{}/account/api/oauth/token", self.account))
            .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
            .form(&[("grant_type", grant), (key, secret), ("token_type", "eg1")])
            .send()
            .await
            .map_err(|_| EpicError::Network)?;
        let status = response.status();
        if matches!(status, StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED) {
            return Err(if grant == "authorization_code" {
                EpicError::InvalidCode
            } else {
                EpicError::Unauthorized
            });
        }
        check_status(status)?;
        let response: TokenResponse = bounded_json(response, 1024 * 1024).await?;
        if !valid_secret(&response.access_token)
            || !valid_secret(&response.refresh_token)
            || response.account_id.is_empty()
            || response.expires_in == 0
        {
            return Err(EpicError::Protocol);
        }
        let lifetime = Duration::from_secs(response.expires_in.saturating_sub(60).max(1));
        let expires_at = Instant::now()
            .checked_add(lifetime)
            .ok_or(EpicError::Protocol)?;
        Ok(Session {
            access_token: response.access_token,
            credentials: Credentials {
                account_id: response.account_id,
                display_name: response.display_name,
                refresh_token: response.refresh_token,
            },
            expires_at,
        })
    }

    pub async fn revoke(&self, access_token: &str) -> Result<(), EpicError> {
        let mut url = Url::parse(&format!(
            "{}/account/api/oauth/sessions/kill/",
            self.account
        ))
        .map_err(|_| EpicError::Protocol)?;
        url.path_segments_mut()
            .map_err(|_| EpicError::Protocol)?
            .pop_if_empty()
            .push(access_token);
        let status = self
            .http
            .delete(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| EpicError::Network)?
            .status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::NOT_FOUND) {
            return Ok(());
        }
        check_status(status)
    }

    pub async fn ownership_token(
        &self,
        access_token: &str,
        account_id: &str,
        namespace: &str,
        catalog_item_id: &str,
    ) -> Result<Vec<u8>, EpicError> {
        let mut url = Url::parse(&self.ecommerce).map_err(|_| EpicError::Protocol)?;
        url.path_segments_mut()
            .map_err(|_| EpicError::Protocol)?
            .clear()
            .extend([
                "ecommerceintegration",
                "api",
                "public",
                "platforms",
                "EPIC",
                "identities",
                account_id,
                "ownershipToken",
            ]);
        let response = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .form(&[("nsCatalogItemId", format!("{namespace}:{catalog_item_id}"))])
            .send()
            .await
            .map_err(|_| EpicError::Network)?;
        check_status(response.status())?;
        let token = bounded_bytes(response, 1024 * 1024).await?;
        if token.is_empty() {
            return Err(EpicError::Protocol);
        }
        Ok(token)
    }

    pub async fn get<T: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        access_token: &str,
    ) -> Result<T, EpicError> {
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| EpicError::Network)?;
        check_status(response.status())?;
        bounded_json(response, 8 * 1024 * 1024).await
    }

    /// Same bounds and status mapping as `get`, for endpoints that take a JSON
    /// body and identify the launcher component making the request.
    pub async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: String,
        access_token: &str,
        agent: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, EpicError> {
        let response = self
            .http
            .post(url)
            .header(reqwest::header::USER_AGENT, agent)
            .bearer_auth(access_token)
            .json(body)
            .send()
            .await
            .map_err(|_| EpicError::Network)?;
        check_status(response.status())?;
        bounded_json(response, 8 * 1024 * 1024).await
    }
}

/// Only HTTPS locations without embedded credentials are handed to the host,
/// for avatars, catalog artwork and achievement icons alike.
pub(crate) fn safe_https_url(value: String) -> Option<String> {
    Url::parse(&value)
        .ok()
        .filter(|url| {
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
        })
        .map(|_| value)
}

/// Embedded credentials and fragments never belong on a URL this crate fetches.
/// Callers layer their own scheme, host and port rules on top of this.
pub(crate) fn credential_free(url: &Url) -> bool {
    url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

pub(crate) async fn bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> Result<T, EpicError> {
    serde_json::from_slice(&bounded_bytes(response, limit).await?).map_err(|_| EpicError::Protocol)
}

pub(crate) async fn bounded_bytes(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, EpicError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(EpicError::Protocol);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| EpicError::Network)? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(EpicError::Protocol);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn valid_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16_384 && !value.chars().any(char::is_whitespace)
}

pub(crate) fn check_status(status: StatusCode) -> Result<(), EpicError> {
    if status.is_success() {
        Ok(())
    } else if status == StatusCode::UNAUTHORIZED {
        Err(EpicError::Unauthorized)
    } else {
        // Do not expose response bodies or reqwest errors: they can contain
        // account credentials and the revocation URL itself contains a token.
        Err(EpicError::Service {
            message: match status {
                StatusCode::TOO_MANY_REQUESTS => "backend.epic.rate_limited".into(),
                StatusCode::FORBIDDEN => "backend.epic.forbidden".into(),
                _ => serde_json::json!({
                    "key": "backend.epic.http",
                    "args": [status.as_u16().to_string()]
                })
                .to_string(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_input_accepts_epic_formats_without_accepting_other_urls() {
        for input in [
            "  abc_123-XYZ \n",
            r#"{"authorizationCode":"abc_123-XYZ","sid":null}"#,
            "https://www.epicgames.com/id/api/redirect?code=abc_123-XYZ",
        ] {
            assert_eq!(authorization_code(input).unwrap(), "abc_123-XYZ");
        }
        for input in [
            "",
            "{broken",
            r#"{"authorizationCode":null}"#,
            "https://example.com/?code=secret",
            "some words",
            "https://www.epicgames.com/?code=one&code=two",
            "https://www.epicgames.com/id/login",
            "code\r\ninjection",
        ] {
            assert!(matches!(
                authorization_code(input),
                Err(EpicError::InvalidCode)
            ));
        }
        let login = Url::parse(&login_url()).unwrap();
        assert_eq!(login.host_str(), Some("www.epicgames.com"));
        assert_eq!(login.path(), "/id/login");
        assert_eq!(login.query(), Some("responseType=code"));
    }
}
