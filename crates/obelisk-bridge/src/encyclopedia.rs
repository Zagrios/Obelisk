//! What Wikipedia tells of a game, for its page.
use obelisk_core::encyclopedia::{self, EncyclopediaError, GameKey};

/// What a game is, in the player's language where Wikipedia has it, and how
/// it was made, in English. Wikipedia's text, under CC BY-SA 4.0: the page
/// credits it and links each article.
#[derive(Clone, Debug, uniffi::Record)]
pub struct GameArticle {
    pub summary: String,
    /// Such as `it` or `en`.
    pub summary_language: String,
    pub summary_url: String,
    pub history: Vec<GameHistorySection>,
    pub history_url: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct GameHistorySection {
    pub heading: Option<String>,
    pub paragraphs: Vec<String>,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum GameArticleError {
    #[error("{reason}")]
    Failure { reason: String },
}

impl From<EncyclopediaError> for GameArticleError {
    fn from(error: EncyclopediaError) -> Self {
        Self::Failure {
            reason: error.to_string(),
        }
    }
}

/// The Wikipedia article of the Steam game `app_id`, its summary in
/// `language`, a language tag such as `it` or `pt-BR`; none when Wikidata
/// links no article to the game.
#[uniffi::export(async_runtime = "tokio")]
pub async fn steam_game_article(
    app_id: u32,
    language: String,
) -> Result<Option<GameArticle>, GameArticleError> {
    Ok(encyclopedia::article(GameKey::Steam(app_id), &language)
        .await?
        .map(|article| GameArticle {
            summary: article.summary,
            summary_language: article.summary_language,
            summary_url: article.summary_url,
            history: article
                .history
                .into_iter()
                .map(|section| GameHistorySection {
                    heading: section.heading,
                    paragraphs: section.paragraphs,
                })
                .collect(),
            history_url: article.history_url,
        }))
}
