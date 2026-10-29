//! Voile's Wine for macOS, which Rosetta runs: the engine of every Windows
//! game whose Launch settings name no other.
use super::{Adapter, Additions, Config, Facts, Furnisher, Game, Graphics, Prerequisite, Programs};
use crate::{
    launch::Error,
    launch_settings::SteamClient,
    supervisor::{Target, architecture::Architecture},
};
use obelisk_core::launch::LaunchTarget;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

/// The prerequisites a prefix ran, one id per line.
const PREREQUISITES: &str = "obelisk-prerequisites";
/// The Wine Mono whose .NET registration the prefix holds.
const MONO: &str = "obelisk-mono";
/// The runtime's graphics backends whose DLLs the prefix holds placeholders
/// for, by folder name.
const BACKENDS: &str = "obelisk-graphics";
/// D3DMetal's DLLs in a runtime, beside Wine's own.
const D3DMETAL_DLLS: &str = "lib/d3dmetal";
/// DXMT's DLLs in a runtime, beside Wine's own.
const DXMT_DLLS: &str = "lib/dxmt";
/// D3DMetal's library, which its unix libraries load.
const D3DMETAL_FRAMEWORK: &str = "lib/external/D3DMetal.framework/D3DMetal";
/// Their output, rewritten by each launch that runs one.
const PREREQUISITES_LOG: &str = "obelisk-prerequisites.log";
/// The registry GStreamer keeps of the runtime's plugins.
const GSTREAMER_REGISTRY: &str = "obelisk-gstreamer-registry.bin";
/// An installer still running after this long is stopped.
const PREREQUISITE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Success, "a newer version is already installed" and "restart required".
const INSTALLED: [i32; 3] = [0, 1638, 3010];

/// Wine for macOS, under Rosetta.
pub(super) struct RosettaWine;

static FACTS: Facts = Facts {
    name: "wine-macos",
    // Wine is an Intel program: Rosetta runs it and the Windows game inside it.
    architecture: Architecture::Intel,
    // It ships with the Linux engine's host programs, in every runtime.
    input_service: Some("linux/engine/obelisk-input"),
    graphics: Graphics::D3DMetal,
    launcher_grace: None,
    reserved: super::Reserved {
        variables: &[
            "WINEPREFIX",
            "WINEDLLOVERRIDES",
            "WINEDEBUG",
            "WINEMSYNC",
            "WINE_LARGE_ADDRESS_AWARE",
            "ROSETTA_ADVERTISE_AVX",
            "DOTNET_EnableWriteXorExecute",
            "GST_PLUGIN_SYSTEM_PATH",
            "GST_REGISTRY",
            "CX_APPLEGPTK_LIBD3DSHARED_PATH",
            "D3DMETAL_FRAMEWORK_PATH",
            "WINEDLLPATH_PREPEND",
            "CX_ACTIVE_GRAPHICS_BACKEND",
            "D3DM_SUPPORT_DXR",
            "CX_HIDE_BUILTIN_VERSION_RESOURCES",
            "PROTON_DISABLE_LSTEAMCLIENT",
            // The Game session adds these to the game's own.
            crate::input::SOCKET,
            crate::metal_hud::ENABLED,
            crate::metal_hud::LOG_ENABLED,
            crate::metal_hud::OPACITY,
            crate::metal_hud::DISABLE_MENU_BAR,
        ],
        dlls: &[
            "mscoree",
            "mshtml",
            "winemenubuilder.exe",
            "gameoverlayrenderer",
            "gameoverlayrenderer64",
            "steam.exe",
        ],
    },
    programs: Programs::All,
};

