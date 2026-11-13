//! Every path Obelisk uses, named once, and the removal of an installation.
use std::path::{Path, PathBuf};

/// Where Obelisk keeps everything, derived from its library directory, its
/// application bundle and the user's home.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ObeliskLocations {
    pub library: String,
    pub home: String,
    /// The Cloud archive every storefront shares. The folder keeps its first
    /// name so existing save versions and running Game sessions stay valid.
    pub cloud_archive: String,
    pub steam_sessions: String,
    pub epic_sessions: String,
    pub gog_sessions: String,
    /// The bundled native host every supervisor runs as.
    pub helper: String,
    pub helpers: String,
    /// Valve's Steam components the Steam host loads, which a Steam game's
    /// download, or a launch that misses them, gets from Valve's CDN.
    pub steam_runtime_package: String,
    pub steam_runtime_cache: String,
    /// The Voile runtimes that run Windows games: the pinned release and a
    /// development runtime.
    pub voile: String,
    /// One Wine prefix per Windows installation, named after its folder.
    pub wine_prefixes: String,
}

#[uniffi::export]
pub fn obelisk_locations(library: String, bundle: String, home: String) -> ObeliskLocations {
    let locations =
        game_runtime::Locations::new(Path::new(&library), Path::new(&bundle), Path::new(&home));
    let path = |path: PathBuf| path.to_string_lossy().into_owned();
    ObeliskLocations {
        library: path(locations.library),
        home: path(locations.home),
        cloud_archive: path(locations.cloud_archive),
        steam_sessions: path(locations.steam_sessions),
        epic_sessions: path(locations.epic_sessions),
        gog_sessions: path(locations.gog_sessions),
        helper: path(locations.helper),
        helpers: path(locations.helpers),
        steam_runtime_package: path(locations.steam_runtime_package),
        steam_runtime_cache: path(locations.steam_runtime_cache),
        voile: path(locations.voile),
        wine_prefixes: path(locations.wine_prefixes),
    }
}

/// Ends the Windows game processes an Obelisk that died left frozen. Runs
/// once at startup, before any launch.
#[uniffi::export]
pub fn end_orphaned_windows_games(locations: ObeliskLocations) -> u32 {
    game_runtime::wine::end_orphans(Path::new(&locations.voile)) as u32
}

/// The Voile release Windows games run in now, once one is installed. Bug
/// reports name it.
#[uniffi::export]
pub fn installed_voile_release(locations: ObeliskLocations) -> Option<String> {
    game_runtime::voile::installed_release(Path::new(&locations.voile))
}

/// Whether Windows games run in Voile's pre-releases too, as Settings' beta
/// releases choose: the next launch takes the newest release or pre-release,
/// else it leaves a pre-release in place for the newest release. Returns
/// whether the choice was kept.
#[uniffi::export]
pub fn take_voile_prereleases(locations: ObeliskLocations, take: bool) -> bool {
    game_runtime::voile::take_prereleases(Path::new(&locations.voile), take).is_ok()
}

impl From<&ObeliskLocations> for game_runtime::Locations {
    fn from(locations: &ObeliskLocations) -> Self {
        Self {
            library: (&locations.library).into(),
            home: (&locations.home).into(),
            cloud_archive: (&locations.cloud_archive).into(),
            steam_sessions: (&locations.steam_sessions).into(),
            epic_sessions: (&locations.epic_sessions).into(),
            gog_sessions: (&locations.gog_sessions).into(),
            helper: (&locations.helper).into(),
            helpers: (&locations.helpers).into(),
            steam_runtime_package: (&locations.steam_runtime_package).into(),
            steam_runtime_cache: (&locations.steam_runtime_cache).into(),
            voile: (&locations.voile).into(),
            wine_prefixes: (&locations.wine_prefixes).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Voile release laid in `voile` as an install lays it.
    fn lay(voile: &Path, version: &str) {
        let runtime = voile.join(version);
        std::fs::create_dir_all(runtime.join("bin")).unwrap();
        std::fs::write(runtime.join("bin/wine"), "wine").unwrap();
        std::fs::write(
            runtime.join("runtime.json"),
            format!(r#"{{"version": "{version}"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn beta_releases_choose_the_voile_release_windows_games_run_in() {
        let directory = tempfile::tempdir().unwrap();
        let library = directory.path().to_string_lossy().into_owned();
        let locations = obelisk_locations(library, "/Obelisk.app".into(), "/home".into());
        let voile = Path::new(&locations.voile);
        lay(voile, "v1.0.0-2");
        lay(voile, "v1.0.0-3");
        let entry = |version: &str| {
            format!(
                r#"{{"version": "{version}", "contract": 1, "url": "https://example.invalid/{version}", "size": 3, "sha256": "{}"}}"#,
                "a".repeat(64)
            )
        };
        std::fs::write(
            voile.join("feed.json"),
            format!(
                r#"{{"format": 1, "releases": [{}], "prereleases": [{}]}}"#,
                entry("v1.0.0-2"),
                entry("v1.0.0-3")
            ),
        )
        .unwrap();
        assert_eq!(
            installed_voile_release(locations.clone()).as_deref(),
            Some("v1.0.0-2")
        );
        assert!(take_voile_prereleases(locations.clone(), true));
        assert_eq!(
            installed_voile_release(locations.clone()).as_deref(),
            Some("v1.0.0-3")
        );
        assert!(take_voile_prereleases(locations.clone(), false));
        assert_eq!(
            installed_voile_release(locations).as_deref(),
            Some("v1.0.0-2")
        );
    }

    #[test]
    fn locations_name_every_folder_once() {
        let locations = obelisk_locations("/library".into(), "/Obelisk.app".into(), "/home".into());
        assert_eq!(locations.cloud_archive, "/library/SteamCloud");
        assert_eq!(locations.steam_sessions, "/library/SteamSessions");
        assert_eq!(locations.epic_sessions, "/library/EpicSessions");
        assert_eq!(locations.gog_sessions, "/library/GOGSessions");
        assert_eq!(
            locations.helpers,
            "/Obelisk.app/Contents/Helpers/SteamNative"
        );
        assert_eq!(
            locations.helper,
            "/Obelisk.app/Contents/Helpers/SteamNative/obelisk-steam-host"
        );
        assert_eq!(
            locations.steam_runtime_package,
            format!(
                "/home/Library/Caches/com.zagrios.obelisk/SteamRuntime/valve-{}.zip",
                game_runtime::package::manifest().version
            )
        );
        assert_eq!(
            locations.steam_runtime_cache,
            "/home/Library/Caches/com.zagrios.obelisk/SteamRuntime"
        );
        assert_eq!(locations.voile, "/library/Voile");
        assert_eq!(locations.wine_prefixes, "/library/Prefixes");
        let runtime = game_runtime::Locations::from(&locations);
        assert_eq!(runtime.home, Path::new("/home"));
        assert_eq!(
            game_runtime::wine::prefix(
                &runtime.wine_prefixes,
                Path::new("/library/Games/Steam/steam-42-windows")
            )
            .unwrap(),
            Path::new("/library/Prefixes/steam-42-windows")
        );
    }
}
