//! What Obelisk knows about particular Windows games, whatever their
//! storefront: the engine that runs one, the graphics backend its Direct3D
//! goes through there, the program started instead of its publisher's, what
//! that program needs besides (arguments, environment variables and DLL
//! overrides), and the oldest Voile release it runs in. The table is
//! `profiles.json`. Obelisk is built with it, and a release build reads it
//! again from Obelisk's `main` as a Windows game launches, keeping the last
//! one beside its library; a development build keeps the one it was built
//! with. Each entry is checked as it is read, and one that sets what a
//! profile may not is left out. It names a game by its storefronts' ids, or
//! by a file of its installation, which finds the game whatever the
//! storefront. Every game it does not name runs with the defaults, in
//! Rosetta's Wine with D3DMetal. The player's launch settings apply over it.
use crate::wine::{Engine, Graphics, GraphicsOptions};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::{LazyLock, Mutex},
};

const TABLE: &str = include_str!("../profiles.json");
/// Where the table is on Obelisk's `main`.
const URL: &str =
    "https://raw.githubusercontent.com/Zagrios/Obelisk/main/crates/game-runtime/profiles.json";
/// The table's format this Obelisk reads. Another one is a table it ignores.
const FORMAT: u32 = 1;
/// The last table read, in Obelisk's library.
const KEPT: &str = "profiles.json";
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Profile {
    pub engine: Engine,
    /// What renders its Direct3D on that engine.
    pub graphics: Graphics,
    /// What it asks of that backend.
    pub options: GraphicsOptions,
    /// The program the game starts with instead of its publisher's, relative
    /// to the installation, when that one cannot run there.
    pub executable: Option<&'static str>,
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
}

static NONE: BTreeMap<String, String> = BTreeMap::new();

impl Default for Profile {
    fn default() -> Self {
        Self {
            engine: Engine::Rosetta,
            graphics: Graphics::default_on(Engine::Rosetta),
            options: GraphicsOptions::default(),
            executable: None,
            arguments: &[],
            variables: &NONE,
            dll_overrides: &NONE,
            minimum_voile: None,
        }
    }
}

/// The table, as `profiles.json` holds it: each game as written there, which
/// `checked` reads.
#[derive(Debug, Deserialize)]
struct Table {
    format: u32,
    games: Vec<serde_json::Value>,
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
}

/// An Epic game as a table names it: its namespace and app name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EpicGame {
    namespace: String,
    app_name: String,
}

/// The table launches read now.
struct Current {
    games: &'static [Known],
    /// The table read last, as it came: none until one is read, which
    /// leaves the one Obelisk was built with. A read that brings the same
    /// leaves `games` in place.
    read: Option<Vec<u8>>,
}

static CURRENT: LazyLock<Mutex<Current>> = LazyLock::new(|| {
    Mutex::new(Current {
        games: Vec::leak(parsed(TABLE.as_bytes()).expect("checked-in profiles")),
        read: None,
    })
});

fn table() -> &'static [Known] {
    CURRENT.lock().unwrap().games
}

/// The entries of `table` that `checked` keeps, when it is a table of the
/// format Obelisk reads.
fn parsed(table: &[u8]) -> Option<Vec<Known>> {
    let table: Table = serde_json::from_slice(table).ok()?;
    (table.format == FORMAT).then(|| table.games.into_iter().filter_map(checked).collect())
}

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
            arguments: &self.arguments,
            variables: &self.variables,
            dll_overrides: &self.dll_overrides,
            minimum_voile: self.minimum_voile.as_deref(),
        }
    }

    /// Whether the installation in `install` holds this entry's file.
    fn found_in(&self, install: &Path) -> bool {
        self.path
            .as_ref()
            .is_some_and(|path| install.join(path).is_file())
    }
}

/// The profile of `game`, installed in `install`: the one the table gives
/// its storefront's id, else the one whose file the installation holds, else
/// the defaults.
pub(crate) fn of(game: Game<'_>, install: &Path) -> Profile {
    let table = table();
    table
        .iter()
        .find(|known| known.names(game))
        .or_else(|| table.iter().find(|known| known.found_in(install)))
        .map(Known::profile)
        .unwrap_or_default()
}

/// What starts `game` in the player's language, `locale` such as `it_IT`,
/// when the game reads its language from its command line rather than from
/// Windows, and the player put no language of their own in `chosen`, their
/// arguments. Rocket League, from Epic or Steam, takes `-language=` in any
/// case for the languages below; another one leaves it to choose. This is no
/// table field: the builds that read the table from `main` refuse an entry
/// with a field they do not know.
pub(crate) fn language_argument(game: Game<'_>, chosen: &[String], locale: &str) -> Option<String> {
    let rocket_league = matches!(
        game,
        Game::Epic {
            namespace: "9773aa1aa54f4f7b80e44bef04986cea",
            ..
        } | Game::Steam { app_id: 252950 }
    );
    let chose = chosen
        .iter()
        .any(|argument| argument.to_ascii_lowercase().starts_with("-language="));
    if !rocket_league || chose {
        return None;
    }
    let value = match locale.split('_').next()? {
        "en" => "INT",
        "fr" => "FRA",
        "it" => "ITA",
        "de" => "DEU",
        _ => return None,
    };
    Some(format!("-language={value}"))
}

/// Every game the table launches read now names by a storefront's id, as
/// its storefront names it.
pub fn named() -> impl Iterator<Item = Game<'static>> {
    table().iter().flat_map(|known| {
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
/// Without the network, launches read the last one kept, else the one
/// Obelisk was built with. A development build keeps the one it was built
/// with, as edited.
pub(crate) async fn refresh(library: &Path) {
    if cfg!(debug_assertions) {
        return;
    }
    let kept = library.join(KEPT);
    match crate::remote::fetched(URL).await {
        Some(table) if read(&table) => {
            let _ = crate::remote::keep(&kept, &table);
        }
        _ => {
            let unread = CURRENT.lock().unwrap().read.is_none();
            if unread && let Ok(table) = fs::read(&kept) {
                read(&table);
            }
        }
    }
}

/// Makes `table` the one launches read, when it is a table Obelisk reads
/// with at least one game. Whether it is.
fn read(table: &[u8]) -> bool {
    let mut current = CURRENT.lock().unwrap();
    if current.read.as_deref().unwrap_or(TABLE.as_bytes()) != table {
        let Some(games) = parsed(table).filter(|games| !games.is_empty()) else {
            return false;
        };
        current.games = Vec::leak(games);
    }
    current.read = Some(table.to_vec());
    true
}

#[cfg(test)]
mod tests;
