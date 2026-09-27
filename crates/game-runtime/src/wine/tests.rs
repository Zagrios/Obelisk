use super::*;

/// The smallest file `portable_executable` accepts: a DOS header whose last
/// field points just past it, at a PE signature.
pub(crate) fn pe() -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
    bytes.extend_from_slice(b"PE\0\0");
    bytes
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

pub(crate) fn config(root: &Path) -> Config {
    Config {
        runtime: root.join("runtime"),
        prefix: root.join("prefix"),
        log: root.join("logs/wine-game.log"),
    }
}

/// A storefront that adds nothing, or the prerequisites it is given.
#[derive(Default)]
struct Plain {
    prerequisites: Vec<Prerequisite>,
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

/// A Windows game inside `root/game`, and its target.
fn game(root: &Path) -> (PathBuf, LaunchTarget) {
    let install = root.join("game");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();
    let target = target(&install.join("Game.exe"), &[]);
    (install, target)
}

/// The environment Wine commands of `config` run with.
fn environment(config: &Config) -> HashMap<String, String> {
    let (install, target) = game(config.prefix.parent().unwrap());
    super::target(&install, &target, HashMap::new(), config)
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

    let target = super::target(
        &install,
        &target(&install.join("Binaries/Game.exe"), &["-EpicPortal"]),
        HashMap::from([("HOME".to_owned(), "/home".to_owned())]),
        &config,
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
fn only_a_windows_program_inside_the_installation_starts_and_readies_a_prefix() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    let install = root.join("game");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("script.exe"), b"#!/bin/sh\n").unwrap();
    fs::write(root.join("outside.exe"), pe()).unwrap();

    for executable in [install.join("script.exe"), root.join("outside.exe")] {
        let target = target(&executable, &[]);
        assert!(matches!(
            super::target(&install, &target, HashMap::new(), &config),
            Err(Error::NotExecutable)
        ));
        assert!(matches!(
            prepare(&config, &root, &install, &target, &Plain::default()),
            Err(Error::NotExecutable)
        ));
    }
    assert!(calls(&root).is_empty());
    assert!(!config.prefix.join("furnished").exists());
}

#[test]
fn a_prefix_is_booted_then_furnished_before_its_prerequisites_run_and_its_log_folder_exists() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    let (install, target) = game(&root);
    fs::write(install.join("setup.exe"), "").unwrap();
    let storefront = Plain {
        prerequisites: vec![Prerequisite {
            id: "setup".into(),
            programs: vec![(install.join("setup.exe"), "/quiet".into())],
        }],
    };

    prepare(&config, &root, &install, &target, &storefront).unwrap();

    assert_eq!(
        calls(&root),
        [
            "wineboot --init".to_owned(),
            format!("{} /quiet", install.join("setup.exe").display()),
        ]
    );
    assert!(config.prefix.join("furnished").is_file());
    assert!(root.join("logs").is_dir());
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

#[test]
fn a_missing_runtime_is_reported_before_any_prefix_is_created() {
    let (_directory, root) = installation();
    let config = config(&root);

    let (install, target) = game(&root);
    assert!(matches!(
        prepare(&config, &root, &install, &target, &Plain::default()),
        Err(Error::WineUnavailable)
    ));
    assert!(!config.prefix.exists());
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

    prepare(&config, &home, &install, &target, &Plain::default()).unwrap();

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
    assert!(!config.prefix.join(MONO).exists());

    fs::write(config.prefix.join("system.reg"), "").unwrap();
    fs::create_dir_all(config.runtime.join("share/wine/mono/wine-mono-10.4.1")).unwrap();
    prepare_plain(&config, &root);
    prepare_plain(&config, &root);

    assert_eq!(calls(&root), ["wineboot --init", "wineboot --update"]);
    assert_eq!(
        fs::read_to_string(config.prefix.join(MONO)).unwrap(),
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
        fs::read_to_string(config.prefix.join(MONO)).unwrap(),
        "wine-mono-10.4.1"
    );
}

fn prepare_plain(config: &Config, root: &Path) {
    let (install, target) = game(root);
    prepare(config, root, &install, &target, &Plain::default()).unwrap();
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
fn each_prerequisite_runs_once_and_a_failed_one_again_on_the_next_launch() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    let redist = root.join("game/redist");
    fs::create_dir_all(&redist).unwrap();
    for name in ["vc_redist.x64.exe", "Visual C++ x64.cmd", "fail.exe"] {
        fs::write(redist.join(name), "").unwrap();
    }
    let prerequisites = [
        Prerequisite {
            id: "vcredist".into(),
            programs: vec![
                (redist.join("Visual C++ x64.cmd"), String::new()),
                (
                    redist.join("vc_redist.x64.exe"),
                    r#"/q "/log=C:\a b.txt""#.into(),
                ),
            ],
        },
        Prerequisite {
            id: "broken".into(),
            programs: vec![(redist.join("fail.exe"), String::new())],
        },
    ];

    install_prerequisites(&config, &root, &prerequisites).unwrap();
    install_prerequisites(&config, &root, &prerequisites).unwrap();

    let script = windows_path(&redist.join("Visual C++ x64.cmd"));
    let program = redist.join("vc_redist.x64.exe");
    let failing = redist.join("fail.exe");
    assert_eq!(
        calls(&root),
        [
            format!("cmd /c {script}"),
            format!("{} /q /log=C:\\a b.txt", program.display()),
            failing.display().to_string(),
            failing.display().to_string(),
        ]
    );
    assert_eq!(
        fs::read_to_string(config.prefix.join(PREREQUISITES)).unwrap(),
        "vcredist"
    );
}

#[test]
fn a_missing_program_leaves_its_prerequisite_pending() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    let prerequisite = Prerequisite {
        id: "missing".into(),
        programs: vec![(root.join("game/absent.exe"), String::new())],
    };
    install_prerequisites(&config, &root, &[prerequisite]).unwrap();
    assert!(calls(&root).is_empty());
    assert!(!config.prefix.join(PREREQUISITES).exists());
}

