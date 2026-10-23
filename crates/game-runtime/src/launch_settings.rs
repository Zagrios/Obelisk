//! Launch settings: how one Windows installation runs, its game's profile
//! with the player's choices over it. The player picks the arguments added
//! to its command line, its engine, then the CPU translator that engine has,
//! then the graphics backend that renders its Direct3D there and that
//! backend's options, each offered as the engine allows, and Metal's
//! Performance HUD over the game. Only what differs from the profile is kept,
//! one file per installation in Obelisk's library, named after the
//! installation's folder as its Wine prefix is. It goes when the
//! installation is uninstalled, and a change applies from the game's next
//! launch.
use crate::{
    profile::{self, Profile},
    wine::{Engine, Graphics, GraphicsOptions},
};
use obelisk_core::launch::LaunchTarget;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// How one Windows installation runs from its next launch: its game's
/// profile with its player's choices over it. It travels whole to the Game
/// session, so nothing there looks the profile up again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub engine: Engine,
    /// What renders its Direct3D on that engine; always one `engine` offers.
    pub graphics: Graphics,
    /// What it asks of that backend.
    pub options: GraphicsOptions,
    /// The program its profile starts instead of its publisher's, relative
    /// to the installation.
    pub executable: Option<String>,
    /// What its profile adds to the storefront's arguments.
    pub profile_arguments: Vec<String>,
    /// What its player adds after those, split as typed. The game's log
    /// shows only these.
    pub arguments: Vec<String>,
    /// The environment variables every Wine command of its prefix runs
    /// with, over Obelisk's own.
    pub variables: BTreeMap<String, String>,
    /// How Wine loads some of its DLLs, as `WINEDLLOVERRIDES` spells them.
    pub dll_overrides: BTreeMap<String, String>,
    /// Metal's Performance HUD shows over the game.
    pub performance_hud: bool,
    /// Which Steam client a Steam game starts through; Obelisk's own on any
    /// engine but Rosetta's Wine.
    #[serde(default)]
    pub steam_client: SteamClient,
}

/// Which Steam client a Windows Steam game starts through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SteamClient {
    /// Obelisk's: the game's steamclient reaches Obelisk's Steam host
    /// through lsteamclient, behind Proton's steam.exe.
    #[default]
    Obelisk,
    /// Valve's Windows Steam, installed in a prefix of its own and signed in
    /// there, which starts the game itself: the one client Valve's servers
    /// trust with a Windows game's module signatures, which a VAC-secured
    /// server asks for.
    Windows,
}

/// How a game no profile names runs without choices: in Rosetta's Wine,
/// with D3DMetal.
#[cfg(test)]
impl Default for Settings {
    fn default() -> Self {
        Choices::default().apply(&Profile::default())
    }
}

impl Settings {
    /// What starts for `target` inside `install`: `target`, or the program
    /// the profile names instead, with the same arguments, in that program's
    /// folder; then the profile's arguments, then the player's.
    pub(crate) fn started(&self, install: &Path, target: &LaunchTarget) -> LaunchTarget {
        let mut started = match &self.executable {
            None => target.clone(),
            Some(executable) => {
                let executable = install.join(executable);
                LaunchTarget {
                    working_directory: executable
                        .parent()
                        .unwrap_or(install)
                        .to_string_lossy()
                        .into_owned(),
                    executable: executable.to_string_lossy().into_owned(),
                    arguments: target.arguments.clone(),
                    is_application_bundle: false,
                }
            }
        };
        started
            .arguments
            .extend(self.profile_arguments.iter().cloned());
        started.arguments.extend(self.arguments.iter().cloned());
        started
    }
}

/// The launch settings of the Windows installation of `game` in `install`,
/// by its profile and the choices kept for it in Obelisk's `library`. An
/// installation without choices, or whose file cannot be read, runs as its
/// profile says.
pub fn of(library: &Path, game: profile::Game<'_>, install: &Path) -> View {
    let profile = profile::of(game, install);
    View::new(&profile, load(library, install))
}

