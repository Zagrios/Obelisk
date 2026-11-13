use super::*;
use serde_json::{Value, json};
use std::collections::HashSet;

fn built_in() -> &'static [Known] {
    NOTES.entries()
}

/// A note Obelisk keeps, with `key` set to `value`.
fn entry_with(key: &str, value: Value) -> Option<Known> {
    let mut entry = json!({
        "name": "A game",
        "steam": 42,
        "text": {"en": "Plays offline.", "fr": "Se joue hors ligne."}
    });
    entry[key] = value;
    checked(entry)
}

#[test]
fn every_note_of_the_table_passes_the_checks_a_table_read_from_main_passes() {
    let table: Value = serde_json::from_str(TABLE).unwrap();
    assert_eq!(built_in().len(), table["games"].as_array().unwrap().len());
}

#[test]
fn no_two_notes_of_the_table_name_the_same_game() {
    let mut names = HashSet::new();
    for known in built_in() {
        let ids = [
            known.steam.map(|app_id| format!("steam {app_id}")),
            known.epic.as_ref().map(|epic| format!("epic {epic:?}")),
            known.gog.as_ref().map(|product| format!("gog {product}")),
        ];
        for id in ids.into_iter().flatten() {
            assert!(names.insert(id.clone()), "{id}");
        }
    }
}

#[test]
fn a_note_names_its_game_by_its_storefront_s_id() {
    let known = entry_with("gog", "1207658924".into()).unwrap();
    assert!(known.names(Game::Steam { app_id: 42 }));
    assert!(known.names(Game::Gog {
        product_id: "1207658924"
    }));
    assert!(!known.names(Game::Steam { app_id: 43 }));
    assert!(!known.names(Game::Gog { product_id: "42" }));
}

#[test]
fn a_note_is_in_obelisk_s_language_else_in_english() {
    let known = entry_with("name", "A game".into()).unwrap();
    assert_eq!(known.text("fr"), "Se joue hors ligne.");
    assert_eq!(known.text("de"), "Plays offline.");
}

#[test]
fn a_note_obelisk_cannot_show_is_left_out() {
    assert!(entry_with("name", "A game".into()).is_some());
    // English always, every language short and not empty.
    assert!(entry_with("text", json!({"fr": "Se joue hors ligne."})).is_none());
    assert!(entry_with("text", json!({"en": "  "})).is_none());
    assert!(entry_with("text", json!({"en": "a".repeat(LONGEST)})).is_some());
    assert!(entry_with("text", json!({"en": "a".repeat(LONGEST + 1)})).is_none());
    // A key Obelisk does not read is left aside, and never runs a game.
    assert!(entry_with("arguments", json!(["-dxlevel", "95"])).is_some());
}
