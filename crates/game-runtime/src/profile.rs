//! What Obelisk knows about particular Windows games, whatever their
//! storefront: the engine that runs one, the graphics backend its Direct3D
//! goes through there, the program started instead of its publisher's, what
//! that program needs besides (arguments, environment variables and DLL
//! overrides), the Steam client a Steam game starts through on Rosetta's
//! Wine, and the oldest Voile release it runs in. The table is
//! `profiles.json`. Obelisk is built with it, and a release build reads it
//! again from Obelisk's `main` as Obelisk starts and as a Windows game
//! launches, keeping the last one beside its library; a development build
//! keeps the one it was built with. Each entry is checked as it is read, and
//! one that sets what a profile may not is left out. It names a game by its
//! storefronts' ids, or by a file of its installation, which finds the game
//! whatever the storefront. A game it does not name whose installation holds
//! Easy Anti-Cheat, which refuses Rosetta's Wine, runs on the Linux engine;
//! every other one runs with the defaults, in Rosetta's Wine with D3DMetal.
//! The player's launch settings apply over it.
use crate::{
    easy_anti_cheat::{self, EasyAntiCheat},
    launch_settings::SteamClient,
    pe::Program,
    remote::Table,
    wine::{Engine, Graphics, GraphicsOptions},
};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path, sync::LazyLock};

const TABLE: &str = include_str!("../profiles.json");
/// The table's file, on Obelisk's `main` and kept in the library.
pub(crate) const NAME: &str = "profiles.json";
/// How `WINEDLLOVERRIDES` loads a DLL: nothing for never.
const MODES: [&str; 6] = ["", "n", "b", "n,b", "b,n", "d"];
/// Variables that load code into the programs Wine starts, on macOS or in
/// the Linux engine: no profile sets them.
const LOADERS: [&str; 2] = ["DYLD_", "LD_"];

/// A game as its storefront names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Game<'a> {
    Steam {
        app_id: u32,
    },
    Epic {
        namespace: &'a str,
        app_name: &'a str,
    },
    Gog {
        product_id: &'a str,
    },
}

impl Game<'_> {
    /// Whether a table entry with these storefronts' ids names this game.
    pub(crate) fn named_by(
        self,
        steam: Option<u32>,
        epic: Option<&EpicGame>,
        gog: Option<&str>,
    ) -> bool {
        match self {
            Game::Steam { app_id } => steam == Some(app_id),
            Game::Epic {
                namespace,
                app_name,
            } => epic.is_some_and(|epic| epic.namespace == namespace && epic.app_name == app_name),
            Game::Gog { product_id } => gog == Some(product_id),
        }
    }
}

/// How Obelisk runs one Windows game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Profile {
    pub engine: Engine,
    /// What renders its Direct3D on that engine.
    pub graphics: Graphics,
    /// What it asks of that backend.
    pub options: GraphicsOptions,
    /// The program the game starts with instead of its publisher's, relative
    /// to the installation, when that one cannot run there.
    pub executable: Option<&'static str>,
    /// Its installation's Easy Anti-Cheat bootstrapper, relative to it, which
    /// starts instead of a publisher's program the engine cannot run: that
    /// program would start it, and it starts the game.
    pub bootstrapper: Option<String>,
    /// What it adds to the storefront's arguments, before the player's.
    pub arguments: &'static [String],
    /// The environment variables every Wine command of its prefix runs
    /// with, over Obelisk's own.
    pub variables: &'static BTreeMap<String, String>,
    /// How Wine loads some of its DLLs, by DLL, as `WINEDLLOVERRIDES` spells
    /// it: `n,b` for the game's own copy first, nothing to never load it.
    pub dll_overrides: &'static BTreeMap<String, String>,
    /// The oldest Voile release it runs in, such as `v26.3.0-16`: its launch
    /// waits for the newest when an older one is in place, and runs in the
    /// newest installed otherwise, as every game does.
    pub minimum_voile: Option<&'static str>,
    /// The Steam client a Steam game starts through on Rosetta's Wine.
    pub steam_client: SteamClient,
}

