//! Resolve publisher-provided launch options without executing a game or a shell.
use std::path::Path;

use forge_core::launch::{LaunchDirectory, LaunchTarget};

use crate::cm::{AppInfo, LaunchConfig, supports_platform};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPlan {
    pub app_id: u32,
    pub platform: String,
    pub target: LaunchTarget,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LaunchError(String);

/// The caller checks account ownership and the installation receipt first.
pub fn plan(app: &AppInfo, platform: &str, directory: &Path) -> Result<LaunchPlan, LaunchError> {
    if !matches!(platform, "macos" | "windows") {
        return Err(LaunchError(
            "backend.steam.launch.unsupportedPlatform".into(),
        ));
    }
    if !supports_platform(&app.os_list, platform) {
        return Err(LaunchError(
            "backend.steam.launch.platformUnavailable".into(),
        ));
    }
    let root = LaunchDirectory::new(directory).map_err(shared_error)?;
    let config = app
        .launch_configs
        .iter()
        .filter(|config| supports_platform(&config.os_list, platform))
        .filter_map(|config| launch_rank(config).map(|rank| (config, rank)))
        .min_by_key(|(config, rank)| {
            (
                *rank,
                config.os_list.is_empty(),
                config.os_arch.as_deref() == Some("32"),
            )
        })
        .map(|(config, _)| config)
        .ok_or(LaunchError(
            "backend.steam.launch.noCompatibleConfiguration".into(),
        ))?;
    let target = root
        .resolve(
            platform,
            &config.executable,
            &config.arguments,
            &config.working_directory,
        )
        .map_err(shared_error)?;
    Ok(LaunchPlan {
        app_id: app.app_id,
        platform: platform.into(),
        target,
    })
}

fn shared_error(error: forge_core::launch::LaunchError) -> LaunchError {
    LaunchError(format!("backend.steam.launch.{}", error.key_suffix()))
}

fn launch_rank(config: &LaunchConfig) -> Option<u8> {
    // Special setup, uninstall, editor and VR entries must not become the default.
    match config.launch_type.to_ascii_lowercase().as_str() {
        "default" => Some(0),
        "" | "none" => Some(1),
        "option1" | "option2" | "option3" | "option4" => Some(2),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(configs: Vec<LaunchConfig>) -> AppInfo {
        AppInfo {
            app_id: 42,
            name: "Fixture".into(),
            os_list: vec!["macos".into(), "windows".into()],
            free_to_download: false,
            depots: vec![],
            launch_configs: configs,
        }
    }
    fn config(executable: &str, os: &str, kind: &str) -> LaunchConfig {
        LaunchConfig {
            executable: executable.into(),
            arguments: "--title \"My game\"".into(),
            working_directory: String::new(),
            launch_type: kind.into(),
            os_list: vec![os.into()],
            os_arch: None,
        }
    }
    fn file(root: &Path, relative: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture, never executed").unwrap();
    }

    #[test]
    fn selects_platform_and_default_game_without_running_setup() {
        let root = tempfile::tempdir().unwrap();
        file(root.path(), "game");
        file(root.path(), "game.exe");
        let app = app(vec![
            config("setup", "macos", "config"),
            config("game.exe", "windows", "default"),
            config("alternate", "macos", "option1"),
            config("game", "macos", "default"),
        ]);
        let plan = plan(&app, "macos", root.path()).unwrap();
        assert_eq!(
            Path::new(&plan.target.executable).file_name().unwrap(),
            "game"
        );
        assert_eq!(plan.target.arguments, ["--title", "My game"]);
        assert!(!plan.target.is_application_bundle);
        assert!(super::plan(&app, "linux", root.path()).is_err());
    }

    #[test]
    fn resolves_bundle_and_working_directory_from_metadata() {
        let root = tempfile::tempdir().unwrap();
        file(root.path(), "My Game.app/Contents/MacOS/My Game");
        let mut launch = config("My Game.app", "macos", "default");
        launch.working_directory = "My Game.app/Contents".into();
        let resolved = plan(&app(vec![launch]), "macos", root.path()).unwrap();
        assert!(resolved.target.is_application_bundle);
        assert!(resolved.target.executable.ends_with("/My Game.app"));
        assert!(
            resolved
                .target
                .working_directory
                .ends_with("/My Game.app/Contents")
        );
        let explicit = config("My Game.app/Contents/MacOS/My Game", "macos", "default");
        let resolved = plan(&app(vec![explicit]), "macos", root.path()).unwrap();
        assert!(!resolved.target.is_application_bundle);
        assert!(
            resolved
                .target
                .executable
                .ends_with("/Contents/MacOS/My Game")
        );
    }

    #[test]
    fn rejects_missing_traversal_absolute_and_wrong_platform_paths() {
        let root = tempfile::tempdir().unwrap();
        file(root.path(), "game.exe");
        for path in [
            "missing",
            "../game",
            "..\\game",
            "/bin/sh",
            "C:\\game.exe",
            "\\\\server\\game",
            "game.exe",
        ] {
            assert!(
                plan(
                    &app(vec![config(path, "macos", "default")]),
                    "macos",
                    root.path()
                )
                .is_err(),
                "{path}"
            );
        }
        let mut app = app(vec![config("game.exe", "windows", "default")]);
        app.os_list = vec!["windows".into()];
        assert!(plan(&app, "macos", root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_executable_and_working_directory_symlink_escapes() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        file(outside.path(), "game");
        symlink(outside.path(), root.path().join("linked")).unwrap();
        assert!(
            plan(
                &app(vec![config("linked/game", "macos", "default")]),
                "macos",
                root.path()
            )
            .is_err()
        );
        file(root.path(), "game");
        let mut config = config("game", "macos", "default");
        config.working_directory = "linked".into();
        assert!(plan(&app(vec![config]), "macos", root.path()).is_err());
    }

    #[test]
    fn shared_resolution_preserves_steam_error_keys_and_validation_order() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            plan(&app(vec![]), "macos", Path::new("missing"))
                .unwrap_err()
                .to_string(),
            "backend.steam.launch.directoryMissing"
        );
        assert_eq!(
            plan(&app(vec![]), "macos", root.path())
                .unwrap_err()
                .to_string(),
            "backend.steam.launch.noCompatibleConfiguration"
        );
        let mut windows_only = app(vec![]);
        windows_only.os_list = vec!["windows".into()];
        assert_eq!(
            plan(&windows_only, "macos", Path::new("missing"))
                .unwrap_err()
                .to_string(),
            "backend.steam.launch.platformUnavailable"
        );
        assert_eq!(
            plan(
                &app(vec![config("../game", "macos", "default")]),
                "macos",
                root.path()
            )
            .unwrap_err()
            .to_string(),
            "backend.steam.launch.pathOutsideGame"
        );
        file(root.path(), "game");
        let mut launch = config("game", "macos", "default");
        launch.arguments = "'unfinished".into();
        assert_eq!(
            plan(&app(vec![launch]), "macos", root.path())
                .unwrap_err()
                .to_string(),
            "backend.steam.launch.unclosedQuotes"
        );
    }
}
