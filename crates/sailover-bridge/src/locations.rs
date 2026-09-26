//! Every path SailOver uses, named once, and the removal of an installation.
use std::path::{Path, PathBuf};

/// Where SailOver keeps everything, derived from its library directory, its
/// application bundle and the user's home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SailOverLocations {
    pub library: String,
    pub home: String,
    /// The Cloud archive every storefront shares. The folder keeps its first
    /// name so existing save versions and running Game sessions stay valid.
    pub cloud_archive: String,
    pub steam_sessions: String,
    pub epic_sessions: String,
    /// The bundled native host every supervisor runs as.
    pub helper: String,
    pub helpers: String,
    pub steam_runtime_package: String,
    pub steam_runtime_cache: String,
    pub steam_diagnostics: String,
    /// Where Wine logs the Windows builds of Epic games.
    pub epic_diagnostics: String,
    /// The Voile runtimes that run Windows games: the pinned release and a
    /// development runtime.
    pub voile: String,
    /// One Wine prefix per Windows installation, named after its folder.
    pub wine_prefixes: String,
}

#[uniffi::export]
pub fn sailover_locations(library: String, bundle: String, home: String) -> SailOverLocations {
    let locations =
        game_runtime::Locations::new(Path::new(&library), Path::new(&bundle), Path::new(&home));
    let path = |path: PathBuf| path.to_string_lossy().into_owned();
    SailOverLocations {
        library: path(locations.library),
        home: path(locations.home),
        cloud_archive: path(locations.cloud_archive),
        steam_sessions: path(locations.steam_sessions),
        epic_sessions: path(locations.epic_sessions),
        helper: path(locations.helper),
        helpers: path(locations.helpers),
        steam_runtime_package: path(locations.steam_runtime_package),
        steam_runtime_cache: path(locations.steam_runtime_cache),
        steam_diagnostics: path(locations.steam_diagnostics),
        epic_diagnostics: path(locations.epic_diagnostics),
        voile: path(locations.voile),
        wine_prefixes: path(locations.wine_prefixes),
    }
}

impl From<&SailOverLocations> for game_runtime::Locations {
    fn from(locations: &SailOverLocations) -> Self {
        Self {
            library: (&locations.library).into(),
            home: (&locations.home).into(),
            cloud_archive: (&locations.cloud_archive).into(),
            steam_sessions: (&locations.steam_sessions).into(),
            epic_sessions: (&locations.epic_sessions).into(),
            helper: (&locations.helper).into(),
            helpers: (&locations.helpers).into(),
            steam_runtime_package: (&locations.steam_runtime_package).into(),
            steam_runtime_cache: (&locations.steam_runtime_cache).into(),
            steam_diagnostics: (&locations.steam_diagnostics).into(),
            epic_diagnostics: (&locations.epic_diagnostics).into(),
            voile: (&locations.voile).into(),
            wine_prefixes: (&locations.wine_prefixes).into(),
        }
    }
}

/// The Wine prefix of the Windows installation in `install_directory`,
/// named after the installation's folder.
#[uniffi::export]
pub fn wine_prefix(locations: SailOverLocations, install_directory: String) -> String {
    game_runtime::wine::prefix(
        Path::new(&locations.wine_prefixes),
        Path::new(&install_directory),
    )
    .unwrap_or_else(|_| PathBuf::from(&locations.wine_prefixes))
    .to_string_lossy()
    .into_owned()
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum RemovalError {
    #[error("not a dedicated installation folder")]
    Unsafe,
    #[error("the installation is in use")]
    Busy,
    #[error("{message}")]
    Storage { message: String },
}

/// Removes a storefront's dedicated installation folder under the lock every
/// Transfer and launch of it takes. `protected` folders are never removed.
#[uniffi::export]
pub fn remove_game_installation(
    directory: String,
    expected_directory_name: String,
    protected: Vec<String>,
) -> Result<(), RemovalError> {
    let protected: Vec<&Path> = protected.iter().map(Path::new).collect();
    sailover_core::installation::remove(Path::new(&directory), &expected_directory_name, &protected)
        .map_err(removal_error)
}

/// Deletes the Wine prefix of the uninstalled Windows installation that was in
/// `install_directory`, as Steam deletes a Proton prefix. A missing prefix is
/// already gone; a link in its place is never followed.
#[uniffi::export]
pub fn remove_wine_prefix(
    locations: SailOverLocations,
    install_directory: String,
) -> Result<(), RemovalError> {
    game_runtime::wine::remove(&(&locations).into(), Path::new(&install_directory))
        .map_err(removal_error)
}

fn removal_error(failure: sailover_core::installation::RemovalFailure) -> RemovalError {
    match failure {
        sailover_core::installation::RemovalFailure::Unsafe(_) => RemovalError::Unsafe,
        sailover_core::installation::RemovalFailure::Busy => RemovalError::Busy,
        sailover_core::installation::RemovalFailure::Io(error) => RemovalError::Storage {
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn an_uninstalled_windows_game_loses_its_prefix_but_never_what_a_link_points_to() {
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        let locations = sailover_locations(
            library.to_string_lossy().into_owned(),
            "/SailOver.app".into(),
            "/home".into(),
        );
        let install = |name: &str| {
            library
                .join("Games/Steam")
                .join(name)
                .to_string_lossy()
                .into_owned()
        };
        let prefix = library.join("Prefixes/steam-42-windows");
        fs::create_dir_all(prefix.join("drive_c/users")).unwrap();
        fs::write(prefix.join("system.reg"), "registry").unwrap();
        remove_wine_prefix(locations.clone(), install("steam-42-windows")).unwrap();
        assert!(!prefix.exists());
        remove_wine_prefix(locations.clone(), install("steam-42-windows")).unwrap();

        let elsewhere = root.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, library.join("Prefixes/steam-43-windows")).unwrap();
        assert!(matches!(
            remove_wine_prefix(locations.clone(), install("steam-43-windows")),
            Err(RemovalError::Unsafe)
        ));
        assert!(elsewhere.is_dir());
        assert!(matches!(
            remove_wine_prefix(locations, "/".into()),
            Err(RemovalError::Unsafe)
        ));
    }

    #[test]
    fn locations_name_every_folder_once() {
        let locations =
            sailover_locations("/library".into(), "/SailOver.app".into(), "/home".into());
        assert_eq!(locations.cloud_archive, "/library/SteamCloud");
        assert_eq!(locations.steam_sessions, "/library/SteamSessions");
        assert_eq!(locations.epic_sessions, "/library/EpicSessions");
        assert_eq!(
            locations.helpers,
            "/SailOver.app/Contents/Helpers/SteamNative"
        );
        assert_eq!(
            locations.helper,
            "/SailOver.app/Contents/Helpers/SteamNative/sailover-steam-host"
        );
        assert_eq!(
            locations.steam_runtime_package,
            "/SailOver.app/Contents/Resources/SteamRuntime/runtime.zip"
        );
        assert_eq!(
            locations.steam_runtime_cache,
            "/home/Library/Caches/com.zagrios.sailover/SteamRuntime"
        );
        assert_eq!(
            locations.steam_diagnostics,
            "/home/Library/Logs/SailOver/Steam"
        );
        assert_eq!(
            locations.epic_diagnostics,
            "/home/Library/Logs/SailOver/Epic"
        );
        assert_eq!(locations.voile, "/library/Voile");
        assert_eq!(locations.wine_prefixes, "/library/Prefixes");
        assert_eq!(
            wine_prefix(locations, "/library/Games/Steam/steam-42-windows".into()),
            "/library/Prefixes/steam-42-windows"
        );
    }
}
