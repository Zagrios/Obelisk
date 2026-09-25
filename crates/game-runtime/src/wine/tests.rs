use super::*;

/// The smallest file `portable_executable` accepts: a DOS header whose last
/// field points just past it, at a PE signature.
fn pe() -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
    bytes.extend_from_slice(b"PE\0\0");
    bytes
}

fn installation() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

fn target(executable: &Path, arguments: &[&str]) -> LaunchTarget {
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

fn config(root: &Path) -> Config {
    Config {
        runtime: root.join("runtime"),
        prefix: root.join("prefix"),
    }
}

#[test]
fn a_windows_game_starts_behind_steam_exe_in_its_prefix() {
    let (_directory, root) = installation();
    let install = root.join("game");
    fs::create_dir_all(install.join("Game_Data/Plugins/x86_64")).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();
    fs::write(
        install.join("Game_Data/Plugins/x86_64/steam_api64.dll"),
        pe(),
    )
    .unwrap();
    let config = config(&root);

    let (target, steam_api) = steam_target(
        &install,
        &target(&install.join("Game.exe"), &["-screen-fullscreen", "0"]),
        &root,
        &config,
    )
    .unwrap();

    assert_eq!(target.executable, root.join("runtime/bin/wine"));
    assert_eq!(
        target.arguments,
        [
            STEAM_HELPER,
            &install.join("Game.exe").to_string_lossy(),
            "-screen-fullscreen",
            "0",
        ]
    );
    assert_eq!(target.working_directory, install);
    assert_eq!(
        target.environment["WINEPREFIX"],
        root.join("prefix").to_string_lossy()
    );
    assert_eq!(
        steam_api,
        install.join("Game_Data/Plugins/x86_64/steam_api64.dll")
    );
}

#[test]
fn only_a_windows_program_inside_the_installation_starts() {
    let (_directory, root) = installation();
    let install = root.join("game");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("steam_api.dll"), pe()).unwrap();
    fs::write(install.join("script.exe"), b"#!/bin/sh\n").unwrap();
    fs::write(root.join("outside.exe"), pe()).unwrap();
    let config = config(&root);

    for executable in [install.join("script.exe"), root.join("outside.exe")] {
        assert!(matches!(
            steam_target(&install, &target(&executable, &[]), &root, &config),
            Err(Error::NotExecutable)
        ));
    }
}

#[test]
fn a_game_without_the_steam_api_is_refused() {
    let (_directory, root) = installation();
    let install = root.join("game");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();

    assert!(matches!(
        steam_target(
            &install,
            &target(&install.join("Game.exe"), &[]),
            &root,
            &config(&root)
        ),
        Err(Error::ApiMissing)
    ));
}

#[test]
fn d3dmetal_is_selected_only_when_the_runtime_carries_it() {
    let (_directory, root) = installation();
    let config = config(&root);
    assert!(
        !config
            .environment()
            .contains_key("CX_ACTIVE_GRAPHICS_BACKEND")
    );

    let library = root.join("runtime/lib/external/libd3dshared.dylib");
    fs::create_dir_all(library.parent().unwrap()).unwrap();
    fs::write(&library, b"").unwrap();

    let environment = config.environment();
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

    assert!(matches!(prepare(&config, &root), Err(Error::Unavailable)));
    assert!(!config.prefix.exists());
}

#[test]
fn wine_reports_its_errors_unless_the_runtime_names_other_channels() {
    let (_directory, root) = installation();
    let config = config(&root);
    assert_eq!(config.environment()["WINEDEBUG"], "err+all,fixme-all");

    fs::create_dir_all(&config.runtime).unwrap();
    fs::write(config.runtime.join("winedebug"), "+steamclient,err+all\n").unwrap();

    assert_eq!(config.environment()["WINEDEBUG"], "+steamclient,err+all");
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

    unlink_home_folders(&profile).unwrap();

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
fn a_windows_game_saves_under_its_prefix_profile() {
    let (_directory, root) = installation();
    let cloud = crate::steam::cloud::Config {
        root: root.join("cloud"),
        home: root.join("home"),
        install: root.join("game"),
        app_id: 42,
        prefix: Some(root.join("prefix")),
    };

    assert_eq!(
        cloud.platform(),
        steam_kit::cloud::CloudPlatform::Windows {
            profile: root.join("prefix/drive_c/users/crossover"),
        }
    );
    assert_eq!(cloud.platform().mask(), 1);
}