impl Adapter for RosettaWine {
    fn facts(&self) -> &'static Facts {
        &FACTS
    }

    fn runtime(&self, voile: &Path) -> Option<PathBuf> {
        crate::voile::runtime(voile)
    }

    fn prefix(&self, rosetta: &Path) -> Result<PathBuf, Error> {
        Ok(rosetta.to_owned())
    }

    /// The prefix is created or updated for the runtime, the storefront
    /// furnishes it and its prerequisites run.
    fn ready(
        &self,
        config: &Config,
        locations: &crate::Locations,
        storefront: &dyn Furnisher,
    ) -> Result<(), Error> {
        boot(config, &locations.home)?;
        storefront.furnish_prefix(config)?;
        install_prerequisites(config, &locations.home, &storefront.prerequisites_to_run()?)?;
        match config.log.parent() {
            Some(logs) => Ok(crate::files::private_directory(logs)?),
            None => Err(Error::Storage),
        }
    }

    fn target(
        &self,
        config: &Config,
        game: &Game<'_>,
        launched: &LaunchTarget,
        program: PathBuf,
        additions: Additions,
        environment: HashMap<String, String>,
    ) -> Result<Target, Error> {
        let mut arguments: Vec<String> = additions.helper.map(str::to_owned).into_iter().collect();
        arguments.push(program.to_string_lossy().into_owned());
        arguments.extend(launched.arguments.iter().cloned());
        let mut environment = environment;
        environment.extend(self::environment(config));
        Ok(Target {
            executable: wine(config),
            arguments,
            working_directory: PathBuf::from(&launched.working_directory),
            environment,
            log: Some(config.log.clone()),
            installation: Some(game.install.to_owned()),
            program: Some(program),
        })
    }

    /// Rosetta's Wine reads it on the Mac's disk, through its Z: drive.
    fn private_file(
        &self,
        _config: &Config,
        name: &str,
        bytes: &[u8],
    ) -> Result<(tempfile::TempDir, String), Error> {
        let (directory, path) =
            crate::files::private_temp_file(&std::env::temp_dir(), "obelisk-", name, bytes)?;
        Ok((directory, super::windows_path(&path)))
    }

    /// Taskkill without /F posts WM_CLOSE to the windows of the game's
    /// program and of the processes it started, in the game's prefix and
    /// with its environment, which its Wine server must agree with.
    fn quit_command(
        &self,
        config: &Config,
        environment: &HashMap<String, String>,
        program: Option<&Path>,
        _pid: u32,
    ) -> Option<Command> {
        let mut command = Command::new(wine(config));
        command
            .args(["taskkill", "/T", "/IM"])
            .arg(program?.file_name()?)
            .env_clear()
            .envs(environment);
        Some(command)
    }

    fn end_command(&self, config: &Config) -> Option<Command> {
        let mut command = Command::new(config.runtime.join("bin/wineserver"));
        command
            .arg("-k")
            .env_clear()
            .env("WINEPREFIX", &config.prefix);
        Some(command)
    }

    /// The prefix, and the folder of its Wine server, which Wine names after
    /// the prefix's device and inode.
    fn private_folders(
        &self,
        config: &Config,
        _environment: &HashMap<String, String>,
        _pid: Option<u32>,
    ) -> Vec<PathBuf> {
        use std::os::unix::fs::MetadataExt;
        let mut folders = vec![config.prefix.clone()];
        if let Ok(prefix) = fs::metadata(&config.prefix) {
            folders.push(PathBuf::from(format!(
                "/tmp/.wine-{}/server-{:x}-{:x}",
                prefix.uid(),
                prefix.dev(),
                prefix.ino()
            )));
        }
        folders
    }

    fn remove(
        &self,
        locations: &crate::Locations,
        prefix: &Path,
    ) -> Result<(), obelisk_core::installation::RemovalFailure> {
        // A Wine server lingers a few seconds after its game and would write
        // the registry back into the prefix.
        if prefix.is_dir()
            && let Some(runtime) = self.runtime(&locations.voile)
        {
            let _ = wineserver(&runtime, prefix, "-k");
        }
        super::remove_folder(prefix)
    }
}

pub(super) fn wine(config: &Config) -> PathBuf {
    config.runtime.join("bin/wine")
}

/// Whether the runtime is in place.
fn installed(config: &Config) -> bool {
    wine(config).is_file()
}

/// Released runtimes carry D3DMetal. When one does, CrossOver's Wine
/// preloads libd3dshared for D3DMetal's DLLs.
fn d3dmetal(config: &Config) -> Option<PathBuf> {
    let library = config.runtime.join("lib/external/libd3dshared.dylib");
    library.is_file().then_some(library)
}

/// The runtime's graphics backends, D3DMetal's DLLs then DXMT's, each in
/// its own folder beside Wine's. A runtime from before them has D3DMetal's
/// DLLs over Wine's own, and none.
fn backends(config: &Config) -> Vec<PathBuf> {
    dll_folders(config, &[D3DMETAL_DLLS, DXMT_DLLS])
}

/// The folders Wine searches before its own for the game's DLLs: its
/// backend's, then, behind D3DMetal, DXMT's for the 32-bit programs D3DMetal
/// does not run.
fn graphics_folders(config: &Config) -> Vec<PathBuf> {
    match config.settings.graphics {
        Graphics::D3DMetal => backends(config),
        Graphics::Dxmt => dll_folders(config, &[DXMT_DLLS]),
    }
}

