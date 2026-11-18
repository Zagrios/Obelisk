//! A game's library artwork as its PICS app info names it, picked as Steam's
//! library asset cache picks it. Newer games publish their images only under a
//! hashed folder, such as `480bd879…/library_600x900.jpg`, and a game whose
//! artwork changed keeps its old images at the plain names: only PICS names
//! the current ones.
use serde_json::Value;

/// Each image's path under the game's store assets, in one language.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryArtwork {
    /// The portrait capsule, 300 × 450.
    pub cover: Option<String>,
    /// The wide hero, 1920 × 620.
    pub hero: Option<String>,
    pub logo: Option<String>,
    /// The library header, or the store's when the game has no library one.
    pub header: Option<String>,
    /// The app whose artwork stands in for the game's missing images, such
    /// as Rust's for its staging branch.
    pub parent: Option<u32>,
    /// What the app is when not a full game, which its cover says.
    pub kind: Option<AppKind>,
}

/// What the library lists beside full games, as Steam types it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKind {
    Demo,
    /// A beta its name calls a playtest, as Steam's playtests are named.
    Playtest,
    /// Any other beta, such as an open beta or a game's beta branch.
    Beta,
}

/// The artwork `pics`, a game's whole PICS app info, names in `language`, or
/// in English where it has none in it. Steam keeps each image at its standard
/// size, never the `image2x` one.
pub fn library_artwork(pics: &Value, language: &str) -> LibraryArtwork {
    let common = &pics["common"];
    let localized = |images: &Value| {
        [language, "english"]
            .into_iter()
            .find_map(|language| images[language].as_str().filter(|path| valid(path)))
            .map(String::from)
    };
    let image = |asset: &str| localized(&common["library_assets_full"][asset]["image"]);
    LibraryArtwork {
        cover: image("library_capsule"),
        hero: image("library_hero"),
        logo: image("library_logo"),
        header: image("library_header").or_else(|| localized(&common["header_image"])),
        parent: positive(&common["parent"]).and_then(|parent| u32::try_from(parent).ok()),
        kind: kind(common),
    }
}

/// What `common` says the app is when not a full game. Steam types a
/// playtest as a beta, and names it one.
fn kind(common: &Value) -> Option<AppKind> {
    let kind = common["type"].as_str()?;
    if kind.eq_ignore_ascii_case("demo") {
        return Some(AppKind::Demo);
    }
    if !kind.eq_ignore_ascii_case("beta") {
        return None;
    }
    let name = common["name"].as_str().unwrap_or_default();
    Some(if name.to_ascii_lowercase().contains("playtest") {
        AppKind::Playtest
    } else {
        AppKind::Beta
    })
}

/// A number of a game's PICS app info, which PICS mostly writes as text;
/// none when absent or 0.
fn positive(value: &Value) -> Option<u64> {
    match value {
        Value::String(text) => text.parse().ok(),
        value => value.as_u64(),
    }
    .filter(|value| *value > 0)
}

/// A file name, or one in a single folder, that stays under the game's assets.
fn valid(path: &str) -> bool {
    path.matches('/').count() <= 1
        && path.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_the_hashed_images_in_the_language_or_in_english() {
        let pics = json!({ "common": {
            "header_image": { "english": "31bac6/header.jpg" },
            "library_assets_full": {
                "library_capsule": {
                    "image": { "english": "480bd8/library_600x900.jpg", "french": "9f0e1d/library_600x900_french.jpg" },
                    "image2x": { "english": "480bd8/library_600x900_2x.jpg" }
                },
                "library_hero": {
                    "image": { "english": "d75184/library_hero.jpg" },
                    "image2x": { "english": "d75184/library_hero_2x.jpg" }
                },
                "library_logo": { "image": { "english": "logo.png" } },
                "library_header": { "image": { "english": "31bac6/library_header.jpg" } }
            }
        }});
        assert_eq!(
            library_artwork(&pics, "french"),
            LibraryArtwork {
                cover: Some("9f0e1d/library_600x900_french.jpg".into()),
                hero: Some("d75184/library_hero.jpg".into()),
                logo: Some("logo.png".into()),
                header: Some("31bac6/library_header.jpg".into()),
                parent: None,
                kind: None,
            }
        );
        assert_eq!(
            library_artwork(&pics, "german").cover.as_deref(),
            Some("480bd8/library_600x900.jpg")
        );
    }

    #[test]
    fn a_game_without_a_library_header_has_its_store_one() {
        let pics = json!({ "common": { "header_image": { "english": "31bac6/header.jpg" } } });
        assert_eq!(
            library_artwork(&pics, "french").header.as_deref(),
            Some("31bac6/header.jpg")
        );
    }

    #[test]
    fn a_staging_branch_names_the_game_it_belongs_to() {
        let pics = json!({ "common": { "type": "Beta", "parent": "252490" } });
        assert_eq!(
            library_artwork(&pics, "english"),
            LibraryArtwork {
                parent: Some(252_490),
                kind: Some(AppKind::Beta),
                ..LibraryArtwork::default()
            }
        );
    }

    #[test]
    fn a_demo_a_playtest_or_a_beta_says_so() {
        for (kind, name, expected) in [
            ("Demo", "Trials Rising Demo", Some(AppKind::Demo)),
            ("Beta", "WARDOGS Playtest", Some(AppKind::Playtest)),
            ("beta", "Open Beta", Some(AppKind::Beta)),
            ("Game", "Playtest Simulator", None),
        ] {
            let pics = json!({ "common": { "type": kind, "name": name } });
            assert_eq!(library_artwork(&pics, "english").kind, expected, "{name}");
        }
    }

    #[test]
    fn names_nothing_a_game_does_not_publish() {
        assert_eq!(
            library_artwork(&json!({}), "english"),
            LibraryArtwork::default()
        );
    }

    #[test]
    fn ignores_a_path_that_would_leave_the_assets() {
        for path in [
            "../440/header.jpg",
            "a/b/header.jpg",
            "/header.jpg",
            "",
            "a/",
            "x?y.jpg",
            "https://evil/x.jpg",
        ] {
            let pics = json!({ "common": { "header_image": { "english": path } } });
            assert_eq!(library_artwork(&pics, "english").header, None, "{path}");
        }
    }
}
