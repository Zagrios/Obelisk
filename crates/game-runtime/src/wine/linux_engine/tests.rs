use super::*;
use crate::launch_settings::Settings;
use crate::wine;
use crate::wine::tests::{installation, pe, target as launch_target};

fn linux_config(root: &Path) -> wine::Config {
    wine::Config {
        runtime: root.join("runtime"),
        prefix: root.join("Prefixes/game-linux/prefix"),
        log: root.join("logs/wine-game.log"),
        settings: Settings {
            engine: wine::Engine::Linux,
            graphics: wine::Graphics::Dxmt,
            ..Settings::default()
        },
    }
}

/// A game installed in `install`, without a store title.
fn game(install: &Path) -> wine::Game<'_> {
    wine::Game { install, title: "" }
}

/// A storefront that adds nothing.
struct Plain;
impl wine::Storefront for Plain {
    fn prerequisites(&self) -> Result<Vec<wine::Prerequisite>, Error> {
        Ok(Vec::new())
    }
}

/// A storefront whose games start behind steam.exe and reach Valve's
/// steamclient, as Steam's do.
struct SteamClient;
impl wine::Storefront for SteamClient {
    const HELPER: Option<&'static str> = Some(r"C:\windows\system32\steam.exe");
    const PASSED: &'static [&'static str] = &["SteamAppId", "Steam3Master"];
    const STEAM_CLIENT: bool = true;
    fn prerequisites(&self) -> Result<Vec<wine::Prerequisite>, Error> {
        Ok(Vec::new())
    }
}

/// The values of every `option` pair in the launcher's arguments.
pub(crate) fn options<'a>(target: &'a Target, option: &str) -> Vec<&'a str> {
    let separator = target
        .arguments
        .iter()
        .position(|argument| argument == "--")
        .unwrap();
    target.arguments[7..separator]
        .chunks(2)
        .filter(|pair| pair[0] == option)
        .map(|pair| pair[1].as_str())
        .collect()
}

/// What the launcher starts in the guest: the program and its arguments.
pub(crate) fn guest_command(target: &Target) -> &[String] {
    let separator = target
        .arguments
        .iter()
        .position(|argument| argument == "--")
        .unwrap();
    &target.arguments[separator + 1..]
}

#[test]
fn the_engine_keeps_its_prefix_in_its_own_folder_beside_rosetta_s() {
    let rosetta = Path::new("/library/Prefixes/epic-game-Windows");
    let prefix = prefix(rosetta).unwrap();
    assert_eq!(
        prefix,
        Path::new("/library/Prefixes/epic-game-Windows-linux/prefix")
    );
    assert_eq!(
        home(&prefix).unwrap(),
        Path::new("/library/Prefixes/epic-game-Windows-linux")
    );
}

#[test]
fn a_game_starts_through_the_native_launcher_with_guest_paths() {
    let (_directory, root) = installation();
    let install = root.join("install");
    fs::create_dir_all(install.join("Binaries/Win64")).unwrap();
    let executable = install.join("Binaries/Win64/Game.exe");
    fs::write(&executable, pe()).unwrap();
    let config = linux_config(&root);
    let mut environment = HashMap::new();
    environment.insert("HOME".to_owned(), "/Users/player".to_owned());

    let target = wine::target::<Plain>(
        &config,
        &game(&install),
        &launch_target(&executable, &["-AUTH_TYPE=exchangecode"]),
        environment,
    )
    .unwrap();

    assert_eq!(target.executable, Path::new("/bin/bash"));
    assert_eq!(target.arguments[0], "-c");
    assert_eq!(target.arguments[1], LAUNCHER);
    assert_eq!(
        target.arguments[3..7],
        [
            root.join("runtime").to_string_lossy().into_owned(),
            root.join("Prefixes/game-linux")
                .to_string_lossy()
                .into_owned(),
            install.to_string_lossy().into_owned(),
            "/opt/game/Binaries/Win64".to_owned(),
        ]
    );
    assert_eq!(
        guest_command(&target),
        [
            "/opt/game/Binaries/Win64/Game.exe",
            "-AUTH_TYPE=exchangecode"
        ]
    );
    assert!(options(&target, "--env").contains(&"WINEPREFIX=/home/user/prefix"));
    assert_eq!(options(&target, "--graphics"), ["dxmt"]);
    // Only a storefront's guest sees more of the runtime.
    assert!(options(&target, "--bind").is_empty());
    assert!(options(&target, "--pass").is_empty());
    assert_eq!(target.environment["HOME"], "/Users/player");
    // Without a title, the Dock names the game after its program.
    assert_eq!(target.environment[APP_NAME], "Game");
    assert_eq!(target.log, Some(config.log.clone()));
}

