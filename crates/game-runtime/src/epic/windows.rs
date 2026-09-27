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
    // The configuration may come from a saved journal: its prefix must be the
    // one of the installation it names.
    let windows = wine::Installation::of(locations, &config.platform, &config.install)?
        .filter(|windows| config.prefix.as_deref() == Some(windows.prefix()))
        .ok_or(Error::Unsupported)?;
    let wine = windows.runtime(
        locations,
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
    /// unless it leaves the installation.
    fn prerequisites(&self) -> std::result::Result<Vec<wine::Prerequisite>, crate::Error> {
        let Some(prerequisite) = self.prerequisite else {
            return Ok(Vec::new());
        };
        let relative = prerequisite.path.replace('\\', "/");
        let relative = Path::new(&relative);
        let inside = relative
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)));
        if !inside {
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

#[cfg(test)]
mod tests;
