//! A Windows game runs in Obelisk's Wine runtime, inside the prefix of its
//! installation. What a storefront adds, such as Steam's steam.exe helper or
//! the prerequisites it names, comes from that storefront's side, and is
//! translated here for the engine its Launch settings name, by that engine's
//! adapter.
use crate::{
    launch::Error,
    launch_settings::SteamClient,
    supervisor::{Target, architecture::Architecture},
};
use obelisk_core::{Platform, launch::LaunchTarget};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(crate) mod linux_engine;
mod rosetta;

/// The user every prefix of CrossOver's Wine names its profile after.
const USER: &str = "crossover";
/// A command that ends a game is stopped after this long: the Wine server it
/// talks to may be frozen with the game.
const QUIT_TIMEOUT: Duration = Duration::from_secs(10);

/// The Windows user profile inside `prefix`, where Auto-Cloud roots such as
/// AppData and Documents live.
pub fn profile(prefix: &Path) -> PathBuf {
    prefix.join("drive_c/users").join(USER)
}

/// How one Windows installation runs: the Wine runtime, the installation's
/// prefix, the log Wine writes while its game runs, and the launch settings
/// it runs with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub runtime: PathBuf,
    pub prefix: PathBuf,
    pub log: PathBuf,
    pub settings: crate::launch_settings::Settings,
}

/// What runs a Windows game's Wine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    /// Voile's Wine for macOS, under Rosetta.
    #[default]
    Rosetta,
    /// Voile's Wine for Linux, which voile-lkd's engine runs natively; for the
    /// games whose Launch settings name it.
    Linux,
}

/// The graphics backend that renders a Windows game's Direct3D 10 to 12
/// through Metal. dxgi is one per process, so a game has one: on DXMT, it has
/// no Direct3D 12.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Graphics {
    /// DXMT: Direct3D 10 and 11. On the Linux engine, in Neptune's arm64
    /// render server.
    Dxmt,
    /// D3DMetal: Direct3D 11 and 12, for 64-bit programs only, so 32-bit
    /// games on Rosetta's Wine render through DXMT. On the Linux engine, in
    /// Neptune's Intel render server under Rosetta.
    D3DMetal,
}

impl Graphics {
    /// What renders on `engine` when neither the game's profile nor its
    /// player picks another backend.
    pub fn default_on(engine: Engine) -> Self {
        engine.adapter().facts().graphics
    }

    /// What renders `program` on `engine` when neither the game's profile
    /// nor its player picks another backend: the engine's default, unless
    /// that is DXMT and the program imports Direct3D 12 without Direct3D 11,
    /// as ELDEN RING does, since DXMT has no Direct3D 12.
    pub(crate) fn default_for(engine: Engine, program: Option<&crate::pe::Program>) -> Self {
        let imports = program.map(crate::pe::Program::imports).unwrap_or_default();
        let imported = |dll: &str| imports.iter().any(|import| import == dll);
        match Self::default_on(engine) {
            Self::Dxmt if imported("d3d12.dll") && !imported("d3d11.dll") => Self::D3DMetal,
            graphics => graphics,
        }
    }

    /// The name Wine, Neptune and Obelisk's reports know it by.
    pub fn name(self) -> &'static str {
        match self {
            Self::D3DMetal => "d3dmetal",
            Self::Dxmt => "dxmt",
        }
    }
}

/// What a game asks of D3DMetal on Rosetta's Wine. The Linux engine's render
/// servers take none of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsOptions {
    /// Direct3D 12 offers DirectX Raytracing, as D3DMetal does by default.
    pub ray_tracing: bool,
}

impl Default for GraphicsOptions {
    fn default() -> Self {
        Self { ray_tracing: true }
    }
}

/// The Wine prefix of the Windows installation in `install`, named after the
/// installation's folder.
pub fn prefix(prefixes: &Path, install: &Path) -> Result<PathBuf, Error> {
    let name = install.file_name().ok_or(Error::NotExecutable)?;
    Ok(prefixes.join(name))
}

/// The prefix where Valve's Windows Steam starts the game of the installation
/// whose Rosetta prefix is `rosetta`, beside it: Windows Steam installs
/// itself where Obelisk's prefix keeps the steamclient lsteamclient stands
/// in for, so the two never share one.
fn windows_steam_prefix(rosetta: &Path) -> Result<PathBuf, Error> {
    let mut name = rosetta.file_name().ok_or(Error::NotExecutable)?.to_owned();
    name.push("-steam");
    Ok(rosetta.with_file_name(name))
}

