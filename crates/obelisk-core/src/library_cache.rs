//! The last library each storefront account loaded, in one JSON file in the
//! library directory: `{"version": 1, "<storefront>": {"<account>": [games]}}`, each
//! game a JSON object named by its `id`. A library shows from it at once, then
//! updates from its storefront. Each game also keeps `lastLaunchedAt`, when
//! the account last launched it from Obelisk.
//!
//! The cache only spares the wait for a storefront: an unreadable file has no
//! library, the next change replaces it, and a failed write is retried by the
//! next change.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::date;

/// Its name in the library directory.
pub const FILE_NAME: &str = "library-cache.json";
const VERSION: u64 = 1;
const LAUNCH: &str = "lastLaunchedAt";
/// How long a write waits for the changes that follow the first.
const WRITE_DELAY: Duration = Duration::from_millis(500);

/// One game as the cache keeps it.
pub type Entry = Map<String, Value>;

/// One game's last launch from Obelisk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub storefront: String,
    pub account: String,
    pub game: String,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

/// The cache of one library directory. Every cache of a directory in this
/// process shares one document, so none overwrites another's changes. Changes
/// reach the file off the caller's thread.
#[derive(Clone)]
pub struct LibraryCache {
    shared: Arc<Shared>,
}

static OPEN: LazyLock<Mutex<HashMap<PathBuf, Weak<Shared>>>> = LazyLock::new(Default::default);

impl LibraryCache {
    pub fn open(directory: impl AsRef<Path>) -> Self {
        let file = directory.as_ref().join(FILE_NAME);
        let mut open = OPEN.lock().unwrap();
        if let Some(shared) = open.get(&file).and_then(Weak::upgrade) {
            return Self { shared };
        }
        open.retain(|_, shared| shared.strong_count() > 0);
        let shared = Arc::new(Shared {
            document: Mutex::new(Document::read(&file)),
            file: file.clone(),
            written: Mutex::new(0),
            scheduled: AtomicBool::new(false),
        });
        open.insert(file, Arc::downgrade(&shared));
        Self { shared }
    }

    /// The account's last library, each game as `read` reads it; empty when
    /// it has none.
    pub fn library<T>(
        &self,
        storefront: &str,
        account: &str,
        read: impl FnMut(&Entry) -> Option<T>,
    ) -> Vec<T> {
        let document = self.shared.document.lock().unwrap();
        document
            .libraries
            .get(storefront)
            .and_then(|libraries| libraries.get(account))
            .map_or_else(Vec::new, |entries| {
                entries.iter().filter_map(read).collect()
            })
    }

    /// Where a session signed in to `account` keeps the libraries it loads,
    /// until the account is forgotten.
    pub fn account(&self, storefront: &str, account: &str) -> AccountLibrary {
        let key = (storefront.to_owned(), account.to_owned());
        let forgotten = self.shared.document.lock().unwrap().forgotten(&key);
        AccountLibrary {
            cache: self.clone(),
            key,
            forgotten,
        }
    }

    /// Records that the account launched `game` from Obelisk, in the game's
    /// entry. A game whose library was never saved has no entry to keep it.
    pub fn record_launch(&self, storefront: &str, account: &str, game: &str, at: u64) {
        self.shared.change(|document| {
            let Some(entry) = document
                .libraries
                .get_mut(storefront)
                .and_then(|libraries| libraries.get_mut(account))
                .and_then(|entries| entries.iter_mut().find(|entry| id(entry) == Some(game)))
            else {
                return false;
            };
            entry.insert(LAUNCH.into(), Value::String(iso8601(at)));
            true
        });
    }

    /// Every game's last launch, for every storefront and account.
    pub fn launches(&self) -> Vec<Launch> {
        let document = self.shared.document.lock().unwrap();
        let mut launches = Vec::new();
        for (storefront, libraries) in &document.libraries {
            for (account, entries) in libraries {
                launches.extend(entries.iter().filter_map(|entry| {
                    Some(Launch {
                        storefront: storefront.clone(),
                        account: account.clone(),
                        game: id(entry)?.to_owned(),
                        at: launched_at(entry)?,
                    })
                }));
            }
        }
        launches
    }

    /// Removes the account's library and its launches, as signing it out
    /// does. A session bound to the account before can no longer write it.
    pub fn forget(&self, storefront: &str, account: &str) {
        self.shared.change(|document| {
            let key = (storefront.to_owned(), account.to_owned());
            *document.forgotten.entry(key).or_default() += 1;
            document
                .libraries
                .get_mut(storefront)
                .and_then(|libraries| libraries.remove(account))
                .is_some()
        });
    }

    /// Writes the last change now, on the caller's thread.
    pub fn flush(&self) {
        self.shared.write();
    }
}

/// One account's library, as the session signed in to it loads it.
#[derive(Clone)]
pub struct AccountLibrary {
    cache: LibraryCache,
    key: (String, String),
    /// How many times the account was forgotten when the session bound it.
    forgotten: u64,
}

