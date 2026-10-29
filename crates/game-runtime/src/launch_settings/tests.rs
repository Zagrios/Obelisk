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
/// A game whose profile names the oldest Voile release it runs in.
const FALL_GUYS: Game<'static> = Game::Epic {
    namespace: "50118b7f954e450f8823df1614b24e80",
    app_name: "0a2d9f6403244d12969e11da6713137b",
};
/// Rocket League's Steam build, which reads its language from its command
/// line as its Epic one does.
const ROCKET_LEAGUE_ON_STEAM: Game<'static> = Game::Steam { app_id: 252950 };
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
        None,
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

    let settings = of(library, OTHER, nowhere(), None).settings;
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.engine, Engine::Rosetta);
    assert_eq!(settings.graphics, Graphics::D3DMetal);
    assert!(settings.options.ray_tracing);

    let settings = of(library, ELDEN_RING, nowhere(), None).settings;
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
        choose(library, OTHER, install, None, same).unwrap().choices,
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
            let chosen = choose(library, game, install, None, choices.clone()).unwrap();
            assert_eq!(
                chosen,
                of(library, game, install, None),
                "{game:?} {choices:?}"
            );
        }
    }
    let [_, dxmt, ..] = every_kind_of_choices();
    let chosen = choose(library, ROCKET_LEAGUE, install, Some("it_IT"), dxmt).unwrap();
    assert_eq!(chosen, of(library, ROCKET_LEAGUE, install, Some("it_IT")));
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

    choose(library, OTHER, install, None, choices.clone()).unwrap();

    assert!(file.is_file());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(&file).unwrap()).unwrap(),
        serde_json::json!({"arguments": "-windowed", "engine": "Linux", "ray_tracing": false})
    );
    assert_eq!(of(library, OTHER, install, None).choices, choices);
    assert_eq!(
        of(
            library,
            OTHER,
            Path::new("/elsewhere/steam-43-windows"),
            None
        )
        .choices,
        Choices::default()
    );
    choose(library, OTHER, install, None, Choices::default()).unwrap();
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
        of(library, OTHER, Path::new("/games/game"), None).choices,
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
    let started = of(library, ROCKET_LEAGUE, nowhere(), None)
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
        of(library, ELDEN_RING, nowhere(), None)
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
        of(library, MINDSEYE, nowhere(), None)
            .settings
            .dll_overrides["vcruntime140_1"],
        "n,b"
    );
    assert_eq!(
        of(library, GOW_RAGNAROK, nowhere(), None)
            .settings
            .variables["SteamDeck"],
        "1"
    );
    let hud = Choices {
        performance_hud: true,
        ..Choices::default()
    };
    assert!(chosen(OTHER, hud).settings.performance_hud);
}

#[test]
fn on_the_linux_engine_a_launcher_it_cannot_run_gives_way_to_the_easy_anti_cheat_bootstrapper() {
    use crate::{
        easy_anti_cheat::tests::{protect, write},
        pe::tests::Fixture,
    };
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("library");
    let install = directory.path().join("game");
    protect(&install, "", "start_protected_game.exe", "VRChat.exe");
    let dotnet = Fixture {
        dotnet: true,
        ..Fixture::default()
    };
    write(&install, "launch.exe", &dotnet.bytes());
    write(&install, "native.exe", &Fixture::default().bytes());
    let launch = |name: &str| publisher(&install.join(name).to_string_lossy(), &["--no-vr"]);
    let settings = of(&library, OTHER, &install, None).settings;
    assert_eq!(settings.engine, Engine::Linux);

    assert_eq!(
        settings.started(&install, &launch("launch.exe")),
        launch("start_protected_game.exe")
    );
    // A native launcher runs there and starts the bootstrapper itself.
    for program in ["native.exe", "start_protected_game.exe"] {
        assert_eq!(
            settings.started(&install, &launch(program)),
            launch(program),
            "{program}"
        );
    }

    // On Rosetta's Wine, which runs the publisher's launcher.
    let rosetta = Choices {
        engine: Some(Engine::Rosetta),
        ..Choices::default()
    };
    let settings = choose(&library, OTHER, &install, None, rosetta)
        .unwrap()
        .settings;
    assert_eq!(
        settings.started(&install, &launch("launch.exe")),
        launch("launch.exe")
    );

    // Without Easy Anti-Cheat, nothing starts instead.
    let linux = Choices {
        engine: Some(Engine::Linux),
        ..Choices::default()
    };
    let bare = directory.path().join("bare");
    write(&bare, "launch.exe", &dotnet.bytes());
    let target = publisher(&bare.join("launch.exe").to_string_lossy(), &[]);
    let settings = choose(&library, OTHER, &bare, None, linux)
        .unwrap()
        .settings;
    assert_eq!(settings.started(&bare, &target), target);
}

/// An installation the table does not name, in `directory`, that holds
/// Easy Anti-Cheat.
fn protected(directory: &Path) -> PathBuf {
    let install = directory.join("protected");
    crate::easy_anti_cheat::tests::protect(&install, "", "start_protected_game.exe", "Game.exe");
    install
}