impl Config {
    /// The adapter of the engine its Launch settings name. It is derived on
    /// each call, never kept: a configuration crosses to its Game session as
    /// JSON.
    fn adapter(&self) -> &'static dyn Adapter {
        self.settings.engine.adapter()
    }

    /// How the game starts.
    pub(crate) fn architecture(&self) -> Architecture {
        self.adapter().facts().architecture
    }

    /// How long the engine's launcher gets on a Force Quit to stop its own
    /// servers before it is killed; none when it has none.
    pub(crate) fn launcher_grace(&self) -> Option<Duration> {
        self.adapter().facts().launcher_grace
    }

    /// The gamepad service the game's supervisor runs while the game does;
    /// none when the engine's launcher starts its own.
    pub(crate) fn input_service(&self) -> Option<PathBuf> {
        self.adapter()
            .facts()
            .input_service
            .map(|path| self.runtime.join(path))
    }

    /// Wine's `own` DLL overrides, then the profile's.
    fn with_profile_overrides(&self, own: &str) -> String {
        std::iter::once(own.to_owned())
            .chain(self.profile_overrides())
            .collect::<Vec<_>>()
            .join(";")
    }

    /// The profile's DLL overrides, each as `WINEDLLOVERRIDES` spells it.
    fn profile_overrides(&self) -> impl Iterator<Item = String> + '_ {
        self.settings
            .dll_overrides
            .iter()
            .map(|(dll, mode)| format!("{dll}={mode}"))
    }

    /// Wine's errors, unless a development runtime's `winedebug` file names
    /// other channels, such as `+steamclient`.
    fn debug_channels(&self) -> String {
        fs::read_to_string(self.runtime.join("winedebug"))
            .ok()
            .and_then(|channels| channels.lines().next().map(str::to_owned))
            .filter(|channels| !channels.is_empty())
            .unwrap_or_else(|| "err+all,fixme-all".into())
    }

    /// The line the game's log starts with: how Obelisk runs it, for whoever
    /// reads the log, such as a bug report's reader. Of the arguments, only
    /// those its player adds: a storefront's may carry credentials.
    pub(crate) fn log_header(&self) -> String {
        let mut header = format!(
            "obelisk: engine={} graphics={}",
            self.adapter().facts().name,
            self.settings.graphics.name()
        );
        let ray_tracing = crate::launch_settings::offer(self.settings.engine)
            .graphics(self.settings.graphics)
            .is_some_and(|offer| {
                offer
                    .options
                    .contains(&crate::launch_settings::GraphicsOption::RayTracing)
            });
        if ray_tracing {
            let state = if self.settings.options.ray_tracing {
                "on"
            } else {
                "off"
            };
            header.push_str(&format!(" ray_tracing={state}"));
        }
        // Only a development runtime has no manifest.
        let voile = crate::voile::release_of(&self.runtime);
        header.push_str(&format!(
            " voile={}",
            voile.as_deref().unwrap_or("development")
        ));
        if !self.settings.variables.is_empty() {
            let variables: Vec<_> = self
                .settings
                .variables
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            header.push_str(&format!(" variables={}", typed(&variables)));
        }
        if !self.settings.dll_overrides.is_empty() {
            let overrides: Vec<_> = self.profile_overrides().collect();
            header.push_str(&format!(" dll_overrides={}", overrides.join(";")));
        }
        if !self.settings.arguments.is_empty() {
            header.push_str(&format!(" arguments={}", typed(&self.settings.arguments)));
        }
        header.push('\n');
        header
    }
}

