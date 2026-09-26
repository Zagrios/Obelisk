//! Every path SailOver uses, named once, and the removal of an installation.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

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
    /// The Voile runtimes that run Windows games: the pinned release and a
    /// development runtime.
    pub voile: String,
    /// One Wine prefix per Windows installation, named after its folder.
    pub wine_prefixes: String,
}

#[uniffi::export]
pub fn sailover_locations(library: String, bundle: String, home: String) -> SailOverLocations {
    let (library, bundle, home) = (
        PathBuf::from(library),
        PathBuf::from(bundle),
        PathBuf::from(home),
    );
    let helpers = bundle.join("Contents/Helpers/SteamNative");
    let path = |path: PathBuf| path.to_string_lossy().into_owned();
    SailOverLocations {
        cloud_archive: path(library.join("SteamCloud")),
        steam_sessions: path(library.join("SteamSessions")),
        epic_sessions: path(library.join("EpicSessions")),
        helper: path(helpers.join(game_runtime::package::HOST)),
        helpers: path(helpers),
        steam_runtime_package: path(bundle.join("Contents/Resources/SteamRuntime/runtime.zip")),
        steam_runtime_cache: path(home.join("Library/Caches/com.zagrios.sailover/SteamRuntime")),
        steam_diagnostics: path(home.join("Library/Logs/SailOver/Steam")),
        voile: path(library.join("Voile")),
        wine_prefixes: path(library.join("Prefixes")),
        library: path(library),
        home: path(home),
    }
}

/// The Wine prefix of the Windows installation in `install_directory`,
/// named after the installation's folder.
#[uniffi::export]
pub fn wine_prefix(locations: SailOverLocations, install_directory: String) -> String {
    prefix(&locations, &install_directory)
}

pub(crate) fn prefix(locations: &SailOverLocations, install_directory: &str) -> String {
    let name = Path::new(install_directory)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    Path::new(&locations.wine_prefixes)
        .join(name.as_ref())
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
        .map_err(|failure| match failure {
            sailover_core::installation::RemovalFailure::Unsafe(_) => RemovalError::Unsafe,
            sailover_core::installation::RemovalFailure::Busy => RemovalError::Busy,
            sailover_core::installation::RemovalFailure::Io(error) => RemovalError::Storage {
                message: error.to_string(),
            },
        })
}

/// Deletes the Wine prefix of the uninstalled Windows installation that was in
/// `install_directory`, as Steam deletes a Proton prefix. A missing prefix is
/// already gone; a link in its place is never followed.
#[uniffi::export]
pub fn remove_wine_prefix(
    locations: SailOverLocations,
    install_directory: String,
) -> Result<(), RemovalError> {
    if Path::new(&install_directory).file_name().is_none() {
        return Err(RemovalError::Unsafe);
    }
    let prefix = PathBuf::from(prefix(&locations, &install_directory));
    let storage = |error: std::io::Error| RemovalError::Storage {
        message: error.to_string(),
    };
    match fs::symlink_metadata(&prefix) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(storage(error)),
        Ok(metadata) if !metadata.is_dir() => return Err(RemovalError::Unsafe),
        Ok(_) => {}
    }
    // A Wine server lingers a few seconds after its game and would write the
    // registry back into the prefix.
    let server = game_runtime::voile::runtime(Path::new(&locations.voile))
        .map(|runtime| runtime.join("bin/wineserver"));
    if let Some(server) = server.filter(|server| server.is_file()) {
        let _ = Command::new(server)
            .arg("-k")
            .env_clear()
            .env("WINEPREFIX", &prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fs::remove_dir_all(&prefix).map_err(storage)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(locations.voile, "/library/Voile");
        assert_eq!(locations.wine_prefixes, "/library/Prefixes");
        assert_eq!(
            wine_prefix(locations, "/library/Games/Steam/steam-42-windows".into()),
            "/library/Prefixes/steam-42-windows"
        );
    }
}