#[test]
fn a_game_that_reads_its_language_from_its_command_line_gets_the_players() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path();
    // What its launch settings add after its profile's own arguments.
    let added = |game: Game<'static>, locale: Option<&'static str>| {
        let profile = of(library, game, nowhere(), None)
            .settings
            .profile_arguments;
        let arguments = of(library, game, nowhere(), locale)
            .settings
            .profile_arguments;
        assert_eq!(arguments[..profile.len()], profile[..], "{game:?}");
        arguments[profile.len()..].to_vec()
    };
    assert_eq!(added(ROCKET_LEAGUE, Some("it_IT")), ["-language=ITA"]);
    assert_eq!(
        added(ROCKET_LEAGUE_ON_STEAM, Some("en_GB")),
        ["-language=INT"]
    );
    // A language it has no value for, a game without a flag, and a player
    // without a language add nothing.
    assert!(added(ROCKET_LEAGUE, Some("ja_JP")).is_empty());
    assert!(added(OTHER, Some("it_IT")).is_empty());
    assert!(added(ROCKET_LEAGUE, None).is_empty());
    // The player's own language wins, whatever its case.
    for chosen in ["-language=FRA", "-LANGUAGE=FRA"] {
        let choices = Choices {
            arguments: Some(chosen.into()),
            ..Choices::default()
        };
        let settings = choose(library, ROCKET_LEAGUE, nowhere(), Some("it_IT"), choices)
            .unwrap()
            .settings;
        assert_eq!(settings.profile_arguments, Vec::<String>::new(), "{chosen}");
        assert_eq!(settings.arguments, [chosen], "{chosen}");
    }
}

#[test]
fn the_language_starts_after_the_profile_s_arguments_and_before_the_player_s() {
    let directory = tempfile::tempdir().unwrap();
    let choices = Choices {
        arguments: Some("-x".into()),
        ..Choices::default()
    };
    let settings = choose(
        directory.path(),
        ROCKET_LEAGUE,
        nowhere(),
        Some("fr_FR"),
        choices,
    )
    .unwrap()
    .settings;
    let launcher = publisher(
        "/games/rl/Binaries/Win64/Launcher.exe",
        &["-AUTH_TYPE=exchangecode"],
    );

    assert_eq!(
        settings
            .started(Path::new("/games/rl"), &launcher)
            .arguments,
        ["-AUTH_TYPE=exchangecode", "-language=FRA", "-x"]
    );
}

#[test]
fn a_game_waits_for_the_voile_release_its_profile_names_whatever_the_player_chose() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("library");
    let rosetta = Choices {
        engine: Some(Engine::Rosetta),
        ..Choices::default()
    };

    assert_eq!(
        of(&library, FALL_GUYS, nowhere(), None).minimum_voile,
        Some("v26.3.0-16")
    );
    assert_eq!(chosen(FALL_GUYS, rosetta).minimum_voile, Some("v26.3.0-16"));
    assert_eq!(
        of(&library, ROCKET_LEAGUE_ON_STEAM, nowhere(), None).minimum_voile,
        Some("v26.3.0-18")
    );
    assert_eq!(of(&library, OTHER, nowhere(), None).minimum_voile, None);
    let install = protected(directory.path());
    assert_eq!(of(&library, OTHER, &install, None).minimum_voile, None);
}

#[test]
fn the_engine_alone_is_the_launch_settings_engine() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("library");
    let protected = protected(directory.path());
    for (game, install) in [
        (OTHER, nowhere()),
        (ELDEN_RING, nowhere()),
        (ROCKET_LEAGUE, nowhere()),
        (OTHER, protected.as_path()),
    ] {
        assert_eq!(
            engine(&library, game, install),
            of(&library, game, install, None).settings.engine,
            "{game:?} {install:?}"
        );
        let other = match engine(&library, game, install) {
            Engine::Rosetta => Engine::Linux,
            Engine::Linux => Engine::Rosetta,
        };
        let choices = Choices {
            engine: Some(other),
            ..Choices::default()
        };
        choose(&library, game, install, None, choices).unwrap();
        assert_eq!(
            engine(&library, game, install),
            other,
            "{game:?} {install:?}"
        );
        assert_eq!(
            of(&library, game, install, None).settings.engine,
            other,
            "{game:?} {install:?}"
        );
        remove(&library, install).unwrap();
    }
}

#[test]
fn a_chosen_engine_needs_no_profile() {
    let directory = tempfile::tempdir().unwrap();
    let library = directory.path().join("library");
    let missing = directory.path().join("missing");
    let linux = Choices {
        engine: Some(Engine::Linux),
        ..Choices::default()
    };
    choose(&library, OTHER, &missing, None, linux).unwrap();
    assert_eq!(engine(&library, OTHER, &missing), Engine::Linux);

    // Its Easy Anti-Cheat alone would put it on the Linux engine.
    let install = protected(directory.path());
    let rosetta = Choices {
        engine: Some(Engine::Rosetta),
        ..Choices::default()
    };
    choose(&library, OTHER, &install, None, rosetta).unwrap();
    assert_eq!(engine(&library, OTHER, &install), Engine::Rosetta);
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