#[test]
fn a_d3dmetal_game_asks_the_launcher_for_the_d3dmetal_render_server() {
    let (_directory, root) = installation();
    let install = root.join("install");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();
    let config = linux_config(&root);
    let config = wine::Config {
        settings: Settings {
            graphics: wine::Graphics::D3DMetal,
            ..config.settings.clone()
        },
        ..config
    };

    let target = wine::target::<Plain>(
        &config,
        &game(&install),
        &launch_target(&install.join("Game.exe"), &[]),
        HashMap::new(),
    )
    .unwrap();

    assert_eq!(options(&target, "--graphics"), ["d3dmetal"]);
}

/// A Steam game whose program is `Game/start_protected_game.exe`, in a
/// runtime that carries steam-hostpid.so when `host_pid`.
fn steam_target(root: &Path, host_pid: bool) -> (wine::Config, Target) {
    let install = root.join("install");
    fs::create_dir_all(install.join("Game")).unwrap();
    let program = install.join("Game/start_protected_game.exe");
    fs::write(&program, pe()).unwrap();
    let config = linux_config(root);
    if host_pid {
        fs::create_dir_all(config.runtime.join("guest")).unwrap();
        fs::write(config.runtime.join("guest").join(HOST_PID), b"elf").unwrap();
    }
    let target = wine::target::<SteamClient>(
        &config,
        &game(&install),
        &launch_target(&program, &["-eac"]),
        HashMap::new(),
    )
    .unwrap();
    (config, target)
}

#[test]
fn a_steam_game_starts_behind_steam_exe_with_valve_s_linux_client_and_its_session_s_values() {
    let (_directory, root) = installation();
    let (config, target) = steam_target(&root, true);

    assert_eq!(
        guest_command(&target),
        [
            r"C:\windows\system32\steam.exe",
            "/opt/game/Game/start_protected_game.exe",
            "-eac"
        ]
    );
    assert_eq!(target.arguments[6], "/opt/game/Game");
    let runtime = config.runtime.display();
    assert_eq!(
        options(&target, "--bind"),
        [
            format!("/opt/steam={runtime}/steam:ro"),
            format!("/opt/obelisk={runtime}/guest:ro"),
        ]
    );
    assert!(options(&target, "--env").contains(&"LD_PRELOAD=/opt/obelisk/steam-hostpid.so"));
    // The host's ports exist only once the session starts the game.
    assert_eq!(options(&target, "--pass"), ["SteamAppId", "Steam3Master"]);
}

#[test]
fn a_runtime_without_the_host_pid_library_preloads_nothing() {
    let (_directory, root) = installation();
    let (config, target) = steam_target(&root, false);

    assert_eq!(
        options(&target, "--bind"),
        [format!("/opt/steam={}/steam:ro", config.runtime.display())]
    );
    assert!(
        !options(&target, "--env")
            .iter()
            .any(|variable| variable.starts_with("LD_PRELOAD="))
    );
}

#[test]
fn the_guest_home_leads_lsteamclient_to_the_runtime_s_linux_steamclient() {
    let (_directory, root) = installation();
    let config = linux_config(&root);
    let link = home(&config.prefix).unwrap().join(".steam/sdk64");

    link_steam_client(&config).unwrap();
    assert_eq!(
        fs::read_link(&link).unwrap(),
        Path::new("/opt/steam/linux64")
    );
    link_steam_client(&config).unwrap();
    assert_eq!(
        fs::read_link(&link).unwrap(),
        Path::new("/opt/steam/linux64")
    );

    // A link elsewhere is replaced; anything else is never removed.
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("/elsewhere", &link).unwrap();
    link_steam_client(&config).unwrap();
    assert_eq!(
        fs::read_link(&link).unwrap(),
        Path::new("/opt/steam/linux64")
    );
    fs::remove_file(&link).unwrap();
    fs::create_dir(&link).unwrap();
    assert!(matches!(link_steam_client(&config), Err(Error::Storage)));
    assert!(link.is_dir());
}

