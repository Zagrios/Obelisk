//! A Windows game runs in Forge's Wine runtime, inside the prefix of its
//! installation, behind Proton's steam.exe helper that stands in for Steam.
use crate::{Error, supervisor::Target};
use forge_core::launch::LaunchTarget;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The user every prefix of CrossOver's Wine names its profile after.
const USER: &str = "crossover";
/// The helper every Windows game starts behind. Wine installs it in each prefix.
const STEAM_HELPER: &str = r"C:\windows\system32\steam.exe";
/// Valve's Windows libraries that steam_api loads through the ActiveProcess
/// registry keys. The runtime's ntdll redirects their exports to lsteamclient.
const STEAM_CLIENTS: [&str; 2] = ["steamclient.dll", "steamclient64.dll"];

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
            // No Mono or Gecko installer prompt, and no menu entries.
            (
                "WINEDLLOVERRIDES".to_owned(),
                "mscoree,mshtml=;winemenubuilder.exe=d".to_owned(),
            ),
            ("WINEDEBUG".to_owned(), self.debug_channels()),
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

/// The Windows game `target` names inside `install`, started by steam.exe,
/// and the Steam API library that shows the game uses Steamworks.
pub fn steam_target(
    install: &Path,
    target: &LaunchTarget,
    home: &Path,
    config: &Config,
) -> Result<(Target, PathBuf), Error> {
    let root = install.canonicalize()?;
    let executable = Path::new(&target.executable)
        .canonicalize()
        .map_err(|_| Error::NotExecutable)?;
    if !executable.starts_with(&root) || !portable_executable(&executable)? {
        return Err(Error::NotExecutable);
    }
    let steam_api = crate::steam::launch::library(target, &["steam_api64.dll", "steam_api.dll"])?;
    let mut arguments = vec![
        STEAM_HELPER.to_owned(),
        executable.to_string_lossy().into_owned(),
    ];
    arguments.extend(target.arguments.iter().cloned());
    let mut environment = crate::native::environment(home);
    environment.extend(config.environment());
    Ok((
        Target {
            executable: config.wine(),
            arguments,
            working_directory: target.working_directory.clone().into(),
            environment,
            log: None,
        },
        steam_api,
    ))
}

/// Creates the prefix on first use, keeps its user folders apart from the Mac
/// home, then places Valve's steamclient libraries where its registry points
/// steam_api.
pub fn prepare(config: &Config, home: &Path) -> Result<(), Error> {
    if !config.installed() {
        return Err(Error::Unavailable);
    }
    crate::files::private_directory(&config.prefix)?;
    if !config.prefix.join("system.reg").is_file() {
        let status = Command::new(config.wine())
            .args(["wineboot", "--init"])
            .env_clear()
            .envs(crate::native::environment(home))
            .envs(config.environment())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        // The game must not start while wineboot's server still writes the registry.
        let server = Command::new(config.runtime.join("bin/wineserver"))
            .arg("-w")
            .env_clear()
            .env("WINEPREFIX", &config.prefix)
            .status()?;
        if !status.success() || !server.success() {
            return Err(Error::StartFailed);
        }
    }
    unlink_home_folders(&profile(&config.prefix))?;
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
mod tests;
