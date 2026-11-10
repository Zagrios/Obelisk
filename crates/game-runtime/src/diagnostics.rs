//! Diagnostics: the files that tell how a game ran, named once. The code
//! that writes them takes their names here, and a bug report finds them
//! here, so the two cannot drift apart.
use crate::queue::Storefront;
use obelisk_core::Platform;
use std::{
    fmt::Display,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// Obelisk's logs, in the user's home.
const LOGS: &str = "Library/Logs/Obelisk";
/// The words starting a launch's reports, in the order a report lists them.
const REPORTS: [&str; 2] = ["launch", "failed"];
/// An Obelisk report attaches the launch reports this recent.
const RECENT: Duration = Duration::from_secs(24 * 3600);

/// A game as its diagnostics name it: Steam's by app ID, Epic's by app name,
/// GOG's by product ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Game<'a> {
    Steam(u32),
    Epic(&'a str),
    Gog(&'a str),
}
impl<'a> From<crate::profile::Game<'a>> for Game<'a> {
    fn from(game: crate::profile::Game<'a>) -> Self {
        use crate::profile::Game::{Epic, Gog, Steam};
        // An Epic game's namespace names none of its files.
        match game {
            Steam { app_id } => Self::Steam(app_id),
            Epic { app_name, .. } => Self::Epic(app_name),
            Gog { product_id } => Self::Gog(product_id),
        }
    }
}
impl Game<'_> {
    fn store(self) -> Storefront {
        match self {
            Self::Steam(_) => Storefront::Steam,
            Self::Epic(_) => Storefront::Epic,
            Self::Gog(_) => Storefront::Gog,
        }
    }
    fn id(self) -> String {
        match self {
            Self::Steam(app_id) => app_id.to_string(),
            Self::Epic(id) | Self::Gog(id) => id.to_owned(),
        }
    }
}

/// Every build, in the order that wins a tie between their logs.
const BUILDS: [Platform; 2] = [Platform::Windows, Platform::Mac];

/// How a build's log starts: Wine runs a Windows build, a Mac build runs as
/// itself.
fn word(build: Platform) -> &'static str {
    match build {
        Platform::Windows => "wine",
        Platform::Mac => "game",
    }
}

/// `store`'s diagnostics folder under `home`.
pub fn folder(home: &Path, store: Storefront) -> PathBuf {
    home.join(LOGS).join(match store {
        Storefront::Steam => "Steam",
        Storefront::Epic => "Epic",
        Storefront::Gog => "GOG",
    })
}

/// The file a `build` of `game` writes its standard error to while it runs.
pub fn game_log(home: &Path, game: Game<'_>, build: Platform) -> PathBuf {
    folder(home, game.store()).join(format!("{}-{}.log", word(build), game.id()))
}

/// Opens `log` empty for a new launch. The last launch's log stays beside it
/// as `<name>.previous.log`: a player who relaunches a game that crashed
/// still has the crash's log to send.
pub(crate) fn open_log(log: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = fs::rename(log, previous_log(log));
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(log)
}

fn previous_log(log: &Path) -> PathBuf {
    log.with_extension("previous.log")
}

/// The log of the input service that serves the game logging to `log`.
pub(crate) fn input_log(log: &Path) -> PathBuf {
    log.with_extension("input.log")
}

/// The report of Steam launch `run` of `app_id`, in `folder`.
pub(crate) fn launch_report(folder: &Path, app_id: u32, run: uuid::Uuid) -> PathBuf {
    report(folder, REPORTS[0], app_id, run)
}

/// The report of failed Steam launch `run` of `app_id`, in `folder`.
pub(crate) fn failure_report(folder: &Path, app_id: u32, run: uuid::Uuid) -> PathBuf {
    report(folder, REPORTS[1], app_id, run)
}

fn report(folder: &Path, word: &str, id: impl Display, run: uuid::Uuid) -> PathBuf {
    folder.join(format!("{}{run}.json", report_prefix(word, id)))
}

/// The start of every `word` report of the game `id`. Its last `-` keeps the
/// reports of app 12456201 from counting as app 1245620's.
fn report_prefix(word: &str, id: impl Display) -> String {
    format!("{}{id}-", report_start(word))
}

/// The start of every `word` report, whichever game it is of.
fn report_start(word: &str) -> String {
    format!("{word}-")
}

/// The diagnostics a bug report attaches, most telling first. For `game`:
/// the logs of the build it launched last, then its newest launch and
/// failure report. For Obelisk: every launch and failure report of the day
/// before `now`, newest first. What cannot be read is left out.
pub fn files(home: &Path, game: Option<Game<'_>>, now: SystemTime) -> Vec<PathBuf> {
    match game {
        Some(game) => game_files(home, game),
        None => recent_reports(home, now),
    }
}

fn game_files(home: &Path, game: Game<'_>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    // A build's logs are dated by its last launch; the input log is not.
    let mut played: Option<(Platform, SystemTime)> = None;
    for build in BUILDS {
        let log = game_log(home, game, build);
        let Some(modified) = [modified(&log), modified(&previous_log(&log))]
            .into_iter()
            .flatten()
            .max()
        else {
            continue;
        };
        if played.is_none_or(|(_, best)| modified > best) {
            played = Some((build, modified));
        }
    }
    if let Some((build, _)) = played {
        let log = game_log(home, game, build);
        let logs = [previous_log(&log), input_log(&log)];
        files.extend(
            std::iter::once(log)
                .chain(logs)
                .filter(|path| modified(path).is_some()),
        );
    }
    let listing = listing(&folder(home, game.store()));
    for word in REPORTS {
        let prefix = report_prefix(word, game.id());
        let mut newest: Option<&(String, PathBuf, SystemTime)> = None;
        for entry in &listing {
            let (name, _, modified) = entry;
            if name.starts_with(&prefix)
                && name.ends_with(".json")
                && newest.is_none_or(|(.., best)| modified > best)
            {
                newest = Some(entry);
            }
        }
        files.extend(newest.map(|(_, path, _)| path.clone()));
    }
    files
}

fn recent_reports(home: &Path, now: SystemTime) -> Vec<PathBuf> {
    let since = now.checked_sub(RECENT);
    let mut reports: Vec<(PathBuf, SystemTime)> =
        [Storefront::Steam, Storefront::Epic, Storefront::Gog]
            .into_iter()
            .flat_map(|store| listing(&folder(home, store)))
            .filter(|(name, _, modified)| {
                REPORTS
                    .iter()
                    .any(|word| name.starts_with(&report_start(word)))
                    && name.ends_with(".json")
                    && since.is_none_or(|since| *modified >= since)
            })
            .map(|(_, path, modified)| (path, modified))
            .collect();
    reports.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    reports.into_iter().map(|(path, _)| path).collect()
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// The files in `folder` with their names and when they last changed,
/// hidden ones left out; nothing when it cannot be read.
fn listing(folder: &Path) -> Vec<(String, PathBuf, SystemTime)> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            let modified = modified(&path)?;
            Some((name, path, modified))
        })
        .collect()
}

#[cfg(test)]
mod tests;
