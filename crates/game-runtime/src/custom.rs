//! Launching and process supervision of custom/non-store Windows executables in Obelisk.

use crate::{
    Locations, voile,
    wine::{Engine, Graphics},
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Configuration for launching a custom executable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomLaunchConfig {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub prefix: PathBuf,
    pub runner: Option<PathBuf>,
    pub engine: Engine,
    pub graphics: Graphics,
    pub ray_tracing: bool,
    pub performance_hud: bool,
    pub render_percent: u32,
    pub environment: HashMap<String, String>,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum CustomError {
    #[error("backend.runtime.wineUnavailable")]
    WineUnavailable,
    #[error("backend.runtime.notExecutable")]
    NotExecutable,
    #[error("backend.runtime.invalidApplication")]
    InvalidApplication,
    #[error("launch start failed")]
    StartFailed,
    #[error("launch storage failed")]
    Storage,
    #[error("{0}")]
    Failed(String),
}

impl From<std::io::Error> for CustomError {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}

/// Launches a custom Windows executable using Wine or a custom runner.
pub fn launch_custom(
    config: &CustomLaunchConfig,
    locations: &Locations,
) -> Result<u32, CustomError> {
    if !config.executable.is_file() {
        return Err(CustomError::NotExecutable);
    }

    // Determine runtime path: either from custom runner parent or Voile runtime
    let runtime = if let Some(ref runner) = config.runner {
        if !runner.exists() {
            return Err(CustomError::NotExecutable);
        }
        runner.parent().unwrap_or(Path::new("")).to_path_buf()
    } else {
        voile::runtime(&locations.voile).ok_or(CustomError::WineUnavailable)?
    };

    // Ensure the prefix directory exists
    fs::create_dir_all(&config.prefix)?;

    // Set up log file in <library>/Logs/Custom/<game_stem>.log
    let logs_dir = locations.library.join("Logs/Custom");
    fs::create_dir_all(&logs_dir)?;
    let exe_stem = config
        .executable
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let log_path = logs_dir.join(format!("{exe_stem}.log"));
    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)?;

    // Runner binary
    let runner_binary = if let Some(ref runner) = config.runner {
        runner.clone()
    } else {
        runtime.join("bin/wine")
    };

    let mut command = Command::new(&runner_binary);

    // Working directory is the parent folder of the executable
    let working_dir = config.executable.parent().unwrap_or(&config.prefix);
    command.current_dir(working_dir);

    // Arguments: first the executable itself, then its command-line arguments
    command.arg(&config.executable);
    for arg in &config.arguments {
        command.arg(arg);
    }

    // Prepare clean environment
    command.env_clear();
    command.envs(crate::native::environment(&locations.home));
    command.env("WINEPREFIX", &config.prefix);
    command.env("WINEDEBUG", "err+all,fixme-all");
    command.env("WINEMSYNC", "1");
    command.env("WINE_LARGE_ADDRESS_AWARE", "1");
    command.env("ROSETTA_ADVERTISE_AVX", "1");
    command.env("DOTNET_EnableWriteXorExecute", "0");

    if config.performance_hud {
        command.env("MTL_HUD_ENABLED", "1");
    }

    // Graphics backend configuration
    let d3dmetal_dylib = runtime.join("lib/external/libd3dshared.dylib");
    if d3dmetal_dylib.is_file() {
        command.env("CX_APPLEGPTK_LIBD3DSHARED_PATH", &d3dmetal_dylib);
    }
    let d3dmetal_framework = runtime.join("lib/external/D3DMetal.framework/D3DMetal");
    if d3dmetal_framework.is_file() {
        command.env("D3DMETAL_FRAMEWORK_PATH", &d3dmetal_framework);
    }

    match config.graphics {
        Graphics::D3DMetal => {
            let folders = [runtime.join("lib/d3dmetal"), runtime.join("lib/dxmt")];
            let search = folders
                .iter()
                .filter(|f| f.is_dir())
                .map(|f| f.to_string_lossy())
                .collect::<Vec<_>>()
                .join(":");
            if !search.is_empty() {
                command.env("WINEDLLPATH_PREPEND", search);
            }
            command.env("CX_ACTIVE_GRAPHICS_BACKEND", "d3dmetal");
            if !config.ray_tracing {
                command.env("D3DM_SUPPORT_DXR", "0");
            }
        }
        Graphics::Dxmt => {
            let dxmt = runtime.join("lib/dxmt");
            if dxmt.is_dir() {
                command.env("WINEDLLPATH_PREPEND", dxmt.to_string_lossy().as_ref());
            }
            command.env("CX_ACTIVE_GRAPHICS_BACKEND", "dxmt");
        }
    }

    // User-configured environment overrides
    for (key, val) in &config.environment {
        command.env(key, val);
    }

    command.stdin(Stdio::null());
    command.stdout(log_file.try_clone()?);
    command.stderr(log_file);

    let child = command.spawn().map_err(|_| CustomError::StartFailed)?;
    Ok(child.id())
}

/// Gracefully stops a running custom executable process.
pub fn stop_custom(
    pid: u32,
    prefix: &Path,
    runner: Option<&Path>,
    locations: &Locations,
) -> Result<(), String> {
    let wineserver_bin = if let Some(runner) = runner {
        runner.parent().map(|p| p.join("wineserver"))
    } else {
        voile::runtime(&locations.voile).map(|r| r.join("bin/wineserver"))
    };

    if let Some(wineserver) = wineserver_bin.filter(|p| p.is_file()) {
        let _ = Command::new(wineserver)
            .arg("-k")
            .env("WINEPREFIX", prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    if pid > 0 {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    Ok(())
}

/// Force quits a running custom executable process immediately.
pub fn force_quit_custom(
    pid: u32,
    prefix: &Path,
    runner: Option<&Path>,
    locations: &Locations,
) -> Result<(), String> {
    let wineserver_bin = if let Some(runner) = runner {
        runner.parent().map(|p| p.join("wineserver"))
    } else {
        voile::runtime(&locations.voile).map(|r| r.join("bin/wineserver"))
    };

    if let Some(wineserver) = wineserver_bin.filter(|p| p.is_file()) {
        let _ = Command::new(wineserver)
            .arg("-k")
            .env("WINEPREFIX", prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    if pid > 0 {
        let _ = Command::new("/bin/kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    Ok(())
}

/// Checks whether a process with `pid` is currently running.
pub fn is_custom_process_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_reported_as_running() {
        let current_pid = std::process::id();
        assert!(is_custom_process_running(current_pid));
    }

    #[test]
    fn invalid_pid_is_reported_as_not_running() {
        assert!(!is_custom_process_running(0));
        assert!(!is_custom_process_running(99999999));
    }

    #[test]
    fn non_existent_executable_fails_validation() {
        let config = CustomLaunchConfig {
            executable: PathBuf::from("/non/existent/game.exe"),
            arguments: vec![],
            prefix: PathBuf::from("/tmp/test-prefix"),
            runner: None,
            engine: Engine::Rosetta,
            graphics: Graphics::D3DMetal,
            ray_tracing: false,
            performance_hud: false,
            render_percent: 100,
            environment: HashMap::new(),
        };
        let locations = Locations::new(Path::new("/tmp"), Path::new("/tmp"), Path::new("/tmp"));
        let result = launch_custom(&config, &locations);
        assert_eq!(result, Err(CustomError::NotExecutable));
    }
}
