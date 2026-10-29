use super::*;
use crate::launch_settings::Settings;
use std::collections::BTreeMap;

/// A 64-bit Windows program.
pub(crate) fn pe() -> Vec<u8> {
    crate::pe::tests::Fixture::default().bytes()
}

pub(crate) fn installation() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

pub(crate) fn target(executable: &Path, arguments: &[&str]) -> LaunchTarget {
    LaunchTarget {
        executable: executable.to_string_lossy().into_owned(),
        arguments: arguments
            .iter()
            .map(|argument| (*argument).into())
            .collect(),
        working_directory: executable.parent().unwrap().to_string_lossy().into_owned(),
        is_application_bundle: false,
    }
}

/// A game no profile names.
const OTHER: crate::profile::Game<'static> = crate::profile::Game::Steam { app_id: 42 };
/// A game whose profile names the Linux engine and D3DMetal.
const ELDEN_RING: crate::profile::Game<'static> = crate::profile::Game::Steam { app_id: 1245620 };

/// The Windows installation of `game` in `install`, as a launch resolves it
/// for a player without a language.
pub(crate) fn windows_of(
    locations: &crate::Locations,
    game: crate::profile::Game<'_>,
    install: &Path,
) -> Installation {
    let launch_settings = crate::launch_settings::of(&locations.library, game, install, None);
    Installation::new(locations, game, install, launch_settings).unwrap()
}

/// Obelisk's folders under `root`: its library, its bundle and its home.
pub(crate) fn locations(root: &Path) -> crate::Locations {
    crate::Locations::new(
        &root.join("library"),
        &root.join("Obelisk.app"),
        &root.join("home"),
    )
}

/// Installs the Linux engine's runtime as Voile's development runtime: its
/// engine, its Wine and Neptune's Direct3D, each a placeholder.
pub(crate) fn engine_runtime(locations: &crate::Locations) -> PathBuf {
    let runtime = locations.voile.join("current/linux");
    for file in ["engine/fexdarwin-run", "wine/bin/wine"] {
        fs::create_dir_all(runtime.join(file).parent().unwrap()).unwrap();
        fs::write(runtime.join(file), "").unwrap();
    }
    fs::create_dir_all(runtime.join("windows")).unwrap();
    for library in ["d3d11.dll", "dxgi.dll", "d3d12.dll"] {
        fs::write(runtime.join("windows").join(library), library).unwrap();
    }
    runtime
}

pub(crate) fn config(root: &Path) -> Config {
    Config {
        runtime: root.join("runtime"),
        prefix: root.join("prefix"),
        log: root.join("logs/wine-game.log"),
        settings: Settings::default(),
    }
}

/// A storefront that adds nothing, or the prerequisites it is given.
#[derive(Default)]
pub(crate) struct Plain {
    pub prerequisites: Vec<Prerequisite>,
}
impl Storefront for Plain {
    fn prerequisites(&self) -> Result<Vec<Prerequisite>, Error> {
        Ok(self.prerequisites.clone())
    }
    fn furnish(&self, config: &Config) -> Result<(), Error> {
        fs::write(config.prefix.join("furnished"), "")?;
        Ok(())
    }
}

/// A storefront whose games start behind its helper and read a variable
/// their session sets.
struct Behind;
impl Storefront for Behind {
    const HELPER: Option<&'static str> = Some(r"C:\helper.exe");
    const PASSED: &'static [&'static str] = &["SESSION_PORT"];
    fn prerequisites(&self) -> Result<Vec<Prerequisite>, Error> {
        Ok(Vec::new())
    }
}

/// A game installed in `install`, without a store title.
fn plain(install: &Path) -> Game<'_> {
    Game { install, title: "" }
}

/// The engine's configuration of a prefix beside `root`.
fn linux_config(root: &Path) -> Config {
    Config {
        prefix: root.join("Prefixes/game-linux/prefix"),
        settings: Settings {
            engine: Engine::Linux,
            ..Settings::default()
        },
        ..config(root)
    }
}

/// A storefront whose games reach Valve's steamclient.
struct Client;
impl Storefront for Client {
    const STEAM_CLIENT: bool = true;
    fn prerequisites(&self) -> Result<Vec<Prerequisite>, Error> {
        Ok(Vec::new())
    }
}

/// `engine`'s configuration of a prefix beside `root`, with the backend it
/// renders with by default.
fn engine_config(root: &Path, engine: Engine) -> Config {
    Config {
        prefix: engine
            .adapter()
            .prefix(&root.join("Prefixes/game"))
            .unwrap(),
        settings: Settings {
            engine,
            graphics: Graphics::default_on(engine),
            ..Settings::default()
        },
        ..config(root)
    }
}

/// What Windows sees of `target`: the command line it starts and the
/// variables Obelisk gives it, in the order it gives them.
fn windows_side(config: &Config, target: &Target) -> (Vec<String>, Vec<(String, String)>) {
    use super::linux_engine::tests::{guest_command, options};
    match config.settings.engine {
        Engine::Rosetta => (
            target.arguments.clone(),
            target.environment.clone().into_iter().collect(),
        ),
        Engine::Linux => (
            guest_command(target).to_vec(),
            options(target, "--env")
                .into_iter()
                .map(|variable| {
                    let (name, value) = variable.split_once('=').unwrap();
                    (name.to_owned(), value.to_owned())
                })
                .collect(),
        ),
    }
}

/// A Windows game inside `root/game`, and its target.
pub(crate) fn game(root: &Path) -> (PathBuf, LaunchTarget) {
    let install = root.join("game");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();
    let target = target(&install.join("Game.exe"), &[]);
    (install, target)
}

/// The environment Wine commands of `config` run with.
fn environment(config: &Config) -> HashMap<String, String> {
    let (install, target) = game(config.prefix.parent().unwrap());
    super::target::<Plain>(config, &plain(&install), &target, HashMap::new())
        .unwrap()
        .environment
}

#[test]
fn a_windows_game_starts_in_wine_itself_when_no_storefront_helper_stands_in() {
    let (_directory, root) = installation();
    let install = root.join("game");
    fs::create_dir_all(install.join("Binaries")).unwrap();
    fs::write(install.join("Binaries/Game.exe"), pe()).unwrap();
    let config = config(&root);

    let target = super::target::<Plain>(
        &config,
        &plain(&install),
        &target(&install.join("Binaries/Game.exe"), &["-EpicPortal"]),
        HashMap::from([("HOME".to_owned(), "/home".to_owned())]),
    )
    .unwrap();

    assert_eq!(target.executable, root.join("runtime/bin/wine"));
    assert_eq!(
        target.arguments,
        [
            install.join("Binaries/Game.exe").to_string_lossy().as_ref(),
            "-EpicPortal"
        ]
    );
    assert_eq!(target.working_directory, install.join("Binaries"));
    assert_eq!(target.environment["HOME"], "/home");
    assert_eq!(target.environment["WINEMSYNC"], "1");
    assert_eq!(
        target.environment["WINEPREFIX"],
        root.join("prefix").to_string_lossy()
    );
    assert_eq!(target.log, Some(root.join("logs/wine-game.log")));
}