/// Keeps `choices` for that installation, without those its profile or its
/// engine already gives, and returns its launch settings from its next
/// launch on. No choice at all leaves no file.
pub fn choose(
    library: &Path,
    game: profile::Game<'_>,
    install: &Path,
    choices: Choices,
) -> std::io::Result<View> {
    let profile = profile::of(game, install);
    let choices = choices.normalized(&profile);
    save(library, install, &choices)?;
    Ok(View::new(&profile, choices))
}

/// What the player changed from the profile. A missing choice follows the
/// profile, or the engine's defaults on another engine than the profile's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Choices {
    /// What the player adds to the game's command line, as typed: split on
    /// the spaces outside double quotes, after the storefront's arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<Engine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graphics: Option<Graphics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ray_tracing: Option<bool>,
    /// Metal's Performance HUD shows over the game, whatever renders it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub performance_hud: bool,
    /// The Steam client a Steam game starts through, on Rosetta's Wine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steam_client: Option<SteamClient>,
}

/// What translates a game's x86 code on one engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Translator {
    Rosetta,
    Fex,
}

/// One of a graphics backend's options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsOption {
    RayTracing,
}

/// What one engine offers, in the order the player sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineOffer {
    pub engine: Engine,
    pub translator: Translator,
    pub graphics: &'static [GraphicsOffer],
}

/// What one graphics backend offers on one engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicsOffer {
    pub graphics: Graphics,
    pub options: &'static [GraphicsOption],
}

/// Every engine, the translator it has and the backends it renders with.
/// Rosetta's Wine takes D3DMetal's options; the Linux engine's render
/// servers take none yet.
pub const ENGINES: [EngineOffer; 2] = [
    EngineOffer {
        engine: Engine::Rosetta,
        translator: Translator::Rosetta,
        graphics: &[
            GraphicsOffer {
                graphics: Graphics::D3DMetal,
                options: &[GraphicsOption::RayTracing],
            },
            GraphicsOffer {
                graphics: Graphics::Dxmt,
                options: &[],
            },
        ],
    },
    EngineOffer {
        engine: Engine::Linux,
        translator: Translator::Fex,
        graphics: &[
            GraphicsOffer {
                graphics: Graphics::Dxmt,
                options: &[],
            },
            GraphicsOffer {
                graphics: Graphics::D3DMetal,
                options: &[],
            },
        ],
    },
];

/// What `engine` offers.
pub fn offer(engine: Engine) -> &'static EngineOffer {
    ENGINES
        .iter()
        .find(|offer| offer.engine == engine)
        .expect("every engine has an offer")
}

impl EngineOffer {
    /// What `graphics` offers on this engine, when the engine renders with it.
    pub fn graphics(&self, graphics: Graphics) -> Option<&'static GraphicsOffer> {
        self.graphics
            .iter()
            .find(|offer| offer.graphics == graphics)
    }
}

impl Choices {
    /// How the game of `profile` runs with these choices. A backend its
    /// engine does not offer falls back to the default one.
    fn apply(&self, profile: &Profile) -> Settings {
        let engine = self.engine.unwrap_or(profile.engine);
        let graphics = self
            .graphics
            .filter(|graphics| offer(engine).graphics(*graphics).is_some())
            .unwrap_or(default_graphics(profile, engine));
        let options = default_options(profile, engine, graphics);
        Settings {
            engine,
            graphics,
            options: GraphicsOptions {
                ray_tracing: self.ray_tracing.unwrap_or(options.ray_tracing),
            },
            executable: profile.executable.map(str::to_owned),
            profile_arguments: profile.arguments.to_vec(),
            arguments: crate::wine::command_line(self.arguments.as_deref().unwrap_or_default()),
            variables: profile.variables.clone(),
            dll_overrides: profile.dll_overrides.clone(),
            performance_hud: self.performance_hud,
            steam_client: match engine {
                Engine::Rosetta => self.steam_client.unwrap_or(profile.steam_client),
                Engine::Linux => SteamClient::Obelisk,
            },
        }
    }