#[test]
fn a_windows_installation_runs_in_the_prefix_named_after_its_folder_whatever_the_storefront() {
    let locations = crate::Locations::new(
        Path::new("/library"),
        Path::new("/SailOver.app"),
        Path::new("/home"),
    );
    for (platform, install) in [
        ("windows", "/library/Games/Steam/steam-42-windows"),
        ("Windows", "/library/Games/Epic/epic-hash-Windows"),
    ] {
        let windows = Installation::of(&locations, platform, Path::new(install))
            .unwrap()
            .expect("a Windows build");
        let folder = Path::new(install).file_name().unwrap();
        assert_eq!(
            windows.prefix(),
            Path::new("/library/Prefixes").join(folder)
        );
    }
    for platform in ["macos", "Mac"] {
        let native = Installation::of(&locations, platform, Path::new("/library/Games/native"));
        assert_eq!(native.unwrap(), None, "a Mac build runs outside Wine");
    }
    assert!(
        Installation::of(&locations, "windows", Path::new("/")).is_err(),
        "no folder names no prefix"
    );
}

#[test]
fn an_uninstalled_windows_game_loses_its_prefix_but_never_what_a_link_points_to() {
    use sailover_core::installation::RemovalFailure;
    let (_directory, root) = installation();
    let library = root.join("library");
    let locations = crate::Locations::new(&library, Path::new("/SailOver.app"), Path::new("/home"));
    let windows = |name: &str| {
        Installation::of(
            &locations,
            "windows",
            &library.join("Games/Steam").join(name),
        )
        .unwrap()
        .unwrap()
    };
    let prefix = library.join("Prefixes/steam-42-windows");
    fs::create_dir_all(prefix.join("drive_c/users")).unwrap();
    fs::write(prefix.join("system.reg"), "registry").unwrap();
    windows("steam-42-windows").remove(&locations).unwrap();
    assert!(!prefix.exists());
    windows("steam-42-windows").remove(&locations).unwrap();

    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, library.join("Prefixes/steam-43-windows")).unwrap();
    assert!(matches!(
        windows("steam-43-windows").remove(&locations),
        Err(RemovalFailure::Unsafe(_))
    ));
    assert!(elsewhere.is_dir());
}

#[test]
fn only_processes_running_the_runtime_are_left_behind_wine_processes() {
    let listing = "p10\nftxt\nn/usr/lib/dyld\np20\nftxt\nn/usr/lib/dyld\nftxt\nn/lib/Voile/current/bin/wine-preloader\nftxt\nn/lib/Voile/current/lib/wine/ntdll.so\np30\nftxt\nn/lib/Voile-other/bin/wine\np40\nftxt\nn/lib/Voile/v1/bin/wineserver\n";
    assert_eq!(running_from(listing, Path::new("/lib/Voile"), 1), [20, 40]);
    assert_eq!(
        running_from(listing, Path::new("/lib/Voile"), 20),
        [40],
        "never SailOver itself"
    );
}
