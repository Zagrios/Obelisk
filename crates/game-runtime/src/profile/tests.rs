use super::*;
use std::collections::HashSet;

/// The profile of `game` found by its id, in an installation that holds no
/// file of the table.
fn by_id(game: Game<'_>) -> Profile {
    of(game, Path::new("/nowhere"))
}

const ROCKET_LEAGUE: Game<'static> = Game::Epic {
    namespace: "9773aa1aa54f4f7b80e44bef04986cea",
    app_name: "Sugar",
};

const FALL_GUYS: Game<'static> = Game::Epic {
    namespace: "50118b7f954e450f8823df1614b24e80",
    app_name: "0a2d9f6403244d12969e11da6713137b",
};

#[test]
fn rocket_league_fall_guys_and_elden_ring_run_on_the_linux_engine_and_nothing_else_does() {
    assert_eq!(by_id(ROCKET_LEAGUE).engine, Engine::Linux);
    assert_eq!(
        by_id(ROCKET_LEAGUE).executable,
        Some("Binaries/Win64/RocketLeague_EAC.exe")
    );
    assert_eq!(by_id(FALL_GUYS).engine, Engine::Linux);
    assert_eq!(by_id(FALL_GUYS).graphics, Graphics::Dxmt);
    assert_eq!(
        by_id(FALL_GUYS).executable,
        Some("start_protected_game.exe")
    );
    assert_eq!(by_id(FALL_GUYS).minimum_voile, Some("v26.3.0-16"));
    let rocket_league_on_steam = by_id(Game::Steam { app_id: 252950 });
    assert_eq!(rocket_league_on_steam.engine, Engine::Linux);
    assert_eq!(rocket_league_on_steam.graphics, Graphics::Dxmt);
    assert_eq!(rocket_league_on_steam.executable, None);
    assert_eq!(rocket_league_on_steam.minimum_voile, Some("v26.3.0-18"));
    assert_eq!(by_id(Game::Steam { app_id: 1245620 }).engine, Engine::Linux);
    for other in [
        Game::Epic {
            namespace: "9773aa1aa54f4f7b80e44bef04986cea",
            app_name: "Other",
        },
        Game::Epic {
            namespace: "5a6d34ba28bc45d081d9311c3885feef",
            app_name: "Sugar",
        },
        Game::Steam { app_id: 1172470 },
        Game::Gog {
            product_id: "1245620",
        },
    ] {
        assert_eq!(by_id(other), Profile::default(), "{other:?}");
        assert_eq!(by_id(other).engine, Engine::Rosetta);
    }
}

#[test]
fn elden_ring_renders_through_d3dmetal_rocket_league_through_dxmt_and_others_through_d3dmetal() {
    assert_eq!(
        by_id(Game::Steam { app_id: 1245620 }).graphics,
        Graphics::D3DMetal
    );
    assert_eq!(by_id(ROCKET_LEAGUE).graphics, Graphics::Dxmt);
    assert_eq!(Profile::default().graphics, Graphics::D3DMetal);
    assert_eq!(Profile::default().options, GraphicsOptions::default());
}

