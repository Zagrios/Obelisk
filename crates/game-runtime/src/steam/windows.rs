//! A Windows Steam game: Wine starts it behind Proton's steam.exe helper,
//! which stands in for Steam, once its prefix ran the steps of the install
//! scripts the installation lists.
use crate::{Error, supervisor::Target, wine};
use sailover_core::launch::LaunchTarget;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// The helper every Windows game starts behind. Wine installs it in each prefix.
const HELPER: &str = r"C:\windows\system32\steam.exe";
/// Valve's Windows libraries that steam_api loads through the ActiveProcess
/// registry keys. The runtime's ntdll redirects their exports to lsteamclient.
const CLIENTS: [&str; 2] = ["steamclient.dll", "steamclient64.dll"];
/// The largest install script read.
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;

/// What Steam adds to the prefix of the installation in `install`.
pub(super) struct Prefix<'a> {
    pub install: &'a Path,
}

impl wine::Storefront for Prefix<'_> {
    /// The steps of the install scripts the installation lists, which Steam
    /// runs before a game first starts. A script that cannot be read is left
    /// out.
    fn prerequisites(&self) -> Result<Vec<wine::Prerequisite>, Error> {
        let read = |relative: &Path| {
            sailover_core::install::read_cached_file(self.install, relative, MAX_SCRIPT_BYTES)
                .map_err(|_| Error::Storage)
        };
        let Some(list) = read(Path::new(steam_kit::install_script::LIST))? else {
            return Ok(Vec::new());
        };
        let windows_install = wine::windows_path(self.install);
        let mut prerequisites = Vec::new();
        for script in String::from_utf8_lossy(&list).lines() {
            let Ok(Some(bytes)) = read(Path::new(script)) else {
                continue;
            };
            let Ok(steps) =
                steam_kit::install_script::steps(&bytes, self.install, &windows_install)
            else {
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

    /// Places Valve's steamclient libraries where the prefix's registry
    /// points steam_api.
    fn furnish(&self, config: &wine::Config) -> Result<(), Error> {
        let steam = config.prefix.join("drive_c/Program Files (x86)/Steam");
        fs::create_dir_all(&steam)?;
        for name in CLIENTS {
            let source = config.runtime.join("steam").join(name);
            let destination = steam.join(name);
            if !destination.is_file() || crate::sha(&destination)? != crate::sha(&source)? {
                fs::copy(&source, &destination)?;
            }
        }
        Ok(())
    }
}

/// The Windows game `target` names inside `install`, started by steam.exe,
/// and the Steam API library that shows the game uses Steamworks.
pub(super) fn target(
    install: &Path,
    target: &LaunchTarget,
    home: &Path,
    config: &wine::Config,
) -> Result<(Target, PathBuf), Error> {
    let mut game = wine::target(install, target, crate::native::environment(home), config)?;
    let steam_api = super::launch::library(target, &["steam_api64.dll", "steam_api.dll"])?;
    game.arguments.insert(0, HELPER.to_owned());
    Ok((game, steam_api))
}

#[cfg(test)]
mod tests;
