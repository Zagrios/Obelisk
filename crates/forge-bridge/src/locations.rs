//! Every path Forge uses, named once, and the removal of an installation.
use std::path::{Path, PathBuf};

/// Where Forge keeps everything, derived from its library directory, its
/// application bundle and the user's home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ForgeLocations {
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
}

#[uniffi::export]
pub fn forge_locations(library: String, bundle: String, home: String) -> ForgeLocations {
    let (library, bundle, home) = (
        PathBuf::from(library),
        PathBuf::from(bundle),
        PathBuf::from(home),
    );
    let helpers = bundle.join("Contents/Helpers/SteamNative");
    let path = |path: PathBuf| path.to_string_lossy().into_owned();
    ForgeLocations {
        cloud_archive: path(library.join("SteamCloud")),
        steam_sessions: path(library.join("SteamSessions")),
        epic_sessions: path(library.join("EpicSessions")),
        helper: path(helpers.join(game_runtime::package::HOST)),
        helpers: path(helpers),
        steam_runtime_package: path(bundle.join("Contents/Resources/SteamRuntime/runtime.zip")),
        steam_runtime_cache: path(home.join("Library/Caches/com.zagrios.forge/SteamRuntime")),
        steam_diagnostics: path(home.join("Library/Logs/Forge/Steam")),
        library: path(library),
        home: path(home),
    }
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
    forge_core::installation::remove(Path::new(&directory), &expected_directory_name, &protected)
        .map_err(|failure| match failure {
            forge_core::installation::RemovalFailure::Unsafe(_) => RemovalError::Unsafe,
            forge_core::installation::RemovalFailure::Busy => RemovalError::Busy,
            forge_core::installation::RemovalFailure::Io(error) => RemovalError::Storage {
                message: error.to_string(),
            },
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_name_every_folder_once() {
        let locations = forge_locations("/library".into(), "/Forge.app".into(), "/home".into());
        assert_eq!(locations.cloud_archive, "/library/SteamCloud");
        assert_eq!(locations.steam_sessions, "/library/SteamSessions");
        assert_eq!(locations.epic_sessions, "/library/EpicSessions");
        assert_eq!(locations.helpers, "/Forge.app/Contents/Helpers/SteamNative");
        assert_eq!(
            locations.helper,
            "/Forge.app/Contents/Helpers/SteamNative/forge-steam-host"
        );
        assert_eq!(
            locations.steam_runtime_package,
            "/Forge.app/Contents/Resources/SteamRuntime/runtime.zip"
        );
        assert_eq!(
            locations.steam_runtime_cache,
            "/home/Library/Caches/com.zagrios.forge/SteamRuntime"
        );
        assert_eq!(
            locations.steam_diagnostics,
            "/home/Library/Logs/Forge/Steam"
        );
    }
}