    /// These choices without those `profile` already gives, or the chosen
    /// engine gives by default, and without arguments that are only spaces.
    fn normalized(&self, profile: &Profile) -> Self {
        let applied = self.apply(profile);
        let graphics = default_graphics(profile, applied.engine);
        let options = default_options(profile, applied.engine, applied.graphics);
        Self {
            arguments: self
                .arguments
                .as_deref()
                .map(str::trim)
                .filter(|arguments| !arguments.is_empty())
                .map(str::to_owned),
            engine: (applied.engine != profile.engine).then_some(applied.engine),
            graphics: (applied.graphics != graphics).then_some(applied.graphics),
            ray_tracing: (applied.options.ray_tracing != options.ray_tracing)
                .then_some(applied.options.ray_tracing),
            performance_hud: self.performance_hud,
            steam_client: (applied.steam_client != default_steam_client(profile, applied.engine))
                .then_some(applied.steam_client),
        }
    }

    /// Whether the player changed nothing.
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What renders `profile`'s game on `engine` without choices: what its
/// profile says on the profile's engine, else that engine's default backend.
fn default_graphics(profile: &Profile, engine: Engine) -> Graphics {
    if engine == profile.engine {
        profile.graphics
    } else {
        Graphics::default_on(engine)
    }
}

/// The Steam client `profile`'s game starts through on `engine` without
/// choices: what its profile says on Rosetta's Wine, where Valve's Windows
/// Steam runs, else Obelisk's.
fn default_steam_client(profile: &Profile, engine: Engine) -> SteamClient {
    match engine {
        Engine::Rosetta => profile.steam_client,
        Engine::Linux => SteamClient::Obelisk,
    }
}

/// The options the profile gives its own engine and backend; any other
/// pair starts from D3DMetal's own defaults.
fn default_options(profile: &Profile, engine: Engine, graphics: Graphics) -> GraphicsOptions {
    if engine == profile.engine && graphics == profile.graphics {
        profile.options
    } else {
        GraphicsOptions::default()
    }
}

/// The folder of every installation's launch settings in Obelisk's library.
const FOLDER: &str = "LaunchSettings";

/// The launch settings file of the installation in `install`, in Obelisk's
/// `library`.
fn file(library: &Path, install: &Path) -> Option<PathBuf> {
    let mut name = install.file_name()?.to_owned();
    name.push(".json");
    Some(library.join(FOLDER).join(name))
}

/// The player's choices for the installation in `install`. An installation
/// without any, or whose file cannot be read, has none.
fn load(library: &Path, install: &Path) -> Choices {
    file(library, install)
        .and_then(|file| fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Keeps `choices` for the installation in `install`. No choice at all
/// leaves no file.
fn save(library: &Path, install: &Path, choices: &Choices) -> std::io::Result<()> {
    let Some(file) = file(library, install) else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    if choices.is_empty() {
        return remove(library, install);
    }
    fs::create_dir_all(library.join(FOLDER))?;
    obelisk_core::write_atomic(&file, &serde_json::to_vec_pretty(choices)?)
}

/// Forgets the choices for the uninstalled installation in `install`.
pub fn remove(library: &Path, install: &Path) -> std::io::Result<()> {
    let Some(file) = file(library, install) else {
        return Ok(());
    };
    match fs::remove_file(file) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// One installation's launch settings as its player sees them: how it runs,
/// what the player changed, and how it runs without choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub settings: Settings,
    pub choices: Choices,
    /// The engine its profile names.
    pub default_engine: Engine,
    /// What renders it on each engine without choices, in `ENGINES`' order.
    pub default_graphics: [Graphics; ENGINES.len()],
    /// The Steam client a Steam game starts through on Rosetta's Wine
    /// without choices.
    pub default_steam_client: SteamClient,
}

impl View {
    /// The launch settings of `profile`'s game with `choices`.
    fn new(profile: &Profile, choices: Choices) -> Self {
        Self {
            settings: choices.apply(profile),
            choices,
            default_engine: profile.engine,
            default_graphics: ENGINES.map(|offer| default_graphics(profile, offer.engine)),
            default_steam_client: default_steam_client(profile, Engine::Rosetta),
        }
    }
}

#[cfg(test)]
mod tests;