#[test]
fn an_engine_game_takes_its_store_title_in_the_dock_without_trademark_signs_or_else_its_program_s_name()
 {
    let (_directory, root) = installation();
    let install = root.join("install");
    fs::create_dir_all(&install).unwrap();
    let executable = install.join("RocketLeague_EAC.exe");
    fs::write(&executable, pe()).unwrap();
    let config = linux_config(&root);
    let named = |title: &str| {
        let game = Game {
            title,
            ..plain(&install)
        };
        super::target::<Plain>(&config, &game, &target(&executable, &[]), HashMap::new())
            .unwrap()
            .environment[super::linux_engine::APP_NAME]
            .clone()
    };

    assert_eq!(named("Rocket League® "), "Rocket League");
    assert_eq!(named(""), "RocketLeague_EAC");
}

#[test]
fn a_storefront_s_helper_and_session_variables_reach_its_game_on_either_engine() {
    use super::linux_engine::tests::{guest_command, options};
    let (_directory, root) = installation();
    let (install, launched) = game(&root);
    let program = install.join("Game.exe").to_string_lossy().into_owned();
    let (rosetta, linux) = (config(&root), linux_config(&root));

    let behind =
        super::target::<Behind>(&rosetta, &plain(&install), &launched, HashMap::new()).unwrap();
    assert_eq!(behind.arguments, [r"C:\helper.exe", program.as_str()]);
    let behind =
        super::target::<Behind>(&linux, &plain(&install), &launched, HashMap::new()).unwrap();
    assert_eq!(
        guest_command(&behind),
        [r"C:\helper.exe", "/opt/game/Game.exe"]
    );
    assert_eq!(options(&behind, "--pass"), ["SESSION_PORT"]);

    let alone =
        super::target::<Plain>(&rosetta, &plain(&install), &launched, HashMap::new()).unwrap();
    assert_eq!(alone.arguments, [program.as_str()]);
    let alone =
        super::target::<Plain>(&linux, &plain(&install), &launched, HashMap::new()).unwrap();
    assert_eq!(guest_command(&alone), ["/opt/game/Game.exe"]);
    assert!(options(&alone, "--pass").is_empty());
}

#[test]
fn the_player_s_arguments_follow_the_storefront_s_on_either_engine() {
    for offer in crate::launch_settings::ENGINES {
        let (_directory, root) = installation();
        let (install, launched) = game(&root);
        let launched = LaunchTarget {
            arguments: vec!["-AUTH_TYPE=exchangecode".into()],
            ..launched
        };
        let config = engine_config(&root, offer.engine);
        let config = Config {
            settings: Settings {
                arguments: vec!["-dx11".into()],
                ..config.settings.clone()
            },
            ..config
        };

        let started =
            super::target::<Plain>(&config, &plain(&install), &launched, HashMap::new()).unwrap();

        let (command, _) = windows_side(&config, &started);
        assert_eq!(
            command[command.len() - 2..],
            ["-AUTH_TYPE=exchangecode", "-dx11"],
            "{:?}",
            offer.engine
        );
    }
}

#[test]
fn a_profile_s_variables_and_dll_overrides_take_effect_on_either_engine() {
    for offer in crate::launch_settings::ENGINES {
        let (_directory, root) = installation();
        let (install, launched) = game(&root);
        let config = engine_config(&root, offer.engine);
        let config = Config {
            settings: Settings {
                // WINEDEBUG is one of Obelisk's own on both engines.
                variables: BTreeMap::from([
                    ("WINENCPU".to_owned(), "10".to_owned()),
                    ("WINEDEBUG".to_owned(), "-all".to_owned()),
                ]),
                dll_overrides: BTreeMap::from([
                    ("xaudio2_7".to_owned(), "n,b".to_owned()),
                    ("SpeechSynthWrapper".to_owned(), String::new()),
                ]),
                ..config.settings.clone()
            },
            ..config
        };

        let started =
            super::target::<Plain>(&config, &plain(&install), &launched, HashMap::new()).unwrap();

        // Of a name given twice, the last counts.
        let (_, variables) = windows_side(&config, &started);
        let last = |name: &str| {
            variables
                .iter()
                .rev()
                .find(|(variable, _)| variable == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            last("WINENCPU").as_deref(),
            Some("10"),
            "{:?}",
            offer.engine
        );
        assert_eq!(
            last("WINEDEBUG").as_deref(),
            Some("-all"),
            "{:?}",
            offer.engine
        );
        let own = match offer.engine {
            Engine::Rosetta => "mscoree,mshtml=;winemenubuilder.exe=d",
            Engine::Linux => "mscoree,mshtml=;winemenubuilder.exe=d;d3d11,dxgi,d3d12=n",
        };
        assert_eq!(
            last("WINEDLLOVERRIDES"),
            Some(format!("{own};SpeechSynthWrapper=;xaudio2_7=n,b"))
        );
    }
}

#[test]
fn every_variable_and_dll_an_engine_sets_is_one_it_reserves() {
    for offer in crate::launch_settings::ENGINES {
        let (_directory, root) = installation();
        let (install, launched) = game(&root);
        let config = engine_config(&root, offer.engine);
        let config = Config {
            settings: Settings {
                graphics: Graphics::D3DMetal,
                options: GraphicsOptions { ray_tracing: false },
                ..config.settings.clone()
            },
            ..config
        };
        // A runtime that carries every optional piece, but no Mono.
        for folder in ["lib/gstreamer-1.0", "guest"] {
            fs::create_dir_all(config.runtime.join(folder)).unwrap();
        }
        backends_runtime(&config);
        fs::write(config.runtime.join("guest/steam-hostpid.so"), b"elf").unwrap();

        let started =
            super::target::<Client>(&config, &plain(&install), &launched, HashMap::new()).unwrap();

        let reserved = offer.engine.reserved();
        let (_, variables) = windows_side(&config, &started);
        for (name, _) in &variables {
            assert!(
                reserved.variables.contains(&name.as_str()),
                "{:?} {name}",
                offer.engine
            );
        }
        let (_, overrides) = variables
            .iter()
            .find(|(name, _)| name == "WINEDLLOVERRIDES")
            .unwrap();
        let dlls = overrides
            .split(';')
            .filter_map(|entry| entry.split_once('='))
            .flat_map(|(dlls, _)| dlls.split(','));
        for dll in dlls {
            assert!(
                reserved.dlls.contains(&dll.to_ascii_lowercase().as_str()),
                "{:?} {dll}",
                offer.engine
            );
        }
    }
}

#[test]
fn only_a_windows_program_inside_the_installation_starts_or_readies_a_prefix_on_either_engine() {
    for offer in crate::launch_settings::ENGINES {
        let (_directory, root) = installation();
        let config = engine_config(&root, offer.engine);
        record_calls(&config.runtime);
        let install = root.join("game");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("script.exe"), b"#!/bin/sh\n").unwrap();
        fs::write(root.join("outside.exe"), pe()).unwrap();

        for executable in [install.join("script.exe"), root.join("outside.exe")] {
            let target = target(&executable, &[]);
            assert!(matches!(
                super::target::<Plain>(&config, &plain(&install), &target, HashMap::new()),
                Err(Error::NotExecutable)
            ));
            assert!(matches!(
                prepare(
                    &config,
                    &locations(&root),
                    &install,
                    &target,
                    &Plain::default()
                ),
                Err(Error::NotExecutable)
            ));
        }
        assert!(!root.join("Prefixes").exists(), "{:?}", offer.engine);
    }
}

