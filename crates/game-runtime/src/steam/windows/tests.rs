use super::*;
use crate::wine::tests::{calls, config, installation, pe, record_calls, target};

fn locations(root: &Path) -> crate::Locations {
    crate::Locations::new(
        &root.join("library"),
        &root.join("SailOver.app"),
        &root.join("home"),
    )
}

/// A Windows game in `root/game` whose Steam API sits beside Unity's plugins.
fn steam_game(root: &Path) -> PathBuf {
    let install = root.join("game");
    fs::create_dir_all(install.join("Game_Data/Plugins/x86_64")).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();
    fs::write(
        install.join("Game_Data/Plugins/x86_64/steam_api64.dll"),
        pe(),
    )
    .unwrap();
    install
}

#[test]
fn a_windows_game_starts_behind_steam_exe_in_its_prefix() {
    let (_directory, root) = installation();
    let install = steam_game(&root);
    let config = config(&root);

    let (target, steam_api) = super::target(
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
            HELPER,
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
fn a_game_without_the_steam_api_is_refused() {
    let (_directory, root) = installation();
    let install = root.join("game");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("Game.exe"), pe()).unwrap();

    assert!(matches!(
        super::target(
            &install,
            &target(&install.join("Game.exe"), &[]),
            &root,
            &config(&root)
        ),
        Err(Error::ApiMissing)
    ));
}

#[test]
fn a_steam_installation_lists_the_steps_of_its_install_scripts() {
    let (_directory, root) = installation();
    let install = root.join("game");
    let redist = install.join("_CommonRedist/vcredist/2022");
    fs::create_dir_all(&redist).unwrap();
    fs::create_dir_all(install.join(".sailover-content")).unwrap();
    fs::write(
        install.join(steam_kit::install_script::LIST),
        "_CommonRedist/missing/installscript.vdf\n_CommonRedist/vcredist/2022/installscript.vdf",
    )
    .unwrap();
    fs::write(
        redist.join("installscript.vdf"),
        r#""installscript" { "Run Process" { "x64 14.51" {
            "HasRunKey" "HKEY_LOCAL_MACHINE\\Software\\Valve\\Steam\\Apps\\CommonRedist\\vcredist\\2022"
            "process 1" "%INSTALLDIR%\\_CommonRedist\\vcredist\\2022\\VC_redist.x64.exe"
            "command 1" "/q /norestart"
        } } }"#,
    )
    .unwrap();

    use wine::Storefront;
    assert_eq!(
        Prefix { install: &install }.prerequisites().unwrap(),
        [wine::Prerequisite {
            id:
                r"HKEY_LOCAL_MACHINE\Software\Valve\Steam\Apps\CommonRedist\vcredist\2022\x64 14.51"
                    .into(),
            programs: vec![(redist.join("VC_redist.x64.exe"), "/q /norestart".into())],
        }]
    );
    assert_eq!(Prefix { install: &root }.prerequisites().unwrap(), []);
}

#[test]
fn a_windows_launch_readies_the_installation_prefix_and_names_it_once() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let runtime = locations.voile.join("current");
    record_calls(&runtime);
    fs::create_dir_all(runtime.join("steam")).unwrap();
    for name in CLIENTS {
        fs::write(runtime.join("steam").join(name), name).unwrap();
    }
    let install = steam_game(&root.join("library/Games/Steam"));
    let install = install.parent().unwrap().join("steam-42-windows");
    fs::rename(root.join("library/Games/Steam/game"), &install).unwrap();

    let request = crate::steam::prepare(
        &locations,
        42,
        "windows",
        &install,
        &target(&install.join("Game.exe"), &[]),
    )
    .unwrap();

    let prefix = root.join("library/Prefixes/steam-42-windows");
    let wine = request.config.wine.as_ref().unwrap();
    assert_eq!(wine.runtime, runtime);
    assert_eq!(wine.prefix, prefix);
    assert_eq!(
        request.cloud.as_ref().unwrap().prefix.as_ref(),
        Some(&prefix)
    );
    assert_eq!(request.target.arguments[0], HELPER);
    assert_eq!(
        request.target.log,
        Some(root.join("home/Library/Logs/SailOver/Steam/wine-42.log"))
    );
    assert_eq!(calls(&root.join("library/Prefixes")), ["wineboot --init"]);
    assert_eq!(
        fs::read(prefix.join("drive_c/Program Files (x86)/Steam/steamclient64.dll")).unwrap(),
        b"steamclient64.dll"
    );
}

#[test]
fn a_windows_launch_waits_for_voile_and_a_mac_one_never_does() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let install = steam_game(&root);

    assert!(matches!(
        crate::steam::prepare(
            &locations,
            42,
            "windows",
            &install,
            &target(&install.join("Game.exe"), &[])
        ),
        Err(Error::WineUnavailable)
    ));
    assert!(!locations.wine_prefixes.exists());
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
