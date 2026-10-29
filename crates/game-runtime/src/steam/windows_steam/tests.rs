use super::*;
use crate::launch_settings::SteamClient;
use crate::wine::{
    Storefront,
    tests::{config, installation, pe, target},
};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

/// What Obelisk installed of TF2: two of its own depots and DirectX's, shared
/// with every game.
fn tf2(install: &Path) -> steam_kit::client::Installation {
    serde_json::from_value(serde_json::json!({
        "app_id": 440,
        "name": "Team Fortress 2",
        "directory": install,
        "platform": "windows",
        "language": "english",
        "total_bytes": 33_381_266_552_u64,
        "manifests": [
            { "depot_id": 228_990, "manifest_id": 1_829_726_630_299_308_803_u64 },
            { "depot_id": 441, "manifest_id": 8_116_815_534_774_807_823_u64 },
            { "depot_id": 232_251, "manifest_id": 3_447_145_031_938_049_636_u64 },
        ],
    }))
    .unwrap()
}

/// TF2 installed in `root/steam-440-windows`, with its receipt and its
/// program.
fn installed(root: &Path) -> PathBuf {
    let install = root.join("steam-440-windows");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join("tf_win64.exe"), pe()).unwrap();
    fs::write(
        install.join(obelisk_core::install::RECEIPT_FILE),
        serde_json::to_vec(&tf2(&install)).unwrap(),
    )
    .unwrap();
    install
}

/// The configuration of a game that Windows Steam starts, under `root`.
fn windows_steam(root: &Path) -> wine::Config {
    let mut config = config(root);
    config.settings.steam_client = SteamClient::Windows;
    config
}

#[test]
fn the_app_manifest_names_the_installed_depots_and_shares_the_redistributables() {
    let manifest = app_manifest(
        &tf2(Path::new("/games/steam-440-windows")),
        "steam-440-windows",
    );

    assert!(manifest.contains("\t\"appid\"\t\t\"440\"\n"), "{manifest}");
    assert!(manifest.contains("\t\"StateFlags\"\t\t\"4\"\n"));
    assert!(manifest.contains("\t\"installdir\"\t\t\"steam-440-windows\"\n"));
    assert!(
        manifest.contains("\t\t\"441\"\n\t\t{\n\t\t\t\"manifest\"\t\t\"8116815534774807823\"\n")
    );
    assert!(
        manifest.contains("\t\t\"232251\"\n\t\t{\n\t\t\t\"manifest\"\t\t\"3447145031938049636\"\n")
    );
    assert!(manifest.contains("\t\"SharedDepots\"\n\t{\n\t\t\"228990\"\t\t\"228980\"\n\t}\n"));
    assert!(!manifest.contains("\t\t\"228990\"\n\t\t{"));
    assert!(manifest.contains("\t\t\"language\"\t\t\"english\"\n"));
    assert_eq!(manifest.matches('{').count(), manifest.matches('}').count());
}