#[test]
fn a_missing_runtime_is_reported_before_any_prefix_is_created_on_either_engine() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    for offer in crate::launch_settings::ENGINES {
        let install = root.join(format!("{:?}", offer.engine));
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("Game.exe"), pe()).unwrap();
        let choices = crate::launch_settings::Choices {
            engine: Some(offer.engine),
            ..Default::default()
        };
        crate::launch_settings::choose(&locations.library, OTHER, &install, None, choices).unwrap();
        let windows = windows_of(&locations, OTHER, &install);

        assert!(matches!(
            windows.ready(
                &locations,
                &target(&install.join("Game.exe"), &[]),
                &Plain::default()
            ),
            Err(Error::WineUnavailable)
        ));
        assert!(!locations.wine_prefixes.exists(), "{:?}", offer.engine);
    }
}

#[test]
fn the_engine_s_launcher_alone_gets_a_grace_and_wine_for_macos_alone_an_input_service() {
    let (_directory, root) = installation();
    let rosetta = engine_config(&root, Engine::Rosetta);
    assert_eq!(rosetta.launcher_grace(), None);
    assert_eq!(
        rosetta.input_service(),
        Some(root.join("runtime/linux/engine/obelisk-input"))
    );
    assert_eq!(rosetta.architecture(), Architecture::Intel);

    let linux = engine_config(&root, Engine::Linux);
    assert_eq!(linux.launcher_grace(), Some(Duration::from_secs(2)));
    assert_eq!(linux.input_service(), None);
    assert_eq!(linux.architecture(), Architecture::Arm64);
}

#[test]
fn a_private_file_is_read_through_the_z_drive_on_rosetta_and_from_the_c_drive_on_the_engine() {
    let (_directory, root) = installation();
    for config in [config(&root), linux_config(&root)] {
        fs::create_dir_all(config.prefix.join("drive_c")).unwrap();

        let (folder, path) = private_file(&config, "ownership.bin", b"token").unwrap();

        let file = match config.settings.engine {
            Engine::Rosetta => PathBuf::from(path.strip_prefix("Z:").unwrap().replace('\\', "/")),
            Engine::Linux => config
                .prefix
                .join("drive_c")
                .join(path.strip_prefix("C:\\").unwrap().replace('\\', "/")),
        };
        assert!(path.ends_with("\\ownership.bin"), "{path}");
        assert_eq!(fs::read(&file).unwrap(), b"token");
        drop(folder);
        assert!(!file.exists());
    }
}

#[test]
fn each_storefront_s_game_is_readied_in_its_prefix_and_logs_into_its_store_s_diagnostics_folder() {
    use crate::profile::Game::{Epic, Gog, Steam};
    use crate::queue::Storefront;
    let rows: [(crate::profile::Game, &str, Storefront, &str); 3] = [
        (
            Steam { app_id: 42 },
            "Steam/steam-42-windows",
            Storefront::Steam,
            "wine-42.log",
        ),
        (
            Epic {
                namespace: "space",
                app_name: "game",
            },
            "Epic/epic-hash-Windows",
            Storefront::Epic,
            "wine-game.log",
        ),
        (
            Gog {
                product_id: "1207658924",
            },
            "GOG/gog-1207658924-windows",
            Storefront::Gog,
            "wine-1207658924.log",
        ),
    ];
    for (game, folder, store, log) in rows {
        let (_directory, root) = installation();
        let locations = locations(&root);
        record_calls(&locations.voile.join("current"));
        let install = locations.library.join("Games").join(folder);
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("Game.exe"), pe()).unwrap();
        fs::write(install.join("setup.exe"), "").unwrap();
        let storefront = Plain {
            prerequisites: vec![Prerequisite {
                id: "setup".into(),
                programs: vec![(install.join("setup.exe"), "/quiet".into())],
            }],
        };
        let windows = windows_of(&locations, game, &install);

        let config = windows
            .ready(
                &locations,
                &target(&install.join("Game.exe"), &[]),
                &storefront,
            )
            .unwrap();

        let prefix = locations.wine_prefixes.join(install.file_name().unwrap());
        assert_eq!(windows.prefix(), prefix);
        assert_eq!(config.prefix, prefix);
        let diagnostics = crate::diagnostics::folder(&locations.home, store);
        assert_eq!(config.log, diagnostics.join(log));
        assert!(diagnostics.is_dir(), "{game:?}");
        assert_eq!(
            calls(&locations.wine_prefixes),
            [
                "wineboot --init".to_owned(),
                format!("{} /quiet", install.join("setup.exe").display()),
            ]
        );
        assert!(prefix.join("furnished").is_file());
    }
}

#[test]
fn the_program_a_profile_names_instead_is_the_one_readied_and_the_one_started() {
    use super::linux_engine::tests::guest_command;
    let rocket_league = crate::profile::Game::Epic {
        namespace: "9773aa1aa54f4f7b80e44bef04986cea",
        app_name: "Sugar",
    };
    let (_directory, root) = installation();
    let locations = locations(&root);
    engine_runtime(&locations);
    record_calls(&locations.voile.join("current"));
    let install = locations.library.join("Games/Epic/epic-rl-Windows");
    fs::create_dir_all(install.join("Binaries/Win64")).unwrap();
    // The publisher's program is no Windows program Wine could start.
    fs::write(install.join("Launcher.exe"), b"#!/bin/sh\n").unwrap();
    fs::write(install.join("Binaries/Win64/RocketLeague_EAC.exe"), pe()).unwrap();
    let launcher = target(&install.join("Launcher.exe"), &["-AUTH_TYPE=exchangecode"]);
    let windows = windows_of(&locations, rocket_league, &install);
    // A prefix the engine already created.
    fs::create_dir_all(windows.prefix().join("drive_c/windows/system32")).unwrap();
    fs::write(windows.prefix().join("system.reg"), "registry").unwrap();

    let config = windows
        .ready(&locations, &launcher, &Plain::default())
        .unwrap();
    let started =
        super::target::<Plain>(&config, &plain(&install), &launcher, HashMap::new()).unwrap();

    assert_eq!(
        guest_command(&started),
        [
            "/opt/game/Binaries/Win64/RocketLeague_EAC.exe",
            "-AUTH_TYPE=exchangecode"
        ]
    );
    // The engine's prefix renders through Neptune's Direct3D.
    assert_eq!(
        fs::read(windows.prefix().join("drive_c/windows/system32/d3d11.dll")).unwrap(),
        b"d3d11.dll"
    );
    // Without the profile, the publisher's program is neither readied nor
    // started.
    let other = windows_of(&locations, OTHER, &install);
    assert!(matches!(
        other.ready(&locations, &launcher, &Plain::default()),
        Err(Error::NotExecutable)
    ));
    let without = Config {
        settings: Settings {
            executable: None,
            ..config.settings.clone()
        },
        ..config.clone()
    };
    assert!(matches!(
        super::target::<Plain>(&without, &plain(&install), &launcher, HashMap::new()),
        Err(Error::NotExecutable)
    ));
    assert!(calls(&locations.wine_prefixes).is_empty());
    assert!(!other.prefix().exists());
}

