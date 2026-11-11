//! Easy Anti-Cheat in a Windows game's installation. Its bootstrapper, which
//! each game names its own way (start_protected_game.exe for ELDEN RING,
//! RocketLeague_EAC.exe, DeadByDaylight.exe), reads the EasyAntiCheat folder
//! beside it, loads the anti-cheat, then starts the game's program its
//! settings there name. The anti-cheat refuses Rosetta's Wine. On the Linux
//! engine it loads its Linux module when the game's publisher allows Linux,
//! which only the anti-cheat's servers know.
use crate::pe::Program;
use serde::Deserialize;
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
};

/// The folder beside the bootstrapper, and its settings there, whatever
/// their case.
const FOLDER: &str = "EasyAntiCheat";
const SETTINGS: &str = "Settings.json";
/// How every bootstrapper describes itself, whatever its file's name: Epic
/// Online Services' adds " (EOS)".
const BOOTSTRAPPER: &str = "Easy Anti-Cheat Bootstrapper";
/// How deep below the installation the bootstrapper's folder lies at most:
/// Rocket League's is Binaries/Win64.
const DEPTH: usize = 2;
/// At most this many folders are looked into, and a settings file holds at
/// most this many bytes.
const FOLDERS: usize = 256;
const SETTINGS_SIZE: usize = 64 * 1024;

/// The Easy Anti-Cheat of one installation, by paths relative to it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EasyAntiCheat {
    /// The program that loads the anti-cheat, then starts the game.
    pub bootstrapper: PathBuf,
    /// The game's program it starts.
    pub game: PathBuf,
}

/// The Easy Anti-Cheat of the installation in `install`, when it holds one: a
/// bootstrapper beside an EasyAntiCheat folder whose settings name the game's
/// program, in the installation's folder or a folder at most two below. The
/// shallowest one wins; links are not followed.
pub(crate) fn of(install: &Path) -> Option<EasyAntiCheat> {
    let mut folders = VecDeque::from([(PathBuf::new(), 0)]);
    for _ in 0..FOLDERS {
        let (folder, depth) = folders.pop_front()?;
        let entries = entries(&install.join(&folder));
        if let Some(found) = beside(install, &folder, &entries) {
            return Some(found);
        }
        if depth < DEPTH {
            folders.extend(
                entries
                    .into_iter()
                    .filter(|entry| entry.folder && !entry.name.starts_with('.'))
                    .map(|entry| (folder.join(entry.name), depth + 1)),
            );
        }
    }
    None
}

/// One entry of a folder.
struct Entry {
    name: String,
    folder: bool,
}

/// The files and folders in `folder`, by name, without links.
fn entries(folder: &Path) -> Vec<Entry> {
    let Ok(read) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter_map(|entry| {
            let kind = entry.file_type().ok()?;
            let name = entry.file_name().into_string().ok()?;
            (!kind.is_symlink()).then(|| Entry {
                name,
                folder: kind.is_dir(),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// The entry of `entries` named `name` in any case, a folder or a file.
fn named<'a>(entries: &'a [Entry], name: &str, folder: bool) -> Option<&'a Entry> {
    entries
        .iter()
        .find(|entry| entry.folder == folder && entry.name.eq_ignore_ascii_case(name))
}

/// The Easy Anti-Cheat whose bootstrapper is in `folder` of `install`,
/// which holds `entries`.
fn beside(install: &Path, folder: &Path, entries: &[Entry]) -> Option<EasyAntiCheat> {
    let anti_cheat = install
        .join(folder)
        .join(&named(entries, FOLDER, true)?.name);
    let game = game(&anti_cheat)?;
    let bootstrapper = entries
        .iter()
        .filter(|entry| {
            !entry.folder
                && Path::new(&entry.name)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        })
        .map(|entry| folder.join(&entry.name))
        .find(|path| {
            Program::open(&install.join(path))
                .and_then(|program| program.description())
                .is_some_and(|description| description.starts_with(BOOTSTRAPPER))
        })?;
    Some(EasyAntiCheat {
        bootstrapper,
        game: folder.join(game),
    })
}

/// The game's program the settings in `anti_cheat`, an EasyAntiCheat folder,
/// name: relative to the bootstrapper's folder, never outside it.
fn game(anti_cheat: &Path) -> Option<PathBuf> {
    #[derive(Deserialize)]
    struct Settings {
        executable: String,
    }
    let file = anti_cheat.join(&named(&entries(anti_cheat), SETTINGS, false)?.name);
    let bytes = obelisk_core::install::read_file_bounded(&file, SETTINGS_SIZE).ok()??;
    // ELDEN RING's starts with a byte order mark, which JSON does not allow.
    let json = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let settings: Settings = serde_json::from_slice(&separators_escaped(json)).ok()?;
    obelisk_core::install::safe_relative_path(&settings.executable).ok()
}

/// `json` with each backslash a publisher wrote as a path separator doubled,
/// as JSON wants it. Halo Infinite's settings name `game\HaloInfinite.exe`
/// with a single one, which the bootstrapper reads as that path and JSON
/// refuses. A backslash stays as written before another one, a quote, a
/// slash or a `uXXXX` code, the escapes a path can hold. Before any other
/// character it is a separator, even where JSON has an escape, as the `\b`
/// of `game\bin`.
fn separators_escaped(json: &[u8]) -> Vec<u8> {
    let mut escaped = Vec::with_capacity(json.len());
    let mut rest = json;
    while let Some((&byte, after)) = rest.split_first() {
        escaped.push(byte);
        rest = after;
        if byte != b'\\' {
            continue;
        }
        match after {
            [next @ (b'\\' | b'"' | b'/'), tail @ ..] => {
                escaped.push(*next);
                rest = tail;
            }
            [b'u', code @ ..]
                if code
                    .get(..4)
                    .is_some_and(|code| code.iter().all(u8::is_ascii_hexdigit)) => {}
            _ => escaped.push(b'\\'),
        }
    }
    escaped
}

#[cfg(test)]
pub(crate) mod tests;
