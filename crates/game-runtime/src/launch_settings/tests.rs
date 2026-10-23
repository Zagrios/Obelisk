use super::*;
use crate::profile::Game;

/// A game no profile names.
const OTHER: Game<'static> = Game::Steam { app_id: 42 };
/// A game whose profile names the Linux engine and D3DMetal.
const ELDEN_RING: Game<'static> = Game::Steam { app_id: 1245620 };
/// A game whose profile starts another program than its publisher's.
const ROCKET_LEAGUE: Game<'static> = Game::Epic {
    namespace: "9773aa1aa54f4f7b80e44bef04986cea",
    app_name: "Sugar",
};
/// A game whose profile adds an argument and DLL overrides.
const MINDSEYE: Game<'static> = Game::Steam { app_id: 3265250 };
/// A game whose profile adds an environment variable.
const GOW_RAGNAROK: Game<'static> = Game::Steam { app_id: 2322010 };
/// A game whose profile starts it through Valve's Windows Steam.
const TEAM_FORTRESS_2: Game<'static> = Game::Steam { app_id: 440 };

/// An installation that holds no file of the table.
fn nowhere() -> &'static Path {
    Path::new("/nowhere")
}

/// The launch settings `choices` give `game`, kept in an empty library.
fn chosen(game: Game<'_>, choices: Choices) -> View {
    let directory = tempfile::tempdir().unwrap();
    choose(
        directory.path(),
        game,
        Path::new("/library/Games/game"),
        choices,
    )
    .unwrap()
}

/// A storefront's launch of the program at `executable`.
fn publisher(executable: &str, arguments: &[&str]) -> LaunchTarget {
    let executable = Path::new(executable);
    LaunchTarget {
        executable: executable.to_string_lossy().into_owned(),
        arguments: arguments.iter().map(|&argument| argument.into()).collect(),
        working_directory: executable.parent().unwrap().to_string_lossy().into_owned(),
        is_application_bundle: false,
    }
}

#[test]
fn a_game_without_choices_runs_as_its_profile_says() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();

    let settings = of(library, OTHER, nowhere()).settings;
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.engine, Engine::Rosetta);
    assert_eq!(settings.graphics, Graphics::D3DMetal);
    assert!(settings.options.ray_tracing);

    let settings = of(library, ELDEN_RING, nowhere()).settings;
    assert_eq!(settings.engine, Engine::Linux);
    assert_eq!(settings.graphics, Graphics::D3DMetal);
}

#[test]
fn another_engine_renders_with_its_own_default_backend_unless_the_player_picked_one() {
    let linux = Choices {
        engine: Some(Engine::Linux),
        ..Choices::default()
    };
    assert_eq!(
        chosen(OTHER, linux.clone()).settings.graphics,
        Graphics::Dxmt
    );

    let picked = Choices {
        graphics: Some(Graphics::D3DMetal),
        ..linux
    };
    assert_eq!(chosen(OTHER, picked).settings.graphics, Graphics::D3DMetal);

    let rosetta = Choices {
        engine: Some(Engine::Rosetta),
        ..Choices::default()
    };
    assert_eq!(
        chosen(ELDEN_RING, rosetta).settings.graphics,
        Graphics::D3DMetal
    );
}

/// Choices that each keep something different, and those that keep nothing.
fn every_kind_of_choices() -> [Choices; 5] {
    [
        Choices {
            arguments: Some("   ".into()),
            engine: Some(Engine::Rosetta),
            graphics: Some(Graphics::D3DMetal),
            ray_tracing: Some(true),
            performance_hud: false,
            steam_client: Some(SteamClient::Obelisk),
        },
        Choices {
            arguments: Some("-dx11 \"+exec autoexec.cfg\"".into()),
            graphics: Some(Graphics::Dxmt),
            ray_tracing: Some(false),
            performance_hud: true,
            ..Choices::default()
        },
        Choices {
            arguments: Some("  -dx11  ".into()),
            ..Choices::default()
        },
        Choices {
            engine: Some(Engine::Linux),
            graphics: Some(Graphics::Dxmt),
            ..Choices::default()
        },
        Choices {
            engine: Some(Engine::Rosetta),
            ray_tracing: Some(false),
            ..Choices::default()
        },
    ]
}

