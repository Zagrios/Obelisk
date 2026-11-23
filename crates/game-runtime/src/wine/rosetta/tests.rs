use super::*;
use crate::wine::{
    prepare,
    tests::{Plain, calls, game, installation, locations, recording_runtime},
    windows_path,
};

/// Readies the created prefix of `config` for a game whose storefront names
/// `prerequisites`.
fn ready(config: &Config, root: &Path, prerequisites: &[Prerequisite]) {
    fs::write(config.prefix.join("system.reg"), "").unwrap();
    let (install, target) = game(root);
    let storefront = Plain {
        prerequisites: prerequisites.to_vec(),
    };
    prepare(config, &locations(root), &install, &target, &storefront).unwrap();
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

    ready(&config, &root, &prerequisites);
    ready(&config, &root, &prerequisites);

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
fn a_visual_cpp_installer_runs_with_the_versions_of_wine_s_own_dlls_hidden() {
    let (_directory, root) = installation();
    let config = recording_runtime(&root);
    fs::write(
        config.runtime.join("bin/wine"),
        "#!/bin/sh\nprintf '%s %s\\n' \"${1##*/}\" \"${CX_HIDE_BUILTIN_VERSION_RESOURCES:-shown}\" \
         >> \"$WINEPREFIX/../calls\"\n",
    )
    .unwrap();
    let redist = root.join("game/redist");
    fs::create_dir_all(&redist).unwrap();
    let names = ["VC_redist.x64.exe", "vcredist_x86.exe", "DXSETUP.exe"];
    for name in names {
        fs::write(redist.join(name), "").unwrap();
    }
    let prerequisite = Prerequisite {
        id: "redist".into(),
        programs: names
            .iter()
            .map(|name| (redist.join(name), String::new()))
            .collect(),
    };

    ready(&config, &root, &[prerequisite]);

    assert_eq!(
        calls(&root),
        [
            "VC_redist.x64.exe 1",
            "vcredist_x86.exe 1",
            "DXSETUP.exe shown"
        ]
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
    ready(&config, &root, &[prerequisite]);
    assert!(calls(&root).is_empty());
    assert!(!config.prefix.join(PREREQUISITES).exists());
}

#[test]
fn the_profile_s_dxmt_options_follow_obelisk_s_own_so_that_its_value_of_a_key_wins() {
    let own = [
        "d3d11.preferredMaxFrameRate=60".to_owned(),
        "dxgi.maxFrameLatency=1".to_owned(),
    ];

    assert_eq!(dxmt_config(&[], None), None);
    assert_eq!(dxmt_config(&[], Some("")), None);
    assert_eq!(
        dxmt_config(&[], Some("d3d11.metalSpatialUpscaleFactor=1.5")).as_deref(),
        Some("d3d11.metalSpatialUpscaleFactor=1.5")
    );
    assert_eq!(
        dxmt_config(&own, None).as_deref(),
        Some("d3d11.preferredMaxFrameRate=60;dxgi.maxFrameLatency=1")
    );
    assert_eq!(
        dxmt_config(&own, Some("d3d11.preferredMaxFrameRate=30;")).as_deref(),
        Some(
            "d3d11.preferredMaxFrameRate=60;dxgi.maxFrameLatency=1;d3d11.preferredMaxFrameRate=30;"
        )
    );
}

#[test]
fn dxmt_keeps_the_game_s_shaders_in_its_prefix() {
    let (_directory, root) = crate::wine::tests::installation();
    let config = crate::wine::tests::config(&root);

    let environment = environment(&config);

    assert_eq!(
        environment["DXMT_SHADER_CACHE_PATH"],
        config.prefix.join("obelisk-dxmt-cache").to_str().unwrap()
    );
    assert!(
        crate::wine::Engine::Rosetta
            .reserved()
            .variables
            .contains(&"DXMT_SHADER_CACHE_PATH")
    );
}

#[test]
fn a_profile_s_dxmt_options_reach_dxmt_whole() {
    let (_directory, root) = installation();
    let mut config = recording_runtime(&root);
    config.settings.variables = std::collections::BTreeMap::from([(
        DXMT_CONFIG.to_owned(),
        "d3d11.metalSpatialUpscaleFactor=1.5".to_owned(),
    )]);

    assert_eq!(
        environment(&config)[DXMT_CONFIG],
        "d3d11.metalSpatialUpscaleFactor=1.5"
    );
}