/// One engine's side of a Windows game: where it keeps an installation's
/// prefix and its runtime, how it readies the prefix and starts the game
/// there, and how what runs there ends. Rosetta's Wine and the Linux engine
/// are its two adapters; `Engine::adapter` picks one.
trait Adapter {
    /// What the engine is, whatever the game.
    fn facts(&self) -> &'static Facts;
    /// Its runtime among Voile's in `voile`, once installed.
    fn runtime(&self, voile: &Path) -> Option<PathBuf>;
    /// Its prefix of the installation whose Rosetta prefix is `rosetta`.
    fn prefix(&self, rosetta: &Path) -> Result<PathBuf, Error>;
    /// Readies config's prefix for a game whose program is already checked:
    /// creates or updates it, has `storefront` furnish it, then makes the
    /// log's folder. Fails with WineUnavailable before the prefix is created.
    fn ready(
        &self,
        config: &Config,
        locations: &crate::Locations,
        storefront: &dyn Furnisher,
    ) -> Result<(), Error>;
    /// How `program`, checked inside `game.install`, starts with
    /// `launched`'s arguments and working directory, behind the storefront's
    /// helper, with `environment`, the Game session's own.
    fn target(
        &self,
        config: &Config,
        game: &Game<'_>,
        launched: &LaunchTarget,
        program: PathBuf,
        additions: Additions,
        environment: HashMap<String, String>,
    ) -> Result<Target, Error>;
    /// A file the game reads, in a private folder that lives as long as the
    /// returned TempDir, and its Windows path.
    fn private_file(
        &self,
        config: &Config,
        name: &str,
        bytes: &[u8],
    ) -> Result<(tempfile::TempDir, String), Error>;
    /// The command asking the game to close its windows, if it can be asked.
    /// `environment` is the game's own, `pid` its launch helper's.
    fn quit_command(
        &self,
        config: &Config,
        environment: &HashMap<String, String>,
        program: Option<&Path>,
        pid: u32,
    ) -> Option<Command>;
    /// The command ending every process of the prefix at once; none when
    /// they follow the engine's launcher, which the session ends.
    fn end_command(&self, config: &Config) -> Option<Command>;
    /// The folders whose files only this game's processes use, given its
    /// environment and, once spawned, its launch helper's `pid`.
    fn private_folders(
        &self,
        config: &Config,
        environment: &HashMap<String, String>,
        pid: Option<u32>,
    ) -> Vec<PathBuf>;
    /// Deletes `prefix`, its prefix of an uninstalled installation. A missing
    /// one is already gone; a link in its place is never followed.
    fn remove(
        &self,
        locations: &crate::Locations,
        prefix: &Path,
    ) -> Result<(), obelisk_core::installation::RemovalFailure>;
}

/// What an engine is, whatever the game it runs.
struct Facts {
    /// How the game's log names it.
    name: &'static str,
    /// How its games start.
    architecture: Architecture,
    /// The gamepad service its supervisor runs while the game does, inside
    /// its runtime; none when the engine's launcher starts its own.
    input_service: Option<&'static str>,
    /// What renders its games when neither the game's profile nor its player
    /// picks another backend.
    graphics: Graphics,
    /// How long its launcher gets on a Force Quit to stop its own servers
    /// before it is killed; none when it has none.
    launcher_grace: Option<Duration>,
    /// The variables and DLL overrides its Wine gets from Obelisk.
    reserved: Reserved,
    /// The Windows programs its Wine runs.
    programs: Programs,
}

/// The Windows programs an engine's Wine runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Programs {
    /// 32-bit and 64-bit ones, and .NET ones through Wine Mono.
    All,
    /// x86-64 ones only, and no .NET one: its Wine has neither 32-bit
    /// support nor Wine Mono.
    X86_64,
}

/// Names Obelisk sets in an engine's Wine. A profile could still replace one
/// for its game; the profiles table sets none, and a table that does loses
/// that game.
pub(crate) struct Reserved {
    pub variables: &'static [&'static str],
    pub dlls: &'static [&'static str],
}

/// What a storefront adds to its games' launches, as an engine reads it.
#[derive(Clone, Copy)]
struct Additions {
    /// `Storefront::HELPER`.
    helper: Option<&'static str>,
    /// `Storefront::PASSED`.
    passed: &'static [&'static str],
    /// `Storefront::STEAM_CLIENT`.
    steam_client: bool,
}

impl Additions {
    fn of<S: Storefront>() -> Self {
        Self {
            helper: S::HELPER,
            passed: S::PASSED,
            steam_client: S::STEAM_CLIENT,
        }
    }
}

