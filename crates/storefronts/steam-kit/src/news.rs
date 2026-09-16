//! Public news never require, receive or persist account credentials.
use std::sync::OnceLock;

use crate::client::{BodyError, ClientError, http_client, read_bounded};
use serde::Deserialize;

const MAX_NEWS_BYTES: usize = 2 * 1024 * 1024;

/// One pooled client for every news request instead of a TLS setup per game opened.
fn shared_client() -> Result<reqwest::Client, ClientError> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = http_client()?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

#[derive(Clone, Debug)]
pub struct NewsItem {
    pub id: String,
    pub title: String,
    pub url: String,
    pub excerpt: String,
    pub date: u64,
    pub source: String,
}

pub async fn for_app(app_id: u32) -> Result<Vec<NewsItem>, ClientError> {
    let response = shared_client()?
        .get("https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/")
        .query(&[
            ("appid", app_id.to_string()),
            ("count", "8".into()),
            ("maxlength", "1500".into()),
            ("feeds", "steam_community_announcements".into()),
            ("format", "json".into()),
        ])
        .send()
        .await
        .map_err(|_| ClientError("backend.steam.news.loadFailed".into()))?
        .error_for_status()
        .map_err(|_| ClientError("backend.steam.news.unavailable".into()))?;
    let bytes = read_bounded(response, MAX_NEWS_BYTES)
        .await
        .map_err(|error| match error {
            BodyError::Network => ClientError("backend.steam.news.incompleteResponse".into()),
            BodyError::TooLarge => ClientError("backend.steam.news.responseTooLarge".into()),
        })?;
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|_| ClientError("backend.steam.news.incompatibleFormat".into()))?;
    if envelope.appnews.appid != app_id {
        return Err(ClientError("backend.steam.news.wrongGame".into()));
    }
    let mut items: Vec<_> = envelope
        .appnews
        .newsitems
        .into_iter()
        .filter_map(|item| {
            let url = reqwest::Url::parse(&item.url).ok()?;
            if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
                return None;
            }
            Some(NewsItem {
                id: item.gid,
                title: plain_text(&item.title),
                url: item.url,
                excerpt: plain_text(&item.contents),
                date: item.date,
                source: item.feedlabel,
            })
        })
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.date));
    Ok(items)
}

#[derive(Deserialize)]
struct Envelope {
    appnews: AppNews,
}
#[derive(Deserialize)]
struct AppNews {
    appid: u32,
    #[serde(default)]
    newsitems: Vec<WireNews>,
}
#[derive(Deserialize)]
struct WireNews {
    gid: String,
    title: String,
    url: String,
    #[serde(default)]
    contents: String,
    date: u64,
    #[serde(default)]
    feedlabel: String,
}

// News are plain excerpts. No remote HTML, script or BBCode is executed by the app.
fn plain_text(value: &str) -> String {
    let mut text = String::new();
    let mut tag = None;
    for c in value.chars() {
        match (tag, c) {
            (None, '<') => tag = Some('>'),
            (None, '[') => tag = Some(']'),
            (Some(end), c) if c == end => {
                tag = None;
                text.push(' ');
            }
            (None, c) => text.push(c),
            _ => {}
        }
    }
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "’")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn excerpts_do_not_render_remote_markup() {
        assert_eq!(
            plain_text("[h1]Patch[/h1]<p>Two &amp; three</p>"),
            "Patch Two & three"
        );
    }
    #[tokio::test]
    #[ignore = "contacts the public Steam news API"]
    async fn live_news() {
        let items = for_app(620).await.unwrap();
        assert!(!items.is_empty());
        assert!(items.iter().all(|item| item.url.starts_with("https://")));
    }
}