static NONE: BTreeMap<String, String> = BTreeMap::new();

impl Default for Profile {
    fn default() -> Self {
        Self {
            engine: Engine::Rosetta,
            graphics: Graphics::default_on(Engine::Rosetta),
            options: GraphicsOptions::default(),
            executable: None,
            bootstrapper: None,
            arguments: &[],
            variables: &NONE,
            dll_overrides: &NONE,
            minimum_voile: None,
            steam_client: SteamClient::Obelisk,
        }
    }
}

/// One game of the table: its name and why it runs so, for the table's
/// readers, the storefronts' names for it, a file only its installation
/// holds, and what its profile changes from the defaults. Nothing else.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Known {
    name: String,
    note: String,
    steam: Option<u32>,
    epic: Option<EpicGame>,
    gog: Option<String>,
    /// Relative to the installation.
    path: Option<String>,
    engine: Option<Engine>,
    graphics: Option<Graphics>,
    executable: Option<String>,
    #[serde(default)]
    arguments: Vec<String>,
    #[serde(default)]
    variables: BTreeMap<String, String>,
    #[serde(default)]
    dll_overrides: BTreeMap<String, String>,
    minimum_voile: Option<String>,
    steam_client: Option<SteamClient>,
}

/// An Epic game as a table names it: its namespace and app name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EpicGame {
    namespace: String,
    app_name: String,
}

/// The profiles Obelisk reads now.
static PROFILES: LazyLock<Table<Known>> = LazyLock::new(|| Table::new(NAME, TABLE, checked));

/// `entry` as Obelisk reads it, when it holds only what Obelisk reads and
/// sets nothing a profile may not: a name and a note, a storefront's id or a
/// file, a known engine and backend, variables and DLL overrides Obelisk
/// leaves to profiles, paths inside the installation and a release's version.
fn checked(entry: serde_json::Value) -> Option<Known> {
    // An entry is an object: serde would also read an array, field by field.
    if !entry.is_object() {
        return None;
    }
    let known: Known = serde_json::from_value(entry).ok()?;
    known.allowed().then_some(known)
}

/// The names Obelisk sets in the Wine of each engine.
fn reserved() -> impl Iterator<Item = &'static crate::wine::Reserved> {
    crate::launch_settings::ENGINES
        .iter()
        .map(|offer| offer.engine.reserved())
}

fn variable_allowed(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|next| next.is_ascii_alphanumeric() || next == '_')
        && !LOADERS.iter().any(|loader| name.starts_with(loader))
        && reserved().all(|own| !own.variables.contains(&name))
}

fn dll_allowed(dll: &str, mode: &str) -> bool {
    let name = dll.to_ascii_lowercase();
    !dll.is_empty()
        && !dll.contains([',', '=', ';'])
        && MODES.contains(&mode)
        && reserved().all(|own| !own.dlls.contains(&name.as_str()))
}

/// Whether `path` stays inside the installation it is relative to.
fn inside(path: &str) -> bool {
    obelisk_core::install::safe_relative_path(path).is_ok()
}

impl Known {
    fn allowed(&self) -> bool {
        !self.name.is_empty()
            && !self.note.is_empty()
            && (self.steam.is_some()
                || self.epic.is_some()
                || self.gog.is_some()
                || self.path.is_some())
            && self.variables.keys().all(|name| variable_allowed(name))
            && self
                .dll_overrides
                .iter()
                .all(|(dll, mode)| dll_allowed(dll, mode))
            && self.executable.as_deref().is_none_or(inside)
            && self.path.as_deref().is_none_or(inside)
            && self
                .minimum_voile
                .as_deref()
                .is_none_or(crate::voile::is_version)
    }

    /// Whether this entry names `game` by its storefront's id.
    fn names(&self, game: Game<'_>) -> bool {
        game.named_by(self.steam, self.epic.as_ref(), self.gog.as_deref())
    }

