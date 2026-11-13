use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use tempfile::TempDir;

fn cache(directory: &TempDir) -> Arc<LibraryCache> {
    LibraryCache::new(directory.path().to_str().unwrap().to_owned())
}

/// The file as the next Obelisk reads it.
fn file(cache: &LibraryCache, directory: &TempDir) -> Value {
    cache.flush();
    let bytes = std::fs::read(directory.path().join(library_cache::FILE_NAME)).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn owned(app_id: u32, name: &str) -> steam_kit::client::OwnedGame {
    steam_kit::client::OwnedGame {
        app_id,
        name: name.into(),
        icon_hash: "hash".into(),
        last_played: 1_790_000_000,
        playtime_minutes: 90,
        record: json!({"appid": app_id, "has_dlc": true}),
    }
}

fn steam_game(app_id: u32, name: &str) -> SteamGame {
    SteamGame {
        app_id,
        name: name.into(),
        icon_hash: "hash".into(),
        last_played: 1_790_000_000,
        playtime_minutes: 90,
    }
}

fn epic(app_name: &str) -> epic_kit::client::Game {
    epic_kit::client::Game {
        app_name: app_name.into(),
        namespace: "space".into(),
        catalog_item_id: "item".into(),
        title: app_name.to_uppercase(),
        description: String::new(),
        image_url: Some("https://cdn.invalid/cover.png".into()),
        header_image_url: None,
        developer: "Psyonix".into(),
        windows: true,
        macos: false,
        added_at: Some(1_590_687_234),
        metadata: json!({"catalogItems": {"item": {"id": "item"}}}),
    }
}

fn playtimes(cache: &LibraryCache) -> Vec<(String, Option<u64>)> {
    cache
        .epic_library("epic-a".into())
        .into_iter()
        .map(|cached| (cached.game.app_name, cached.playtime_seconds))
        .collect()
}

#[test]
fn a_steam_session_keeps_every_field_steam_returned_and_each_games_platforms() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let library = cache.steam(7);
    let games = library.games(vec![owned(620, "Portal 2"), owned(570, "Dota 2")]);
    assert_eq!(
        games,
        [steam_game(620, "Portal 2"), steam_game(570, "Dota 2")]
    );
    library.platforms(vec![steam_kit::client::GamePlatforms {
        app_id: 620,
        platforms: vec!["macos".into(), "windows".into()],
        pics: Some(json!({"common": {
            "review_score": "9",
            "header_image": {"english": "31bac6/header.jpg", "french": "31bac6/header_french.jpg"},
        }})),
    }]);

    library.games(vec![owned(620, "Portal 2")]);
    assert_eq!(
        cache.steam_library(7, "french".into()),
        [CachedSteamGame {
            game: steam_game(620, "Portal 2"),
            platforms: Some(vec!["macos".into(), "windows".into()]),
            artwork: Some(SteamGameArtwork {
                header: Some("31bac6/header_french.jpg".into()),
                ..Default::default()
            }),
            added_at: None,
            achievement_share: None,
        }],
        "a reloaded library keeps the platforms and artwork loaded before"
    );
    let saved = &file(&cache, &directory)["steam"]["7"][0];
    assert_eq!(
        saved["record"]["has_dlc"], true,
        "stored as JSON, not as text"
    );
    assert_eq!(saved["pics"]["common"]["review_score"], "9");
    assert!(cache.steam_library(8, "english".into()).is_empty());
}

#[test]
fn an_epic_session_keeps_each_games_playtime_whichever_answers_first() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let first = cache.web::<epic::Epic>("epic-a");
    first.games(vec![epic("rocket"), epic("fall")]);
    first.playtimes(&BTreeMap::from([("rocket".into(), 3600)]));
    assert_eq!(
        playtimes(&cache),
        [("rocket".into(), Some(3600)), ("fall".into(), None)]
    );

    let next = cache.web::<epic::Epic>("epic-a");
    next.games(vec![epic("rocket"), epic("fall")]);
    assert_eq!(
        playtimes(&cache)[0],
        ("rocket".into(), Some(3600)),
        "until its playtimes answer, a game keeps the earlier one"
    );
    let next = cache.web::<epic::Epic>("epic-a");
    next.playtimes(&BTreeMap::from([("fall".into(), 60)]));
    next.games(vec![epic("rocket"), epic("fall")]);
    assert_eq!(
        playtimes(&cache),
        [("rocket".into(), None), ("fall".into(), Some(60))]
    );
    let file = file(&cache, &directory);
    let saved = &file["epic"]["epic-a"][0];
    assert_eq!(saved["id"], "space:rocket");
    assert_eq!(
        file["epic"]["epic-a"][1]["playtimeSeconds"], 60,
        "the file keeps Epic's seconds"
    );
    assert_eq!(saved["metadata"]["catalogItems"]["item"]["id"], "item");
    assert!(saved.get("headerImageUrl").is_none());
}

