//! Bounded reads and the status mapping every GOG request shares.
use std::{io::Read, time::Duration};

use obelisk_core::transfer::{BodyError, BodySize, CancelToken, read_body};
use reqwest::{RequestBuilder, Response, StatusCode, Url};

use crate::GogError;

/// GOG's services answer a desktop client; some refuse an empty agent.
pub(crate) const USER_AGENT: &str = "GOGGalaxyClient/2.0.80.21 (Windows_NT 10.0.19045) Obelisk";

pub(crate) fn client() -> Result<reqwest::Client, GogError> {
    // Redirects never carry the account's bearer token elsewhere.
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(|_| GogError::Network)
}

pub(crate) async fn bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> Result<T, GogError> {
    serde_json::from_slice(&bounded_bytes(response, limit).await?).map_err(|_| GogError::Protocol)
}

pub(crate) async fn bounded_bytes(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, GogError> {
    read_body(
        response.content_length(),
        response.bytes_stream(),
        BodySize::AtMost(limit as u64),
        &CancelToken::new(),
        |_| {},
    )
    .await
    .map_err(|error| match error {
        BodyError::Network => GogError::Network,
        // An oversized answer is malformed, unlike a chunk of the wrong size.
        BodyError::Size => GogError::Protocol,
        BodyError::Cancelled => GogError::Cancelled,
    })
}

/// A content-system document: zlib-compressed JSON, or plain JSON for the
/// few GOG serves uncompressed.
pub(crate) fn inflate_json<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    limit: u64,
) -> Result<T, GogError> {
    let mut inflated = Vec::new();
    let decoded = flate2::read::ZlibDecoder::new(bytes)
        .take(limit + 1)
        .read_to_end(&mut inflated);
    let json = match decoded {
        Ok(_) if inflated.len() as u64 > limit => return Err(GogError::Protocol),
        Ok(_) => &inflated[..],
        Err(_) => bytes,
    };
    serde_json::from_slice(json).map_err(|_| GogError::Protocol)
}

/// Sends `request` and refuses any answer but a success.
pub(crate) async fn send(request: RequestBuilder) -> Result<Response, GogError> {
    let response = request.send().await.map_err(|_| GogError::Network)?;
    check_status(response.status())?;
    Ok(response)
}

/// Sends the request `request` builds, again after a lost connection, a
/// server error or a rate limit, up to `attempts` times, each `backoff`
/// longer apart; `None` when what it asks for does not exist.
pub(crate) async fn send_retrying(
    request: impl Fn() -> RequestBuilder,
    attempts: u32,
    backoff: Duration,
) -> Result<Option<Response>, GogError> {
    let mut failure = GogError::Network;
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(backoff * attempt).await;
        }
        let Ok(response) = request().send().await else {
            continue;
        };
        match response.status() {
            StatusCode::NOT_FOUND => return Ok(None),
            status if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
                failure = status_error(status);
            }
            status => {
                check_status(status)?;
                return Ok(Some(response));
            }
        }
    }
    Err(failure)
}

pub(crate) fn check_status(status: StatusCode) -> Result<(), GogError> {
    match status.is_success() {
        true => Ok(()),
        false => Err(status_error(status)),
    }
}

fn status_error(status: StatusCode) -> GogError {
    if status == StatusCode::UNAUTHORIZED {
        return GogError::Unauthorized;
    }
    // Response bodies never reach the player: they may echo a token.
    GogError::Service {
        message: match status {
            StatusCode::TOO_MANY_REQUESTS => "backend.gog.rate_limited".into(),
            StatusCode::FORBIDDEN => "backend.gog.forbidden".into(),
            _ => serde_json::json!({
                "key": "backend.gog.http",
                "args": [status.as_u16().to_string()]
            })
            .to_string(),
        },
    }
}

/// Only HTTPS locations without embedded credentials reach the host.
pub(crate) fn safe_https_url(value: &str) -> Option<String> {
    // GOG spells many image locations without their scheme.
    let value = match value.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => value.to_owned(),
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn content_documents_inflate_or_read_as_plain_json() {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(br#"{"a":1}"#).unwrap();
        let compressed = encoder.finish().unwrap();
        let value: serde_json::Value = inflate_json(&compressed, 1024).unwrap();
        assert_eq!(value["a"], 1);
        let value: serde_json::Value = inflate_json(br#"{"a":2}"#, 1024).unwrap();
        assert_eq!(value["a"], 2);
        assert!(matches!(
            inflate_json::<serde_json::Value>(&compressed, 3),
            Err(GogError::Protocol)
        ));
    }

    #[test]
    fn images_gain_their_scheme_and_other_locations_are_refused() {
        assert_eq!(
            safe_https_url("//images.gog-statics.com/a.jpg").as_deref(),
            Some("https://images.gog-statics.com/a.jpg")
        );
        assert_eq!(safe_https_url("http://images.gog.com/a.jpg"), None);
        assert_eq!(safe_https_url("https://user:pw@images.gog.com/a.jpg"), None);
        assert_eq!(safe_https_url("not a url"), None);
    }
}