#[test]
fn only_what_differs_from_the_profile_is_kept() {
    let [same, dxmt, padded, linux, _] = every_kind_of_choices();

    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();
    let install = Path::new("/library/Games/Steam/steam-42-windows");
    assert_eq!(
        choose(library, OTHER, install, same).unwrap().choices,
        Choices::default()
    );
    assert!(
        !library
            .join("LaunchSettings/steam-42-windows.json")
            .exists()
    );

    assert_eq!(chosen(OTHER, dxmt.clone()).choices, dxmt);
    assert_eq!(
        chosen(OTHER, padded).choices.arguments.as_deref(),
        Some("-dx11")
    );

    // On the Linux engine, DXMT is the default: only the engine is kept.
    assert_eq!(
        chosen(OTHER, linux).choices,
        Choices {
            engine: Some(Engine::Linux),
            ..Choices::default()
        }
    );
}

#[test]
fn choose_returns_what_the_next_launch_runs() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();
    let install = Path::new("/library/Games/game");
    for game in [OTHER, ELDEN_RING, ROCKET_LEAGUE] {
        for choices in every_kind_of_choices() {
            let chosen = choose(library, game, install, choices.clone()).unwrap();
            assert_eq!(chosen, of(library, game, install), "{game:?} {choices:?}");
        }
    }
}

#[test]
fn choices_are_kept_per_installation_folder_and_none_leaves_no_file() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();
    let file = library.join("LaunchSettings/steam-42-windows.json");
    let install = Path::new("/library/Games/Steam/steam-42-windows");
    let choices = Choices {
        arguments: Some("-windowed".into()),
        engine: Some(Engine::Linux),
        ray_tracing: Some(false),
        ..Choices::default()
    };

    choose(library, OTHER, install, choices.clone()).unwrap();

    assert!(file.is_file());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&file).unwrap()).unwrap(),
        serde_json::json!({"arguments": "-windowed", "engine": "Linux", "ray_tracing": false})
    );
    assert_eq!(of(library, OTHER, install).choices, choices);
    assert_eq!(
        of(library, OTHER, Path::new("/elsewhere/steam-43-windows")).choices,
        Choices::default()
    );
    choose(library, OTHER, install, Choices::default()).unwrap();
    assert!(!file.exists());
    remove(library, install).unwrap();
}

#[test]
fn an_unreadable_file_means_no_choices() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();
    fs::create_dir_all(library.join("LaunchSettings")).unwrap();
    fs::write(library.join("LaunchSettings/game.json"), "not json").unwrap();

    assert_eq!(
        of(library, OTHER, Path::new("/games/game")).choices,
        Choices::default()
    );
}

#[test]
fn the_view_shows_each_engine_s_defaults_and_the_player_s_choices() {
    let choices = Choices {
        engine: Some(Engine::Rosetta),
        ..Choices::default()
    };
    let view = chosen(ELDEN_RING, choices.clone());

    assert_eq!(view.settings.engine, Engine::Rosetta);
    assert_eq!(view.choices, choices);
    assert_eq!(view.default_engine, Engine::Linux);
    assert_eq!(
        view.default_graphics,
        [Graphics::D3DMetal, Graphics::D3DMetal]
    );
}

#[test]
fn each_engine_has_its_translator_and_only_rosetta_s_d3dmetal_takes_options() {
    assert_eq!(offer(Engine::Rosetta).translator, Translator::Rosetta);
    assert_eq!(offer(Engine::Linux).translator, Translator::Fex);
    for engine in [Engine::Rosetta, Engine::Linux] {
        for graphics in [Graphics::D3DMetal, Graphics::Dxmt] {
            let options = offer(engine).graphics(graphics).unwrap().options;
            let expected: &[GraphicsOption] = match (engine, graphics) {
                (Engine::Rosetta, Graphics::D3DMetal) => &[GraphicsOption::RayTracing],
                _ => &[],
            };
            assert_eq!(options, expected, "{engine:?} {graphics:?}");
        }
    }
}

#[test]
fn the_player_s_launch_settings_apply_over_the_profile_and_keep_its_program() {
    let choices = Choices {
        engine: Some(Engine::Rosetta),
        ray_tracing: Some(false),
        ..Choices::default()
    };

    let settings = chosen(ROCKET_LEAGUE, choices).settings;

    assert_eq!(settings.engine, Engine::Rosetta);
    assert_eq!(settings.graphics, Graphics::D3DMetal);
    assert!(!settings.options.ray_tracing);
    assert_eq!(
        settings.executable.as_deref(),
        Some("Binaries/Win64/RocketLeague_EAC.exe")
    );
}

#[test]
fn the_player_s_typed_arguments_are_split_outside_quotes() {
    let choices = Choices {
        arguments: Some("-dx11 \"+exec auto exec.cfg\"".into()),
        ..Choices::default()
    };

    assert_eq!(
        chosen(OTHER, choices).settings.arguments,
        ["-dx11", "+exec auto exec.cfg"]
    );
}