#[test]
fn launches_name_their_game_and_forgetting_an_account_stops_its_session_writing() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let steam = cache.steam(7);
    steam.games(vec![owned(620, "Portal 2")]);
    cache
        .web::<epic::Epic>("epic-a")
        .games(vec![epic("rocket")]);
    let portal = GameIdentity::Steam {
        app_id: 620,
        account_id: 7,
    };
    let rocket = GameIdentity::Epic {
        account_id: "epic-a".into(),
        namespace: "space".into(),
        app_name: "rocket".into(),
    };
    cache.record_launch(GameLaunch {
        game: portal.clone(),
        at: 1_790_000_000,
    });
    cache.record_launch(GameLaunch {
        game: rocket.clone(),
        at: 1_789_430_000,
    });
    let mut launches = cache.launches();
    launches.sort_by_key(|launch| launch.at);
    assert_eq!(
        launches,
        [
            GameLaunch {
                game: rocket.clone(),
                at: 1_789_430_000
            },
            GameLaunch {
                game: portal,
                at: 1_790_000_000
            },
        ]
    );

    cache.forget(GameAccount::Steam { steam_id: 7 });
    steam.games(vec![owned(620, "Portal 2")]);
    assert!(
        cache.steam_library(7, "english".into()).is_empty(),
        "a signed-out session's late answer"
    );
    assert_eq!(
        cache.launches(),
        [GameLaunch {
            game: rocket,
            at: 1_789_430_000
        }]
    );
}

#[test]
fn a_cache_written_by_obelisk_before_still_shows() {
    let directory = TempDir::new().unwrap();
    std::fs::write(
        directory.path().join(library_cache::FILE_NAME),
        json!({
            "version": 1,
            "steam": {"7": [{
                "id": "620", "appId": 620, "name": "Portal 2", "iconHash": "hash",
                "lastPlayed": 1_790_000_000, "playtimeMinutes": 90, "platforms": ["macos"],
                "record": {"appid": 620}, "pics": {"common": {}},
                "lastLaunchedAt": "2026-09-21T14:13:20Z",
            }]},
            "epic": {"epic-a": [{
                "id": "space:rocket", "appName": "rocket", "namespace": "space", "catalogItemId": "item",
                "title": "ROCKET", "description": "", "imageUrl": "https://cdn.invalid/cover.png",
                "developer": "Psyonix", "windows": true, "macos": false, "playtimeSeconds": 3600,
                "metadata": {},
            }]},
        })
        .to_string(),
    )
    .unwrap();
    let cache = cache(&directory);
    assert_eq!(
        cache.steam_library(7, "english".into()),
        [CachedSteamGame {
            game: steam_game(620, "Portal 2"),
            platforms: Some(vec!["macos".into()]),
            artwork: Some(SteamGameArtwork::default()),
            added_at: None,
            achievement_share: None,
        }]
    );
    let cached = cache.epic_library("epic-a".into());
    assert_eq!(
        cached[0].game,
        EpicGame {
            added_at: None,
            ..EpicGame::from(epic("rocket"))
        },
        "an entry written before games had a library date has none"
    );
    assert_eq!(cached[0].playtime_seconds, Some(3600));
    assert_eq!(cache.launches().len(), 1);
}

fn gog(product_id: &str) -> gog_kit::client::Game {
    gog_kit::client::Game {
        product_id: product_id.into(),
        title: format!("Game {product_id}"),
        image_url: None,
        header_image_url: None,
        windows: true,
        macos: false,
        info: gog_kit::client::GameInfo {
            summary: "Survival horror.".into(),
            developers: vec!["Tango Gameworks".into()],
            release_date: Some(1_413_158_400),
            rating: Some(75),
            ..Default::default()
        },
        added_at: Some(1_500_000_000),
        metadata: json!({"catalog": {"type": "game"}}),
    }
}