/// Those of the DLL `folders` the runtime carries.
fn dll_folders(config: &Config, folders: &[&str]) -> Vec<PathBuf> {
    folders
        .iter()
        .map(|folder| config.runtime.join(folder))
        .filter(|folder| folder.is_dir())
        .collect()
}

/// The backend CrossOver's Wine is told renders, when the runtime carries
/// it: Wine reports the hardware scheduling D3D12 expects from D3DMetal
/// alone.
fn active_graphics(config: &Config) -> Option<&'static str> {
    let carried = match config.settings.graphics {
        Graphics::D3DMetal => d3dmetal(config).is_some(),
        Graphics::Dxmt => config.runtime.join(DXMT_DLLS).is_dir(),
    };
    carried.then_some(config.settings.graphics.name())
}

/// The GStreamer plugins the runtime carries, which decode games' videos and
/// sounds through Media Foundation, DirectShow and WMReader.
fn gstreamer_plugins(config: &Config) -> Option<PathBuf> {
    let plugins = config.runtime.join("lib/gstreamer-1.0");
    plugins.is_dir().then_some(plugins)
}

/// The Wine Mono the runtime carries, such as `wine-mono-10.4.1`.
fn mono(config: &Config) -> Option<String> {
    fs::read_dir(config.runtime.join("share/wine/mono"))
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("wine-mono-"))
        .max()
}

/// No Gecko installer prompt and no menu entries. .NET games run on the Wine
/// Mono a runtime carries; without it, mscoree stays off so that no Mono
/// installer prompt appears either. The profile's overrides follow.
fn overrides(config: &Config) -> String {
    let own = if mono(config).is_some() {
        "mshtml=;winemenubuilder.exe=d"
    } else {
        "mscoree,mshtml=;winemenubuilder.exe=d"
    };
    // Valve's overlay forces the game's Direct3D device to reset as it hooks
    // it, and TF2's 64-bit build crashes on that reset under Wine.
    let own = match config.settings.steam_client {
        SteamClient::Windows => format!("{own};gameoverlayrenderer,gameoverlayrenderer64="),
        SteamClient::Obelisk => own.to_owned(),
    };
    config.with_profile_overrides(&own)
}

pub(super) fn environment(config: &Config) -> HashMap<String, String> {
    let mut environment = HashMap::from([
        (
            "WINEPREFIX".to_owned(),
            config.prefix.to_string_lossy().into_owned(),
        ),
        ("WINEDLLOVERRIDES".to_owned(), overrides(config)),
        ("WINEDEBUG".to_owned(), config.debug_channels()),
        // Events, mutexes and semaphores become Mach semaphores instead of
        // wineserver requests. The server and every process of the prefix
        // must agree on it, so every Wine command sets it.
        ("WINEMSYNC".to_owned(), "1".to_owned()),
        // 32-bit games get 4 GB of address space, as on 64-bit Windows, even
        // when their executable does not ask for it.
        ("WINE_LARGE_ADDRESS_AWARE".to_owned(), "1".to_owned()),
        // Rosetta translates AVX and AVX2 but hides them from CPUID unless
        // asked, and some games refuse to start without them.
        ("ROSETTA_ADVERTISE_AVX".to_owned(), "1".to_owned()),
        // CrossOver turns off the W^X mode in which .NET 7 and 8 map their
        // JIT code: it breaks those programs under Rosetta.
        ("DOTNET_EnableWriteXorExecute".to_owned(), "0".to_owned()),
    ]);
    // Only the runtime's plugins load, never a GStreamer installed on the Mac
    // or in the home, and the prefix keeps their registry.
    if let Some(plugins) = gstreamer_plugins(config) {
        environment.insert(
            "GST_PLUGIN_SYSTEM_PATH".into(),
            plugins.to_string_lossy().into_owned(),
        );
        environment.insert(
            "GST_REGISTRY".into(),
            config
                .prefix
                .join(GSTREAMER_REGISTRY)
                .to_string_lossy()
                .into_owned(),
        );
    }
    // CrossOver's Wine preloads libd3dshared whatever renders.
    if let Some(library) = d3dmetal(config) {
        environment.insert(
            "CX_APPLEGPTK_LIBD3DSHARED_PATH".into(),
            library.to_string_lossy().into_owned(),
        );
    }
    // Outside Apple's own layout, D3DMetal's unix libraries do not find their
    // framework by themselves.
    let framework = config.runtime.join(D3DMETAL_FRAMEWORK);
    if framework.is_file() {
        environment.insert(
            "D3DMETAL_FRAMEWORK_PATH".into(),
            framework.to_string_lossy().into_owned(),
        );
    }
    if let Some(folders) = search_path(&graphics_folders(config)) {
        environment.insert("WINEDLLPATH_PREPEND".into(), folders);
    }
    if let Some(active) = active_graphics(config) {
        environment.insert("CX_ACTIVE_GRAPHICS_BACKEND".into(), active.into());
    }
    // Direct3D 12 offers DirectX Raytracing unless D3DMetal is told not to.
    if config.settings.graphics == Graphics::D3DMetal && !config.settings.options.ray_tracing {
        environment.insert("D3DM_SUPPORT_DXR".into(), "0".into());
    }
    // Wine's loader hands steamclient's exports to lsteamclient unless told
    // not to: Valve's Windows Steam then runs its own.
    if config.settings.steam_client == SteamClient::Windows {
        environment.insert("PROTON_DISABLE_LSTEAMCLIENT".into(), "1".into());
    }
    // The profile's variables come last, so that they may replace Obelisk's
    // own for their game.
    environment.extend(config.settings.variables.clone());
    environment
}

