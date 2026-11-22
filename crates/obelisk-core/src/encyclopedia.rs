//! What Wikipedia tells of a game: what it is, in the player's language where
//! Wikipedia has it, else in English, and how it was made, in English, where
//! most of that history is written. Obelisk finds the game's article through
//! Wikidata by a storefront's ID for it, never by its name, so it shows a
//! game's own article or none. The text is Wikipedia's, under CC BY-SA 4.0:
//! a page that shows it credits Wikipedia and links the article.
//!
//! The requests carry nothing of the player's but the game's ID and the
//! language.
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::transfer::{BodySize, CancelToken, read_body};

/// A whole English article in plain text stays well below this.
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
/// How much of the history a page shows, cut at a paragraph's end.
const MAX_HISTORY_CHARACTERS: usize = 16_000;

/// A game as a storefront names it, which Wikidata links to the game's item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameKey {
    /// A Steam app ID: Wikidata's property P1733.
    Steam(u32),
}

impl GameKey {
    fn statement(self) -> String {
        match self {
            GameKey::Steam(app_id) => format!("haswbstatement:P1733={app_id}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Article {
    /// What the game is: the opening of its article, as plain text.
    pub summary: String,
    /// The language the summary is written in, such as `it` or `en`.
    pub summary_language: String,
    /// The article the summary comes from.
    pub summary_url: String,
    /// How the game was made, from its English article's development
    /// section; empty when the article has none.
    pub history: Vec<HistorySection>,
    /// The English article the history comes from.
    pub history_url: Option<String>,
}

/// A part of a game's development history: its heading, none for the part
/// before the first one, then its paragraphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySection {
    pub heading: Option<String>,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EncyclopediaError {
    #[error("Wikipedia could not be reached")]
    Unreachable,
    #[error("Wikipedia answered in a way Obelisk does not read")]
    Incompatible,
}

/// What Wikipedia tells of `game`, its summary in `language`, an ISO 639
/// code such as `it`, where Wikipedia has its article in it; none when no
/// Wikidata item names the game, or its item has no article.
pub async fn article(game: GameKey, language: &str) -> Result<Option<Article>, EncyclopediaError> {
    let language = wiki_language(language);
    let Some(titles) = titles(game, &language).await? else {
        return Ok(None);
    };
    let (summary_language, summary_title) = match (&titles.local, &titles.english) {
        (Some(local), _) => (language.as_str(), local.as_str()),
        (None, Some(english)) => ("en", english.as_str()),
        (None, None) => return Ok(None),
    };
    let Some((summary, summary_url)) = summary(summary_language, summary_title).await? else {
        return Ok(None);
    };
    let (history, history_url) = match &titles.english {
        Some(english) => {
            let (history, url) = history(english).await?;
            (history, Some(url))
        }
        None => (Vec::new(), None),
    };
    Ok(Some(Article {
        summary,
        summary_language: summary_language.into(),
        summary_url,
        history_url: history_url.filter(|_| !history.is_empty()),
        history,
    }))
}

/// A Wikipedia subdomain for a language tag: `pt-BR` is `pt`, `zh-Hans`
/// is `zh`. Anything else reads as English.
fn wiki_language(tag: &str) -> String {
    let primary = tag
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if (2..=3).contains(&primary.len()) && primary.chars().all(|c| c.is_ascii_lowercase()) {
        primary
    } else {
        "en".into()
    }
}

struct Titles {
    local: Option<String>,
    english: Option<String>,
}

/// The game's article titles, from the first Wikidata item that names the
/// game and has an article in either language.
async fn titles(game: GameKey, language: &str) -> Result<Option<Titles>, EncyclopediaError> {
    let found = get(
        "https://www.wikidata.org/w/api.php",
        &[
            ("action", "query"),
            ("list", "search"),
            ("srsearch", &game.statement()),
            ("srnamespace", "0"),
            ("srlimit", "5"),
            ("format", "json"),
            ("formatversion", "2"),
        ],
    )
    .await?;
    let items: Vec<String> = found["query"]["search"]
        .as_array()
        .ok_or(EncyclopediaError::Incompatible)?
        .iter()
        .filter_map(|hit| hit["title"].as_str())
        .filter(|title| title.starts_with('Q') && title[1..].chars().all(|c| c.is_ascii_digit()))
        .map(String::from)
        .collect();
    if items.is_empty() {
        return Ok(None);
    }
    let local_site = format!("{language}wiki");
    let entities = get(
        "https://www.wikidata.org/w/api.php",
        &[
            ("action", "wbgetentities"),
            ("ids", &items.join("|")),
            ("props", "sitelinks"),
            ("sitefilter", &format!("enwiki|{local_site}")),
            ("format", "json"),
            ("formatversion", "2"),
        ],
    )
    .await?;
    let link = |item: &str, site: &str| {
        entities["entities"][item]["sitelinks"][site]["title"]
            .as_str()
            .map(String::from)
    };
    Ok(items.iter().find_map(|item| {
        let titles = Titles {
            local: (language != "en")
                .then(|| link(item, &local_site))
                .flatten(),
            english: link(item, "enwiki"),
        };
        (titles.local.is_some() || titles.english.is_some()).then_some(titles)
    }))
}

/// The article's opening as plain text, and its address; none for a
/// disambiguation page.
async fn summary(
    language: &str,
    title: &str,
) -> Result<Option<(String, String)>, EncyclopediaError> {
    #[derive(Deserialize)]
    struct Summary {
        #[serde(rename = "type", default)]
        kind: String,
        #[serde(default)]
        extract: String,
        content_urls: Option<ContentUrls>,
    }
    #[derive(Deserialize)]
    struct ContentUrls {
        desktop: Option<Page>,
    }
    #[derive(Deserialize)]
    struct Page {
        page: String,
    }
    let mut url = reqwest::Url::parse(&format!(
        "https://{language}.wikipedia.org/api/rest_v1/page/summary/"
    ))
    .map_err(|_| EncyclopediaError::Incompatible)?;
    url.path_segments_mut()
        .map_err(|_| EncyclopediaError::Incompatible)?
        .pop_if_empty()
        .push(&title.replace(' ', "_"));
    let page: Summary = serde_json::from_value(get(url.as_str(), &[]).await?)
        .map_err(|_| EncyclopediaError::Incompatible)?;
    let extract = page.extract.trim().to_string();
    if page.kind == "disambiguation" || extract.is_empty() {
        return Ok(None);
    }
    let address = page
        .content_urls
        .and_then(|urls| urls.desktop)
        .map(|desktop| desktop.page)
        .filter(|address| address.starts_with("https://"))
        .unwrap_or_else(|| article_url(language, title));
    Ok(Some((extract, address)))
}

/// The development section of the English article `title`, and the
/// article's address.
async fn history(title: &str) -> Result<(Vec<HistorySection>, String), EncyclopediaError> {
    let page = get(
        "https://en.wikipedia.org/w/api.php",
        &[
            ("action", "query"),
            ("prop", "extracts"),
            ("explaintext", "1"),
            ("exsectionformat", "wiki"),
            ("redirects", "1"),
            ("titles", title),
            ("format", "json"),
            ("formatversion", "2"),
        ],
    )
    .await?;
    let extract = page["query"]["pages"][0]["extract"]
        .as_str()
        .unwrap_or_default();
    Ok((development(extract), article_url("en", title)))
}

fn article_url(language: &str, title: &str) -> String {
    let mut url = reqwest::Url::parse(&format!("https://{language}.wikipedia.org/wiki/"))
        .expect("a Wikipedia address parses");
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().push(&title.replace(' ', "_"));
    }
    url.into()
}

/// The section of a plain-text article, its headings written `== Name ==`,
/// that tells how the game was made: the first top-level one named for its
/// development or production, with its subsections, up to the next
/// top-level one.
fn development(extract: &str) -> Vec<HistorySection> {
    let mut sections: Vec<HistorySection> = Vec::new();
    let mut inside = false;
    let mut length = 0;
    for line in extract.lines().map(str::trim) {
        if let Some(name) = heading(line, 2) {
            if inside {
                break;
            }
            let name = name.to_ascii_lowercase();
            inside = name.starts_with("development") || name.starts_with("production");
            if inside {
                sections.push(HistorySection {
                    heading: None,
                    paragraphs: Vec::new(),
                });
            }
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(name) = (3..=6).find_map(|level| heading(line, level)) {
            sections.push(HistorySection {
                heading: Some(name.into()),
                paragraphs: Vec::new(),
            });
        } else if !line.is_empty() {
            if length + line.len() > MAX_HISTORY_CHARACTERS && length > 0 {
                break;
            }
            length += line.len();
            if let Some(section) = sections.last_mut() {
                section.paragraphs.push(line.into());
            }
        }
    }
    sections.retain(|section| !section.paragraphs.is_empty());
    sections
}

/// The name of a heading line of `level`, written with as many `=` on
/// each side.
fn heading(line: &str, level: usize) -> Option<&str> {
    let marks = "=".repeat(level);
    let name = line.strip_prefix(&marks)?.strip_suffix(&marks)?;
    // `== Name ==` is level 2, not a level 3 heading's inside.
    (!name.starts_with('=') && !name.ends_with('='))
        .then(|| name.trim())
        .filter(|name| !name.is_empty())
}

fn client() -> Result<reqwest::Client, EncyclopediaError> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    // Wikimedia asks clients to name themselves and a way to reach them.
    let client = reqwest::Client::builder()
        .user_agent(concat!(
            "Obelisk/",
            env!("CARGO_PKG_VERSION"),
            " (https://github.com/Zagrios/Obelisk)"
        ))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| EncyclopediaError::Unreachable)?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

async fn get(url: &str, query: &[(&str, &str)]) -> Result<Value, EncyclopediaError> {
    let response = client()?
        .get(url)
        .query(query)
        .send()
        .await
        .map_err(|_| EncyclopediaError::Unreachable)?
        .error_for_status()
        .map_err(|_| EncyclopediaError::Unreachable)?;
    let bytes = read_body(
        response.content_length(),
        response.bytes_stream(),
        BodySize::AtMost(MAX_RESPONSE_BYTES),
        &CancelToken::new(),
        |_| {},
    )
    .await
    .map_err(|_| EncyclopediaError::Unreachable)?;
    serde_json::from_slice(&bytes).map_err(|_| EncyclopediaError::Incompatible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_development_section_reads_with_its_subsections() {
        let extract = "Portal 2 is a game.\n\n\n== Gameplay ==\nYou place portals.\n\n\n\
            == Development ==\nWork began after Portal.\nThe team grew.\n\n\n\
            === Design ===\nPuzzles came first.\n\n\n=== Empty ===\n\n\n\
            == Release ==\nIt shipped in 2011.";
        assert_eq!(
            development(extract),
            vec![
                HistorySection {
                    heading: None,
                    paragraphs: vec!["Work began after Portal.".into(), "The team grew.".into()],
                },
                HistorySection {
                    heading: Some("Design".into()),
                    paragraphs: vec!["Puzzles came first.".into()],
                },
            ]
        );
    }

    #[test]
    fn production_counts_and_an_article_without_either_has_no_history() {
        assert_eq!(
            development("== Production and release ==\nShot in 1998.")[0].paragraphs,
            vec!["Shot in 1998."]
        );
        assert!(development("== Plot ==\nA story.\n== Reception ==\nPraise.").is_empty());
    }

    #[test]
    fn a_long_history_ends_at_a_paragraph() {
        let paragraph = "word ".repeat(2_000);
        let extract =
            format!("== Development ==\n{paragraph}\n{paragraph}\n{paragraph}\n{paragraph}");
        let history = development(&extract);
        let length: usize = history
            .iter()
            .flat_map(|section| &section.paragraphs)
            .map(String::len)
            .sum();
        assert!(length <= MAX_HISTORY_CHARACTERS && length > 0);
    }

    #[test]
    fn languages_read_as_wikipedia_subdomains() {
        assert_eq!(wiki_language("it"), "it");
        assert_eq!(wiki_language("pt-BR"), "pt");
        assert_eq!(wiki_language("zh-Hans"), "zh");
        assert_eq!(wiki_language("../evil"), "en");
        assert_eq!(heading("=== Design ===", 2), None);
        assert_eq!(heading("== Development ==", 2), Some("Development"));
    }

    #[test]
    fn article_addresses_escape_their_title() {
        assert_eq!(
            article_url("en", "Portal 2"),
            "https://en.wikipedia.org/wiki/Portal_2"
        );
        assert_eq!(
            article_url("it", "Pokémon Rosso e Blu"),
            "https://it.wikipedia.org/wiki/Pok%C3%A9mon_Rosso_e_Blu"
        );
    }

    #[tokio::test]
    #[ignore = "contacts Wikidata and Wikipedia"]
    async fn live_article() {
        let article = article(GameKey::Steam(620), "it").await.unwrap().unwrap();
        assert_eq!(article.summary_language, "it");
        assert!(article.summary.contains("Portal 2"));
        assert!(!article.history.is_empty());
        assert_eq!(
            article.history_url.as_deref(),
            Some("https://en.wikipedia.org/wiki/Portal_2")
        );
    }
}