#[test]
fn a_gog_session_keeps_each_games_page_and_playtime() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let library = cache.web::<gog::Gog>("48628349957132247");
    let shown = library.games(vec![gog("1"), gog("2")]);
    assert_eq!(shown[0].developers, ["Tango Gameworks"]);
    library.playtimes(&BTreeMap::from([("2".to_owned(), 90)]));
    // A later load keeps the playtimes the session last loaded.
    library.games(vec![gog("1"), gog("2")]);
    let cached = cache.gog_library("48628349957132247".into());
    let read: Vec<_> = cached
        .iter()
        .map(|cached| (cached.game.product_id.as_str(), cached.playtime_seconds))
        .collect();
    assert_eq!(
        read,
        [("1", None), ("2", Some(5400))],
        "GOG's minutes show in seconds"
    );
    assert_eq!(cached[0].game.release_date, Some(1_413_158_400));
    assert_eq!(cached[0].game.rating, Some(75));
    let file = file(&cache, &directory);
    let kept = &file["gog"]["48628349957132247"][1];
    assert_eq!(kept["metadata"]["catalog"]["type"], "game");
    assert_eq!(kept["playtimeMinutes"], 90, "the file keeps GOG's minutes");
    assert!(kept.get("playtimeSeconds").is_none());

    let launch = GameLaunch {
        game: GameIdentity::Gog {
            account_id: "48628349957132247".into(),
            product_id: "2".into(),
        },
        at: 1_790_000_000,
    };
    cache.record_launch(launch.clone());
    assert_eq!(cache.launches(), [launch]);
    cache.forget(GameAccount::Gog {
        user_id: "48628349957132247".into(),
    });
    assert!(cache.gog_library("48628349957132247".into()).is_empty());
}

#[test]
fn a_gog_game_cached_before_its_page_still_shows() {
    let directory = TempDir::new().unwrap();
    std::fs::write(
        directory.path().join(library_cache::FILE_NAME),
        json!({"version": 1, "gog": {"7": [{"id": "1", "productId": "1", "title": "Old",
                                            "windows": true, "macos": false}]}})
        .to_string(),
    )
    .unwrap();
    let cached = cache(&directory).gog_library("7".into());
    assert_eq!(cached[0].game.title, "Old");
    assert!(cached[0].game.developers.is_empty());
}

#[test]
fn a_cached_playtime_shows_in_seconds_whatever_its_storefront_counts() {
    let directory = TempDir::new().unwrap();
    std::fs::write(
        directory.path().join(library_cache::FILE_NAME),
        json!({
            "version": 1,
            "epic": {"epic-a": [{
                "id": "space:rocket", "appName": "rocket", "namespace": "space", "catalogItemId": "item",
                "title": "ROCKET", "description": "", "developer": "Psyonix", "windows": true,
                "macos": false, "playtimeSeconds": 7200,
            }]},
            "gog": {"7": [
                {"id": "1", "productId": "1", "title": "Played", "windows": true, "macos": false,
                 "playtimeMinutes": 120},
                {"id": "2", "productId": "2", "title": "Absurd", "windows": true, "macos": false,
                 "playtimeMinutes": u64::MAX},
                {"id": "3", "productId": "3", "title": "Never played", "windows": true, "macos": false},
            ]},
        })
        .to_string(),
    )
    .unwrap();
    let cache = cache(&directory);
    assert_eq!(
        cache.epic_library("epic-a".into())[0].playtime_seconds,
        Some(7200),
        "Epic's seconds as they are"
    );
    let gog: Vec<_> = cache
        .gog_library("7".into())
        .into_iter()
        .map(|cached| cached.playtime_seconds)
        .collect();
    assert_eq!(
        gog,
        [Some(7200), Some(u64::MAX), None],
        "GOG's minutes once, and a count too large saturates"
    );
}

fn achievement(id: &str, unlocked: bool) -> LibraryAchievement {
    LibraryAchievement {
        id: id.into(),
        name: format!("Achievement {id}"),
        description: String::new(),
        icon_url: String::new(),
        hidden: false,
        unlocked,
        unlocked_at: unlocked.then_some(1_790_000_000),
    }
}

fn read(id: &str, unlocked: bool) -> GameAchievements {
    GameAchievements {
        id: id.into(),
        achievements: vec![achievement("a", unlocked)],
    }
}

/// What a storefront tells of a game it was asked.
type Told = Result<Vec<LibraryAchievement>, &'static str>;

/// How `achievements` asks the storefront: each id it reads, told unless it
/// is `failing`.
fn storefront(
    asked: &Mutex<Vec<String>>,
    failing: &'static [&'static str],
) -> impl Fn(String) -> std::future::Ready<Told> {
    move |id: String| {
        let told = match failing.contains(&id.as_str()) {
            true => Err("unavailable"),
            false => Ok(vec![achievement("a", true)]),
        };
        asked.lock().unwrap().push(id);
        std::future::ready(told)
    }
}