/// `folders` as one search path, or `None` when there are none.
fn search_path(folders: &[PathBuf]) -> Option<String> {
    let folders: Vec<_> = folders
        .iter()
        .map(|folder| folder.to_string_lossy())
        .collect();
    (!folders.is_empty()).then(|| folders.join(":"))
}

/// Creates the prefix on first use, registers the runtime's Wine Mono in it,
/// and keeps its user folders apart from the Mac home.
fn boot(config: &Config, home: &Path) -> Result<(), Error> {
    if !installed(config) {
        return Err(Error::WineUnavailable);
    }
    crate::files::private_directory(&config.prefix)?;
    let created = !config.prefix.join("system.reg").is_file();
    if created {
        wineboot(config, home, "--init")?;
    }
    // wineboot installs Mono's .NET registration and a placeholder for every
    // DLL of the graphics backends when it creates a prefix: Wine loads a
    // DLL only through its placeholder, such as DXMT's winemetal. A prefix
    // created by a runtime without Mono or without one of the backends lacks
    // them: updating the prefix once adds them.
    let backends = backends(config)
        .iter()
        .filter_map(|folder| folder.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let records = [(MONO, mono(config)), (BACKENDS, Some(backends))];
    let stale: Vec<_> = records
        .into_iter()
        .filter_map(|(record, value)| Some((config.prefix.join(record), value?)))
        .filter(|(record, value)| fs::read_to_string(record).unwrap_or_default() != *value)
        .collect();
    if !stale.is_empty() {
        if !created {
            wineboot(config, home, "--update")?;
        }
        for (record, value) in stale {
            fs::write(record, value)?;
        }
    }
    unlink_home_folders(&super::profile(&config.prefix))
}

/// Runs wineboot in the prefix and waits until its server stops writing the
/// registry, which the game must not start before. Every backend's folder
/// comes first, so that the prefix gets a placeholder for each of their DLLs.
fn wineboot(config: &Config, home: &Path, argument: &str) -> Result<(), Error> {
    let mut environment = environment(config);
    if let Some(folders) = search_path(&backends(config)) {
        environment.insert("WINEDLLPATH_PREPEND".into(), folders);
    }
    // Windows Steam asks to start at login, and wineboot runs what does: the
    // server below would then wait on a Steam that never ends.
    if config.settings.steam_client == SteamClient::Windows {
        let overrides = format!("{};steam.exe=d", overrides(config));
        environment.insert("WINEDLLOVERRIDES".into(), overrides);
    }
    let status = Command::new(wine(config))
        .args(["wineboot", argument])
        .env_clear()
        .envs(crate::native::environment(home))
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    let server = Command::new(config.runtime.join("bin/wineserver"))
        .arg("-w")
        .env_clear()
        .env("WINEPREFIX", &config.prefix)
        .status()?;
    if !status.success() || !server.success() {
        return Err(Error::StartFailed);
    }
    Ok(())
}

/// Games save into the profile's Documents and similar folders, which Cloud
/// syncs and never follows through links. A prefix created before the runtime
/// stopped linking them to the Mac home still holds those links: each becomes
/// an empty folder, and the Mac folder it pointed at stays untouched.
fn unlink_home_folders(profile: &Path) -> Result<(), Error> {
    for (folder, recreate) in [(profile.to_owned(), true), (profile.join("Desktop"), false)] {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                fs::remove_file(entry.path())?;
                if recreate {
                    fs::create_dir(entry.path())?;
                }
            }
        }
    }
    Ok(())
}

