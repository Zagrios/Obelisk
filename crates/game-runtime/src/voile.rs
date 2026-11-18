//! Voile, the Wine runtime Windows games run in. Obelisk runs the newest
//! release installed that keeps its contract: the paths, variables and
//! programs it relies on in a runtime, which docs/CONTRACT.md lists. As a
//! Windows game launches, Obelisk reads Voile's feed: the player takes a
//! newer release of its contract before the game starts, with what it brings
//! in view, or starts the game in the release in place unless it needs the
//! newer one. A Windows build's Transfer gets a release before the game's
//! files when none is installed. The release built into Obelisk stands in for
//! the feed until one was read. A player who takes beta releases runs Voile's
//! pre-releases too, whichever is newest; one who does not leaves a
//! pre-release in place for the newest release, as Obelisk leaves a
//! withdrawn release. A development runtime that voile's install-dev.sh put
//! in `current` takes precedence.
//!
//! The release also carries the Linux engine's runtime, in its `linux` folder.
use crate::runtimes::{Standing, Update};
use obelisk_core::transfer::CancelToken;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

mod feed;

use feed::Offered;
pub(crate) use feed::refresh;
pub use feed::take_prereleases;

const RELEASE: &str = include_str!("../voile-release.json");
/// Where install-dev.sh puts a locally built runtime.
const DEVELOPMENT: &str = "current";
/// What every release archive describes itself with.
const MANIFEST: &str = "runtime.json";
/// Unfinished work in the runtimes folder starts with this, and the next
/// install removes it.
const UNFINISHED: &str = ".";
/// The Linux engine's runtime, inside a release (or the development runtime).
const LINUX: &str = "linux";

/// Two installs never extract into the same folder.
static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A Voile release: its version and its archive.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// `voile-release.json`: the contract Obelisk keeps with Voile, and the
/// release Obelisk was built with, of that contract.
#[derive(Debug, Deserialize)]
struct BuiltIn {
    contract: u32,
    #[serde(flatten)]
    release: Release,
}

fn built_in() -> &'static BuiltIn {
    static VALUE: OnceLock<BuiltIn> = OnceLock::new();
    VALUE.get_or_init(|| serde_json::from_str(RELEASE).expect("checked-in Voile release"))
}

/// The release Obelisk was built with, which Transfers and launches install
/// until they read a feed.
pub fn release() -> &'static Release {
    &built_in().release
}