#[test]
fn a_substitute_starts_with_the_storefront_s_then_the_profile_s_then_the_player_s_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();

    let launcher = publisher(
        "/games/rl/Binaries/Win64/Launcher.exe",
        &["-AUTH_TYPE=exchangecode"],
    );
    let started = of(library, ROCKET_LEAGUE, nowhere())
        .settings
        .started(Path::new("/games/rl"), &launcher);
    assert_eq!(
        started.executable,
        "/games/rl/Binaries/Win64/RocketLeague_EAC.exe"
    );
    assert_eq!(started.working_directory, "/games/rl/Binaries/Win64");
    assert_eq!(started.arguments, launcher.arguments);

    let protected = publisher("/games/er/Game/start_protected_game.exe", &[]);
    let install = Path::new("/games/er");
    assert_eq!(
        of(library, ELDEN_RING, nowhere())
            .settings
            .started(install, &protected),
        protected
    );
    assert_eq!(Settings::default().started(install, &protected), protected);

    let choices = Choices {
        arguments: Some("-dx11 \"+exec a b.cfg\"".into()),
        ..Choices::default()
    };
    let game = publisher("/games/me/MindsEye.exe", &["-AUTH_TYPE=exchangecode"]);
    assert_eq!(
        chosen(MINDSEYE, choices)
            .settings
            .started(Path::new("/games/me"), &game)
            .arguments,
        [
            "-AUTH_TYPE=exchangecode",
            "-windowed",
            "-dx11",
            "+exec a b.cfg"
        ]
    );
}

#[test]
fn the_profile_s_variables_and_dll_overrides_and_the_player_s_hud_are_carried() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();

    assert_eq!(
        of(library, MINDSEYE, nowhere()).settings.dll_overrides["vcruntime140_1"],
        "n,b"
    );
    assert_eq!(
        of(library, GOW_RAGNAROK, nowhere()).settings.variables["SteamDeck"],
        "1"
    );
    let hud = Choices {
        performance_hud: true,
        ..Choices::default()
    };
    assert!(chosen(OTHER, hud).settings.performance_hud);
}

#[test]
fn windows_steam_is_kept_on_rosetta_s_wine_and_dropped_elsewhere() {
    let windows = Choices {
        steam_client: Some(SteamClient::Windows),
        ..Choices::default()
    };
    let view = chosen(OTHER, windows.clone());
    assert_eq!(view.settings.steam_client, SteamClient::Windows);
    assert_eq!(view.choices.steam_client, Some(SteamClient::Windows));

    // The Linux engine runs no Windows Steam: the choice goes with the move.
    let view = chosen(ELDEN_RING, windows);
    assert_eq!(view.settings.steam_client, SteamClient::Obelisk);
    assert_eq!(view.choices, Choices::default());

    // Obelisk's own client is the default, which keeps no file.
    let obelisk = Choices {
        steam_client: Some(SteamClient::Obelisk),
        ..Choices::default()
    };
    assert_eq!(chosen(OTHER, obelisk).choices, Choices::default());
}

#[test]
fn a_profile_s_steam_client_is_the_default_on_rosetta_s_wine() {
    let view = chosen(TEAM_FORTRESS_2, Choices::default());
    assert_eq!(view.settings.steam_client, SteamClient::Windows);
    assert_eq!(view.default_steam_client, SteamClient::Windows);
    assert_eq!(
        chosen(OTHER, Choices::default()).default_steam_client,
        SteamClient::Obelisk
    );

    // Windows Steam is the profile's, which keeps no file; Obelisk's is kept.
    let windows = Choices {
        steam_client: Some(SteamClient::Windows),
        ..Choices::default()
    };
    assert_eq!(chosen(TEAM_FORTRESS_2, windows).choices, Choices::default());
    let obelisk = Choices {
        steam_client: Some(SteamClient::Obelisk),
        ..Choices::default()
    };
    let view = chosen(TEAM_FORTRESS_2, obelisk.clone());
    assert_eq!(view.settings.steam_client, SteamClient::Obelisk);
    assert_eq!(view.choices, obelisk);

    // On the Linux engine Obelisk's runs it, and back on Rosetta's Wine the
    // profile's does again.
    let linux = Choices {
        engine: Some(Engine::Linux),
        ..Choices::default()
    };
    let view = chosen(TEAM_FORTRESS_2, linux.clone());
    assert_eq!(view.settings.steam_client, SteamClient::Obelisk);
    assert_eq!(view.choices, linux);
    let rosetta = Choices {
        engine: Some(Engine::Rosetta),
        ..Choices::default()
    };
    assert_eq!(
        chosen(TEAM_FORTRESS_2, rosetta).settings.steam_client,
        SteamClient::Windows
    );
}
