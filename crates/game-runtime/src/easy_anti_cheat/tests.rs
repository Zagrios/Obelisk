use super::*;
use crate::pe::tests::Fixture;

/// How the bootstrappers of Epic Online Services describe themselves.
const EOS_BOOTSTRAPPER: &str = "Easy Anti-Cheat Bootstrapper (EOS)";

/// Writes `bytes` to `relative` in `install`, making its folders.
pub(crate) fn write(install: &Path, relative: &str, bytes: &[u8]) {
    let path = install.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Writes, in the folder `at` of `install`, a bootstrapper named
/// `bootstrapper` and the EasyAntiCheat folder whose settings name
/// `executable`.
pub(crate) fn protect(install: &Path, at: &str, bootstrapper: &str, executable: &str) {
    let settings = serde_json::json!({ "productid": "p", "executable": executable });
    let folder = Path::new(at);
    write(
        install,
        &folder.join("EasyAntiCheat/Settings.json").to_string_lossy(),
        settings.to_string().as_bytes(),
    );
    write(
        install,
        &folder.join(bootstrapper).to_string_lossy(),
        &Fixture::described(EOS_BOOTSTRAPPER).bytes(),
    );
}

#[test]
fn an_installation_s_bootstrapper_is_found_by_its_description_beside_its_easyanticheat_folder() {
    let directory = tempfile::tempdir().unwrap();
    let install = directory.path();
    write(
        install,
        "Binaries/Win64/Launcher.exe",
        &Fixture {
            dotnet: true,
            ..Fixture::described("Launcher")
        }
        .bytes(),
    );
    write(
        install,
        "Binaries/Win64/RocketLeague.exe",
        &Fixture::importing(&["d3d11.dll"]).bytes(),
    );
    write(install, "Engine/Config/BaseEngine.ini", b"");
    protect(
        install,
        "Binaries/Win64",
        "RocketLeague_EAC.exe",
        "RocketLeague.exe",
    );

    assert_eq!(
        of(install),
        Some(EasyAntiCheat {
            bootstrapper: "Binaries/Win64/RocketLeague_EAC.exe".into(),
            game: "Binaries/Win64/RocketLeague.exe".into(),
        })
    );
}

#[test]
fn settings_in_any_case_with_a_byte_order_mark_or_windows_separators_name_the_game() {
    let directory = tempfile::tempdir().unwrap();
    let install = directory.path();
    write(
        install,
        "Game/easyanticheat/settings.json",
        b"\xef\xbb\xbf{\r\n\t\"executable\"\t: \"eldenring.exe\"\r\n}",
    );
    write(
        install,
        "Game/start_protected_game.exe",
        &Fixture::described(EOS_BOOTSTRAPPER).bytes(),
    );
    assert_eq!(
        of(install).map(|found| found.game),
        Some("Game/eldenring.exe".into())
    );

    let directory = tempfile::tempdir().unwrap();
    let install = directory.path();
    protect(
        install,
        "",
        "DeadByDaylight.exe",
        r"DeadByDaylight\Binaries\Win64\DeadByDaylight-Win64-Shipping.exe",
    );
    assert_eq!(
        of(install),
        Some(EasyAntiCheat {
            bootstrapper: "DeadByDaylight.exe".into(),
            game: "DeadByDaylight/Binaries/Win64/DeadByDaylight-Win64-Shipping.exe".into(),
        })
    );
}

#[test]
fn settings_that_write_a_path_separator_as_a_single_backslash_still_name_the_game() {
    // As Halo Infinite's do, then with folders whose first letter makes a JSON
    // escape of the backslash, then as JSON wants them.
    for (settings, game) in [
        (
            &br#"{"title":  "Halo Infinite", "executable":  "game\HaloInfinite.exe"}"#[..],
            "game/HaloInfinite.exe",
        ),
        (
            br#"{"executable": "game\bin\tools\update.exe"}"#,
            "game/bin/tools/update.exe",
        ),
        (
            br#"{"title": "A \"game\" \u00e9", "executable": "game\\bin\/Game.exe"}"#,
            "game/bin/Game.exe",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        protect(directory.path(), "", "HaloInfinite.exe", "Game.exe");
        write(directory.path(), "EasyAntiCheat/Settings.json", settings);
        assert_eq!(
            of(directory.path()).map(|found| found.game),
            Some(PathBuf::from(game)),
            "{}",
            String::from_utf8_lossy(settings)
        );
    }
}

#[test]
fn an_installation_without_a_bootstrapper_beside_easyanticheat_settings_has_no_easy_anti_cheat() {
    // Nothing of the anti-cheat.
    let directory = tempfile::tempdir().unwrap();
    write(
        directory.path(),
        "Game.exe",
        &Fixture::described("A game").bytes(),
    );
    assert_eq!(of(directory.path()), None);

    // Its folder, without a bootstrapper beside it.
    let directory = tempfile::tempdir().unwrap();
    write(
        directory.path(),
        "EasyAntiCheat/Settings.json",
        br#"{"executable": "Game.exe"}"#,
    );
    write(
        directory.path(),
        "Game.exe",
        &Fixture::described("A game").bytes(),
    );
    assert_eq!(of(directory.path()), None);

    // Settings that name no program, or one outside the bootstrapper's folder.
    for settings in [
        &br#"{"title": "A game"}"#[..],
        br#"{"executable": "..\\Game.exe"}"#,
        b"{",
    ] {
        let directory = tempfile::tempdir().unwrap();
        protect(directory.path(), "", "start_protected_game.exe", "Game.exe");
        write(directory.path(), "EasyAntiCheat/Settings.json", settings);
        assert_eq!(
            of(directory.path()),
            None,
            "{}",
            String::from_utf8_lossy(settings)
        );
    }

    // A bootstrapper deeper than any game puts one.
    let directory = tempfile::tempdir().unwrap();
    protect(
        directory.path(),
        "a/b/c",
        "start_protected_game.exe",
        "Game.exe",
    );
    assert_eq!(of(directory.path()), None);
}

#[test]
fn a_bootstrapper_behind_a_link_or_in_a_hidden_folder_is_not_the_installation_s() {
    let elsewhere = tempfile::tempdir().unwrap();
    protect(elsewhere.path(), "", "start_protected_game.exe", "Game.exe");
    let directory = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), directory.path().join("Game")).unwrap();
    protect(
        directory.path(),
        ".obelisk-content",
        "start_protected_game.exe",
        "Game.exe",
    );
    assert_eq!(of(directory.path()), None);
}