    fn profile(&'static self) -> Profile {
        let engine = self.engine.unwrap_or_default();
        Profile {
            engine,
            graphics: self.graphics.unwrap_or(Graphics::default_on(engine)),
            options: GraphicsOptions::default(),
            executable: self.executable.as_deref(),
            bootstrapper: None,
            arguments: &self.arguments,
            variables: &self.variables,
            dll_overrides: &self.dll_overrides,
            minimum_voile: self.minimum_voile.as_deref(),
            steam_client: self.steam_client.unwrap_or_default(),
        }
    }

    /// Whether the installation in `install` holds this entry's file.
    fn found_in(&self, install: &Path) -> bool {
        self.path
            .as_ref()
            .is_some_and(|path| install.join(path).is_file())
    }
}

/// The engine of a game the table does not name whose installation holds
/// Easy Anti-Cheat, which refuses Rosetta's Wine.
const PROTECTED: Engine = Engine::Linux;

/// The profile the table gives `game`, installed in `install`: the one of
/// its storefront's id, else the one whose file the installation holds.
fn known(game: Game<'_>, install: &Path) -> Option<Profile> {
    let table = PROFILES.entries();
    table
        .iter()
        .find(|known| known.names(game))
        .or_else(|| table.iter().find(|known| known.found_in(install)))
        .map(Known::profile)
}

/// The profile of `game`, installed in `install`: the table's, else the one
/// of its Easy Anti-Cheat, else the defaults; with the installation's Easy
/// Anti-Cheat bootstrapper in any case. `engine` gives its engine alone, in
/// the same order.
pub(crate) fn of(game: Game<'_>, install: &Path) -> Profile {
    let easy_anti_cheat = easy_anti_cheat::of(install);
    let profile = match (known(game, install), &easy_anti_cheat) {
        (Some(profile), _) => profile,
        (None, Some(found)) => protected(install, found),
        (None, None) => Profile::default(),
    };
    Profile {
        bootstrapper: easy_anti_cheat
            .map(|found| found.bootstrapper.to_string_lossy().into_owned()),
        ..profile
    }
}

/// The engine `game`, installed in `install`, runs on without choices, as
/// `of` gives it: the table's, else the Linux engine when the installation
/// holds Easy Anti-Cheat, else Rosetta's Wine. Only an installation the table
/// does not name is searched for its anti-cheat.
pub(crate) fn engine(game: Game<'_>, install: &Path) -> Engine {
    match known(game, install) {
        Some(profile) => profile.engine,
        None if easy_anti_cheat::of(install).is_some() => PROTECTED,
        None => Profile::default().engine,
    }
}

/// The profile of a game the table does not name whose installation in
/// `install` holds `easy_anti_cheat`, which refuses Rosetta's Wine: the
/// Linux engine, where the anti-cheat loads its Linux module, with the
/// backend that renders the game's program there.
fn protected(install: &Path, easy_anti_cheat: &EasyAntiCheat) -> Profile {
    let engine = PROTECTED;
    let program = Program::open(&install.join(&easy_anti_cheat.game));
    Profile {
        engine,
        graphics: Graphics::default_for(engine, program.as_ref()),
        ..Profile::default()
    }
}

/// Every game the table Obelisk reads now names by a storefront's id, as
/// its storefront names it.
pub fn named() -> impl Iterator<Item = Game<'static>> {
    PROFILES.entries().iter().flat_map(|known| {
        let steam = known.steam.map(|app_id| Game::Steam { app_id });
        let epic = known.epic.as_ref().map(|epic| Game::Epic {
            namespace: &epic.namespace,
            app_name: &epic.app_name,
        });
        let gog = known
            .gog
            .as_deref()
            .map(|product_id| Game::Gog { product_id });
        [steam, epic, gog].into_iter().flatten()
    })
}

/// Reads the table again from Obelisk's `main` and keeps it in `library`.
pub async fn refresh(library: &Path) {
    PROFILES.refresh(library).await;
}

#[cfg(test)]
mod tests;