/// Every game of `profiles.json` as written there.
fn entries() -> Vec<serde_json::Map<String, serde_json::Value>> {
    let table: serde_json::Value = serde_json::from_str(TABLE).unwrap();
    table["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_object().unwrap().clone())
        .collect()
}

#[test]
fn every_game_of_the_table_passes_the_checks_a_table_read_from_main_passes() {
    assert_eq!(table().len(), entries().len());
}

/// A table entry Obelisk keeps, with `key` set to `value`, or removed
/// without one.
fn entry_with(key: &str, value: Option<serde_json::Value>) -> Option<Known> {
    let mut entry = serde_json::json!({
        "name": "A game",
        "note": "Why it runs so.",
        "steam": 42,
        "engine": "Linux",
        "graphics": "Dxmt",
        "executable": "Binaries/Win64/Game.exe",
        "variables": {"SteamDeck": "1"},
        "dll_overrides": {"xaudio2_7": "n,b"},
        "minimum_voile": "v26.3.0-16"
    });
    let fields = entry.as_object_mut().unwrap();
    match value {
        Some(value) => fields.insert(key.into(), value),
        None => fields.remove(key),
    };
    checked(entry)
}

#[test]
fn an_entry_that_sets_what_a_profile_may_not_is_left_out() {
    use serde_json::json;
    let kept = entry_with("gog", Some(json!("1207658924"))).unwrap();
    assert_eq!(kept.minimum_voile.as_deref(), Some("v26.3.0-16"));

    for (what, key, value) in [
        ("a key Obelisk does not read", "launcher", Some(json!("x"))),
        ("no note", "note", None),
        ("no id nor file", "steam", None),
        (
            "an Epic id with more",
            "epic",
            Some(json!({"namespace": "n", "app_name": "a", "id": "i"})),
        ),
        ("an unknown engine", "engine", Some(json!("Windows"))),
        (
            "one of Obelisk's variables",
            "variables",
            Some(json!({"WINEPREFIX": "/tmp"})),
        ),
        (
            "a variable the macOS loader reads",
            "variables",
            Some(json!({"DYLD_INSERT_LIBRARIES": "/tmp/x.dylib"})),
        ),
        (
            "a variable the Linux loader reads",
            "variables",
            Some(json!({"LD_LIBRARY_PATH": "/tmp"})),
        ),
        (
            "a variable no shell names",
            "variables",
            Some(json!({"1A": "x"})),
        ),
        (
            "one of Obelisk's DLLs",
            "dll_overrides",
            Some(json!({"MSCOREE": "n"})),
        ),
        (
            "an unknown DLL mode",
            "dll_overrides",
            Some(json!({"xaudio2_7": "native"})),
        ),
        (
            "a program outside the installation",
            "executable",
            Some(json!("../Game.exe")),
        ),
        (
            "a program at an absolute path",
            "executable",
            Some(json!("/Applications/Game.exe")),
        ),
        (
            "a file outside the installation",
            "path",
            Some(json!("../Game.exe")),
        ),
        (
            "a release that is no version",
            "minimum_voile",
            Some(json!("latest")),
        ),
    ] {
        assert!(entry_with(key, value).is_none(), "{what}");
    }
}

#[test]
fn a_table_of_another_format_is_not_read() {
    let mut table: serde_json::Value = serde_json::from_str(TABLE).unwrap();
    table["format"] = 2.into();
    assert!(parsed(table.to_string().as_bytes()).is_none());
    table.as_object_mut().unwrap().remove("format");
    assert!(parsed(table.to_string().as_bytes()).is_none());
    assert!(parsed(b"404: Not Found").is_none());
}

#[test]
fn no_two_games_of_the_table_share_a_storefront_s_name() {
    let mut names = HashSet::new();
    for known in table() {
        let ids = [
            known.steam.map(|app_id| format!("steam {app_id}")),
            known
                .epic
                .as_ref()
                .map(|epic| format!("epic {}:{}", epic.namespace, epic.app_name)),
            known.gog.as_ref().map(|product| format!("gog {product}")),
        ];
        for id in ids.into_iter().flatten() {
            assert!(names.insert(id.clone()), "{id}");
        }
    }
}

#[test]
fn a_game_of_the_table_keeps_the_defaults_it_does_not_change_and_adds_its_arguments() {
    let known: &'static Known = Box::leak(Box::new(
        serde_json::from_str(
            r#"{
                "name": "A game",
                "note": "Why it runs so.",
                "steam": 42,
                "arguments": ["-dx11"],
                "variables": {"WINENCPU": "10"},
                "dll_overrides": {"xaudio2_7": "n,b"}
            }"#,
        )
        .unwrap(),
    ));
    let profile = known.profile();
    assert_eq!(profile.engine, Engine::Rosetta);
    assert_eq!(profile.graphics, Graphics::D3DMetal);
    assert_eq!(profile.variables["WINENCPU"], "10");
    assert_eq!(profile.dll_overrides["xaudio2_7"], "n,b");
    assert_eq!(profile.arguments, ["-dx11"]);
}

#[test]
fn a_file_of_the_table_is_inside_the_installation_and_only_one_game_holds_it() {
    const SHARED: [&str; 10] = [
        "start_protected_game.exe",
        "launcher.exe",
        "game.exe",
        "client.exe",
        "start.exe",
        "setup.exe",
        "play.exe",
        "crashreportclient.exe",
        "unitycrashhandler64.exe",
        "unitycrashhandler32.exe",
    ];
    let mut paths = HashSet::new();
    for path in table().iter().filter_map(|known| known.path.as_deref()) {
        let name = Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        assert!(!SHARED.contains(&name.as_str()), "{path}");
        assert!(paths.insert(path.to_lowercase()), "{path}");
    }
}

#[test]
fn a_game_is_found_by_its_storefront_s_id_first_then_by_a_file_of_its_installation() {
    let directory = tempfile::tempdir().unwrap();
    let install = directory.path();
    let fallout_4 = by_id(Game::Steam { app_id: 377160 });
    let gog = Game::Gog {
        product_id: "not in the table",
    };
    assert_eq!(of(gog, install), Profile::default());

    std::fs::write(install.join("Fallout4.exe"), "").unwrap();

    assert_eq!(of(gog, install), fallout_4);
    assert_eq!(
        of(Game::Steam { app_id: 1245620 }, install),
        by_id(Game::Steam { app_id: 1245620 })
    );
}

#[test]
fn a_game_that_reads_its_language_from_its_command_line_gets_the_players() {
    let rocket_league = Game::Epic {
        namespace: "9773aa1aa54f4f7b80e44bef04986cea",
        app_name: "Sugar",
    };
    let on_steam = Game::Steam { app_id: 252950 };
    let other = Game::Steam { app_id: 620 };
    let none: &[String] = &[];
    assert_eq!(
        language_argument(rocket_league, none, "it_IT").as_deref(),
        Some("-language=ITA")
    );
    assert_eq!(
        language_argument(on_steam, none, "en_GB").as_deref(),
        Some("-language=INT")
    );
    // A language it has no value for, and a game without a flag, add nothing.
    assert_eq!(language_argument(rocket_league, none, "ja_JP"), None);
    assert_eq!(language_argument(other, none, "it_IT"), None);
    // The player's own language wins, whatever its case.
    for chosen in ["-language=FRA", "-LANGUAGE=FRA"] {
        assert_eq!(
            language_argument(rocket_league, &[chosen.to_owned()], "it_IT"),
            None,
            "{chosen}"
        );
    }
}

#[test]
fn the_named_games_are_the_ones_the_table_names_by_a_storefront_s_id() {
    let named: Vec<_> = named().collect();

    assert!(named.contains(&Game::Steam { app_id: 1245620 }));
    assert!(named.contains(&Game::Steam { app_id: 377160 }));
    assert!(named.contains(&Game::Steam { app_id: 252950 }));
    assert!(!named.contains(&Game::Steam { app_id: 1172470 }));
    assert!(named.contains(&ROCKET_LEAGUE));
}
