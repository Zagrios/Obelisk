//! Public news never require, receive or persist account credentials.
use std::sync::OnceLock;

use crate::client::{ClientError, http_client, read_bounded};
use obelisk_core::transfer::BodyError;
use serde::Deserialize;

const MAX_NEWS_BYTES: usize = 2 * 1024 * 1024;

/// One pooled client for every news and store page request instead of a TLS
/// setup per game opened.
pub(crate) fn shared_client() -> Result<reqwest::Client, ClientError> {
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
        .map_err(|_| ClientError::Failure("backend.steam.news.loadFailed".into()))?
        .error_for_status()
        .map_err(|_| ClientError::Failure("backend.steam.news.unavailable".into()))?;
    let bytes = read_bounded(response, MAX_NEWS_BYTES)
        .await
        .map_err(|error| match error {
            BodyError::Size => ClientError::Failure("backend.steam.news.responseTooLarge".into()),
            BodyError::Network | BodyError::Cancelled => {
                ClientError::Failure("backend.steam.news.incompleteResponse".into())
            }
        })?;
    let envelope: Envelope = serde_json::from_slice(&bytes)
        .map_err(|_| ClientError::Failure("backend.steam.news.incompatibleFormat".into()))?;
    if envelope.appnews.appid != app_id {
        return Err(ClientError::Failure("backend.steam.news.wrongGame".into()));
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
    text(value, true)
}

/// A store page's text as plain text: its HTML goes, but its brackets stay,
/// since a store page writes no BBCode.
pub(crate) fn store_text(value: &str) -> String {
    text(value, false)
}

/// Tags that style words within a line: removing one leaves no space, so
/// `<b>Knight</b>!` reads `Knight!`.
const INLINE_TAGS: &[&str] = &["a", "b", "em", "i", "span", "strong", "u"];

fn text(value: &str, bbcode: bool) -> String {
    let mut text = String::new();
    let mut tag = None;
    // The tag's element name, such as `b` for `</b>`, read until its
    // attributes start.
    let mut name = String::new();
    let mut in_name = false;
    for c in value.chars() {
        match (tag, c) {
            (None, '<') | (None, '[') if c == '<' || bbcode => {
                tag = Some(if c == '<' { '>' } else { ']' });
                name.clear();
                in_name = true;
            }
            (Some(end), c) if c == end => {
                tag = None;
                if !INLINE_TAGS.contains(&name.to_ascii_lowercase().as_str()) {
                    text.push(' ');
                }
            }
            (Some(_), '/') if name.is_empty() => {}
            (Some(_), c) if in_name && c.is_ascii_alphanumeric() => name.push(c),
            (Some(_), _) => in_name = false,
            (None, c) => text.push(c),
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
    #[test]
    fn styled_words_keep_their_punctuation() {
        assert_eq!(
            plain_text(r#"<B>Knight</B>! <a href="https://example.com/x">Map</a>.<br/>Next"#),
            "Knight! Map. Next"
        );
        assert_eq!(
            store_text("[Redacted] <i>edition</i>"),
            "[Redacted] edition"
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