#[test]
fn a_quote_in_a_name_stays_inside_its_value() {
    let mut installed = tf2(Path::new("/games/steam-440-windows"));
    installed.name = r#"The "Best" \ Game"#.into();

    assert!(
        app_manifest(&installed, "steam-440-windows").contains(r#""name"		"The \"Best\" \\ Game""#)
    );
}

#[test]
fn the_installation_joins_steam_s_library_and_follows_the_games_folder() {
    let (_directory, root) = installation();
    let prefix = root.join("prefix");
    let install = installed(&root);

    link(&prefix, &install, 440).unwrap();

    let apps = prefix.join(STEAM).join("steamapps");
    let linked = apps.join("common/steam-440-windows");
    assert_eq!(fs::read_link(&linked).unwrap(), install);
    let manifest = apps.join("appmanifest_440.acf");
    assert!(
        fs::read_to_string(&manifest)
            .unwrap()
            .contains("\"installdir\"\t\t\"steam-440-windows\"")
    );

    // Steam keeps what it learned in its manifest; a games folder that moved
    // moves the link.
    fs::write(&manifest, "Steam's own").unwrap();
    let moved = root.join("moved");
    fs::create_dir_all(&moved).unwrap();
    fs::rename(&install, moved.join("steam-440-windows")).unwrap();
    let install = moved.join("steam-440-windows");
    link(&prefix, &install, 440).unwrap();
    assert_eq!(fs::read_link(&linked).unwrap(), install);
    assert_eq!(fs::read_to_string(&manifest).unwrap(), "Steam's own");
}

#[test]
fn a_folder_in_the_link_s_place_is_never_replaced() {
    let (_directory, root) = installation();
    let prefix = root.join("prefix");
    let install = installed(&root);
    let folder = prefix
        .join(STEAM)
        .join("steamapps/common/steam-440-windows");
    fs::create_dir_all(&folder).unwrap();

    assert!(link(&prefix, &install, 440).is_err());
    assert!(folder.is_dir());
}

#[test]
fn an_installation_of_another_game_writes_no_manifest() {
    let (_directory, root) = installation();
    let prefix = root.join("prefix");
    let install = installed(&root);

    assert!(link(&prefix, &install, 570).is_err());
    assert!(
        !prefix
            .join(STEAM)
            .join("steamapps/appmanifest_570.acf")
            .exists()
    );
}

#[test]
fn steam_s_installer_runs_once_until_steam_is_installed_then_the_install_scripts() {
    let (_directory, root) = installation();
    let prefix = root.join("prefix");
    let install = installed(&root);
    let storefront = Prefix {
        install: &install,
        app_id: 440,
        prefix: &prefix,
    };

    let prerequisites = storefront.prerequisites().unwrap();
    assert_eq!(prerequisites.len(), 1);
    assert_eq!(prerequisites[0].id, INSTALL_STEAM);
    assert_eq!(
        prerequisites[0].programs,
        [(prefix.join(INSTALLER), "/S".to_owned())]
    );

    fs::create_dir_all(prefix.join(STEAM)).unwrap();
    fs::write(prefix.join(STEAM).join("steam.exe"), pe()).unwrap();
    assert!(storefront.prerequisites().unwrap().is_empty());
}

#[test]
fn steam_starts_the_game_hidden_with_its_profile_s_and_player_s_arguments_only() {
    let (_directory, root) = installation();
    let install = installed(&root);
    let mut config = windows_steam(&root);
    config.settings.profile_arguments = vec!["-dxlevel".into(), "95".into()];
    config.settings.arguments = vec!["-novid".into()];

    let started = super::target(
        &config,
        &wine::Game {
            install: &install,
            title: "",
        },
        &target(&install.join("tf_win64.exe"), &["-steam", "-game", "tf"]),
        440,
        &root,
    )
    .unwrap();

    assert_eq!(started.executable, Path::new("/bin/bash"));
    assert_eq!(started.arguments[0], "-c");
    assert_eq!(started.arguments[1], LAUNCHER);
    assert_eq!(
        started.arguments[2..],
        [
            "obelisk-windows-steam".to_owned(),
            config
                .runtime
                .join("bin/wine")
                .to_string_lossy()
                .into_owned(),
            STEAM_EXE.to_owned(),
            "440".to_owned(),
            "tf_win64.exe".to_owned(),
            "-dxlevel".to_owned(),
            "95".to_owned(),
            "-novid".to_owned(),
        ]
    );
    // Quitting asks the game, not Steam, to close.
    assert_eq!(started.program, Some(install.join("tf_win64.exe")));
    assert_eq!(started.installation, Some(install.clone()));
    assert_eq!(started.environment["PROTON_DISABLE_LSTEAMCLIENT"], "1");
    assert_eq!(
        started.environment["WINEPREFIX"],
        config.prefix.to_string_lossy()
    );
}

#[test]
fn only_rosetta_s_wine_starts_windows_steam() {
    let (_directory, root) = installation();
    let install = installed(&root);
    let mut config = windows_steam(&root);
    config.settings.engine = crate::wine::Engine::Linux;

    assert!(
        super::target(
            &config,
            &wine::Game {
                install: &install,
                title: "",
            },
            &target(&install.join("tf_win64.exe"), &[]),
            440,
            &root,
        )
        .is_err()
    );
}

/// A Wine that stands in for Windows Steam: `-applaunch` starts Steam and,
/// a moment later, the game, as processes named by their Windows paths,
/// recording its arguments; `-shutdown` ends Steam.
fn fake_wine(root: &Path, game: &str, game_seconds: u32) -> PathBuf {
    let wine = root.join("wine");
    fs::write(
        &wine,
        format!(
            r#"#!/bin/bash
cd "{root}"
case " $* " in
*" -shutdown "*) [ -f steam.pid ] && kill "$(cat steam.pid)"; echo shutdown >> calls; exit 0 ;;
esac
echo "$*" >> calls
(exec -a 'C:\Program Files (x86)\Steam\steam.exe' sleep 60) &
echo $! > steam.pid
sleep 1
(exec -a 'C:\Games\{game}' sleep {game_seconds}) &
wait
"#,
            root = root.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&wine, fs::Permissions::from_mode(0o755)).unwrap();
    wine
}

#[test]
fn the_launcher_lasts_as_long_as_the_game_then_shuts_steam_down() {
    let (_directory, root) = installation();
    // A name no other process on the machine runs.
    let game = format!("obelisk-test-{}.exe", uuid::Uuid::new_v4().simple());
    let wine = fake_wine(&root, &game, 3);
    let started = Instant::now();

    let status = Command::new("/bin/bash")
        .args(["-c", LAUNCHER, "obelisk-windows-steam"])
        .arg(&wine)
        .args([STEAM_EXE, "440", &game, "-dxlevel", "95"])
        .status()
        .unwrap();

    assert!(status.success());
    // The game ran about three seconds after Steam started it.
    assert!(started.elapsed() >= Duration::from_secs(3));
    let calls = fs::read_to_string(root.join("calls")).unwrap();
    let mut calls = calls.lines();
    assert_eq!(
        calls.next(),
        Some(
            format!(
                r"{STEAM_EXE} -silent -nofriendsui -nochatui -cef-disable-gpu -cef-disable-gpu-compositing -applaunch 440 -dxlevel 95"
            )
            .as_str()
        )
    );
    assert_eq!(calls.next(), Some("shutdown"));
}

/// Steam's loginusers.vdf with an account it forgot, then two it remembers,
/// the second the most recent.
const LOGIN_USERS_VDF: &str = "\"users\"\n{\n\t\"76561197960287930\"\n\t{\n\t\t\"AccountName\"\t\t\"forgotten\"\n\t\t\"RememberPassword\"\t\t\"0\"\n\t\t\"MostRecent\"\t\t\"1\"\n\t}\n\t\"76561197960287931\"\n\t{\n\t\t\"AccountName\"\t\t\"older\"\n\t\t\"RememberPassword\"\t\t\"1\"\n\t\t\"MostRecent\"\t\t\"0\"\n\t}\n\t\"76561197960287932\"\n\t{\n\t\t\"AccountName\"\t\t\"goose\"\n\t\t\"RememberPassword\"\t\t\"1\"\n\t\t\"MostRecent\"\t\t\"1\"\n\t}\n}\n";

#[test]
fn steam_signs_in_by_itself_to_the_most_recent_account_whose_login_it_kept() {
    assert_eq!(
        remembered_account(LOGIN_USERS_VDF).as_deref(),
        Some("goose")
    );
    assert_eq!(
        remembered_account(
            &LOGIN_USERS_VDF.replace("\"MostRecent\"\t\t\"1\"", "\"MostRecent\"\t\t\"0\"")
        )
        .as_deref(),
        Some("older")
    );
    assert_eq!(remembered_account("\"users\"\n{\n}\n"), None);
}

#[test]
fn a_prefix_without_steam_s_kept_token_is_signed_out() {
    let (_directory, root) = installation();
    let prefix = root.join("prefix");
    let users = prefix.join(STEAM).join(LOGIN_USERS);
    fs::create_dir_all(users.parent().unwrap()).unwrap();
    fs::write(&users, LOGIN_USERS_VDF).unwrap();
    assert_eq!(account(&prefix), None);

    let tokens = prefix.join(LOGIN_TOKENS);
    fs::create_dir_all(tokens.parent().unwrap()).unwrap();
    fs::write(&tokens, "\"MachineUserConfigStore\"\n{\n}\n").unwrap();
    assert_eq!(account(&prefix).as_deref(), Some("goose"));
}
