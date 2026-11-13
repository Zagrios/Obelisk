//! What Obelisk tells players about particular games on their page, such as
//! what works and what does not yet: a game's note, whatever its storefront
//! or build. The table is `notes.json`, apart from the profiles so that an
//! Obelisk that reads only profiles never meets its keys. Obelisk is built
//! with it, and a release build reads it again from Obelisk's `main` when it
//! starts, keeping the last one beside its library; a development build
//! keeps the one it was built with. A table that does not parse, or holds no
//! note Obelisk can show, leaves the last one read, and an entry that is not
//! a note Obelisk can show is left out.
use crate::{
    profile::{EpicGame, Game},
    remote::Table,
};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path, sync::LazyLock};

const TABLE: &str = include_str!("../notes.json");
/// The table's file, on Obelisk's `main` and kept in the library.
pub(crate) const NAME: &str = "notes.json";
/// The language every note has, which stands in for one it lacks.
const FALLBACK: &str = "en";
/// The longest note shown, in characters: a few sentences beside the
/// game's details, not an article.
const LONGEST: usize = 400;

/// One game of the table: the storefronts' ids for it and its note by
/// language. Other keys, such as the `name` the table's readers go by, are
/// left aside.
#[derive(Debug, Deserialize)]
struct Known {
    steam: Option<u32>,
    epic: Option<EpicGame>,
    gog: Option<String>,
    /// The note in each language it is written in, by language code such
    /// as `en` or `fr`; English always.
    text: BTreeMap<String, String>,
}

/// The notes pages read now.
static NOTES: LazyLock<Table<Known>> = LazyLock::new(|| Table::new(NAME, TABLE, checked));

/// `entry` as Obelisk reads it, when it is a note Obelisk can show.
fn checked(entry: serde_json::Value) -> Option<Known> {
    serde_json::from_value::<Known>(entry)
        .ok()
        .filter(Known::allowed)
}

impl Known {
    /// Whether Obelisk can show this note: in English, and in each language
    /// short enough to show beside a game.
    fn allowed(&self) -> bool {
        self.text.contains_key(FALLBACK)
            && self
                .text
                .values()
                .all(|note| !note.trim().is_empty() && note.chars().count() <= LONGEST)
    }

    fn names(&self, game: Game<'_>) -> bool {
        game.named_by(self.steam, self.epic.as_ref(), self.gog.as_deref())
    }

    /// The note in `language`, else in English.
    fn text(&self, language: &str) -> &str {
        self.text.get(language).unwrap_or(&self.text[FALLBACK])
    }
}

/// The note on `game` in `language`, else in English. None when the table
/// has none for it.
pub fn of(game: Game<'_>, language: &str) -> Option<String> {
    NOTES
        .entries()
        .iter()
        .find(|known| known.names(game))
        .map(|known| known.text(language).to_owned())
}

/// Reads the table again from Obelisk's `main` and keeps it in `library`.
pub async fn refresh(library: &Path) {
    NOTES.refresh(library).await;
}

#[cfg(test)]
mod tests;
