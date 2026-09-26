//! A Windows game runs in SailOver's Wine runtime, inside the prefix of its
//! installation, behind Proton's steam.exe helper that stands in for Steam.
use crate::{Error, supervisor::Target};
use sailover_core::launch::LaunchTarget;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// The user every prefix of CrossOver's Wine names its profile after.
const USER: &str = "crossover";
/// The helper every Windows game starts behind. Wine installs it in each prefix.
const STEAM_HELPER: &str = r"C:\windows\system32\steam.exe";
/// Valve's Windows libraries that steam_api loads through the ActiveProcess
/// registry keys. The runtime's ntdll redirects their exports to lsteamclient.
const STEAM_CLIENTS: [&str; 2] = ["steamclient.dll", "steamclient64.dll"];
/// The prerequisites a prefix ran, one id per line.
const PREREQUISITES: &str = "sailover-prerequisites";
/// The Wine Mono whose .NET registration the prefix holds.
const MONO: &str = "sailover-mono";
/// Their output, rewritten by each launch that runs one.
const PREREQUISITES_LOG: &str = "sailover-prerequisites.log";
/// An installer still running after this long is stopped.
const PREREQUISITE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Success, "a newer version is already installed" and "restart required".
const INSTALLED: [i32; 3] = [0, 1638, 3010];
/// The largest install script read.
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;

/// The Windows user profile inside `prefix`, where Auto-Cloud roots such as
/// AppData and Documents live.
pub fn profile(prefix: &Path) -> PathBuf {
    prefix.join("drive_c/users").join(USER)
}

/// The Wine runtime and the prefix of one Windows installation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub runtime: PathBuf,
    pub prefix: PathBuf,
}

impl Config {
    fn wine(&self) -> PathBuf {
        self.runtime.join("bin/wine")
    }

    /// Whether the runtime is in place.
    pub fn installed(&self) -> bool {
        self.wine().is_file()
    }

    /// Released runtimes carry D3DMetal. When one does, CrossOver's Wine
    /// preloads libd3dshared for D3DMetal's DLLs.
    fn d3dmetal(&self) -> Option<PathBuf> {
        let library = self.runtime.join("lib/external/libd3dshared.dylib");
        library.is_file().then_some(library)
    }

    /// The Wine Mono the runtime carries, such as `wine-mono-10.4.1`.
    fn mono(&self) -> Option<String> {
        fs::read_dir(self.runtime.join("share/wine/mono"))
            .ok()?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("wine-mono-"))
            .max()
    }

    /// No Gecko installer prompt and no menu entries. .NET games run on the
    /// Wine Mono a runtime carries; without it, mscoree stays off so that no
    /// Mono installer prompt appears either.
    fn dll_overrides(&self) -> &'static str {
        if self.mono().is_some() {
            "mshtml=;winemenubuilder.exe=d"
        } else {
            "mscoree,mshtml=;winemenubuilder.exe=d"
        }
    }

    /// Wine's errors, unless a development runtime's `winedebug` file names
    /// other channels, such as `+steamclient`.
    fn debug_channels(&self) -> String {
        fs::read_to_string(self.runtime.join("winedebug"))
            .ok()
            .and_then(|channels| channels.lines().next().map(str::to_owned))
            .filter(|channels| !channels.is_empty())
            .unwrap_or_else(|| "err+all,fixme-all".into())
    }

    fn environment(&self) -> HashMap<String, String> {
        let mut environment = HashMap::from([
            (
                "WINEPREFIX".to_owned(),
                self.prefix.to_string_lossy().into_owned(),
            ),
            (
                "WINEDLLOVERRIDES".to_owned(),
                self.dll_overrides().to_owned(),
            ),
            ("WINEDEBUG".to_owned(), self.debug_channels()),
            // Events, mutexes and semaphores become Mach semaphores instead of
            // wineserver requests. The server and every process of the prefix
            // must agree on it, so every Wine command sets it.
            ("WINEMSYNC".to_owned(), "1".to_owned()),
            // 32-bit games get 4 GB of address space, as on 64-bit Windows,
            // even when their executable does not ask for it.
            ("WINE_LARGE_ADDRESS_AWARE".to_owned(), "1".to_owned()),
        ]);
        if let Some(library) = self.d3dmetal() {
            environment.insert(
                "CX_APPLEGPTK_LIBD3DSHARED_PATH".into(),
                library.to_string_lossy().into_owned(),
            );
            environment.insert("CX_ACTIVE_GRAPHICS_BACKEND".into(), "d3dmetal".into());
        }
        environment
    }
}