#[test]
fn a_d3dmetal_game_is_translated_once_it_is_readied() {
    use std::os::unix::fs::PermissionsExt;
    let (_directory, root) = installation();
    let locations = locations(&root);
    engine_runtime(&locations);
    // Obelisk's Intel helper only records what it was asked.
    let helper = locations.helpers.join(crate::package::SDK_INTEL);
    fs::create_dir_all(&locations.helpers).unwrap();
    let record = root.join("translated");
    fs::write(
        &helper,
        format!("#!/bin/sh\necho \"$1\" >> \"{}\"\n", record.display()),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let (install, target) = game(&root);
    let windows = windows_of(&locations, ELDEN_RING, &install);
    fs::create_dir_all(windows.prefix().join("drive_c/windows/system32")).unwrap();
    fs::write(windows.prefix().join("system.reg"), "registry").unwrap();

    windows
        .ready(&locations, &target, &Plain::default())
        .unwrap();

    assert_eq!(fs::read_to_string(&record).unwrap(), "warm-d3dmetal\n");
}

#[test]
fn d3dmetal_is_selected_only_when_the_runtime_carries_it() {
    let (_directory, root) = installation();
    let config = config(&root);
    assert!(!environment(&config).contains_key("CX_ACTIVE_GRAPHICS_BACKEND"));

    let library = root.join("runtime/lib/external/libd3dshared.dylib");
    fs::create_dir_all(library.parent().unwrap()).unwrap();
    fs::write(&library, b"").unwrap();

    let environment = environment(&config);
    assert_eq!(environment["CX_ACTIVE_GRAPHICS_BACKEND"], "d3dmetal");
    assert_eq!(
        environment["CX_APPLEGPTK_LIBD3DSHARED_PATH"],
        library.to_string_lossy()
    );
}

/// Lays in `config`'s runtime D3DMetal, its DLLs beside Wine's, and DXMT's.
fn backends_runtime(config: &Config) {
    for folder in [
        "lib/d3dmetal/x86_64-windows",
        "lib/dxmt/i386-windows",
        "lib/external/D3DMetal.framework",
    ] {
        fs::create_dir_all(config.runtime.join(folder)).unwrap();
    }
    for file in [
        "lib/external/libd3dshared.dylib",
        "lib/external/D3DMetal.framework/D3DMetal",
    ] {
        fs::write(config.runtime.join(file), b"").unwrap();
    }
}

#[test]
fn a_game_s_backend_comes_before_wine_s_own_dlls_with_dxmt_behind_d3dmetal() {
    let (_directory, root) = installation();
    let d3dmetal = config(&root);
    backends_runtime(&d3dmetal);
    let runtime = d3dmetal.runtime.to_string_lossy().into_owned();

    let environment = environment(&d3dmetal);
    assert_eq!(
        environment["WINEDLLPATH_PREPEND"],
        format!("{runtime}/lib/d3dmetal:{runtime}/lib/dxmt")
    );
    assert_eq!(environment["CX_ACTIVE_GRAPHICS_BACKEND"], "d3dmetal");
    assert_eq!(
        environment["D3DMETAL_FRAMEWORK_PATH"],
        format!("{runtime}/lib/external/D3DMetal.framework/D3DMetal")
    );

    let dxmt = Config {
        settings: Settings {
            graphics: Graphics::Dxmt,
            ..d3dmetal.settings.clone()
        },
        ..d3dmetal
    };
    let environment = self::environment(&dxmt);
    assert_eq!(
        environment["WINEDLLPATH_PREPEND"],
        format!("{runtime}/lib/dxmt")
    );
    assert_eq!(environment["CX_ACTIVE_GRAPHICS_BACKEND"], "dxmt");
    assert_eq!(
        environment["CX_APPLEGPTK_LIBD3DSHARED_PATH"],
        format!("{runtime}/lib/external/libd3dshared.dylib")
    );
}

#[test]
fn a_runtime_with_d3dmetal_over_wine_s_dlls_searches_wine_s_folder_alone() {
    let (_directory, root) = installation();
    let config = config(&root);
    fs::create_dir_all(config.runtime.join("lib/external")).unwrap();
    fs::write(config.runtime.join("lib/external/libd3dshared.dylib"), b"").unwrap();

    let environment = environment(&config);

    assert!(!environment.contains_key("WINEDLLPATH_PREPEND"));
    assert_eq!(environment["CX_ACTIVE_GRAPHICS_BACKEND"], "d3dmetal");
}

#[test]
fn turning_ray_tracing_off_reaches_d3dmetal_alone() {
    let (_directory, root) = installation();
    let config = config(&root);
    let defaults = environment(&config);
    assert!(!defaults.keys().any(|name| name.starts_with("D3DM_")));

    let options = Config {
        settings: Settings {
            options: GraphicsOptions { ray_tracing: false },
            ..config.settings.clone()
        },
        ..config
    };
    let environment = self::environment(&options);
    assert_eq!(environment["D3DM_SUPPORT_DXR"], "0");

    let dxmt = Config {
        settings: Settings {
            graphics: Graphics::Dxmt,
            ..options.settings.clone()
        },
        ..options
    };
    assert!(
        !self::environment(&dxmt)
            .keys()
            .any(|name| name.starts_with("D3DM_"))
    );
}

/// A runtime whose wine records each call with the folders it searches
/// first, in `calls` beside the prefix.
fn backend_recording_runtime(root: &Path) -> Config {
    let config = recording_runtime(root);
    fs::write(
        config.runtime.join("bin/wine"),
        "#!/bin/sh\nprintf '%s %s\\n' \"$*\" \"$WINEDLLPATH_PREPEND\" >> \"$WINEPREFIX/../calls\"\n",
    )
    .unwrap();
    config
}

#[test]
fn a_prefix_gets_every_backend_s_placeholders_when_created_or_when_its_runtime_gains_one() {
    let (_directory, root) = installation();
    let config = backend_recording_runtime(&root);
    let config = Config {
        settings: Settings {
            graphics: Graphics::Dxmt,
            ..config.settings.clone()
        },
        ..config
    };
    let runtime = config.runtime.to_string_lossy().into_owned();
    prepare_plain(&config, &root);
    fs::write(config.prefix.join("system.reg"), "").unwrap();
    prepare_plain(&config, &root);
    assert_eq!(calls(&root), ["wineboot --init "]);

    backends_runtime(&config);
    prepare_plain(&config, &root);
    prepare_plain(&config, &root);

    assert_eq!(
        calls(&root),
        [
            "wineboot --init ".to_owned(),
            format!("wineboot --update {runtime}/lib/d3dmetal:{runtime}/lib/dxmt"),
        ]
    );
    assert_eq!(
        fs::read_to_string(config.prefix.join("obelisk-graphics")).unwrap(),
        "d3dmetal dxmt"
    );
}

#[test]
fn launch_settings_move_a_game_to_the_engine_and_backend_its_player_chose() {
    let (_directory, root) = installation();
    let library = root.join("library");
    let locations = locations(&root);
    engine_runtime(&locations);
    let install = library.join("Games/Steam/steam-42-windows");
    let choices = crate::launch_settings::Choices {
        engine: Some(Engine::Linux),
        graphics: Some(Graphics::D3DMetal),
        performance_hud: true,
        ..Default::default()
    };
    crate::launch_settings::choose(&locations.library, OTHER, &install, None, choices).unwrap();

    let windows = windows_of(&locations, OTHER, &install);

    assert_eq!(
        windows.prefix(),
        library.join("Prefixes/steam-42-windows-linux/prefix")
    );
    let config = windows.runtime(&locations).unwrap();
    assert_eq!(config.settings.engine, Engine::Linux);
    assert_eq!(config.settings.graphics, Graphics::D3DMetal);
    assert!(config.settings.performance_hud);
}

#[test]
fn the_game_s_log_starts_with_how_obelisk_runs_it_and_only_the_player_s_arguments() {
    let (_directory, root) = installation();
    let config = Config {
        settings: Settings {
            options: GraphicsOptions { ray_tracing: false },
            profile_arguments: vec!["-secret".into()],
            arguments: vec!["-dx11".into(), "+exec auto exec.cfg".into(), String::new()],
            ..Settings::default()
        },
        ..config(&root)
    };
    assert_eq!(
        config.log_header(),
        "obelisk: engine=wine-macos graphics=d3dmetal ray_tracing=off voile=development \
         arguments=-dx11 \"+exec auto exec.cfg\" \"\"\n"
    );
    assert_eq!(
        command_line(
            config
                .log_header()
                .split_once("arguments=")
                .unwrap()
                .1
                .trim_end()
        ),
        config.settings.arguments
    );
    assert!(!config.log_header().contains("-secret"));

    // The Linux engine's render servers take no options; a release names
    // itself in its manifest, which the engine's runtime sits inside.
    fs::create_dir_all(root.join("v26.3.0-15/linux")).unwrap();
    fs::write(
        root.join("v26.3.0-15/runtime.json"),
        r#"{"version": "v26.3.0-15"}"#,
    )
    .unwrap();
    let linux = Config {
        runtime: root.join("v26.3.0-15/linux"),
        settings: Settings {
            engine: Engine::Linux,
            graphics: Graphics::Dxmt,
            ..Settings::default()
        },
        ..self::config(&root)
    };
    assert_eq!(
        linux.log_header(),
        "obelisk: engine=wine-linux graphics=dxmt voile=v26.3.0-15\n"
    );
}

#[test]
fn wine_reports_its_errors_unless_the_runtime_names_other_channels() {
    let (_directory, root) = installation();
    let config = config(&root);
    assert_eq!(environment(&config)["WINEDEBUG"], "err+all,fixme-all");

    fs::create_dir_all(&config.runtime).unwrap();
    fs::write(config.runtime.join("winedebug"), "+steamclient,err+all\n").unwrap();

    assert_eq!(environment(&config)["WINEDEBUG"], "+steamclient,err+all");
}

#[test]
fn dotnet_games_run_only_on_the_mono_a_runtime_carries() {
    let (_directory, root) = installation();
    let config = config(&root);
    assert_eq!(
        environment(&config)["WINEDLLOVERRIDES"],
        "mscoree,mshtml=;winemenubuilder.exe=d"
    );

    fs::create_dir_all(config.runtime.join("share/wine/mono/wine-mono-10.4.1")).unwrap();

    assert_eq!(
        environment(&config)["WINEDLLOVERRIDES"],
        "mshtml=;winemenubuilder.exe=d"
    );
}

#[test]
fn every_wine_command_syncs_through_mach_and_gives_32_bit_games_4_gb() {
    let (_directory, root) = installation();
    let environment = environment(&config(&root));

    assert_eq!(environment["WINEMSYNC"], "1");
    assert_eq!(environment["WINE_LARGE_ADDRESS_AWARE"], "1");
}

#[test]
fn a_profile_replaces_obelisk_s_own_variables_on_rosetta_s_wine_and_opens_the_log() {
    let (_directory, root) = installation();
    let config = Config {
        settings: Settings {
            variables: BTreeMap::from([
                ("ROSETTA_ADVERTISE_AVX".to_owned(), String::new()),
                ("WINENCPU".to_owned(), "10".to_owned()),
            ]),
            dll_overrides: BTreeMap::from([
                ("xaudio2_7".to_owned(), "n,b".to_owned()),
                ("SpeechSynthWrapper".to_owned(), String::new()),
            ]),
            ..Settings::default()
        },
        ..config(&root)
    };

    assert_eq!(environment(&config)["ROSETTA_ADVERTISE_AVX"], "");
    assert_eq!(
        config.log_header(),
        "obelisk: engine=wine-macos graphics=d3dmetal ray_tracing=on voile=development \
         variables=ROSETTA_ADVERTISE_AVX= WINENCPU=10 \
         dll_overrides=SpeechSynthWrapper=;xaudio2_7=n,b\n"
    );
}

#[test]
fn every_wine_command_sees_the_avx_rosetta_translates() {
    let (_directory, root) = installation();

    assert_eq!(environment(&config(&root))["ROSETTA_ADVERTISE_AVX"], "1");
}

#[test]
fn dotnet_programs_map_their_jit_code_without_w_xor_x() {
    let (_directory, root) = installation();

    assert_eq!(
        environment(&config(&root))["DOTNET_EnableWriteXorExecute"],
        "0"
    );
}

#[test]
fn gstreamer_loads_only_the_plugins_the_runtime_carries() {
    let (_directory, root) = installation();
    let config = config(&root);
    assert!(!environment(&config).contains_key("GST_PLUGIN_SYSTEM_PATH"));
    assert!(!environment(&config).contains_key("GST_REGISTRY"));

    let plugins = config.runtime.join("lib/gstreamer-1.0");
    fs::create_dir_all(&plugins).unwrap();

    let environment = environment(&config);
    assert_eq!(
        environment["GST_PLUGIN_SYSTEM_PATH"],
        plugins.to_string_lossy()
    );
    assert_eq!(
        environment["GST_REGISTRY"],
        config
            .prefix
            .join("obelisk-gstreamer-registry.bin")
            .to_string_lossy()
    );
}

#[test]
fn a_prefix_profile_stops_linking_into_the_mac_home() {
    let (_directory, root) = installation();
    let home = root.join("home");
    fs::create_dir_all(home.join("Documents")).unwrap();
    fs::write(home.join("Documents/keep.txt"), b"mine").unwrap();
    let profile = profile(&root.join("prefix"));
    fs::create_dir_all(profile.join("Desktop")).unwrap();
    fs::create_dir_all(profile.join("AppData/LocalLow")).unwrap();
    std::os::unix::fs::symlink(home.join("Documents"), profile.join("Documents")).unwrap();
    std::os::unix::fs::symlink(&home, profile.join("Desktop/My Mac Desktop")).unwrap();
    let config = recording_runtime(&root);
    fs::write(config.prefix.join("system.reg"), "").unwrap();
    let (install, target) = game(&root);

    prepare(
        &config,
        &locations(&root),
        &install,
        &target,
        &Plain::default(),
    )
    .unwrap();

    assert!(
        fs::symlink_metadata(profile.join("Documents"))
            .unwrap()
            .is_dir()
    );
    assert_eq!(fs::read_dir(profile.join("Documents")).unwrap().count(), 0);
    assert!(!profile.join("Desktop/My Mac Desktop").exists());
    assert!(profile.join("AppData/LocalLow").is_dir());
    assert_eq!(fs::read(home.join("Documents/keep.txt")).unwrap(), b"mine");
}

#[test]
fn a_prefix_registers_the_runtime_mono_once_even_when_an_older_runtime_created_it() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    // A runtime without Mono only creates the prefix.
    prepare_plain(&config, &root);
    assert_eq!(calls(&root), ["wineboot --init"]);
    assert!(!config.prefix.join("obelisk-mono").exists());

    fs::write(config.prefix.join("system.reg"), "").unwrap();
    fs::create_dir_all(config.runtime.join("share/wine/mono/wine-mono-10.4.1")).unwrap();
    prepare_plain(&config, &root);
    prepare_plain(&config, &root);

    assert_eq!(calls(&root), ["wineboot --init", "wineboot --update"]);
    assert_eq!(
        fs::read_to_string(config.prefix.join("obelisk-mono")).unwrap(),
        "wine-mono-10.4.1"
    );
}

