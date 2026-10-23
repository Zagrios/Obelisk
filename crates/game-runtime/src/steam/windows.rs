//! A Windows Steam game: Wine starts it behind Proton's steam.exe helper,
//! which stands in for Steam, once its prefix ran the steps of the install
//! scripts the installation lists.
use crate::{Error, supervisor::Target, wine};
use obelisk_core::launch::LaunchTarget;
use std::{fs, io::Read, path::Path};

/// The helper every Windows game starts behind. Wine installs it in each prefix.
const HELPER: &str = r"C:\windows\system32\steam.exe";
/// Valve's Windows libraries that steam_api loads through the ActiveProcess
/// registry keys. The runtime's ntdll redirects their exports to lsteamclient.
const CLIENTS: [&str; 2] = ["steamclient.dll", "steamclient64.dll"];
/// What the game's steam_api and lsteamclient read, which its session sets
/// as it starts the game (see `launch::game_environment` and the host's
/// ports). The Linux engine's guest receives them from its launcher.
const GAME_ENVIRONMENT: &[&str] = &[
    "SteamAppId",
    "SteamGameId",
    "SteamOS",
    "Steam3Master",
    "SteamClientService",
];
/// The largest install script read.
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
/// How much of each library is compared at a time.
const BLOCK: u64 = 64 * 1024;

/// What Steam adds to the prefix of the installation in `install`.
pub(super) struct Prefix<'a> {
    pub install: &'a Path,
}

impl wine::Storefront for Prefix<'_> {
    const HELPER: Option<&'static str> = Some(HELPER);
    const PASSED: &'static [&'static str] = GAME_ENVIRONMENT;
    const STEAM_CLIENT: bool = true;

    /// The steps of the install scripts the installation lists, which Steam
    /// runs before a game first starts. A script that cannot be read is left
    /// out.
    fn prerequisites(&self) -> Result<Vec<wine::Prerequisite>, Error> {
        install_scripts(self.install)
    }

    /// Places Valve's steamclient libraries where the prefix's registry
    /// points steam_api.
    fn furnish(&self, config: &wine::Config) -> Result<(), Error> {
        let steam = config.prefix.join("drive_c/Program Files (x86)/Steam");
        fs::create_dir_all(&steam)?;
        for name in CLIENTS {
            let source = config.runtime.join("steam").join(name);
            let destination = steam.join(name);
            if !destination.is_file() || !same_contents(&destination, &source)? {
                fs::copy(&source, &destination)?;
            }
        }
        Ok(())
    }
}

/// The steps of the install scripts the installation in `install` lists,
/// which Steam runs before a game first starts. A script that cannot be read
/// is left out.
pub(super) fn install_scripts(install: &Path) -> Result<Vec<wine::Prerequisite>, Error> {
    let read = |relative: &Path| {
        obelisk_core::install::read_cached_file(install, relative, MAX_SCRIPT_BYTES)
            .map_err(|_| Error::Storage)
    };
    let Some(list) = read(Path::new(steam_kit::install_script::LIST))? else {
        return Ok(Vec::new());
    };
    let windows_install = wine::windows_path(install);
    let mut prerequisites = Vec::new();
    for script in String::from_utf8_lossy(&list).lines() {
        let Ok(Some(bytes)) = read(Path::new(script)) else {
            continue;
        };
        let Ok(steps) = steam_kit::install_script::steps(&bytes, install, &windows_install) else {
            continue;
        };
        prerequisites.extend(steps.into_iter().map(|step| {
            wine::Prerequisite {
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

/// Whether the files at `a` and `b` hold the same bytes. Reading stops at
/// their first difference.
fn same_contents(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (mut a, mut b) = (fs::File::open(a)?, fs::File::open(b)?);
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    loop {
        left.clear();
        right.clear();
        let read = (&mut a).take(BLOCK).read_to_end(&mut left)?;
        (&mut b).take(BLOCK).read_to_end(&mut right)?;
        if left != right {
            return Ok(false);
        }
        if read == 0 {
            return Ok(true);
        }
    }
}

/// The Windows game `target` names, started by steam.exe. A game that ships
/// no Steam API starts all the same, and its Game session keeps the host,
/// Cloud and quitting.
pub(super) fn target(
    config: &wine::Config,
    game: &wine::Game<'_>,
    target: &LaunchTarget,
    home: &Path,
) -> Result<Target, Error> {
    wine::target::<Prefix<'_>>(config, game, target, crate::native::environment(home))
}

#[cfg(test)]
mod tests;