/// A storefront as an engine readies a prefix for it, whatever its type.
/// Its methods are named apart from `Storefront`'s: with both traits in
/// scope, a call on a concrete storefront would be ambiguous.
trait Furnisher {
    /// What it adds to its games' launches.
    fn additions(&self) -> Additions;
    /// `Storefront::furnish`.
    fn furnish_prefix(&self, config: &Config) -> Result<(), Error>;
    /// `Storefront::prerequisites`.
    fn prerequisites_to_run(&self) -> Result<Vec<Prerequisite>, Error>;
}

impl<S: Storefront> Furnisher for S {
    fn additions(&self) -> Additions {
        Additions::of::<S>()
    }
    fn furnish_prefix(&self, config: &Config) -> Result<(), Error> {
        Storefront::furnish(self, config)
    }
    fn prerequisites_to_run(&self) -> Result<Vec<Prerequisite>, Error> {
        Storefront::prerequisites(self)
    }
}

impl Engine {
    /// The adapter that runs this engine's games.
    fn adapter(self) -> &'static dyn Adapter {
        match self {
            Self::Rosetta => &rosetta::RosettaWine,
            Self::Linux => &linux_engine::LinuxEngine,
        }
    }

    /// The Wine prefix this engine keeps of the Windows installation in
    /// `install`, among `prefixes`.
    pub(crate) fn prefix(self, prefixes: &Path, install: &Path) -> Result<PathBuf, Error> {
        self.adapter().prefix(&prefix(prefixes, install)?)
    }

    /// The names Obelisk sets in this engine's Wine.
    pub(crate) fn reserved(self) -> &'static Reserved {
        &self.adapter().facts().reserved
    }

    /// Whether this engine's Wine runs the Windows program at `path`. A file
    /// that is no Windows program is left to the launch to refuse.
    pub(crate) fn runs(self, path: &Path) -> bool {
        match self.adapter().facts().programs {
            Programs::All => true,
            Programs::X86_64 => crate::pe::Program::open(path)
                .is_none_or(|program| program.is_x86_64() && !program.is_dotnet()),
        }
    }
}

/// What one storefront adds to the prefix of its Windows installations, and
/// to the games that start there.
pub trait Storefront {
    /// The Windows program its games start behind, given the game's program
    /// and arguments, such as Steam's steam.exe.
    const HELPER: Option<&'static str> = None;
    /// Variables its games read that exist only once their Game session
    /// starts them, such as the Steam host's ports.
    const PASSED: &'static [&'static str] = &[];
    /// Whether its games reach Valve's steamclient through lsteamclient.
    const STEAM_CLIENT: bool = false;
    /// The programs it names to run once in a prefix before a game first
    /// starts, in order.
    fn prerequisites(&self) -> Result<Vec<Prerequisite>, Error>;
    /// Places its own files in the prefix, before any prerequisite runs.
    fn furnish(&self, _config: &Config) -> Result<(), Error> {
        Ok(())
    }
}

/// A storefront's Windows game as Wine starts it: its installation's folder
/// and its name in the store.
pub(crate) struct Game<'a> {
    pub install: &'a Path,
    pub title: &'a str,
}

/// Readies the prefix of the Windows installation in `install` to start
/// `target`: the game must be a Windows program inside the installation; then
/// its engine readies the prefix. Nothing touches the prefix for a game that
/// cannot start.
fn prepare<S: Storefront>(
    config: &Config,
    locations: &crate::Locations,
    install: &Path,
    target: &LaunchTarget,
    storefront: &S,
) -> Result<(), Error> {
    program(install, target)?;
    config.adapter().ready(config, locations, storefront)
}

/// The game `target` names, or the program its launch settings start
/// instead, that Wine starts in the prefix with `environment` and Wine's own
/// settings, as S completes it, logging to the installation's log.
pub(crate) fn target<S: Storefront>(
    config: &Config,
    game: &Game<'_>,
    target: &LaunchTarget,
    environment: HashMap<String, String>,
) -> Result<Target, Error> {
    let launched = config.settings.started(game.install, target);
    let program = program(game.install, &launched)?;
    config.adapter().target(
        config,
        game,
        &launched,
        program,
        Additions::of::<S>(),
        environment,
    )
}