#[test]
fn a_new_prefix_gets_mono_from_its_creation_alone() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    fs::create_dir_all(config.runtime.join("share/wine/mono/wine-mono-10.4.1")).unwrap();

    prepare_plain(&config, &root);

    assert_eq!(calls(&root), ["wineboot --init"]);
    assert_eq!(
        fs::read_to_string(config.prefix.join("obelisk-mono")).unwrap(),
        "wine-mono-10.4.1"
    );
}

fn prepare_plain(config: &Config, root: &Path) {
    let (install, target) = game(root);
    prepare(
        config,
        &locations(root),
        &install,
        &target,
        &Plain::default(),
    )
    .unwrap();
}

/// A runtime whose wine records each call in `calls` beside the prefix, and
/// fails for any program named `fail`.
pub(crate) fn recording_runtime(root: &Path) -> Config {
    let config = config(root);
    fs::create_dir_all(&config.prefix).unwrap();
    record_calls(&config.runtime);
    config
}

/// Installs in `runtime` a wine that records each call in `calls` beside the
/// prefix it runs, and fails for any program named `fail`.
pub(crate) fn record_calls(runtime: &Path) {
    let bin = runtime.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, body) in [
        (
            "wine",
            "printf '%s\\n' \"$*\" >> \"$WINEPREFIX/../calls\"\ncase \"$*\" in *fail*) exit 1;; esac\n",
        ),
        ("wineserver", ""),
    ] {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

pub(crate) fn calls(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_windows_installation_runs_in_the_prefix_named_after_its_folder_whatever_the_storefront() {
    let locations = crate::Locations::new(
        Path::new("/library"),
        Path::new("/Obelisk.app"),
        Path::new("/home"),
    );
    for install in [
        "/library/Games/Steam/steam-42-windows",
        "/library/Games/Epic/epic-hash-Windows",
    ] {
        let windows = windows_of(&locations, OTHER, Path::new(install));
        let folder = Path::new(install).file_name().unwrap();
        assert_eq!(
            windows.prefix(),
            Path::new("/library/Prefixes").join(folder)
        );
    }
    let root = Path::new("/");
    let launch_settings = crate::launch_settings::of(&locations.library, OTHER, root, None);
    assert!(
        Installation::new(&locations, OTHER, root, launch_settings).is_err(),
        "no folder names no prefix"
    );
    assert!(
        Engine::Rosetta
            .prefix(&locations.wine_prefixes, root)
            .is_err()
    );
}

#[test]
fn each_engine_keeps_its_prefix_of_an_installation_where_its_launch_runs() {
    let locations = crate::Locations::new(
        Path::new("/library"),
        Path::new("/Obelisk.app"),
        Path::new("/home"),
    );
    let install = Path::new("/library/Games/Steam/steam-42-windows");
    let prefixes = &locations.wine_prefixes;

    assert_eq!(
        Engine::Rosetta.prefix(prefixes, install).unwrap(),
        Path::new("/library/Prefixes/steam-42-windows")
    );
    assert_eq!(
        Engine::Linux.prefix(prefixes, install).unwrap(),
        Path::new("/library/Prefixes/steam-42-windows-linux/prefix")
    );
    for (game, engine) in [(OTHER, Engine::Rosetta), (ELDEN_RING, Engine::Linux)] {
        assert_eq!(
            windows_of(&locations, game, install).prefix(),
            engine.prefix(prefixes, install).unwrap(),
            "{game:?}"
        );
    }
}

#[test]
fn a_configuration_reaches_its_game_session_whole() {
    let config = Config {
        runtime: "/Voile/v1/linux".into(),
        prefix: "/Prefixes/game-linux/prefix".into(),
        log: "/logs/game.log".into(),
        settings: Settings {
            engine: Engine::Linux,
            graphics: Graphics::Dxmt,
            options: GraphicsOptions::default(),
            executable: Some("Binaries/Game.exe".into()),
            bootstrapper: Some("Binaries/start_protected_game.exe".into()),
            profile_arguments: vec!["-windowed".into()],
            arguments: vec!["-dx11".into(), "+exec auto exec.cfg".into()],
            variables: BTreeMap::from([("SteamDeck".to_owned(), "1".to_owned())]),
            dll_overrides: BTreeMap::from([("xaudio2_7".to_owned(), "n,b".to_owned())]),
            performance_hud: true,
            steam_client: SteamClient::Windows,
        },
    };

    let again: Config = serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();

    assert_eq!(again, config);
}

#[test]
fn a_game_whose_profile_names_the_linux_engine_runs_there_in_its_own_prefix_with_its_graphics() {
    let (_directory, root) = installation();
    let library = root.join("library");
    let locations = locations(&root);
    let runtime = engine_runtime(&locations);
    let install = library.join("Games/Steam/steam-1245620-windows");

    let windows = windows_of(&locations, ELDEN_RING, &install);

    assert_eq!(
        windows.prefix(),
        library.join("Prefixes/steam-1245620-windows-linux/prefix")
    );
    let config = windows.runtime(&locations).unwrap();
    assert_eq!(config.settings.engine, Engine::Linux);
    assert_eq!(config.settings.graphics, Graphics::D3DMetal);
    assert_eq!(config.runtime, runtime);
    let other = windows_of(&locations, OTHER, &install);
    assert_eq!(
        other.prefix(),
        library.join("Prefixes/steam-1245620-windows")
    );
    // Its launch waits for the Voile release its profile names.
    let rocket_league = crate::profile::Game::Steam { app_id: 252950 };
    assert_eq!(
        windows_of(&locations, rocket_league, &install).minimum_voile(),
        Some("v26.3.0-18")
    );
    assert_eq!(windows.minimum_voile(), None);
}

#[test]
fn an_uninstalled_windows_game_loses_its_prefix_but_never_what_a_link_points_to() {
    use obelisk_core::installation::RemovalFailure;
    let (_directory, root) = installation();
    let library = root.join("library");
    let locations = crate::Locations::new(&library, Path::new("/Obelisk.app"), Path::new("/home"));
    let removed = |name: &str| remove(&locations, &library.join("Games/Steam").join(name)).unwrap();
    let prefix = library.join("Prefixes/steam-42-windows");
    fs::create_dir_all(prefix.join("drive_c/users")).unwrap();
    fs::write(prefix.join("system.reg"), "registry").unwrap();
    removed("steam-42-windows").unwrap();
    assert!(!prefix.exists());
    removed("steam-42-windows").unwrap();

    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, library.join("Prefixes/steam-43-windows")).unwrap();
    assert!(matches!(
        removed("steam-43-windows"),
        Err(RemovalFailure::Unsafe(_))
    ));
    assert!(elsewhere.is_dir());
    assert!(
        remove(&locations, Path::new("/")).is_none(),
        "no folder names no prefix"
    );
}

#[test]
fn uninstalling_removes_both_engines_prefixes_and_the_launch_settings() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let install = root.join("library/Games/Steam/steam-1245620-windows");
    let rosetta = windows_of(&locations, OTHER, &install);
    fs::create_dir_all(rosetta.prefix().join("drive_c")).unwrap();
    let linux = windows_of(&locations, ELDEN_RING, &install);
    fs::create_dir_all(linux.prefix().join("drive_c")).unwrap();
    let choices = crate::launch_settings::Choices {
        engine: Some(Engine::Rosetta),
        ..Default::default()
    };
    crate::launch_settings::choose(&locations.library, ELDEN_RING, &install, None, choices)
        .unwrap();
    let settings = locations
        .library
        .join("LaunchSettings/steam-1245620-windows.json");
    assert!(settings.is_file());

    // The player moved the game back to Rosetta: the engine's folder still goes.
    remove(&locations, &install).unwrap().unwrap();

    assert!(!rosetta.prefix().exists());
    assert!(!linux.prefix().parent().unwrap().exists());
    assert!(!settings.exists());
}

