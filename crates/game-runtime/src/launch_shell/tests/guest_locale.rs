//! The locale a Windows game gets, by storefront and engine, as each really
//! builds its target. Steam builds it in Obelisk's own process, where
//! Obelisk's `LANG` is the player's language. Epic and GOG build it in their
//! supervisor, whose process starts with `PATH` alone (see
//! `a_mac_build_hands_its_supervisor_this_request_and_nothing_more`), so the
//! player's locale reaches it in the request, with the launch settings.
//! `native::player_locale` reads the process environment, which a test cannot
//! set, so each scenario runs in a copy of this test binary whose environment
//! is exactly the one it stands for. For Epic and GOG, the copy standing for
//! Obelisk starts the one standing for the supervisor while its library is
//! still there.
use super::{
    super::{Launchable, Prerequisites, Request},
    Library, Setup,
};
use crate::{
    Locations,
    epic::{self, Epic, content::tests::Fixture as EpicLibrary},
    gog::content::tests::Fixture as GogLibrary,
    installations::{Receipt, Storefront},
    launch_settings::Choices,
    steam::{self, Steam},
    supervisor::{Target, storage},
    wine::{
        Engine,
        tests::{engine_runtime, installation, locations, pe, record_calls, target as plan},
    },
};
use obelisk_core::{Platform, launch::LaunchTarget};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Names the scenario a copy of this binary runs, and the folder it leaves
/// what it builds in.
const SCENARIO: &str = "OBELISK_GUEST_LOCALE_SCENARIO";
const FOLDER: &str = "OBELISK_GUEST_LOCALE_FOLDER";
/// The environment a copy standing for Obelisk gives the one it starts for
/// its supervisor, as JSON.
const SUPERVISOR_ENVIRONMENT: &str = "OBELISK_GUEST_LOCALE_SUPERVISOR";

type Environment<'a> = &'a [(&'a str, &'a str)];

/// The environment of Obelisk for a player whose language is Italian: Obelisk
/// sets `LANG` as it starts (`GameLocale.adopt`) and nothing else of a locale.
const ITALIAN: Environment = &[("PATH", "/usr/bin:/bin"), ("LANG", "it_IT.UTF-8")];
/// The environment of a supervisor: `supervisor_command` clears it and sets
/// `PATH`.
const SUPERVISOR: Environment = &[("PATH", "/usr/bin:/bin")];
/// The environment of an Obelisk that has no language.
const NO_LANGUAGE: Environment = SUPERVISOR;
/// A supervisor that happens to hold a locale of its own, as no real one does.
const OTHER_SUPERVISOR: Environment = &[
    ("PATH", "/usr/bin:/bin"),
    ("LANG", "fr_FR.UTF-8"),
    ("LC_ALL", "fr_FR.UTF-8"),
];

const ENGINES: [(&str, Engine); 2] = [("rosetta", Engine::Rosetta), ("linux", Engine::Linux)];