/// The game `target` names, or the program its launch settings start instead,
/// which another program in the prefix starts: `launcher`, a bash script
/// named `name` that lasts as long as the game, runs with Rosetta's Wine,
/// `before`, the program's file name, then `after`. Quitting still asks the
/// game's program to close. Rosetta's Wine only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn launcher_target(
    config: &Config,
    game: &Game<'_>,
    target: &LaunchTarget,
    launcher: &str,
    name: &str,
    before: Vec<String>,
    after: Vec<String>,
    environment: HashMap<String, String>,
) -> Result<Target, Error> {
    if config.settings.engine != Engine::Rosetta {
        return Err(Error::NotExecutable);
    }
    let program = program(game.install, &config.settings.started(game.install, target))?;
    let file_name = program
        .file_name()
        .ok_or(Error::NotExecutable)?
        .to_string_lossy()
        .into_owned();
    let mut arguments = vec![
        "-c".to_owned(),
        launcher.to_owned(),
        name.to_owned(),
        rosetta::wine(config).to_string_lossy().into_owned(),
    ];
    arguments.extend(before);
    arguments.push(file_name);
    arguments.extend(after);
    let mut environment = environment;
    environment.extend(rosetta::environment(config));
    Ok(Target {
        executable: "/bin/bash".into(),
        arguments,
        working_directory: game.install.to_owned(),
        environment,
        log: Some(config.log.clone()),
        installation: Some(game.install.to_owned()),
        program: Some(program),
    })
}

/// Rosetta's Wine, as every command of config's prefix runs it, under the
/// Mac `home`; the caller adds the program and its arguments. Rosetta's Wine
/// only.
pub(crate) fn command(config: &Config, home: &Path) -> Result<Command, Error> {
    if config.settings.engine != Engine::Rosetta {
        return Err(Error::NotExecutable);
    }
    let mut command = Command::new(rosetta::wine(config));
    command
        .env_clear()
        .envs(crate::native::environment(home))
        .envs(rosetta::environment(config));
    Ok(command)
}

/// A file the game reads, in a private folder that lives as long as the
/// returned TempDir, and its Windows path. Rosetta's Wine reads it on the
/// Mac's disk, through its Z: drive; the engine's guest, which cannot see
/// that disk, in the prefix's C: drive.
pub(crate) fn private_file(
    config: &Config,
    name: &str,
    bytes: &[u8],
) -> Result<(tempfile::TempDir, String), Error> {
    config.adapter().private_file(config, name, bytes)
}

/// The Windows program `target` names, when it is one inside `install`.
fn program(install: &Path, target: &LaunchTarget) -> Result<PathBuf, Error> {
    let root = install.canonicalize()?;
    let executable = Path::new(&target.executable)
        .canonicalize()
        .map_err(|_| Error::NotExecutable)?;
    if !executable.starts_with(&root) || crate::pe::Program::open(&executable).is_none() {
        return Err(Error::NotExecutable);
    }
    Ok(executable)
}

/// A program run once in a prefix before its game starts, such as a DirectX
/// installer. Storefronts name them: Steam in install scripts, Epic in its
/// manifests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prerequisite {
    /// A prefix runs each id once.
    pub id: String,
    /// Run in order, each with its Windows command line.
    pub programs: Vec<(PathBuf, String)>,
}

/// Where `path` is inside a prefix of Rosetta's Wine, whose Z: drive is the
/// Mac's root.
pub(crate) fn windows_path(path: &Path) -> String {
    format!("Z:{}", path.to_string_lossy().replace('/', "\\"))
}

/// One Windows installation: its game runs in Wine, in the installation's own
/// prefix, where its saves live too. The prefix is readied before each launch
/// and goes when the installation is uninstalled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installation {
    install: PathBuf,
    prefix: PathBuf,
    /// Where Wine logs while its game runs.
    log: PathBuf,
    settings: crate::launch_settings::Settings,
    /// The oldest Voile release its game runs in, as its profile says.
    minimum_voile: Option<&'static str>,
}

impl Installation {
    /// The Windows installation of `game` in `install`, run as
    /// `launch_settings` say, resolved once for this launch.
    pub(crate) fn new(
        locations: &crate::Locations,
        game: crate::profile::Game<'_>,
        install: &Path,
        launch_settings: crate::launch_settings::View,
    ) -> Result<Self, Error> {
        let settings = launch_settings.settings;
        let rosetta = prefix(&locations.wine_prefixes, install)?;
        let prefix = match settings.steam_client {
            SteamClient::Windows => windows_steam_prefix(&rosetta)?,
            SteamClient::Obelisk => settings.engine.adapter().prefix(&rosetta)?,
        };
        Ok(Self {
            install: install.to_owned(),
            prefix,
            log: log(locations, game),
            settings,
            minimum_voile: launch_settings.minimum_voile,
        })
    }

