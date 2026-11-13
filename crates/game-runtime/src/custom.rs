//! Launching and process supervision of custom/non-store Windows executables in Obelisk.

use crate::{
    Locations, pe, voile,
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

/// Downloads and installs the default Voile Wine engine (CrossOver + Apple GPTK 4).
pub async fn download_default_wine_engine(locations: &Locations) -> Result<String, CustomError> {
    let install = voile::Install::new();
    let runtime = install
        .run(&locations.voile, None)
        .await
        .map_err(|e| CustomError::Failed(e.to_string()))?;
    let release = voile::release_of(&runtime).unwrap_or_else(|| "default".to_string());
    Ok(release)
}

/// Checks whether the default Wine runtime (Voile / CrossOver / GPTK) is installed.
pub fn is_default_wine_installed(locations: &Locations) -> bool {
    voile::runtime(&locations.voile).is_some()
}

/// Inspects binary headers to determine if an executable is an ELF Linux binary or Linux script.
pub fn is_linux_binary(executable: &Path) -> bool {
    let mut file = match fs::File::open(executable) {
        Ok(f) => f,
        Err(_) => return false,
    };
    use std::io::Read;
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    magic == [0x7f, b'E', b'L', b'F'] || &magic[0..2] == b"#!"
}

/// Recommends the optimal launch engine: Linux engine for ELF binaries, Rosetta for Windows PE binaries.
pub fn recommended_engine(executable: &Path) -> Engine {
    if is_linux_binary(executable) {
        Engine::Linux
    } else {
        Engine::Rosetta
    }
}

/// Inspects the PE headers of the target executable and recommends the optimal graphics backend.
/// DirectX 12 games receive D3DMetal (GPTK 4), while DirectX 11/10/9 titles and Linux binaries receive DXMT.
pub fn recommended_graphics(executable: &Path) -> Graphics {
    if is_linux_binary(executable) {
        return Graphics::Dxmt;
    }
    let program = pe::Program::open(executable);
    Graphics::default_for(Engine::Rosetta, program.as_ref())
}

/// Launches a custom Windows or Linux executable using Wine, voile-lkd, or a custom runner.
pub fn launch_custom(
    config: &CustomLaunchConfig,
    locations: &Locations,
) -> Result<u32, CustomError> {
    if !config.executable.is_file() {
        return Err(CustomError::NotExecutable);
    }

    // Launch via Voile Linux Engine (voile-lkd) if selected
    if config.engine == Engine::Linux && config.runner.is_none() {
        let linux_runtime = voile::linux_runtime(&locations.voile)
            .ok_or(CustomError::WineUnavailable)?;

        let working_dir = config.executable.parent().unwrap_or(&config.prefix);
        let install_dir = working_dir.canonicalize().unwrap_or_else(|_| working_dir.to_path_buf());

        // Ensure prefix and home directories exist
        fs::create_dir_all(&config.prefix)?;
        let home_dir = config.prefix.join("home");
        fs::create_dir_all(&home_dir)?;

        // Set up logs in <library>/Logs/Custom/<game_stem>.log
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

        let file_name = config
            .executable
            .file_name()
            .unwrap_or(config.executable.as_os_str())
            .to_string_lossy();
        let guest_exe = format!("/opt/game/{file_name}");

        let mut arguments = vec![
            "-c".to_owned(),
            crate::wine::linux_engine::LAUNCHER.to_owned(),
            "obelisk-linux-engine".to_owned(),
            linux_runtime.to_string_lossy().into_owned(),
            home_dir.to_string_lossy().into_owned(),
            install_dir.to_string_lossy().into_owned(),
            "/opt/game".to_owned(),
            "--graphics".to_owned(),
            config.graphics.name().to_owned(),
        ];

        if config.render_percent != 100 {
            arguments.push("--render-percent".to_owned());
            arguments.push(config.render_percent.to_string());
        }

        for (k, v) in &config.environment {
            arguments.push("--env".to_owned());
            arguments.push(format!("{k}={v}"));
        }

        arguments.push("--".to_owned());
        arguments.push(guest_exe);
        for arg in &config.arguments {
            arguments.push(arg.clone());
        }

        let mut command = Command::new("/bin/bash");
        command.args(&arguments);
        command.current_dir(&install_dir);
        command.env_clear();
        command.envs(crate::native::environment(&locations.home));
        command.env(
            crate::wine::linux_engine::APP_NAME,
            config.executable.file_stem().unwrap_or_default().to_string_lossy().as_ref(),
        );
        command.stdin(Stdio::null());
        command.stdout(log_file.try_clone()?);
        command.stderr(log_file);

        let child = command.spawn().map_err(|_| CustomError::StartFailed)?;
        return Ok(child.id());
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

    // Runner binary
    let runner_binary = if let Some(ref runner) = config.runner {
        runner.clone()
    } else {
        runtime.join("bin/wine")
    };

    let working_dir = config.executable.parent().unwrap_or(&config.prefix);

    // Boot prefix if it has not been initialized yet (system.reg does not exist)
    let system_reg = config.prefix.join("system.reg");
    if config.runner.is_none() && !system_reg.is_file() {
        let mut boot = Command::new(&runner_binary);
        boot.args(["wineboot", "--init"])
            .current_dir(working_dir)
            .env_clear()
            .envs(crate::native::environment(&locations.home))
            .env("WINEPREFIX", &config.prefix)
            .env("WINEDLLOVERRIDES", "mscoree,mshtml=;winemenubuilder.exe=d")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = boot.status();

        // Wait for wineserver to settle registry updates
        let wineserver_bin = runtime.join("bin/wineserver");
        if wineserver_bin.is_file() {
            let _ = Command::new(&wineserver_bin)
                .arg("-w")
                .env("WINEPREFIX", &config.prefix)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

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

    let mut command = Command::new(&runner_binary);
    command.current_dir(working_dir);

    // Pass the executable as ./<name> inside working_dir, followed by arguments
    let file_name = config
        .executable
        .file_name()
        .unwrap_or(config.executable.as_os_str());
    command.arg(format!("./{}", file_name.to_string_lossy()));
    for arg in &config.arguments {
        command.arg(arg);
    }

    // Prepare clean environment matching Obelisk runtime conventions
    command.env_clear();
    command.envs(crate::native::environment(&locations.home));
    command.env("WINEPREFIX", &config.prefix);
    command.env("WINEDEBUG", "err+all,fixme-all");
    command.env("WINEMSYNC", "1");
    command.env("WINE_LARGE_ADDRESS_AWARE", "1");
    command.env("ROSETTA_ADVERTISE_AVX", "1");
    command.env("DOTNET_EnableWriteXorExecute", "0");

    // Standard native DLL overrides for standalone / cracked / non-store games
    // so native proxy DLLs (winmm, version, d3d11, dxgi, etc.) in the game folder load first.
    command.env(
        "WINEDLLOVERRIDES",
        "winmm,version,d3d11,dxgi,msvcp140,vcruntime140,vcruntime140_1=n,b;mshtml=;winemenubuilder.exe=d",
    );

    // GStreamer multimedia plugins
    let gstreamer = runtime.join("lib/gstreamer-1.0");
    if gstreamer.is_dir() {
        command.env("GST_PLUGIN_SYSTEM_PATH", &gstreamer);
        command.env(
            "GST_REGISTRY",
            config.prefix.join("obelisk-gstreamer-registry.bin"),
        );
    }

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
            let folders = [runtime.join("lib/dxmt"), runtime.join("lib/d3dmetal")];
            let search = folders
                .iter()
                .filter(|f| f.is_dir())
                .map(|f| f.to_string_lossy())
                .collect::<Vec<_>>()
                .join(":");
            if !search.is_empty() {
                command.env("WINEDLLPATH_PREPEND", search);
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