/// Runs `scenario` in a copy of this binary whose whole environment is
/// `environment` and the names above, working in `folder`. `supervisor` is
/// the environment its supervisor will start with, if it starts one.
fn in_process(scenario: &str, folder: &Path, environment: Environment, supervisor: Environment) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "launch_shell::tests::guest_locale::process_fixture",
            "--nocapture",
        ])
        .env_clear()
        .envs(environment.iter().copied())
        .env(SCENARIO, scenario)
        .env(FOLDER, folder)
        .env(
            SUPERVISOR_ENVIRONMENT,
            serde_json::to_string(supervisor).unwrap(),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{scenario}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The target a scenario left in `folder` for `engine`.
fn built(folder: &Path, engine: &str, file: &str) -> Target {
    let bytes = fs::read(folder.join(engine).join(file))
        .unwrap_or_else(|error| panic!("the scenario left no {engine}/{file}: {error}"));
    serde_json::from_slice(&bytes).unwrap()
}

/// Voile's runtime for `engine` in place for the installation `install` and,
/// for the Linux engine, the prefix it already created. Returns the runtime.
fn runtime_for(locations: &Locations, install: &Path, engine: Engine) -> PathBuf {
    match engine {
        Engine::Rosetta => {
            let runtime = locations.voile.join("current");
            record_calls(&runtime);
            runtime
        }
        Engine::Linux => {
            let runtime = engine_runtime(locations);
            let prefix = Engine::Linux
                .prefix(&locations.wine_prefixes, install)
                .unwrap();
            fs::create_dir_all(prefix.join("drive_c/windows/system32")).unwrap();
            fs::write(prefix.join("system.reg"), "registry").unwrap();
            runtime
        }
    }
}

/// The Windows installation of `receipt`, whose program is Game.exe, with
/// Voile's runtime for `engine` in place and the player's choice of it, which
/// only the Linux engine needs: no profile names these games. Returns the
/// runtime.
fn installed<S: Storefront>(
    locations: &Locations,
    receipt: &Receipt<S>,
    engine: Engine,
) -> PathBuf {
    let install = Path::new(&receipt.directory);
    fs::create_dir_all(install).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();
    let runtime = runtime_for(locations, install, engine);
    if engine == Engine::Linux {
        let choices = Choices {
            engine: Some(Engine::Linux),
            ..Choices::default()
        };
        receipt
            .choose_launch_settings(&locations.library, choices)
            .unwrap();
    }
    runtime
}

/// Obelisk's side of a Steam launch, in this process: the Steam game's
/// target for each engine, as `steam::prepare` builds it into the request.
fn steam_in_obelisk(folder: &Path) {
    for (name, engine) in ENGINES {
        let root = folder.join(name);
        fs::create_dir_all(&root).unwrap();
        let locations = locations(&root);
        let install = locations.library.join("Games/steam-42-windows");
        let receipt = Receipt::<Steam> {
            product: steam::Product {
                app_id: 42,
                language: "english".into(),
            },
            name: "Fixture".into(),
            directory: install.to_string_lossy().into_owned(),
            platform: Platform::Windows,
            total_bytes: 7,
            needs_verification: false,
        };
        let runtime = installed(&locations, &receipt, engine);
        // Steam places Valve's steamclient libraries in the prefix.
        fs::create_dir_all(runtime.join("steam")).unwrap();
        for client in ["steamclient.dll", "steamclient64.dll"] {
            fs::write(runtime.join("steam").join(client), client).unwrap();
        }
        let windows = receipt.windows(&locations).unwrap();
        let request = steam::prepare(
            &locations,
            &receipt,
            windows.as_ref(),
            &plan(&install.join("Game.exe"), &[]),
        )
        .unwrap();
        fs::write(
            root.join("target.json"),
            serde_json::to_vec(&request.target).unwrap(),
        )
        .unwrap();
    }
}

/// What a storefront says its game starts in its installation: a program and
/// its arguments.
#[derive(Serialize, Deserialize)]
struct Launched {
    program: String,
    arguments: Vec<String>,
}

impl Launched {
    fn of(program: &str, arguments: &[&str]) -> Self {
        Self {
            program: program.to_owned(),
            arguments: arguments.iter().map(|each| (*each).to_owned()).collect(),
        }
    }

    fn target(&self, install: &Path) -> LaunchTarget {
        let arguments: Vec<&str> = self.arguments.iter().map(String::as_str).collect();
        plan(&install.join(&self.program), &arguments)
    }
}

/// Obelisk's side of a launch of the installed `receipt`, in this process:
/// the receipt's Wine as `Receipt::windows` and `ready` give it, and the
/// request that `Request::new` builds from it, which goes to the supervisor.
/// As a control, the target that very request gives when built here.
fn hand_over<L: Library>(library: &L, receipt: &Receipt<L::G>, launched: &Launched, root: &Path) {
    fs::create_dir_all(root).unwrap();
    let locations = library.locations();
    let target = launched.target(Path::new(&receipt.directory));
    let wine = receipt
        .windows(locations)
        .unwrap()
        .unwrap()
        .ready(locations, &target, &Prerequisites(Vec::new()))
        .unwrap();
    let request = Request::<L::G>::new(
        library.configuration(receipt, Some(wine.prefix.clone())),
        L::account(),
        L::credential(),
        Some(wine),
    );
    let mut packet = Vec::new();
    storage::send(&mut packet, &request).unwrap();
    fs::write(root.join("request.json"), packet).unwrap();
    fs::write(
        root.join("launched.json"),
        serde_json::to_vec(launched).unwrap(),
    )
    .unwrap();
    let control = request.target(&target, None).unwrap().target;
    fs::write(
        root.join("obelisk-target.json"),
        serde_json::to_vec(&control).unwrap(),
    )
    .unwrap();
}

/// Starts the copy standing for the supervisor of the requests in `folder`,
/// in the environment the test gave this one for it.
fn start_supervisor<L: Library>(folder: &Path) {
    let environment: Vec<(String, String)> =
        serde_json::from_str(&std::env::var(SUPERVISOR_ENVIRONMENT).unwrap()).unwrap();
    let environment: Vec<(&str, &str)> = environment
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    in_process(
        &format!("{}:supervisor", L::SUPERVISE),
        folder,
        &environment,
        &[],
    );
}

/// Obelisk's side of an Epic or GOG launch for each engine, then its
/// supervisor's.
fn in_obelisk<L: Library>(folder: &Path) {
    let library = L::new();
    for (name, engine) in ENGINES {
        let receipt = library.receipt(Platform::Windows);
        installed(library.locations(), &receipt, engine);
        hand_over(
            &library,
            &receipt,
            &Launched::of("Game.exe", &[]),
            &folder.join(name),
        );
    }
    start_supervisor::<L>(folder);
}

/// The supervisor's side: opens the request Obelisk handed over for each
/// engine it holds one for, and builds the target of the program its
/// storefront says to start, as `Launch::target` does.
fn in_supervisor<L: Library>(folder: &Path) {
    for (name, _) in ENGINES {
        let root = folder.join(name);
        let Ok(packet) = fs::read(root.join("request.json")) else {
            continue;
        };
        let (_, request) = Request::<L::G>::open(&mut packet.as_slice()).unwrap();
        let launched: Launched =
            serde_json::from_slice(&fs::read(root.join("launched.json")).unwrap()).unwrap();
        let install = L::G::build(&request.config).install.to_owned();
        let target = request
            .target(&launched.target(&install), None)
            .unwrap()
            .target;
        fs::write(
            root.join("supervisor-target.json"),
            serde_json::to_vec(&target).unwrap(),
        )
        .unwrap();
    }
}

/// The program Epic starts for Rocket League, whose profile names the Linux
/// engine and reads its language from its command line.
const ROCKET_LEAGUE: &str = "Binaries/Win64/Launcher.exe";

/// Obelisk's side of Rocket League's launch on Epic, then its supervisor's.
fn rocket_league_in_obelisk(folder: &Path) {
    let library = EpicLibrary::new();
    let receipt = Receipt::<Epic> {
        product: epic::Product {
            namespace: "9773aa1aa54f4f7b80e44bef04986cea".into(),
            catalog_item_id: "catalog".into(),
            app_name: "Sugar".into(),
        },
        ..library.receipt(Platform::Windows)
    };
    let install = Path::new(&receipt.directory);
    installed(library.locations(), &receipt, Engine::Linux);
    fs::create_dir_all(install.join("Binaries/Win64")).unwrap();
    for program in [ROCKET_LEAGUE, "Binaries/Win64/RocketLeague_EAC.exe"] {
        fs::write(install.join(program), pe()).unwrap();
    }
    let launched = Launched::of(ROCKET_LEAGUE, &["-AUTH_TYPE=exchangecode"]);
    hand_over(&library, &receipt, &launched, &folder.join("linux"));
    start_supervisor::<EpicLibrary>(folder);
}

/// Obelisk's side of the first launch of a Linux-engine game, in this
/// process: `ready` creates the prefix through the engine, whose command a
/// stand-in for the timeout wrapper keeps in boot.txt.
fn boot_in_obelisk<L: Library>(folder: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let root = folder.join("linux");
    fs::create_dir_all(&root).unwrap();
    let library = L::new();
    let locations = library.locations();
    let receipt = library.receipt(Platform::Windows);
    installed(locations, &receipt, Engine::Linux);
    let install = Path::new(&receipt.directory);
    let prefix = Engine::Linux
        .prefix(&locations.wine_prefixes, install)
        .unwrap();
    fs::remove_file(prefix.join("system.reg")).unwrap();
    let wrapper = locations
        .voile
        .join("current/linux/engine/run-with-timeout.sh");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf registry > '{}'\n",
            root.join("boot.txt").display(),
            prefix.join("system.reg").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    receipt
        .windows(locations)
        .unwrap()
        .unwrap()
        .ready(
            locations,
            &Launched::of("Game.exe", &[]).target(install),
            &Prerequisites(Vec::new()),
        )
        .unwrap();
}

/// What `side` of `L`'s launch does.
fn on_side<L: Library>(side: &str, folder: &Path) {
    match side {
        "obelisk" => in_obelisk::<L>(folder),
        "supervisor" => in_supervisor::<L>(folder),
        other => panic!("unknown side {other}"),
    }
}

/// Re-entered as a disposable Obelisk or supervisor process.
#[test]
fn process_fixture() {
    let (Some(scenario), Some(folder)) = (
        std::env::var_os(SCENARIO),
        std::env::var_os(FOLDER).map(PathBuf::from),
    ) else {
        return;
    };
    match scenario.to_str().unwrap() {
        "steam" => steam_in_obelisk(&folder),
        "boot" => boot_in_obelisk::<EpicLibrary>(&folder),
        "rocket-league" => rocket_league_in_obelisk(&folder),
        other => match other.split_once(':') {
            Some((storefront, side)) if storefront == EpicLibrary::SUPERVISE => {
                on_side::<EpicLibrary>(side, &folder)
            }
            Some((storefront, side)) if storefront == GogLibrary::SUPERVISE => {
                on_side::<GogLibrary>(side, &folder)
            }
            _ => panic!("unknown scenario {other}"),
        },
    }
}

/// The folder a scenario left its targets in.
struct Flow {
    _directory: tempfile::TempDir,
    folder: PathBuf,
}

impl Flow {
    /// `scenario`, whose Obelisk runs with `obelisk` and its supervisor, if
    /// it starts one, with `supervisor`.
    fn run(scenario: &str, obelisk: Environment, supervisor: Environment) -> Self {
        let (directory, folder) = installation();
        in_process(scenario, &folder, obelisk, supervisor);
        Self {
            _directory: directory,
            folder,
        }
    }

    /// Steam's launches for a player whose Obelisk runs with `obelisk`.
    fn of_steam(obelisk: Environment) -> Self {
        Self::run("steam", obelisk, &[])
    }

    /// `L`'s launches: Obelisk runs with `obelisk`, then its supervisor with
    /// `supervisor`.
    fn of<L: Library>(obelisk: Environment, supervisor: Environment) -> Self {
        Self::run(&format!("{}:obelisk", L::SUPERVISE), obelisk, supervisor)
    }

    /// The target Steam's request carries.
    fn steam(&self, engine: &str) -> Target {
        built(&self.folder, engine, "target.json")
    }

    /// The target the supervisor builds from the request.
    fn in_supervisor(&self, engine: &str) -> Target {
        built(&self.folder, engine, "supervisor-target.json")
    }

    /// The target the same request gives when built in Obelisk's process.
    fn in_obelisk(&self, engine: &str) -> Target {
        built(&self.folder, engine, "obelisk-target.json")
    }

    /// The request as the supervisor reads it.
    fn request(&self, engine: &str) -> serde_json::Value {
        let packet = fs::read(self.folder.join(engine).join("request.json")).unwrap();
        serde_json::from_slice(&packet).unwrap()
    }
}

/// The launcher's options before the guest's command, after its seven
/// fixed arguments, as pairs.
fn launcher_options(target: &Target) -> impl Iterator<Item = (&str, &str)> {
    let separator = target
        .arguments
        .iter()
        .position(|argument| argument == "--")
        .unwrap();
    target.arguments[7..separator]
        .chunks(2)
        .map(|pair| (pair[0].as_str(), pair[1].as_str()))
}

/// What the launcher starts in the guest: the program and its arguments.
fn guest_command(target: &Target) -> &[String] {
    let separator = target
        .arguments
        .iter()
        .position(|argument| argument == "--")
        .unwrap();
    &target.arguments[separator + 1..]
}

/// The variables the Linux engine's launcher gives the guest, in order.
fn guest(target: &Target) -> Vec<(String, String)> {
    launcher_options(target)
        .filter(|(option, _)| *option == "--env")
        .map(|(_, variable)| {
            let (name, value) = variable.split_once('=').unwrap();
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

fn value<'a>(variables: &'a [(String, String)], name: &str) -> Option<&'a str> {
    variables
        .iter()
        .find(|(variable, _)| variable == name)
        .map(|(_, value)| value.as_str())
}

/// The locale variables of the process a target starts: Rosetta's Wine, or
/// the Linux engine's launcher.
fn process_locale(target: &Target) -> BTreeMap<&str, &str> {
    target
        .environment
        .iter()
        .filter(|(name, _)| name.as_str() == "LANG" || name.starts_with("LC_"))
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// A target's command line and process environment.
fn invocation(target: &Target) -> (Vec<String>, Vec<(String, String)>) {
    let mut environment: Vec<_> = target.environment.clone().into_iter().collect();
    environment.sort();
    (target.arguments.clone(), environment)
}

#[test]
fn a_steam_game_gets_the_player_s_locale_in_its_guest_on_the_linux_engine_and_in_its_process_on_rosetta()
 {
    // Steam builds the target in Obelisk's process, where `LANG` holds the
    // player's language.
    let flow = Flow::of_steam(ITALIAN);

    let linux = flow.steam("linux");
    let rosetta = flow.steam("rosetta");

    let guest = guest(&linux);
    assert_eq!(value(&guest, "LC_ALL"), Some("it_IT.UTF-8"));
    assert_eq!(value(&guest, "LANG"), Some("en_US.UTF-8"));
    // The launcher's own process holds the player's `LANG` too; it hands the
    // guest none of its environment.
    let player = BTreeMap::from([("LANG", "it_IT.UTF-8")]);
    assert_eq!(process_locale(&linux), player);
    assert_eq!(process_locale(&rosetta), player);
    assert!(
        !rosetta.arguments.iter().any(|argument| argument == "--env"),
        "Rosetta's Wine has no guest of its own"
    );
}

#[test]
fn a_player_whose_obelisk_has_no_language_gets_no_guest_lc_all_and_english_on_rosetta() {
    let flow = Flow::of_steam(NO_LANGUAGE);

    assert_eq!(value(&guest(&flow.steam("linux")), "LC_ALL"), None);
    assert_eq!(
        process_locale(&flow.steam("rosetta")),
        BTreeMap::from([("LANG", "en_US.UTF-8")])
    );
}

/// An Epic or a GOG game whose Obelisk runs in the player's language gets it
/// as a Steam game does, whichever process builds its target: the request
/// carries it, with the launch settings, to a supervisor that holds none.
fn the_player_s_locale_reaches_the_target_its_supervisor_builds<L: Library>() {
    let flow = Flow::of::<L>(ITALIAN, SUPERVISOR);

    for (engine, _) in ENGINES {
        let request = flow.request(engine);
        assert_eq!(request["environment"]["LANG"], "it_IT.UTF-8", "{engine}");
        assert_eq!(request["wine"]["settings"]["locale"], "it_IT", "{engine}");
        // Nothing differs from what Obelisk's process builds: not the command
        // line, nor the game's process environment.
        assert_eq!(
            invocation(&flow.in_supervisor(engine)),
            invocation(&flow.in_obelisk(engine)),
            "{engine}"
        );
        assert_eq!(
            process_locale(&flow.in_supervisor(engine)),
            BTreeMap::from([("LANG", "it_IT.UTF-8")]),
            "{engine}"
        );
    }
    // The guest's `LC_ALL` is the one Steam's gets, though the supervisor's
    // own environment holds none.
    let guest = guest(&flow.in_supervisor("linux"));
    assert_eq!(value(&guest, "LC_ALL"), Some("it_IT.UTF-8"));
    assert_eq!(value(&guest, "LANG"), Some("en_US.UTF-8"));
}

#[test]
fn an_epic_game_gets_the_player_s_locale_whichever_process_builds_its_target() {
    the_player_s_locale_reaches_the_target_its_supervisor_builds::<EpicLibrary>();
}

#[test]
fn a_gog_game_gets_the_player_s_locale_whichever_process_builds_its_target() {
    the_player_s_locale_reaches_the_target_its_supervisor_builds::<GogLibrary>();
}

/// A supervisor with a locale of its own gives the guest none of it: the
/// player's comes in the request, or the guest has none.
fn a_supervisor_s_own_environment_never_reaches_the_guest<L: Library>() {
    let italian = Flow::of::<L>(ITALIAN, OTHER_SUPERVISOR);
    assert_eq!(
        value(&guest(&italian.in_supervisor("linux")), "LC_ALL"),
        Some("it_IT.UTF-8")
    );

    let without = Flow::of::<L>(NO_LANGUAGE, OTHER_SUPERVISOR);
    assert_eq!(
        value(&guest(&without.in_supervisor("linux")), "LC_ALL"),
        None
    );
}

#[test]
fn an_epic_supervisor_s_own_locale_never_reaches_the_guest() {
    a_supervisor_s_own_environment_never_reaches_the_guest::<EpicLibrary>();
}

#[test]
fn a_gog_supervisor_s_own_locale_never_reaches_the_guest() {
    a_supervisor_s_own_environment_never_reaches_the_guest::<GogLibrary>();
}

#[test]
fn the_engine_creates_a_new_prefix_with_the_same_guest_variables_as_a_launch_in_obelisk_s_process()
{
    let flow = Flow::run("boot", ITALIAN, &[]);

    let recorded = fs::read_to_string(flow.folder.join("linux/boot.txt")).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    let booted: Vec<&str> = lines
        .windows(2)
        .filter(|pair| pair[0] == "--env")
        .map(|pair| pair[1])
        .collect();

    // `boot` runs in Obelisk's process, so wineboot gets the player's locale,
    // as the game does when its target is built there.
    assert!(booted.contains(&"LC_ALL=it_IT.UTF-8"), "{booted:?}");
    let launched = Flow::of::<EpicLibrary>(ITALIAN, SUPERVISOR).in_obelisk("linux");
    let launched: Vec<String> = guest(&launched)
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    assert_eq!(booted, launched);
}

#[test]
fn an_epic_game_that_reads_its_language_from_its_command_line_gets_it_from_its_launch_settings_in_the_supervisor()
 {
    // The player's language is resolved once, in Obelisk's process, with the
    // game's launch settings, and travels with them in the request.
    let flow = Flow::run("rocket-league", ITALIAN, SUPERVISOR);

    let target = flow.in_supervisor("linux");

    assert_eq!(
        guest_command(&target),
        [
            "/opt/game/Binaries/Win64/RocketLeague_EAC.exe",
            "-AUTH_TYPE=exchangecode",
            "-language=ITA"
        ]
    );
}
