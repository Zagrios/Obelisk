use super::*;
use crate::supervisor::architecture::tests::{ARM64, thin};
use std::fs;

fn target(executable: &Path, bundle: bool) -> LaunchTarget {
    LaunchTarget {
        executable: executable.to_string_lossy().into_owned(),
        arguments: vec![],
        working_directory: String::new(),
        is_application_bundle: bundle,
    }
}

fn installation() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    (directory, root)
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn application(root: &Path, declared: &str) -> PathBuf {
    let bundle = root.join("Game.app");
    fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
    fs::write(
        bundle.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleExecutable</key><string>{declared}</string></dict></plist>"#
        ),
    )
    .unwrap();
    bundle
}

#[test]
fn a_standalone_macho_gets_only_its_owner_execute_permission_back() {
    let (_directory, root) = installation();
    let game = root.join("game");
    fs::write(&game, thin(ARM64, true)).unwrap();
    fs::set_permissions(&game, fs::Permissions::from_mode(0o640)).unwrap();

    let (executable, architecture) = executable(&root, &target(&game, false)).unwrap();

    assert_eq!(executable, game);
    assert_eq!(architecture, Architecture::Arm64);
    assert_eq!(mode(&game), 0o740);
}

#[test]
fn a_file_that_is_not_a_macho_is_never_made_executable() {
    let (_directory, root) = installation();
    let script = root.join("game.sh");
    fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(executable(&root, &target(&script, false)).is_err());
    assert_eq!(mode(&script), 0o644);
}

#[test]
fn a_bundle_launches_its_declared_executable_without_touching_resources() {
    let (_directory, root) = installation();
    let bundle = application(&root, "Game");
    let binary = bundle.join("Contents/MacOS/Game");
    let resource = bundle.join("Contents/Resources/data.bin");
    fs::write(&binary, thin(ARM64, true)).unwrap();
    fs::write(&resource, thin(ARM64, true)).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&resource, fs::Permissions::from_mode(0o644)).unwrap();

    let (executable, _) = executable(&root, &target(&bundle, true)).unwrap();

    assert_eq!(executable, binary);
    assert_eq!(mode(&binary), 0o744);
    assert_eq!(mode(&resource), 0o644);
}

#[test]
fn a_bundle_cannot_declare_an_executable_outside_itself_or_missing() {
    let (_directory, root) = installation();
    for declared in ["", "../../outside", "Missing"] {
        let bundle = application(&root, declared);
        assert!(
            matches!(
                executable(&root, &target(&bundle, true)),
                Err(Error::InvalidApplication)
            ),
            "{declared:?}"
        );
        fs::remove_dir_all(&bundle).unwrap();
    }
}

#[test]
fn nothing_outside_the_installation_is_a_target() {
    let (_directory, root) = installation();
    let outside = tempfile::tempdir().unwrap();
    let game = outside.path().join("game");
    fs::write(&game, thin(ARM64, true)).unwrap();
    assert!(matches!(
        executable(&root, &target(&game, false)),
        Err(Error::NotExecutable)
    ));
    let link = root.join("game");
    std::os::unix::fs::symlink(&game, &link).unwrap();
    assert!(matches!(
        executable(&root, &target(&link, false)),
        Err(Error::NotExecutable)
    ));
}

#[test]
fn a_game_inherits_the_players_language_and_account_only() {
    let forge = HashMap::from([
        ("USER", "player"),
        ("LANG", "fr_FR.UTF-8"),
        ("LC_ALL", ""),
        ("TMPDIR", "/private/var/folders/forge/T/"),
        ("DYLD_INSERT_LIBRARIES", "/tmp/inject.dylib"),
        ("FORGE_LIBRARY_DIRECTORY", "/tmp/library"),
    ]);
    let environment = environment_from(Path::new("/Users/player"), |key| {
        forge.get(key).map(|value| (*value).to_owned())
    });
    assert_eq!(
        environment,
        HashMap::from([
            ("HOME".into(), "/Users/player".into()),
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("TMPDIR".into(), "/private/var/folders/forge/T/".into()),
            ("USER".into(), "player".into()),
            ("LOGNAME".into(), "player".into()),
            ("LANG".into(), "fr_FR.UTF-8".into()),
        ])
    );
}

#[test]
fn a_game_without_any_language_gets_utf8_english() {
    let environment = environment_from(Path::new("/Users/player"), |_| None);
    assert_eq!(
        environment.get("LANG").map(String::as_str),
        Some("en_US.UTF-8")
    );
    assert!(!environment.contains_key("USER"));
}