/// The prerequisites `prefix` ran, as its record keeps them.
fn ran_prerequisites(prefix: &Path) -> std::io::Result<BTreeSet<String>> {
    match fs::read_to_string(prefix.join(PREREQUISITES)) {
        Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    }
}

/// Records `ran` as the prerequisites `prefix` ran.
fn record_prerequisites(prefix: &Path, ran: &BTreeSet<String>) -> std::io::Result<()> {
    let text: Vec<_> = ran.iter().map(String::as_str).collect();
    obelisk_core::write_atomic(&prefix.join(PREREQUISITES), text.join("\n").as_bytes())
}

/// Forgets the prerequisites `forget` picks among those `prefix` ran.
pub(super) fn forget_prerequisites(
    prefix: &Path,
    forget: impl Fn(&str) -> bool,
) -> std::io::Result<()> {
    let mut ran = ran_prerequisites(prefix)?;
    let before = ran.len();
    ran.retain(|id| !forget(id));
    if ran.len() == before {
        return Ok(());
    }
    record_prerequisites(prefix, &ran)
}

/// Runs, in order, the prerequisites this prefix has not run yet. One that
/// fails or outlasts its timeout runs again on the next launch; the game still
/// starts, and may tell what it misses.
fn install_prerequisites(
    config: &Config,
    home: &Path,
    prerequisites: &[Prerequisite],
) -> Result<(), Error> {
    let mut ran = ran_prerequisites(&config.prefix)?;
    let pending: Vec<_> = prerequisites
        .iter()
        .filter(|prerequisite| !prerequisite.id.contains('\n') && !ran.contains(&prerequisite.id))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    let log = fs::File::create(config.prefix.join(PREREQUISITES_LOG))?;
    for prerequisite in pending {
        if run_prerequisite(config, home, prerequisite, &log)? {
            ran.insert(prerequisite.id.clone());
            record_prerequisites(&config.prefix, &ran)?;
        }
    }
    // The game must not start while an installer's processes still run.
    wineserver(&config.runtime, &config.prefix, "-w")?;
    Ok(())
}

/// Whether every program of `prerequisite` ran and reported success.
fn run_prerequisite(
    config: &Config,
    home: &Path,
    prerequisite: &Prerequisite,
    log: &fs::File,
) -> Result<bool, Error> {
    for (program, arguments) in &prerequisite.programs {
        let Ok(program) = program.canonicalize() else {
            return Ok(false);
        };
        let mut command = Command::new(wine(config));
        // Wine starts scripts only through cmd, which reads Windows paths.
        let script = program
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat")
            });
        if script {
            command
                .args(["cmd", "/c"])
                .arg(super::windows_path(&program));
        } else {
            command.arg(&program);
        }
        let mut environment = environment(config);
        // Wine's msvcp140, msvcp140_2 and vcruntime140_1 claim the version of
        // the newest Visual C++ runtime, so its installer keeps them over
        // Microsoft's. With their versions hidden, it installs Microsoft's,
        // which a game whose profile loads them native first then gets.
        if visual_cpp_installer(&program) {
            environment.insert("CX_HIDE_BUILTIN_VERSION_RESOURCES".into(), "1".into());
        }
        command
            .args(super::command_line(arguments))
            .current_dir(program.parent().unwrap_or(&config.prefix))
            .env_clear()
            .envs(crate::native::environment(home))
            .envs(environment)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        let code = super::wait(command.spawn()?, PREREQUISITE_TIMEOUT)?;
        if code.is_none() {
            wineserver(&config.runtime, &config.prefix, "-k")?;
        }
        if !code.is_some_and(|code| INSTALLED.contains(&code)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `program` installs a Visual C++ runtime, as Microsoft names its
/// installers: vc_redist or, before 2015, vcredist.
fn visual_cpp_installer(program: &Path) -> bool {
    program
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|name| name.starts_with("vc_redist") || name.starts_with("vcredist"))
}

/// Waits for, or stops, the Wine server of `prefix`. Neither starts a server,
/// so no Wine setting is needed.
fn wineserver(runtime: &Path, prefix: &Path, argument: &str) -> Result<(), Error> {
    Command::new(runtime.join("bin/wineserver"))
        .arg(argument)
        .env_clear()
        .env("WINEPREFIX", prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

#[cfg(test)]
mod tests;
