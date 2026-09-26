//! Every path SailOver uses, named once.
use std::path::{Path, PathBuf};

/// Where SailOver keeps everything, derived from its library directory, its
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
    /// The bundled native host every supervisor runs as.
    pub helper: PathBuf,
    pub helpers: PathBuf,
    pub steam_runtime_package: PathBuf,
    pub steam_runtime_cache: PathBuf,
    pub steam_diagnostics: PathBuf,
    /// Where Wine logs the Windows builds of Epic games.
    pub epic_diagnostics: PathBuf,
    /// The Voile runtimes that run Windows games: the pinned release and a
    /// development runtime.
    pub voile: PathBuf,
    /// One Wine prefix per Windows installation, named after its folder.
    pub wine_prefixes: PathBuf,
}

impl Locations {
    pub fn new(library: &Path, bundle: &Path, home: &Path) -> Self {
        let helpers = bundle.join("Contents/Helpers/SteamNative");
        Self {
            cloud_archive: library.join("SteamCloud"),
            steam_sessions: library.join("SteamSessions"),
            epic_sessions: library.join("EpicSessions"),
            helper: helpers.join(crate::package::HOST),
            helpers,
            steam_runtime_package: bundle.join("Contents/Resources/SteamRuntime/runtime.zip"),
            steam_runtime_cache: home.join("Library/Caches/com.zagrios.sailover/SteamRuntime"),
            steam_diagnostics: home.join("Library/Logs/SailOver/Steam"),
            epic_diagnostics: home.join("Library/Logs/SailOver/Epic"),
            voile: library.join("Voile"),
            wine_prefixes: library.join("Prefixes"),
            library: library.to_owned(),
            home: home.to_owned(),
        }
    }
}