#[test]
fn a_steam_library_keeps_its_dates_and_achievements_across_loads() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let library = cache.steam(7);
    library.games(vec![owned(620, "Portal 2"), owned(570, "Dota 2")]);
    library.acquisitions(&BTreeMap::from([(620, 1_300_000_000)]));
    library.achievements().keep(&[read("620", true)], "french");

    library.games(vec![owned(620, "Portal 2"), owned(570, "Dota 2")]);
    let cached = cache.steam_library(7, "english".into());
    let read: Vec<_> = cached
        .iter()
        .map(|cached| {
            (
                cached.game.app_id,
                cached.added_at,
                cached.achievement_share,
            )
        })
        .collect();
    assert_eq!(
        read,
        [(620, Some(1_300_000_000), Some(1.0)), (570, None, None),]
    );
}

#[test]
fn a_games_achievements_are_read_again_once_it_was_played_or_in_another_language() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let library = cache.web::<gog::Gog>("7");
    library.games(vec![gog("1"), gog("2"), gog("3"), gog("4")]);
    library.playtimes(&BTreeMap::from([
        ("1".to_owned(), 60),
        ("2".to_owned(), 60),
    ]));
    let ids = ["1", "2", "3", "4", "5"].map(String::from);
    let (current, stale) = library.achievements().current(ids.to_vec(), "fr-FR");
    assert!(current.is_empty());
    assert_eq!(stale, ids, "nothing was read yet");

    library.achievements().keep(
        &[read("1", true), read("2", false), read("3", true)],
        "fr-FR",
    );
    // 1 was played on another computer, Obelisk launched 3 since.
    library.playtimes(&BTreeMap::from([
        ("1".to_owned(), 90),
        ("2".to_owned(), 60),
    ]));
    cache.record_launch(GameLaunch {
        game: GameIdentity::Gog {
            account_id: "7".into(),
            product_id: "3".into(),
        },
        at: 4_000_000_000,
    });
    library.games(vec![gog("1"), gog("2"), gog("3"), gog("4")]);

    let (current, stale) = library.achievements().current(ids.to_vec(), "fr-FR");
    assert_eq!(current, [read("2", false)]);
    assert_eq!(stale, ["1", "3", "4", "5"].map(String::from));
    let (current, _) = library.achievements().current(ids.to_vec(), "en-US");
    assert!(
        current.is_empty(),
        "another language reads every game again"
    );
    assert_eq!(
        cache.gog_library("7".into())[0].achievement_share,
        Some(1.0),
        "what was read shows until it is read again"
    );
}

#[test]
fn games_sharing_an_epic_sandbox_are_read_once() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let library = cache.web::<epic::Epic>("epic-a");
    library.games(vec![epic("rocket"), epic("rocket-bonus")]);
    library.achievements().keep(&[read("space", true)], "fr");
    let (current, stale) = library
        .achievements()
        .current(vec!["space".to_owned()], "fr");
    assert_eq!(current, [read("space", true)]);
    assert!(stale.is_empty());
    let cached = cache.epic_library("epic-a".into());
    assert!(
        cached
            .iter()
            .all(|cached| cached.achievement_share == Some(1.0))
    );
}

#[tokio::test]
async fn achievements_come_from_the_library_unless_fresh_and_fail_only_when_none_could_be_told() {
    let directory = TempDir::new().unwrap();
    let cache = cache(&directory);
    let library = cache.web::<gog::Gog>("7");
    library.games(vec![gog("1"), gog("2")]);
    let asked = Mutex::new(Vec::new());
    let ids = vec!["1".to_owned(), "2".to_owned()];
    let read = |ids: Vec<String>, fresh, failing| {
        achievements(
            Some(library.achievements()),
            ids,
            "fr-FR",
            fresh,
            2,
            storefront(&asked, failing),
        )
    };

    let first = read(ids.clone(), false, &["2"]).await.unwrap();
    assert_eq!(
        first
            .iter()
            .map(|game| game.id.as_str())
            .collect::<Vec<_>>(),
        ["1"],
        "a failed game is left out"
    );
    let again = read(ids.clone(), false, &[]).await.unwrap();
    assert_eq!(again.len(), 2);
    read(vec!["1".into()], true, &[]).await.unwrap();
    assert_eq!(
        *asked.lock().unwrap(),
        ["1", "2", "2", "1"],
        "only the game read last time failed is asked again, and a fresh read asks anyway"
    );
    assert_eq!(
        achievements(
            None,
            vec!["2".to_owned()],
            "fr-FR",
            false,
            1,
            storefront(&asked, &["2"])
        )
        .await,
        Err("unavailable")
    );
}