impl AccountLibrary {
    /// Replaces the account's library with `games`. Each game keeps its last
    /// launch, and the fields `kept` that its previous entry had and `games`
    /// lacks, such as details another request loads.
    pub fn replace(&self, games: Vec<Entry>, kept: &[&str]) {
        self.change(|libraries, account| {
            let previous = libraries.get(account);
            let by_id: HashMap<&str, &Entry> = previous
                .into_iter()
                .flatten()
                .filter_map(|entry| Some((id(entry)?, entry)))
                .collect();
            let games: Vec<Entry> = games
                .into_iter()
                .map(|mut game| {
                    if let Some(old) = id(&game).and_then(|id| by_id.get(id)) {
                        for key in kept.iter().copied().chain([LAUNCH]) {
                            if !game.contains_key(key)
                                && let Some(value) = old.get(key)
                            {
                                game.insert(key.into(), value.clone());
                            }
                        }
                    }
                    game
                })
                .collect();
            // A library loaded again unchanged is not written again.
            if previous == Some(&games) {
                return false;
            }
            libraries.insert(account.to_owned(), games);
            true
        });
    }

    /// The account's library, each game as `read` reads it; empty when it has
    /// none.
    pub fn read<T>(&self, read: impl FnMut(&Entry) -> Option<T>) -> Vec<T> {
        let (storefront, account) = &self.key;
        self.cache.library(storefront, account, read)
    }

    /// Changes every game of the account's library, as a request that loaded
    /// more of each does; `change` tells whether it changed the game. An
    /// account with no library is left without one.
    pub fn update(&self, mut change: impl FnMut(&mut Entry) -> bool) {
        self.change(|libraries, account| {
            let Some(entries) = libraries.get_mut(account) else {
                return false;
            };
            entries
                .iter_mut()
                .fold(false, |changed, entry| change(entry) | changed)
        });
    }

    fn change(&self, change: impl FnOnce(&mut BTreeMap<String, Vec<Entry>>, &str) -> bool) {
        let (storefront, account) = &self.key;
        self.cache.shared.change(|document| {
            if document.forgotten(&self.key) != self.forgotten {
                return false;
            }
            change(
                document.libraries.entry(storefront.clone()).or_default(),
                account,
            )
        });
    }
}

struct Shared {
    file: PathBuf,
    document: Mutex<Document>,
    /// The document revision on disk, held while it is written.
    written: Mutex<u64>,
    /// A write waits for the changes that follow the first.
    scheduled: AtomicBool,
}

impl Shared {
    fn change(self: &Arc<Self>, change: impl FnOnce(&mut Document) -> bool) {
        let mut document = self.document.lock().unwrap();
        if !change(&mut document) {
            return;
        }
        document.revision += 1;
        drop(document);
        // Changes in quick succession, such as a library's batches of
        // platforms, share one write.
        if !self.scheduled.swap(true, Ordering::AcqRel) {
            let shared = self.clone();
            std::thread::spawn(move || {
                std::thread::sleep(WRITE_DELAY);
                shared.scheduled.store(false, Ordering::Release);
                shared.write();
            });
        }
    }

    /// Writes the document, unless a later write already did.
    fn write(&self) {
        let mut written = self.written.lock().unwrap();
        let (revision, bytes) = {
            let document = self.document.lock().unwrap();
            if document.revision <= *written {
                return;
            }
            (document.revision, document.bytes())
        };
        if let Some(directory) = self.file.parent() {
            let _ = fs::create_dir_all(directory);
        }
        if bytes.is_some_and(|bytes| crate::write_atomic(&self.file, &bytes).is_ok()) {
            *written = revision;
        }
    }
}

#[derive(Default)]
struct Document {
    /// Each storefront's libraries, by account.
    libraries: BTreeMap<String, BTreeMap<String, Vec<Entry>>>,
    revision: u64,
    /// How many times each storefront account was forgotten.
    forgotten: HashMap<(String, String), u64>,
}

/// The file: its version beside each storefront's libraries.
#[derive(Serialize, Deserialize)]
struct File<Libraries> {
    version: u64,
    #[serde(flatten)]
    libraries: Libraries,
}

impl Document {
    fn read(file: &Path) -> Self {
        let file = fs::read(file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<File<_>>(&bytes).ok())
            .filter(|file| file.version == VERSION);
        Self {
            libraries: file.map(|file| file.libraries).unwrap_or_default(),
            ..Self::default()
        }
    }

    fn bytes(&self) -> Option<Vec<u8>> {
        serde_json::to_vec(&File {
            version: VERSION,
            libraries: &self.libraries,
        })
        .ok()
    }

    fn forgotten(&self, key: &(String, String)) -> u64 {
        self.forgotten.get(key).copied().unwrap_or(0)
    }
}

fn id(entry: &Entry) -> Option<&str> {
    entry.get("id")?.as_str()
}

/// When the account last launched the entry's game from Obelisk, in seconds
/// since the Unix epoch.
pub fn launched_at(entry: &Entry) -> Option<u64> {
    entry.get(LAUNCH)?.as_str().and_then(parse_iso8601)
}

/// `at` in UTC, as Swift's `ISO8601DateFormatter` writes a date.
fn iso8601(at: u64) -> String {
    let (days, seconds) = (at / 86_400, at % 86_400);
    let (year, month, day) = date::civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn parse_iso8601(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let separators = [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'Z'),
    ];
    if bytes.len() != 20 || separators.iter().any(|&(at, byte)| bytes[at] != byte) {
        return None;
    }
    let number = |start: usize, end: usize| text[start..end].parse::<u64>().ok();
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = date::days_from_civil(year as i64, month as i64, day as i64) as u64;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests;