#[test]
fn a_moved_installation_forgets_only_the_prerequisites_picked_in_its_rosetta_prefix() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let install = root.join("library/Games/GOG/gog-1207658924-windows");
    let prefix = locations.wine_prefixes.join("gog-1207658924-windows");
    fs::create_dir_all(&prefix).unwrap();
    fs::write(prefix.join("obelisk-prerequisites"), "gog:isi:1\nvcredist").unwrap();

    forget_prerequisites(&locations, &install, |id| id.starts_with("gog:isi:")).unwrap();

    assert_eq!(
        fs::read_to_string(prefix.join("obelisk-prerequisites")).unwrap(),
        "vcredist"
    );
    forget_prerequisites(&locations, Path::new("/"), |_| true).unwrap();
}

#[test]
fn only_processes_running_the_runtime_are_left_behind_wine_processes() {
    let listing = "p10\nftxt\nn/usr/lib/dyld\np20\nftxt\nn/usr/lib/dyld\nftxt\nn/lib/Voile/current/bin/wine-preloader\nftxt\nn/lib/Voile/current/lib/wine/ntdll.so\np30\nftxt\nn/lib/Voile-other/bin/wine\np40\nftxt\nn/lib/Voile/v1/bin/wineserver\n";
    assert_eq!(running_from(listing, Path::new("/lib/Voile"), 1), [20, 40]);
    assert_eq!(
        running_from(listing, Path::new("/lib/Voile"), 20),
        [40],
        "never Obelisk itself"
    );
}