/// A runtime whose window server and render servers only listen, and whose
/// engine records what it was asked to run into `engine.txt`.
fn recording_runtime(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let runtime = root.join("runtime");
    let script = |relative: &str, body: &str| {
        let path = runtime.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    script("engine/wsd", r#"exec /usr/bin/nc -lU "$2/wsd.sock""#);
    let server = r#"printf '%s\n' "$NPT_BACKEND $NPT_D3D12_LIBRARY_PATH" "$RENDER_SERVER_EXEC_PATH" "residency set ${DMN_RESIDENCY_SET-default}" > "$OBELISK_TEST_ROOT/server.txt"
exec /usr/bin/nc -lU "$4""#;
    script("graphics/bin/virgl_test_server", server);
    script("graphics/x86_64/bin/virgl_test_server", server);
    script(
        "graphics/libexec/virgl_render_server",
        r#"echo "arm64 $*" >> "$OBELISK_TEST_ROOT/servers.txt""#,
    );
    script(
        "graphics/x86_64/libexec/virgl_render_server",
        r#"echo "Intel $*" >> "$OBELISK_TEST_ROOT/servers.txt""#,
    );
    script(
        "engine/fexdarwin-run",
        r#"{ printf '%s\n' "$FEXDARWIN_BIND" "files $(ulimit -S -n)"; printf '%s\n' "$@"; } > "$OBELISK_TEST_ROOT/engine.txt""#,
    );
    runtime
}

/// Runs the launcher with `options` and the program `Game.exe`, from an
/// environment with `variables` and the Finder's 256 open files.
fn run_launcher(root: &Path, options: &[&str], variables: &[(&str, &str)]) -> (String, String) {
    let runtime = recording_runtime(root);
    let mut command = Command::new("/bin/bash");
    command
        .arg("-c")
        .arg(r#"ulimit -S -n 256; exec /bin/bash -c "$0" "$@""#)
        .arg(LAUNCHER)
        .arg("obelisk-linux-engine")
        .arg(&runtime)
        .arg(root.join("home"))
        .arg(root.join("install"))
        .arg("/opt/game")
        .args(options)
        .args(["--", "/opt/game/Game.exe", "-flag"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", root)
        .env("OBELISK_TEST_ROOT", root)
        .envs(variables.iter().copied());
    let status = command.stdin(Stdio::null()).status().unwrap();
    assert!(status.success(), "{status:?}");
    (
        fs::read_to_string(root.join("engine.txt")).unwrap(),
        fs::read_to_string(root.join("server.txt")).unwrap(),
    )
}

#[test]
fn the_launcher_hands_the_guest_its_binds_its_variables_and_the_session_s_values() {
    let (_directory, root) = installation();
    let runtime = root.join("runtime");

    let (engine, server) = run_launcher(
        &root,
        &[
            "--graphics",
            "dxmt",
            "--bind",
            "/opt/steam=/runtime/steam:ro",
            "--env",
            "WINEPREFIX=/home/user/prefix",
            "--pass",
            "Steam3Master",
            "--pass",
            "SteamClientService",
        ],
        &[("Steam3Master", "127.0.0.1:5000")],
    );

    let lines: Vec<&str> = engine.lines().collect();
    assert!(
        lines[0].starts_with(&format!(
            "/opt/wine={}/wine:ro;/home/user={};/opt/game={};/run/obelisk=",
            runtime.display(),
            root.join("home").display(),
            root.join("install").display()
        )),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].ends_with(";/opt/steam=/runtime/steam:ro"),
        "{}",
        lines[0]
    );
    // The Finder's 256 files are not enough for Wine.
    let files: u64 = lines[1].strip_prefix("files ").unwrap().parse().unwrap();
    assert!(files >= 10240, "{files}");
    let variables: Vec<&str> = lines
        .windows(2)
        .filter(|pair| pair[0] == "--env" && !pair[1].starts_with("TZ="))
        .map(|pair| pair[1])
        .collect();
    assert_eq!(
        variables,
        [
            "WINEPREFIX=/home/user/prefix",
            "Steam3Master=127.0.0.1:5000"
        ]
    );
    assert!(lines.ends_with(&["sh", "/opt/game/Game.exe", "-flag"]));
    let dxmt = runtime.join("graphics/dxmt-native.framework/Versions/A/dxmt-native");
    assert_eq!(
        server.lines().next().unwrap(),
        format!("dxmt {}", dxmt.display())
    );
    assert_eq!(server.lines().nth(2).unwrap(), "residency set default");
}

#[test]
fn the_guest_gets_the_mac_s_time_zone_unless_the_caller_passes_one() {
    // The guest root has no zone file: without TZ, every program ran in UTC.
    let mac = fs::read("/etc/localtime").ok().and_then(|zone| {
        let text = String::from_utf8_lossy(&zone).into_owned();
        let rule: String = text
            .trim_end()
            .rsplit('\n')
            .next()?
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .collect();
        (!rule.is_empty()).then(|| format!("TZ={rule}"))
    });
    for (options, expected) in [
        (&[][..], mac.clone()),
        (&["--env", "TZ=UTC0"][..], Some("TZ=UTC0".to_owned())),
    ] {
        let (_directory, root) = installation();

        let (engine, _) = run_launcher(&root, options, &[]);

        let lines: Vec<&str> = engine.lines().collect();
        let zones: Vec<&str> = lines
            .windows(2)
            .filter(|pair| pair[0] == "--env" && pair[1].starts_with("TZ="))
            .map(|pair| pair[1])
            .collect();
        assert_eq!(
            zones,
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_launcher_runs_d3dmetal_in_the_intel_render_server() {
    let (_directory, root) = installation();

    let (_, server) = run_launcher(&root, &["--graphics", "d3dmetal"], &[]);

    let library = root.join("runtime/graphics/x86_64/lib/libd3dmetal-native.dylib");
    assert_eq!(
        server.lines().next().unwrap(),
        format!("d3dmetal {}", library.display())
    );
    // Buffers are resident per command buffer, not pinned in a residency set.
    assert_eq!(server.lines().nth(2).unwrap(), "residency set 0");
}

#[test]
fn each_backend_s_render_server_runs_from_its_own_bundle_inside_the_game_s() {
    use std::os::unix::fs::MetadataExt;
    // Metal keeps compiled shaders per bundle identifier: a server outside any
    // bundle shared one cache with every other, which DXMT's emptied.
    for (backend, source, translated) in [
        ("dxmt", "graphics/libexec/virgl_render_server", ""),
        (
            "d3dmetal",
            "graphics/x86_64/libexec/virgl_render_server",
            "Intel --obelisk-translate\nIntel --obelisk-translate\n",
        ),
    ] {
        let (_directory, root) = installation();
        let helper = root.join(format!(
            "obelisk-apps/My-Game/My Game.app/Contents/Helpers/{backend}.app"
        ));
        let executable = helper.join("Contents/MacOS/virgl_render_server");
        let mut copies = Vec::new();

        for _ in 0..2 {
            let (_, server) = run_launcher(
                &root,
                &["--graphics", backend],
                &[("OBELISK_APP_NAME", "My Game")],
            );
            assert_eq!(server.lines().nth(1).unwrap(), executable.to_str().unwrap());
            copies.push(fs::metadata(&executable).unwrap().ino());
        }

        assert_eq!(
            fs::read(&executable).unwrap(),
            fs::read(root.join("runtime").join(source)).unwrap()
        );
        // The copy stays from one launch to the next, with its Rosetta
        // translation, and the Intel one is translated before the game.
        assert_eq!(copies[0], copies[1]);
        let runs = fs::read_to_string(root.join("servers.txt")).unwrap_or_default();
        assert_eq!(runs, translated);
        let plist = fs::read_to_string(helper.join("Contents/Info.plist")).unwrap();
        assert!(
            plist.contains(&format!(
                "<string>com.zagrios.obelisk.game.My-Game.{backend}</string>"
            )),
            "{plist}"
        );
    }
}

#[test]
fn the_launcher_refuses_an_option_it_does_not_know_or_a_variable_name_that_is_not_one() {
    let (_directory, root) = installation();
    let runtime = recording_runtime(&root);
    for options in [&["--other", "x"][..], &["--pass", "A;B"], &["--env"]] {
        let status = Command::new("/bin/bash")
            .arg("-c")
            .arg(LAUNCHER)
            .arg("obelisk-linux-engine")
            .arg(&runtime)
            .arg(root.join("home"))
            .arg(root.join("install"))
            .arg("/opt/game")
            .args(options)
            .args(["--", "/opt/game/Game.exe"])
            .env_clear()
            .env("TMPDIR", &root)
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(2), "{options:?}");
    }
    assert!(!root.join("engine.txt").exists());
}

/// A release whose Intel render servers and Obelisk's Intel helper only
/// record that they ran, into `calls`; the helper succeeds unless `failing`.
fn translating_release(root: &Path, failing: bool) -> (wine::Config, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let config = linux_config(root);
    let config = wine::Config {
        runtime: root.join("release/linux"),
        settings: Settings {
            graphics: wine::Graphics::D3DMetal,
            ..config.settings.clone()
        },
        ..config
    };
    let helpers = root.join("Helpers");
    let calls = root.join("calls");
    let script = |path: PathBuf, body: String| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    let record = format!(r#"echo "$(basename "$0") $*" >> "{}""#, calls.display());
    let server = config.runtime.join("graphics/x86_64");
    script(server.join("bin/virgl_test_server"), record.clone());
    script(server.join("libexec/virgl_render_server"), record.clone());
    let exit = if failing { "exit 1" } else { "exit 0" };
    script(
        helpers.join(crate::package::SDK_INTEL),
        format!("{record}\n{exit}"),
    );
    (config, calls)
}

#[test]
fn d3dmetal_is_translated_once_per_runtime_and_macos_build_before_its_first_game() {
    let (_directory, root) = installation();
    let (config, calls) = translating_release(&root, false);
    let helpers = root.join("Helpers");

    translate_d3dmetal(&config, &helpers);

    let server = config.runtime.join("graphics/x86_64");
    assert_eq!(
        fs::read_to_string(&calls)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "virgl_test_server --obelisk-translate".to_owned(),
            "virgl_render_server --obelisk-translate".to_owned(),
            format!(
                "{} warm-d3dmetal {} {}",
                crate::package::SDK_INTEL,
                server.join("lib/libd3dmetal-native.dylib").display(),
                root.join("release/lib/external/D3DMetal.framework")
                    .display()
            ),
        ]
    );
    let record = config.runtime.join(TRANSLATED);
    assert_eq!(fs::read_to_string(&record).ok(), macos_build());

    fs::remove_file(&calls).unwrap();
    translate_d3dmetal(&config, &helpers);
    assert!(!calls.exists(), "already translated");

    // A macOS update drops Rosetta's translations.
    fs::write(&record, "an older build").unwrap();
    translate_d3dmetal(&config, &helpers);
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 3);
}

#[test]
fn a_failed_translation_is_tried_again_and_a_dxmt_game_needs_none() {
    let (_directory, root) = installation();
    let (config, calls) = translating_release(&root, true);
    let helpers = root.join("Helpers");

    translate_d3dmetal(&config, &helpers);
    assert!(!config.runtime.join(TRANSLATED).exists());
    translate_d3dmetal(&config, &helpers);
    assert_eq!(fs::read_to_string(&calls).unwrap().lines().count(), 6);

    fs::remove_file(&calls).unwrap();
    let dxmt = wine::Config {
        settings: Settings {
            graphics: wine::Graphics::Dxmt,
            ..config.settings.clone()
        },
        ..config
    };
    translate_d3dmetal(&dxmt, &helpers);
    assert!(!calls.exists());
}

/// End to end on an installed engine runtime, through `prepare` and the
/// launcher as a launch runs them: `OBELISK_LINUX_ENGINE_RUNTIME` names the
/// runtime, `OBELISK_LINUX_ENGINE_PROGRAM` a Windows program that exits by
/// itself, `OBELISK_LINUX_ENGINE_ARGUMENTS` its arguments, and
/// `OBELISK_LINUX_ENGINE_GRAPHICS=d3dmetal` its renderer. It opens a window.
#[test]
#[ignore]
fn smoke() {
    let runtime = PathBuf::from(std::env::var("OBELISK_LINUX_ENGINE_RUNTIME").unwrap());
    let program = PathBuf::from(std::env::var("OBELISK_LINUX_ENGINE_PROGRAM").unwrap());
    let arguments = std::env::var("OBELISK_LINUX_ENGINE_ARGUMENTS").unwrap_or_default();
    let (_directory, root) = installation();
    let install = root.join("install");
    fs::create_dir_all(&install).unwrap();
    let executable = install.join(program.file_name().unwrap());
    fs::copy(&program, &executable).unwrap();
    let config = wine::Config {
        runtime,
        prefix: root.join("Prefixes/game-linux/prefix"),
        log: root.join("logs/game.log"),
        settings: Settings {
            engine: wine::Engine::Linux,
            graphics: match std::env::var("OBELISK_LINUX_ENGINE_GRAPHICS").as_deref() {
                Ok("d3dmetal") => wine::Graphics::D3DMetal,
                _ => wine::Graphics::Dxmt,
            },
            ..Settings::default()
        },
    };
    let arguments: Vec<&str> = arguments.split_whitespace().collect();
    let launch = launch_target(&executable, &arguments);

    let locations = crate::wine::tests::locations(&root);
    wine::prepare(&config, &locations, &install, &launch, &Plain).unwrap();
    let target = wine::target::<Plain>(&config, &game(&install), &launch, HashMap::new()).unwrap();
    let output = Command::new(&target.executable)
        .args(&target.arguments)
        .current_dir(&target.working_directory)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    let errors = String::from_utf8_lossy(&output.stderr);
    println!(
        "{}",
        errors
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(output.status.success(), "{:?}", output.status);
}
