//! Every path Obelisk uses, named once.
use std::path::{Path, PathBuf};

/// The Cloud archive's folder inside the library.
pub(crate) const CLOUD_ARCHIVE: &str = "SteamCloud";

/// Where Obelisk keeps everything, derived from its library directory, its
/// application bundle and the user's home.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    pub library: PathBuf,
    pub home: PathBuf,
    /// The Cloud archive every storefront shares. The folder keeps its first
    /// name so existing save versions and running Game sessions stay valid.
    pub cloud_archive: PathBuf,
    pub steam_sessions: PathBuf,
    pub epic_sessions: PathBuf,
    pub gog_sessions: PathBuf,
    /// The bundled native host every supervisor runs as.
    pub helper: PathBuf,
    pub helpers: PathBuf,
    /// Valve's Steam components the Steam host loads, which a Steam game's
    /// download, or a launch that misses them, gets from Valve's CDN.
    pub steam_runtime_package: PathBuf,
    pub steam_runtime_cache: PathBuf,
    /// The Voile runtimes that run Windows games: the pinned release and a
    /// development runtime.
    pub voile: PathBuf,
    /// One Wine prefix per Windows installation, named after its folder.
    pub wine_prefixes: PathBuf,
}

impl Locations {
    pub fn new(library: &Path, bundle: &Path, home: &Path) -> Self {
        let helpers = bundle.join("Contents/Helpers/SteamNative");
        let steam_runtime_cache = home.join("Library/Caches/com.zagrios.obelisk/SteamRuntime");
        Self {
            cloud_archive: library.join(CLOUD_ARCHIVE),
            steam_sessions: library.join("SteamSessions"),
            epic_sessions: library.join("EpicSessions"),
            gog_sessions: library.join("GOGSessions"),
            helper: helpers.join(crate::package::HOST),
            helpers,
            steam_runtime_package: crate::valve::payload(&steam_runtime_cache),
            steam_runtime_cache,
            voile: library.join("Voile"),
            wine_prefixes: library.join("Prefixes"),
            library: library.to_owned(),
            home: home.to_owned(),
        }
    }
}
