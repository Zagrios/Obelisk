//! What Obelisk reads anew from GitHub, fetched within a few seconds and kept
//! for when it finds no network: the tables of games it is built with and
//! reads again from its `main`, the game profiles and the notes, and Voile's
//! feed, which reads its own.
use serde::Deserialize;
use std::{
    borrow::Cow,
    fs,
    future::Future,
    io,
    path::Path,
    sync::{Mutex, OnceLock},
    time::Duration,
};

/// How long Obelisk waits for what it reads anew before going on with the
/// last one kept.
const WAIT: Duration = Duration::from_secs(3);
/// Where Obelisk's tables are on its `main`, each by its file's name.
const MAIN: &str = "https://raw.githubusercontent.com/Zagrios/Obelisk/main/crates/game-runtime/";
/// The tables' format this Obelisk reads. Another one is a table it ignores.
const FORMAT: u32 = 1;

/// The body at `url`, over HTTPS, unless it takes longer than `WAIT`. One
/// client serves every read, so a launch reuses the connections of the last.
pub(crate) async fn fetched(url: &str) -> Option<Vec<u8>> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = match CLIENT.get() {
        Some(client) => client,
        None => {
            let client = reqwest::Client::builder().https_only(true).build().ok()?;
            CLIENT.get_or_init(|| client)
        }
    };
    let response = client
        .get(url)
        .timeout(WAIT)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    Some(response.bytes().await.ok()?.to_vec())
}

/// Writes `bytes` at `path` in one step, unless they are there already.
pub(crate) fn keep(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|kept| kept == bytes) {
        return Ok(());
    }
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    obelisk_core::write_atomic(path, bytes)
}

/// A table as its file holds it: each game as written there, which the
/// table's entry check reads.
#[derive(Deserialize)]
struct File {
    format: u32,
    games: Vec<serde_json::Value>,
}

/// A table of games Obelisk is built with and reads again from its `main`,
/// keeping the last one read in its library under the same name: the game
/// profiles or the notes. Lookups read the one read from `main`, else the
/// one kept last, else the one Obelisk was built with; the kept one is read
/// once, at the first refresh, before `main` answers. A table of another
/// format, or one that leaves no entry once each is checked, is neither read
/// nor kept. A development build keeps the one it was built with, as edited.
pub(crate) struct Table<E: 'static> {
    name: &'static str,
    entry: fn(serde_json::Value) -> Option<E>,
    current: Mutex<Current<E>>,
}

/// The entries lookups read now, and the table they come from.
struct Current<E: 'static> {
    /// Leaked: a launch may hold its game's entry while another table is read.
    entries: &'static [E],
    /// The table `entries` come from, as it came: one that brings the same
    /// leaves them in place.
    bytes: Cow<'static, [u8]>,
    /// Whether the kept table was looked for, which only the first refresh
    /// does.
    kept_read: bool,
}

impl<E: Send + Sync + 'static> Table<E> {
    /// The table named `name`, built with `built_in`, whose entries `entry`
    /// checks one by one: an entry it refuses is left out.
    pub(crate) fn new(
        name: &'static str,
        built_in: &'static str,
        entry: fn(serde_json::Value) -> Option<E>,
    ) -> Self {
        let entries = parsed(built_in.as_bytes(), entry).unwrap_or_default();
        Self {
            name,
            entry,
            current: Mutex::new(Current {
                entries: Vec::leak(entries),
                bytes: Cow::Borrowed(built_in.as_bytes()),
                kept_read: false,
            }),
        }
    }

    /// The entries read now.
    pub(crate) fn entries(&self) -> &'static [E] {
        self.current.lock().unwrap().entries
    }

    /// Reads the table again from Obelisk's `main` and keeps it in `library`.
    /// Nothing in a development build.
    pub(crate) async fn refresh(&self, library: &Path) {
        if cfg!(debug_assertions) {
            return;
        }
        let url = url(self.name);
        self.read_again(library, fetched(&url)).await;
    }

    /// `refresh`, with `fetched` as `main`'s answer: the kept table is read
    /// before it is awaited.
    async fn read_again(&self, library: &Path, fetched: impl Future<Output = Option<Vec<u8>>>) {
        let kept = library.join(self.name);
        {
            // Looked for and read under one lock, so a refresh that overlaps
            // this one never brings an older table back.
            let mut current = self.current.lock().unwrap();
            if !current.kept_read {
                current.kept_read = true;
                if let Ok(table) = fs::read(&kept) {
                    read(&mut current, &table, self.entry);
                }
            }
        }
        let Some(table) = fetched.await else {
            return;
        };
        if read(&mut self.current.lock().unwrap(), &table, self.entry) {
            let _ = keep(&kept, &table);
        }
    }
}

/// Makes `bytes` the table `current` reads, when Obelisk reads it. Whether
/// it does.
fn read<E>(
    current: &mut Current<E>,
    bytes: &[u8],
    entry: fn(serde_json::Value) -> Option<E>,
) -> bool {
    if !current.entries.is_empty() && *current.bytes == *bytes {
        return true;
    }
    let Some(entries) = parsed(bytes, entry) else {
        return false;
    };
    current.entries = Vec::leak(entries);
    current.bytes = Cow::Owned(bytes.to_vec());
    true
}

/// The entries of `table` that `entry` keeps, when it is a table of the
/// format Obelisk reads with at least one of them.
fn parsed<E>(table: &[u8], entry: fn(serde_json::Value) -> Option<E>) -> Option<Vec<E>> {
    let file: File = serde_json::from_slice(table).ok()?;
    (file.format == FORMAT)
        .then(|| file.games.into_iter().filter_map(entry).collect::<Vec<_>>())
        .filter(|entries| !entries.is_empty())
}

/// Where the table named `name` is on Obelisk's `main`.
fn url(name: &str) -> String {
    format!("{MAIN}{name}")
}

#[cfg(test)]
mod tests;
