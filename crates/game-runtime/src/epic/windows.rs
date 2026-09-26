//! A Windows Epic build: Wine starts it in its installation's prefix, once the
//! prefix ran the prerequisite the installation's manifest names.
use super::{Configuration, Error, Result};
use crate::wine;
use std::path::Path;

/// Readies a Windows build for its Game session: its prefix, the prerequisite
/// its installation names, and the folder of Wine's log. This runs before any
/// short-lived game credential exists, since a prerequisite may take minutes.
pub fn prepare_windows(
    config: &Configuration,
    locations: &crate::Locations,
) -> Result<wine::Config> {
    let prefix = config.prefix.clone().ok_or(Error::Unsupported)?;
    if config.platform != "Windows" {
        return Err(Error::Unsupported);
    }
    let wine = wine::Config::locate(
        &locations.voile,
        prefix,
        locations
            .epic_diagnostics
            .join(format!("wine-{}.log", config.app_name)),
    )?;
    let installed = epic_kit::client::installed_launch(
        &config.namespace,
        &config.catalog_item_id,
        &config.app_name,
        &config.platform,
        &config.install,
    )?;
    let prefix = Prefix {
        install: &config.install,
        prerequisite: installed.prerequisite.as_ref(),
    };
    wine::prepare(
        &wine,
        &config.home,
        &config.install,
        &installed.target,
        &prefix,
    )?;
    Ok(wine)
}

/// What Epic adds to the prefix of the installation in `install`.
struct Prefix<'a> {
    install: &'a Path,
    prerequisite: Option<&'a epic_kit::client::Prerequisite>,
}

impl wine::Storefront for Prefix<'_> {
    /// The program the manifest names to run before its game first starts,
    /// unless it only installs the Visual C++ runtime or leaves the
    /// installation.
    fn prerequisites(&self) -> std::result::Result<Vec<wine::Prerequisite>, crate::Error> {
        let Some(prerequisite) = self.prerequisite else {
            return Ok(Vec::new());
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
            return Ok(Vec::new());
        }
        let id = if prerequisite.ids.is_empty() {
            relative.to_string_lossy().into_owned()
        } else {
            prerequisite.ids.join(",")
        };
        Ok(vec![wine::Prerequisite {
            id: format!("epic:{id}"),
            programs: vec![(self.install.join(relative), prerequisite.arguments.clone())],
        }])
    }
}

/// Installers that only lay down the Visual C++ runtime. Unreal's
/// prerequisites installer is mostly that runtime.
fn visual_cpp_installer(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["vcredist", "vc_redist", "ueprereqsetup"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests;
