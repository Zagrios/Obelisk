//! A Windows Epic build: Wine starts it in its installation's prefix, once the
//! prefix ran the prerequisite the installation's manifest names.
use super::{Configuration, Result};
use crate::{launch_shell::Installed, wine};
use std::path::Path;

/// What the installation's files say its build starts, and the prerequisite
/// they name for a Windows build to run once in its prefix first.
pub(super) fn installed(config: &Configuration) -> Result<Installed> {
    let installed = epic_kit::client::installed_launch(
        &config.namespace,
        &config.catalog_item_id,
        &config.app_name,
        config.platform,
        &config.install,
    )?;
    Ok(Installed {
        prerequisites: prerequisites(&config.install, installed.prerequisite.as_ref()),
        target: installed.target,
    })
}

/// The program the manifest names to run before its game first starts, unless
/// it leaves the installation in `install`.
fn prerequisites(
    install: &Path,
    prerequisite: Option<&epic_kit::client::Prerequisite>,
) -> Vec<wine::Prerequisite> {
    let Some(prerequisite) = prerequisite else {
        return Vec::new();
    };
    let relative = prerequisite.path.replace('\\', "/");
    let relative = Path::new(&relative);
    let inside = relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    if !inside {
        return Vec::new();
    }
    let id = if prerequisite.ids.is_empty() {
        relative.to_string_lossy().into_owned()
    } else {
        prerequisite.ids.join(",")
    };
    vec![wine::Prerequisite {
        id: format!("epic:{id}"),
        programs: vec![(install.join(relative), prerequisite.arguments.clone())],
    }]
}

#[cfg(test)]
mod tests;