#[test]
fn quitting_a_windows_game_asks_its_windows_to_close_and_kills_nothing() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    let environment = environment(&config);
    let program = root.join("game/Game.exe");
    let command = config
        .adapter()
        .quit_command(&config, &environment, Some(&program), 7)
        .unwrap();
    assert_eq!(command.get_program(), config.runtime.join("bin/wine"));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["taskkill", "/T", "/IM", "Game.exe"],
        "without /F, taskkill only posts WM_CLOSE"
    );
    // The game's own environment, which its Wine server agrees with.
    let envs: HashMap<_, _> = command
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.unwrap().to_owned()))
        .collect();
    assert_eq!(envs.len(), environment.len());
    assert!(
        environment
            .iter()
            .all(|(name, value)| envs[std::ffi::OsStr::new(name)] == **value)
    );

    quit(&config, &environment, Some(&program), 7);
    assert_eq!(calls(&root), ["taskkill /T /IM Game.exe"]);
    assert!(
        config
            .adapter()
            .quit_command(&config, &environment, None, 7)
            .is_none()
    );
}

#[test]
fn quitting_a_linux_engine_game_uses_its_window_server_quit() {
    let (_directory, root) = installation();
    let config = Config {
        settings: Settings {
            engine: Engine::Linux,
            ..Settings::default()
        },
        ..config(&root)
    };
    let environment = HashMap::from([("TMPDIR".to_owned(), "/var/folders/T/".to_owned())]);
    let command = config
        .adapter()
        .quit_command(&config, &environment, None, 77)
        .unwrap();
    assert_eq!(command.get_program(), config.runtime.join("engine/wsdctl"));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "/var/folders/T/obelisk-engine-77/wsd.sock",
            "window",
            "quit"
        ]
    );
    let command = config
        .adapter()
        .quit_command(&config, &HashMap::new(), None, 77)
        .unwrap();
    assert_eq!(
        command.get_args().next().unwrap(),
        "/tmp/obelisk-engine-77/wsd.sock"
    );
}