    /// The oldest Voile release its game runs in: its launch waits for one
    /// when an older one is in place.
    pub(crate) fn minimum_voile(&self) -> Option<&'static str> {
        self.minimum_voile
    }

    /// The Windows machine the installation runs in.
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    /// Which Steam client its game starts through, if a Steam game.
    pub(crate) fn steam_client(&self) -> SteamClient {
        self.settings.steam_client
    }

    /// Readies the prefix, with `storefront`'s additions, to start `target`
    /// or the program the game's profile names instead, and returns how its
    /// game runs. D3DMetal is translated before a game that renders with it
    /// starts.
    pub fn ready<S: Storefront>(
        &self,
        locations: &crate::Locations,
        target: &LaunchTarget,
        storefront: &S,
    ) -> Result<Config, Error> {
        let config = self.runtime(locations)?;
        let target = self.settings.started(&self.install, target);
        prepare(&config, locations, &self.install, &target, storefront)?;
        Ok(config)
    }

    /// Readies the prefix, with `storefront`'s additions, for what runs there
    /// besides the game, such as the storefront's own client, and returns how
    /// its programs run.
    pub(crate) fn ready_prefix<S: Storefront>(
        &self,
        locations: &crate::Locations,
        storefront: &S,
    ) -> Result<Config, Error> {
        let config = self.runtime(locations)?;
        config.adapter().ready(&config, locations, storefront)?;
        Ok(config)
    }

    /// How its game runs, once Voile is installed.
    fn runtime(&self, locations: &crate::Locations) -> Result<Config, Error> {
        let runtime = self.settings.engine.adapter().runtime(&locations.voile);
        Ok(Config {
            runtime: runtime.ok_or(Error::WineUnavailable)?,
            prefix: self.prefix.clone(),
            log: self.log.clone(),
            settings: self.settings.clone(),
        })
    }
}

/// Forgets the prerequisites `forget` picks among those the Rosetta prefix of
/// the Windows installation in `install` ran: they run again at the next
/// launch. Only Rosetta's Wine runs prerequisites, and an installation that
/// can have no prefix ran none.
pub(crate) fn forget_prerequisites(
    locations: &crate::Locations,
    install: &Path,
    forget: impl Fn(&str) -> bool,
) -> std::io::Result<()> {
    let Ok(rosetta) = prefix(&locations.wine_prefixes, install) else {
        return Ok(());
    };
    rosetta::forget_prerequisites(&rosetta, forget)
}

/// Deletes the prefixes of the uninstalled Windows installation in `install`,
/// as Steam deletes a Proton prefix, and its launch settings. Both engines'
/// go, whichever ran the game last: an earlier launch may have made the
/// other's before the game moved, and so does Valve's Windows Steam's. A
/// missing prefix is already gone; a link in its place is never followed.
/// None when it can have no prefix.
pub(crate) fn remove(
    locations: &crate::Locations,
    install: &Path,
) -> Option<Result<(), obelisk_core::installation::RemovalFailure>> {
    let rosetta = prefix(&locations.wine_prefixes, install).ok()?;
    Some(remove_prefixes(locations, install, &rosetta))
}

/// Deletes both engines' prefixes and Windows Steam's of the installation in
/// `install`, whose Rosetta prefix is `rosetta`, then its launch settings.
fn remove_prefixes(
    locations: &crate::Locations,
    install: &Path,
    rosetta: &Path,
) -> Result<(), obelisk_core::installation::RemovalFailure> {
    use obelisk_core::installation::RemovalFailure;
    let unsafe_prefix = |_| RemovalFailure::Unsafe(rosetta.to_owned());
    for engine in [Engine::Linux, Engine::Rosetta] {
        let adapter = engine.adapter();
        adapter.remove(locations, &adapter.prefix(rosetta).map_err(unsafe_prefix)?)?;
    }
    let windows_steam = windows_steam_prefix(rosetta).map_err(unsafe_prefix)?;
    Engine::Rosetta
        .adapter()
        .remove(locations, &windows_steam)?;
    crate::launch_settings::remove(&locations.library, install)?;
    Ok(())
}