/// The Windows game `target` names inside `install`, which Wine starts in the
/// prefix with `environment` and Wine's own settings.
pub fn target(
    install: &Path,
    target: &LaunchTarget,
    environment: HashMap<String, String>,
    config: &Config,
) -> Result<Target, Error> {
    let root = install.canonicalize()?;
    let executable = Path::new(&target.executable)
        .canonicalize()
        .map_err(|_| Error::NotExecutable)?;
    if !executable.starts_with(&root) || !portable_executable(&executable)? {
        return Err(Error::NotExecutable);
    }
    let mut arguments = vec![executable.to_string_lossy().into_owned()];
    arguments.extend(target.arguments.iter().cloned());
    let mut environment = environment;
    environment.extend(config.environment());
    Ok(Target {
        executable: config.wine(),
        arguments,
        working_directory: target.working_directory.clone().into(),
        environment,
        log: None,
    })
}

/// The Windows game `target` names inside `install`, started by steam.exe,
/// and the Steam API library that shows the game uses Steamworks.
pub fn steam_target(
    install: &Path,
    target: &LaunchTarget,
    home: &Path,
    config: &Config,
) -> Result<(Target, PathBuf), Error> {
    let mut game = self::target(install, target, crate::native::environment(home), config)?;
    let steam_api = crate::steam::launch::library(target, &["steam_api64.dll", "steam_api.dll"])?;
    game.arguments.insert(0, STEAM_HELPER.to_owned());
    Ok((game, steam_api))
}

/// Creates the prefix on first use, registers the runtime's Wine Mono in it,
/// and keeps its user folders apart from the Mac home.
pub fn prepare(config: &Config, home: &Path) -> Result<(), Error> {
    if !config.installed() {
        return Err(Error::WineUnavailable);
    }
    crate::files::private_directory(&config.prefix)?;
    let created = !config.prefix.join("system.reg").is_file();
    if created {
        wineboot(config, home, "--init")?;
    }
    // wineboot installs Mono's .NET registration when it creates a prefix.
    // A prefix created by a runtime without Mono lacks it: updating the
    // prefix once adds it.
    if let Some(mono) = config.mono() {
        let record = config.prefix.join(MONO);
        if fs::read_to_string(&record).ok().as_deref() != Some(mono.as_str()) {
            if !created {
                wineboot(config, home, "--update")?;
            }
            fs::write(&record, &mono)?;
        }
    }
    unlink_home_folders(&profile(&config.prefix))
}

