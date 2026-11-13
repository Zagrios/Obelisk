//! The note a game's page shows, such as what works of it and what does not
//! yet, and the table it comes from; and the games the profile table names,
//! which Home shows as Obelisk Verified.
use crate::{ObeliskLocations, SessionStorefront, installations::Named};
use game_runtime::{epic, gog, notes, profile, profile::Game, steam};
use std::path::Path;

/// The note on the game `id` names, as the app names it, in `language`, else
/// in English: none for a game the table leaves out.
#[uniffi::export]
pub fn game_note(storefront: SessionStorefront, id: String, language: String) -> Option<String> {
    match storefront {
        SessionStorefront::Steam => notes::of(
            Game::Steam {
                app_id: <steam::Steam as Named>::named(&id)?,
            },
            &language,
        ),
        SessionStorefront::Epic => {
            let store = <epic::Epic as Named>::named(&id)?;
            notes::of(
                Game::Epic {
                    namespace: &store.namespace,
                    app_name: &store.app_name,
                },
                &language,
            )
        }
        SessionStorefront::Gog => notes::of(
            Game::Gog {
                product_id: &<gog::Gog as Named>::named(&id)?,
            },
            &language,
        ),
    }
}

/// The games of `storefront` the profile table names, as the app names
/// them, read as Obelisk reads the table now.
#[uniffi::export]
pub fn verified_games(storefront: SessionStorefront) -> Vec<String> {
    profile::named()
        .filter_map(|game| match (storefront, game) {
            (SessionStorefront::Steam, Game::Steam { app_id }) => {
                Some(<steam::Steam as Named>::name(&app_id))
            }
            (
                SessionStorefront::Epic,
                Game::Epic {
                    namespace,
                    app_name,
                },
            ) => Some(<epic::Epic as Named>::name(&epic::Store {
                namespace: namespace.into(),
                app_name: app_name.into(),
            })),
            (SessionStorefront::Gog, Game::Gog { product_id }) => {
                Some(<gog::Gog as Named>::name(&product_id.into()))
            }
            _ => None,
        })
        .collect()
}

/// Reads the game notes and the profiles again from Obelisk's `main`,
/// keeping them in the library. Runs once at startup, besides each Windows
/// launch for the profiles: until `main` answers, pages read the last ones
/// kept, else those Obelisk was built with.
#[uniffi::export(async_runtime = "tokio")]
pub async fn refresh_notes_and_profiles(locations: ObeliskLocations) {
    let library = Path::new(&locations.library);
    tokio::join!(notes::refresh(library), profile::refresh(library));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_s_note_is_found_by_the_id_the_app_gives_it() {
        let note = |id: &str| game_note(SessionStorefront::Steam, id.into(), "en".into());
        assert!(note("440").is_some());
        assert_eq!(note("tf2"), None);
    }

    #[test]
    fn verified_games_are_named_as_the_app_names_them() {
        assert!(verified_games(SessionStorefront::Steam).contains(&"1245620".into()));
        let epic = verified_games(SessionStorefront::Epic);
        assert!(epic.contains(&"9773aa1aa54f4f7b80e44bef04986cea:Sugar".into()));
        assert!(!epic.contains(&"1245620".into()));
    }

    #[test]
    fn an_id_that_names_no_game_of_its_storefront_has_no_note() {
        assert_eq!(
            game_note(SessionStorefront::Epic, "Sugar".into(), "en".into()),
            None
        );
        assert_eq!(
            game_note(SessionStorefront::Gog, "440".into(), "en".into()),
            None
        );
    }
}