/// Where Wine logs `game`: in its storefront's diagnostics folder.
fn log(locations: &crate::Locations, game: crate::profile::Game<'_>) -> PathBuf {
    crate::diagnostics::game_log(&locations.home, game.into(), Platform::Windows)
}

/// Deletes `folder`. A missing one is already gone; a link in its place is
/// never followed.
fn remove_folder(folder: &Path) -> Result<(), obelisk_core::installation::RemovalFailure> {
    use obelisk_core::installation::RemovalFailure;
    match fs::symlink_metadata(folder) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(RemovalFailure::Unsafe(folder.to_owned()));
        }
        Ok(_) => {}
    }
    Ok(fs::remove_dir_all(folder)?)
}

/// The exit code of `child`, or `None` once it ran for `timeout` and was killed.
pub(crate) fn wait(mut child: Child, timeout: Duration) -> Result<Option<i32>, Error> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status.code().unwrap_or(-1)));
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Ends every process of the prefix: Wine asks them to quit, then kills
/// those that remain, including the ones it started apart from the game.
pub(crate) fn stop(config: &Config) {
    if let Some(mut command) = config.adapter().end_command(config) {
        let _ = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Asks the Windows game to quit as its player closing its windows would, and
/// kills nothing. `environment` is the game's own, `pid` its launch helper's.
pub(crate) fn quit(
    config: &Config,
    environment: &HashMap<String, String>,
    program: Option<&Path>,
    pid: u32,
) {
    if let Some(command) = config
        .adapter()
        .quit_command(config, environment, program, pid)
    {
        let _ = run(command, QUIT_TIMEOUT);
    }
}

/// Kills every process of the prefix at once. Unlike `stop`, it never waits
/// on a Wine server frozen with its game for longer than `QUIT_TIMEOUT`.
pub(crate) fn kill(config: &Config) {
    if let Some(command) = config.adapter().end_command(config) {
        let _ = run(command, QUIT_TIMEOUT);
    }
}

/// The folders whose files only the game's processes use, given its
/// `environment` and, once spawned, its launch helper's `pid`.
pub(crate) fn private_folders(
    config: &Config,
    environment: &HashMap<String, String>,
    pid: Option<u32>,
) -> Vec<PathBuf> {
    config.adapter().private_folders(config, environment, pid)
}

/// Runs `command` without input or output for at most `timeout`.
fn run(mut command: Command, timeout: Duration) -> Result<Option<i32>, Error> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait(child, timeout)
}

/// Ends the Wine processes an Obelisk that died left behind, and returns how
/// many. macOS ends every process of an Obelisk that dies, its sessions'
/// supervisors and Steam hosts included, but not the Windows processes Wine
/// starts apart: those froze without their session, and nothing else runs
/// from `voile`.
pub fn end_orphans(voile: &Path) -> usize {
    let mut command = Command::new("/usr/sbin/lsof");
    command.args(["-n", "-P", "-d", "txt", "-Fpn"]);
    let Some(listing) = crate::supervisor::quit::listing(command) else {
        return 0;
    };
    let voile = voile.canonicalize().unwrap_or_else(|_| voile.to_owned());
    let orphans = running_from(&listing, &voile, std::process::id());
    crate::supervisor::quit::kill(&orphans);
    orphans.len()
}

/// The processes of an `lsof -Fpn` listing that run files from `root`.
fn running_from(listing: &str, root: &Path, own: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut pid = None;
    for line in listing.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse::<u32>().ok().filter(|pid| *pid != own);
        } else if let Some(path) = line.strip_prefix('n')
            && let Some(current) = pid
            && Path::new(path).starts_with(root)
            && !found.contains(&current)
        {
            found.push(current);
        }
    }
    found
}

/// Splits a Windows command line on the spaces outside double quotes.
pub(crate) fn command_line(line: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in line.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(character);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// `arguments` as a command line `command_line` splits back into them.
fn typed(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| {
            if argument.is_empty() || argument.contains([' ', '\t']) {
                format!("\"{argument}\"")
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
pub(crate) mod tests;