/// Runs wineboot in the prefix and waits until its server stops writing the
/// registry, which the game must not start before.
fn wineboot(config: &Config, home: &Path, argument: &str) -> Result<(), Error> {
    let status = Command::new(config.wine())
        .args(["wineboot", argument])
        .env_clear()
        .envs(crate::native::environment(home))
        .envs(config.environment())
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

/// Places Valve's steamclient libraries where the prefix's registry points
/// steam_api.
pub fn place_steam_clients(config: &Config) -> Result<(), Error> {
    let steam = config.prefix.join("drive_c/Program Files (x86)/Steam");
    fs::create_dir_all(&steam)?;
    for name in STEAM_CLIENTS {
        let source = config.runtime.join("steam").join(name);
        let destination = steam.join(name);
        if !destination.is_file() || crate::sha(&destination)? != crate::sha(&source)? {
            fs::copy(&source, &destination)?;
        }
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

/// A program run once in a prefix before its game starts, such as the Visual C++
/// runtime installer. Storefronts name them: Steam in install scripts, Epic in
/// its manifests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prerequisite {
    /// A prefix runs each id once.
    pub id: String,
    /// Run in order, each with its Windows command line.
    pub programs: Vec<(PathBuf, String)>,
}

/// Where `path` is inside every prefix: Wine's Z: drive is the Mac's root.
pub fn windows_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

/// The steps of the install scripts a Steam installation lists, which Steam
/// runs before a game first starts. A script that cannot be read is left out.
pub fn steam_prerequisites(install: &Path) -> Result<Vec<Prerequisite>, Error> {
    let read = |relative: &Path| {
        sailover_core::install::read_cached_file(install, relative, MAX_SCRIPT_BYTES)
            .map_err(|_| Error::Storage)
    };
    let Some(list) = read(Path::new(steam_kit::install_script::LIST))? else {
        return Ok(Vec::new());
    };
    let windows_install = windows_path(install);
    let mut prerequisites = Vec::new();
    for script in String::from_utf8_lossy(&list).lines() {
        let Ok(Some(bytes)) = read(Path::new(script)) else {
            continue;
        };
        let Ok(steps) = steam_kit::install_script::steps(&bytes, install, &windows_install) else {
            continue;
        };
        prerequisites.extend(steps.into_iter().map(|step| {
            Prerequisite {
                id: step.id,
                programs: step
                    .programs
                    .into_iter()
                    .map(|program| (program.path, program.arguments))
                    .collect(),
            }
        }));
    }
    Ok(prerequisites)
}

/// Installers that only lay down the Visual C++ runtime, which games get from
/// Wine instead, as they do the Visual C++ packages Steam lists. Unreal's
/// prerequisites installer is mostly that runtime.
fn visual_cpp_installer(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["vcredist", "vc_redist", "ueprereqsetup"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// The program an Epic installation names to run before its game first
/// starts, unless it only installs the Visual C++ runtime or leaves the
/// installation.
pub fn epic_prerequisites(
    install: &Path,
    prerequisite: Option<&epic_kit::client::Prerequisite>,
) -> Vec<Prerequisite> {
    let Some(prerequisite) = prerequisite else {
        return Vec::new();
    };
    let relative = prerequisite.path.replace('\\', "/");
    let relative = Path::new(&relative);
    let inside = relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    let installer = relative
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(visual_cpp_installer);
    if !inside || installer {
        return Vec::new();
    }
    let id = if prerequisite.ids.is_empty() {
        relative.to_string_lossy().into_owned()
    } else {
        prerequisite.ids.join(",")
    };
    vec![Prerequisite {
        id: format!("epic:{id}"),
        programs: vec![(install.join(relative), prerequisite.arguments.clone())],
    }]
}

/// Runs, in order, the prerequisites this prefix has not run yet. One that
/// fails or outlasts its timeout runs again on the next launch; the game still
/// starts, and may tell what it misses.
pub fn install_prerequisites(
    config: &Config,
    home: &Path,
    prerequisites: &[Prerequisite],
) -> Result<(), Error> {
    let record = config.prefix.join(PREREQUISITES);
    let mut ran: BTreeSet<String> = match fs::read_to_string(&record) {
        Ok(text) => text.lines().map(str::to_owned).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => return Err(error.into()),
    };
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
            let text: Vec<_> = ran.iter().map(String::as_str).collect();
            let temporary = record.with_extension("partial");
            fs::write(&temporary, text.join("\n"))?;
            fs::rename(&temporary, &record)?;
        }
    }
    // The game must not start while an installer's processes still run.
    wineserver(config, "-w")?;
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
        let mut command = Command::new(config.wine());
        // Wine starts scripts only through cmd, which reads Windows paths.
        let script = program
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat")
            });
        if script {
            command.args(["cmd", "/c"]).arg(windows_path(&program));
        } else {
            command.arg(&program);
        }
        command
            .args(command_line(arguments))
            .current_dir(program.parent().unwrap_or(&config.prefix))
            .env_clear()
            .envs(crate::native::environment(home))
            .envs(config.environment())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        let code = wait(command.spawn()?, PREREQUISITE_TIMEOUT)?;
        if code.is_none() {
            wineserver(config, "-k")?;
        }
        if !code.is_some_and(|code| INSTALLED.contains(&code)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The exit code of `child`, or `None` once it ran for `timeout` and was killed.
fn wait(mut child: Child, timeout: Duration) -> Result<Option<i32>, Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.code().unwrap_or(-1)));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wineserver(config: &Config, argument: &str) -> Result<(), Error> {
    Command::new(config.runtime.join("bin/wineserver"))
        .arg(argument)
        .env_clear()
        .env("WINEPREFIX", &config.prefix)
        .status()?;
    Ok(())
}

/// Splits a Windows command line on the spaces outside double quotes.
fn command_line(line: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(character);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// Whether `path` starts like a Windows program: a DOS header naming a PE header.
fn portable_executable(path: &Path) -> Result<bool, Error> {
    let mut file = fs::File::open(path)?;
    let mut dos = [0u8; 64];
    if file.read_exact(&mut dos).is_err() || &dos[..2] != b"MZ" {
        return Ok(false);
    }
    let offset = u32::from_le_bytes(dos[60..64].try_into().expect("four bytes")) as u64;
    let mut signature = [0u8; 4];
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    Ok(file.read_exact(&mut signature).is_ok() && &signature == b"PE\0\0")
}

#[cfg(test)]
pub(crate) mod tests;
