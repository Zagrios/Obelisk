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

/// Ends the Windows game processes a SailOver that died left frozen. Runs
/// once at startup, before any launch.
#[uniffi::export]
pub fn end_orphaned_windows_games(locations: SailOverLocations) -> u32 {
    game_runtime::wine::end_orphans(Path::new(&locations.voile)) as u32
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

#[cfg(test)]
mod tests {
    use super::*;

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
            game_runtime::wine::prefix(
                &game_runtime::Locations::from(&locations).wine_prefixes,
                Path::new("/library/Games/Steam/steam-42-windows")
            )
            .unwrap(),
            Path::new("/library/Prefixes/steam-42-windows")
        );
    }
}