/// What Obelisk relies on in a runtime, as Voile's `CONTRACT` counts it:
/// Obelisk runs only releases of this contract.
pub fn contract() -> u32 {
    built_in().contract
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Failure {
    #[error("backend.voile.downloadFailed")]
    Download,
    #[error("backend.voile.damaged")]
    Damaged,
    #[error("backend.voile.storageFailed")]
    Storage,
    #[error("backend.voile.cancelled")]
    Cancelled,
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}

/// The runtime in `root` Windows games run in: the development runtime, else
/// the newest release installed there that Obelisk runs.
pub fn runtime(root: &Path) -> Option<PathBuf> {
    if let Some(development) = development(root) {
        return Some(development);
    }
    newest(root, &Offer::read(root)).map(|(_, runtime)| runtime)
}

/// The runtime install-dev.sh put in `root`, when there is one.
fn development(root: &Path) -> Option<PathBuf> {
    let development = root.join(DEVELOPMENT);
    has_wine(&development).then_some(development)
}

fn has_wine(runtime: &Path) -> bool {
    runtime.join("bin/wine").is_file()
}

/// Whether `runtime` holds the Wine of `release`, as its runtime.json names
/// it.
fn holds(runtime: &Path, release: &str) -> bool {
    has_wine(runtime) && version(runtime).as_deref() == Some(release)
}

/// The Linux engine's runtime in `root`: the development runtime's, else the
/// one inside the release Windows games run in.
pub fn linux_runtime(root: &Path) -> Option<PathBuf> {
    let development = root.join(DEVELOPMENT).join(LINUX);
    if has_engine(&development) {
        return Some(development);
    }
    let linux = newest(root, &Offer::read(root))?.1.join(LINUX);
    has_engine(&linux).then_some(linux)
}

/// The engine and the Wine it runs.
fn has_engine(runtime: &Path) -> bool {
    runtime.join("engine/fexdarwin-run").is_file() && runtime.join("wine/bin/wine").exists()
}

/// The Voile release Windows games run in now, as its runtime.json names it.
pub fn installed_release(root: &Path) -> Option<String> {
    release_of(&runtime(root)?)
}

/// The Voile release `runtime` comes from: a Rosetta runtime, or the Linux
/// engine's runtime inside one.
pub(crate) fn release_of(runtime: &Path) -> Option<String> {
    version(runtime).or_else(|| version(runtime.parent()?))
}

/// What a release says of itself in its runtime.json.
#[derive(Deserialize)]
struct Manifest {
    version: String,
    /// The releases before `CONTRACT` kept the first contract.
    #[serde(default = "first_contract")]
    contract: u32,
}

fn first_contract() -> u32 {
    1
}

fn manifest(runtime: &Path) -> Option<Manifest> {
    serde_json::from_slice(&fs::read(runtime.join(MANIFEST)).ok()?).ok()
}

fn version(runtime: &Path) -> Option<String> {
    manifest(runtime).map(|manifest| manifest.version)
}

/// The numbers that order a release's version, as Voile's tags write it:
/// `v<CrossOver version>-<revision>`, such as `v26.3.0-16`, which is
/// [26, 3, 0, 16, 1, 0]. Its betas come before it: `v26.3.0-16.beta2` is
/// [26, 3, 0, 16, 0, 2]. Anything else names no release.
fn numbers(version: &str) -> Option<Vec<u64>> {
    let version = version.strip_prefix('v')?;
    let (release, stage) = match version.split_once(".beta") {
        Some((release, beta)) => (release, [0, digits(beta)?]),
        None => (version, [1, 0]),
    };
    let (crossover, revision) = release.split_once('-')?;
    let mut numbers = crossover
        .split('.')
        .map(digits)
        .collect::<Option<Vec<_>>>()?;
    if numbers.len() != 3 {
        return None;
    }
    numbers.push(digits(revision)?);
    numbers.extend(stage);
    Some(numbers)
}

/// The number `text` writes in digits alone: no sign, unlike `str::parse`.
fn digits(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Whether `version` names a release, not a beta, as Voile's tags do, such
/// as `v26.3.0-16`. Profiles name releases only: an Obelisk older than betas
/// leaves out an entry whose `minimum_voile` it cannot read.
pub(crate) fn is_version(version: &str) -> bool {
    numbers(version).is_some_and(|numbers| numbers[4] == 1)
}

/// Whether `version` names a beta, such as `v26.3.0-16.beta2`: a pre-release
/// even where the feed no longer lists it.
fn is_beta(version: &str) -> bool {
    numbers(version).is_some_and(|numbers| numbers[4] == 0)
}

/// The newest release installed in `root` that Obelisk runs, with its
/// version's numbers: a folder named after the release its runtime.json
/// names, of Obelisk's contract, that `offer` does not leave.
fn newest(root: &Path, offer: &Offer) -> Option<(Vec<u64>, PathBuf)> {
    fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let order = numbers(&name)?;
            let runtime = entry.path();
            let manifest = manifest(&runtime)?;
            (has_wine(&runtime)
                && manifest.version == name
                && manifest.contract == contract()
                && !offer.leaves(&name))
            .then_some((order, runtime))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
}

/// What the last feed read in a runtimes' folder offers the player, as they
/// chose to take beta releases or not.
struct Offer {
    offered: Vec<Offered>,
    /// The player takes Voile's pre-releases too.
    prereleases: bool,
}

impl Offer {
    fn read(root: &Path) -> Self {
        Self {
            offered: feed::offered(root),
            prereleases: feed::takes_prereleases(root),
        }
    }

    /// Whether Obelisk leaves `offered` for the newest release left: it is
    /// withdrawn, or a pre-release while the player takes none.
    fn left(&self, offered: &Offered) -> bool {
        offered.withdrawn.is_some() || (offered.prerelease && !self.prereleases)
    }

    /// Whether Obelisk leaves the release named `version`, as `left` tells.
    /// A release the feed does not name stays, unless it is a beta while the
    /// player takes none: the feed keeps no beta older than its newest
    /// release.
    fn leaves(&self, version: &str) -> bool {
        match self
            .offered
            .iter()
            .find(|offered| offered.release.version == version)
        {
            Some(offered) => self.left(offered),
            None => !self.prereleases && is_beta(version),
        }
    }

    /// The releases Obelisk runs, with their version's numbers: of its
    /// contract, and not left.
    fn runnable(&self) -> impl Iterator<Item = (Vec<u64>, &Offered)> {
        self.offered
            .iter()
            .filter(|offered| offered.contract == contract() && !self.left(offered))
            .filter_map(|offered| Some((numbers(&offered.release.version)?, offered)))
    }

    /// The release Obelisk wants: the newest it runs, else the one it was
    /// built with.
    fn wanted(&self) -> Release {
        self.runnable()
            .max_by(|a, b| a.0.cmp(&b.0))
            .map_or_else(|| release().clone(), |(_, newest)| newest.release.clone())
    }
}

/// What a launch makes of the runtimes in `root`, for a game whose profile
/// needs `minimum` or a newer release. A newer release comes with what each
/// release newer than the one in place brings, up to it, newest first.
pub(crate) fn standing(root: &Path, minimum: Option<&str>) -> Standing {
    if development(root).is_some() {
        return Standing::Current;
    }
    let offer = Offer::read(root);
    let Some((installed, _)) = newest(root, &offer) else {
        return Standing::Missing;
    };
    let version = offer.wanted().version;
    let Some(wanted) = numbers(&version) else {
        return Standing::Current;
    };
    if installed >= wanted {
        return Standing::Current;
    }
    // A game that needs a newer release cannot skip the one wanted, when that
    // one is enough.
    let required = minimum
        .and_then(numbers)
        .is_some_and(|minimum| installed < minimum && wanted >= minimum);
    let mut newer: Vec<_> = offer
        .runnable()
        .filter(|(order, _)| *order > installed && *order <= wanted)
        .collect();
    newer.sort_by(|a, b| b.0.cmp(&a.0));
    let releases = newer
        .into_iter()
        .map(|(_, offered)| Notes {
            version: offered.release.version.clone(),
            notes: offered.notes.clone(),
        })
        .collect();
    Standing::Outdated(Update {
        version,
        required,
        releases,
    })
}

/// What a Voile release brings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notes {
    pub version: String,
    /// In Markdown, as its GitHub release tells it; empty when the feed has
    /// none for it.
    pub notes: String,
}

/// One install of a release, which the player can cancel and watch.
#[derive(Default)]
pub struct Install {
    received: AtomicU64,
    /// The size of the archive being installed, once known.
    total: AtomicU64,
    cancel: CancelToken,
}

impl Install {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The bytes downloaded so far, and the archive's size.
    pub fn progress(&self) -> (u64, u64) {
        let total = self.total.load(Ordering::Acquire);
        let total = if total == 0 { release().size } else { total };
        (self.received.load(Ordering::Acquire), total)
    }

    /// Installs in `root` the release Obelisk wants there once it read
    /// Voile's feed again, unless it or a development runtime is already
    /// there. It keeps the releases it replaces: a game may run from one, and
    /// `runtimes` decides when they go. `archive`, a local copy of the
    /// release Obelisk was built with, stands in for the feed and the
    /// download.
    pub async fn run(&self, root: &Path, archive: Option<&Path>) -> Result<PathBuf, Failure> {
        let release = match archive {
            Some(_) => release().clone(),
            None => {
                self.cancellable(feed::refresh(root)).await?;
                Offer::read(root).wanted()
            }
        };
        self.install(root, &release, archive).await
    }

    async fn install(
        &self,
        root: &Path,
        release: &Release,
        archive: Option<&Path>,
    ) -> Result<PathBuf, Failure> {
        let _installing = self.cancellable(INSTALLING.lock()).await?;
        if let Some(development) = development(root) {
            return Ok(development);
        }
        let destination = root.join(&release.version);
        if holds(&destination, &release.version) {
            return Ok(destination);
        }
        let extracted = self.extract(root, release, archive).await?;
        let runtime = extracted.path().join(format!("voile-{}", release.version));
        if !holds(&runtime, &release.version) {
            return Err(Failure::Damaged);
        }
        tokio::task::spawn_blocking(move || {
            if destination.exists() {
                fs::remove_dir_all(&destination)?;
            }
            fs::rename(&runtime, &destination)?;
            Ok(destination)
        })
        .await
        .map_err(|_| Failure::Storage)?
    }

    /// Checks `archive` (or downloads it) and extracts it into a folder of
    /// `root` that goes away unless the caller moves what it holds.
    async fn extract(
        &self,
        root: &Path,
        expected: &Release,
        archive: Option<&Path>,
    ) -> Result<tempfile::TempDir, Failure> {
        self.total.store(expected.size, Ordering::Release);
        let unfinished = root.to_owned();
        tokio::task::spawn_blocking(move || {
            fs::create_dir_all(&unfinished)?;
            remove_entries(&unfinished, |name| name.starts_with(UNFINISHED));
            Ok::<_, Failure>(())
        })
        .await
        .map_err(|_| Failure::Storage)??;
        let downloaded;
        let archive = match archive {
            Some(archive) => {
                let (path, expected_sha) = (archive.to_owned(), expected.sha256.clone());
                let matches = tokio::task::spawn_blocking(move || {
                    crate::sha(&path).is_ok_and(|sha| sha == expected_sha)
                })
                .await
                .map_err(|_| Failure::Storage)?;
                if !matches {
                    return Err(Failure::Damaged);
                }
                self.received.store(expected.size, Ordering::Release);
                archive
            }
            None => {
                downloaded = self.download(root, expected).await?;
                &*downloaded
            }
        };
        let extracted = tempfile::Builder::new()
            .prefix(UNFINISHED)
            .tempdir_in(root)?;
        let mut tar = tokio::process::Command::new("/usr/bin/tar")
            .arg("-xJf")
            .arg(archive)
            .arg("-C")
            .arg(extracted.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        if !self.cancellable(tar.wait()).await??.success() {
            return Err(Failure::Damaged);
        }
        Ok(extracted)
    }

    async fn download(
        &self,
        root: &Path,
        release: &Release,
    ) -> Result<tempfile::TempPath, Failure> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| Failure::Download)?;
        let mut response = self
            .cancellable(client.get(&release.url).send())
            .await?
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| Failure::Download)?;
        let mut file = tempfile::Builder::new()
            .prefix(UNFINISHED)
            .tempfile_in(root)?;
        let mut digest = Sha256::new();
        let mut size = 0_u64;
        while let Some(bytes) = self
            .cancellable(response.chunk())
            .await?
            .map_err(|_| Failure::Download)?
        {
            size += bytes.len() as u64;
            if size > release.size {
                return Err(Failure::Damaged);
            }
            digest.update(&bytes);
            file.write_all(&bytes)?;
            self.received.store(size, Ordering::Release);
        }
        if size != release.size || format!("{:x}", digest.finalize()) != release.sha256 {
            return Err(Failure::Damaged);
        }
        Ok(file.into_temp_path())
    }

    /// Stops waiting for `work` as soon as the install is cancelled.
    async fn cancellable<T>(&self, work: impl Future<Output = T>) -> Result<T, Failure> {
        self.cancel.race(work).await.map_err(|_| Failure::Cancelled)
    }
}

/// Removes the releases in `root` games no longer run in: all but the newest
/// Obelisk runs, beside the development runtime, the last feed read, the
/// player's choice of beta releases and unfinished installs. `runtimes` calls
/// it once a launch finds no Game session, of any storefront, that may run a
/// game from one.
pub fn remove_replaced(root: &Path) {
    let Some((_, kept)) = newest(root, &Offer::read(root)) else {
        return;
    };
    let kept = kept
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    remove_entries(root, |name| {
        name != DEVELOPMENT
            && Some(name) != kept.as_deref()
            && name != feed::KEPT
            && name != feed::PRERELEASES
            && !name.starts_with(UNFINISHED)
    });
}

/// Removes what `root` holds under the names `remove` picks. Whatever cannot be
/// removed now goes with a later install.
pub(crate) fn remove_entries(root: &Path, remove: impl Fn(&str) -> bool) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !remove(&name.to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        let _ = match entry.file_type() {
            Ok(kind) if kind.is_dir() => fs::remove_dir_all(path),
            _ => fs::remove_file(path),
        };
    }
}

#[cfg(test)]
mod tests;