#[test]
fn only_a_prefix_and_its_wine_server_belong_to_one_windows_game() {
    use std::os::unix::fs::MetadataExt;
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    let prefix = fs::metadata(&config.prefix).unwrap();
    assert_eq!(
        private_folders(&config, &HashMap::new(), None),
        [
            config.prefix.clone(),
            PathBuf::from(format!(
                "/tmp/.wine-{}/server-{:x}-{:x}",
                prefix.uid(),
                prefix.dev(),
                prefix.ino()
            )),
        ]
    );
    let linux = Config {
        prefix: root.join("Game-linux/prefix"),
        settings: Settings {
            engine: Engine::Linux,
            ..config.settings.clone()
        },
        ..config
    };
    assert_eq!(
        private_folders(&linux, &HashMap::new(), Some(7)),
        [
            root.join("Game-linux"),
            PathBuf::from("/tmp/obelisk-engine-7")
        ]
    );
    assert_eq!(
        private_folders(&linux, &HashMap::new(), None),
        [root.join("Game-linux")]
    );
}

#[test]
fn the_linux_engine_runs_only_x86_64_programs_that_are_not_dotnet_and_rosetta_s_wine_runs_them_all()
{
    use crate::pe::tests::Fixture;
    let (_directory, root) = installation();
    let program = |name: &str, fixture: Fixture<'_>| {
        let path = root.join(name);
        fs::write(&path, fixture.bytes()).unwrap();
        path
    };
    let native = program("native.exe", Fixture::default());
    let dotnet = program(
        "dotnet.exe",
        Fixture {
            dotnet: true,
            ..Fixture::default()
        },
    );
    let x86 = program(
        "x86.exe",
        Fixture {
            machine: crate::pe::tests::I386,
            ..Fixture::default()
        },
    );
    let script = root.join("start.bat");
    fs::write(&script, "@echo off").unwrap();

    assert!(Engine::Linux.runs(&native));
    assert!(!Engine::Linux.runs(&dotnet));
    assert!(!Engine::Linux.runs(&x86));
    // The launch refuses what is no Windows program, on either engine.
    assert!(Engine::Linux.runs(&script));
    for path in [&native, &dotnet, &x86, &script] {
        assert!(Engine::Rosetta.runs(path), "{}", path.display());
    }
}

#[test]
fn a_program_that_imports_direct3d_12_without_11_renders_through_d3dmetal_where_dxmt_is_the_default()
 {
    let (_directory, root) = installation();
    let path = root.join("Game.exe");
    let graphics = |engine, imports: &[&str]| {
        fs::write(&path, crate::pe::tests::Fixture::importing(imports).bytes()).unwrap();
        Graphics::default_for(engine, crate::pe::Program::open(&path).as_ref())
    };
    assert_eq!(
        graphics(Engine::Linux, &["d3d12.dll", "dxgi.dll"]),
        Graphics::D3DMetal
    );
    assert_eq!(
        graphics(Engine::Linux, &["dxgi.dll", "d3d11.dll"]),
        Graphics::Dxmt
    );
    assert_eq!(
        graphics(Engine::Linux, &["d3d12.dll", "d3d11.dll"]),
        Graphics::Dxmt
    );
    assert_eq!(graphics(Engine::Linux, &[]), Graphics::Dxmt);
    assert_eq!(
        graphics(Engine::Rosetta, &["d3d11.dll"]),
        Graphics::D3DMetal
    );
    assert_eq!(Graphics::default_for(Engine::Linux, None), Graphics::Dxmt);
}

/// Launch settings that start `install`'s Steam game through Valve's Windows
/// Steam, on the engine its profile names.
fn choose_windows_steam(
    locations: &crate::Locations,
    game: crate::profile::Game<'_>,
    install: &Path,
) {
    let choices = crate::launch_settings::Choices {
        steam_client: Some(SteamClient::Windows),
        ..Default::default()
    };
    crate::launch_settings::choose(&locations.library, game, install, None, choices).unwrap();
}

#[test]
fn windows_steam_runs_a_game_in_a_prefix_of_its_own_beside_its_rosetta_prefix() {
    let (_directory, root) = installation();
    let library = root.join("library");
    let locations = locations(&root);
    engine_runtime(&locations);
    let install = library.join("Games/Steam/steam-42-windows");
    choose_windows_steam(&locations, OTHER, &install);

    let windows = windows_of(&locations, OTHER, &install);

    assert_eq!(windows.steam_client(), SteamClient::Windows);
    assert_eq!(
        windows.prefix(),
        library.join("Prefixes/steam-42-windows-steam")
    );
    // Only Rosetta's Wine runs Windows Steam: a game on the Linux engine keeps
    // Obelisk's Steam client and the engine's prefix.
    let elden_ring = library.join("Games/Steam/steam-1245620-windows");
    choose_windows_steam(&locations, ELDEN_RING, &elden_ring);
    let linux = windows_of(&locations, ELDEN_RING, &elden_ring);
    assert_eq!(linux.steam_client(), SteamClient::Obelisk);
    assert_eq!(
        linux.prefix(),
        library.join("Prefixes/steam-1245620-windows-linux/prefix")
    );
}

#[test]
fn uninstalling_removes_the_windows_steam_prefix_too() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let install = root.join("library/Games/Steam/steam-42-windows");
    let rosetta = windows_of(&locations, OTHER, &install);
    fs::create_dir_all(rosetta.prefix().join("drive_c")).unwrap();
    choose_windows_steam(&locations, OTHER, &install);
    let steam = windows_of(&locations, OTHER, &install);
    fs::create_dir_all(steam.prefix().join("drive_c/Program Files (x86)/Steam")).unwrap();

    remove(&locations, &install).unwrap().unwrap();

    assert!(!rosetta.prefix().exists());
    assert!(!steam.prefix().exists());
}

#[test]
fn windows_steam_keeps_its_own_steamclient_and_no_overlay_in_every_wine_command() {
    let (_directory, root) = installation();
    let mut config = config(&root);
    let obelisk = environment(&config);
    assert!(!obelisk.contains_key("PROTON_DISABLE_LSTEAMCLIENT"));
    assert!(!obelisk["WINEDLLOVERRIDES"].contains("gameoverlayrenderer"));

    config.settings.steam_client = SteamClient::Windows;
    let windows = environment(&config);

    assert_eq!(windows["PROTON_DISABLE_LSTEAMCLIENT"], "1");
    assert!(
        windows["WINEDLLOVERRIDES"].contains("gameoverlayrenderer,gameoverlayrenderer64="),
        "{}",
        windows["WINEDLLOVERRIDES"]
    );
}

#[test]
fn wineboot_never_starts_the_windows_steam_that_asks_to_start_at_login() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    fs::write(
        config.runtime.join("bin/wine"),
        "#!/bin/sh\nprintf '%s %s\\n' \"$*\" \"$WINEDLLOVERRIDES\" >> \"$WINEPREFIX/../calls\"\n",
    )
    .unwrap();
    let mut windows_steam = config.clone();
    windows_steam.settings.steam_client = SteamClient::Windows;

    prepare_plain(&windows_steam, &root);

    let calls = calls(&root);
    assert!(calls[0].starts_with("wineboot --init "), "{calls:?}");
    assert!(calls[0].ends_with(";steam.exe=d"), "{calls:?}");
    // Its game starts with Steam, which no override disables there.
    assert!(!environment(&windows_steam)["WINEDLLOVERRIDES"].contains("steam.exe"));
}
