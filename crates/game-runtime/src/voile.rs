//! Voile, the Wine runtime Windows games run in. SailOver downloads the release
//! it pins on the first Windows launch; a development runtime that voile's
//! install-dev.sh put in `current` takes precedence.
//!
//! The release also carries the Linux engine's runtime, a second archive that
//! goes into the release's `linux` folder on the first launch that needs it.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

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

/// Two installs never extract into the same folder, and a release never goes
/// while its Linux runtime moves into it.
static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The Voile release SailOver runs Windows games with.
#[derive(Debug, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// The Linux engine's archive of the same release, when it has one.
    #[serde(default)]
    pub linux: Option<Asset>,
}

/// One more archive of a release.
#[derive(Debug, Deserialize)]
pub struct Asset {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// What an install downloads and checks.
struct Archive<'a> {
    url: &'a str,
    size: u64,
    sha256: &'a str,
}

impl Release {
    fn archive(&self) -> Archive<'_> {
        Archive {
            url: &self.url,
            size: self.size,
            sha256: &self.sha256,
        }
    }
}

impl Asset {
    fn archive(&self) -> Archive<'_> {
        Archive {
            url: &self.url,
            size: self.size,
            sha256: &self.sha256,
        }
    }
}

pub fn release() -> &'static Release {
    static VALUE: OnceLock<Release> = OnceLock::new();
    VALUE.get_or_init(|| serde_json::from_str(RELEASE).expect("checked-in Voile release"))
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
/// the pinned release once installed.
pub fn runtime(root: &Path) -> Option<PathBuf> {
    located(root, release())
}

fn located(root: &Path, release: &Release) -> Option<PathBuf> {
    let development = root.join(DEVELOPMENT);
    if has_wine(&development) {
        return Some(development);
    }
    let pinned = root.join(&release.version);
    (has_wine(&pinned) && version(&pinned).as_deref() == Some(release.version.as_str()))
        .then_some(pinned)
}

fn has_wine(runtime: &Path) -> bool {
    runtime.join("bin/wine").is_file()
}

/// The Linux engine's runtime in `root`: the development runtime's, else the
/// pinned release's once installed.
pub fn linux_runtime(root: &Path) -> Option<PathBuf> {
    located_linux(root, release())
}

fn located_linux(root: &Path, release: &Release) -> Option<PathBuf> {
    let development = root.join(DEVELOPMENT).join(LINUX);
    if has_engine(&development) {
        return Some(development);
    }
    let pinned = root.join(&release.version).join(LINUX);
    (has_engine(&pinned) && version(&pinned).as_deref() == Some(release.version.as_str()))
        .then_some(pinned)
}

/// The engine and the Wine it runs.
fn has_engine(runtime: &Path) -> bool {
    runtime.join("engine/fexdarwin-run").is_file() && runtime.join("wine/bin/wine").exists()
}

fn version(runtime: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Manifest {
        version: String,
    }
    let manifest = fs::read(runtime.join(MANIFEST)).ok()?;
    serde_json::from_slice::<Manifest>(&manifest)
        .ok()
        .map(|manifest| manifest.version)
}

/// One install of the pinned release, which the player can cancel and watch.
#[derive(Default)]
pub struct Install {
    received: AtomicU64,
    /// The size of the archive being installed, once known.
    total: AtomicU64,
    cancelled: AtomicBool,
    cancel: Notify,
}

impl Install {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancel.notify_waiters();
    }

    /// The bytes downloaded so far, and the archive's size.
    pub fn progress(&self) -> (u64, u64) {
        let total = self.total.load(Ordering::Acquire);
        let total = if total == 0 { release().size } else { total };
        (self.received.load(Ordering::Acquire), total)
    }

    /// Installs the pinned release in `root` unless a runtime is already there,
    /// then removes the releases it replaces. `archive`, a local copy of the
    /// release, stands in for the download while the release is private.
    pub async fn run(&self, root: &Path, archive: Option<&Path>) -> Result<PathBuf, Failure> {
        self.install(root, release(), archive).await
    }

    async fn install(
        &self,
        root: &Path,
        release: &Release,
        archive: Option<&Path>,
    ) -> Result<PathBuf, Failure> {
        let _installing = self.cancellable(INSTALLING.lock()).await?;
        if let Some(runtime) = located(root, release) {
            return Ok(runtime);
        }
        let extracted = self.extract(root, release.archive(), archive).await?;
        let runtime = extracted.path().join(format!("voile-{}", release.version));
        if !has_wine(&runtime) || version(&runtime).as_deref() != Some(release.version.as_str()) {
            return Err(Failure::Damaged);
        }
        let destination = root.join(&release.version);
        if destination.exists() {
            // A Linux runtime installed first stays with its release.
            let linux = destination.join(LINUX);
            if linux.exists() {
                fs::rename(&linux, runtime.join(LINUX))?;
            }
            fs::remove_dir_all(&destination)?;
        }
        fs::rename(&runtime, &destination)?;
        // No game runs from the releases removed here: only Steam launches
        // Windows games, and this install runs inside the one Game session
        // its Reservation gate admits.
        remove_entries(root, |name| {
            name != DEVELOPMENT && name != release.version && !name.starts_with(UNFINISHED)
        });
        Ok(destination)
    }

    /// Installs the pinned release's Linux engine runtime in its `linux` folder
    /// unless one is already there, then removes the releases it replaces.
    /// `archive` stands in for the download while the release is private.
    pub async fn run_linux(&self, root: &Path, archive: Option<&Path>) -> Result<PathBuf, Failure> {
        let release = release();
        let linux = release.linux.as_ref().ok_or(Failure::Download)?;
        self.install_linux(root, release, linux, archive).await
    }

    async fn install_linux(
        &self,
        root: &Path,
        release: &Release,
        linux: &Asset,
        archive: Option<&Path>,
    ) -> Result<PathBuf, Failure> {
        let _installing = self.cancellable(INSTALLING.lock()).await?;
        if let Some(runtime) = located_linux(root, release) {
            return Ok(runtime);
        }
        let extracted = self.extract(root, linux.archive(), archive).await?;
        let runtime = extracted
            .path()
            .join(format!("voile-linux-{}", release.version));
        if !has_engine(&runtime) || version(&runtime).as_deref() != Some(release.version.as_str()) {
            return Err(Failure::Damaged);
        }
        let folder = root.join(&release.version);
        fs::create_dir_all(&folder)?;
        let destination = folder.join(LINUX);
        if destination.exists() {
            fs::remove_dir_all(&destination)?;
        }
        fs::rename(&runtime, &destination)?;
        // Same reasoning as the Windows runtime's install: no game runs from
        // the releases removed here.
        remove_entries(root, |name| {
            name != DEVELOPMENT && name != release.version && !name.starts_with(UNFINISHED)
        });
        Ok(destination)
    }

    /// Checks `archive` (or downloads it) and extracts it into a folder of
    /// `root` that goes away unless the caller moves what it holds.
    async fn extract(
        &self,
        root: &Path,
        expected: Archive<'_>,
        archive: Option<&Path>,
    ) -> Result<tempfile::TempDir, Failure> {
        self.total.store(expected.size, Ordering::Release);
        fs::create_dir_all(root)?;
        remove_entries(root, |name| name.starts_with(UNFINISHED));
        let downloaded;
        let archive = match archive {
            Some(archive) => {
                let (path, expected_sha) = (archive.to_owned(), expected.sha256.to_owned());
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
                downloaded = self.download(root, &expected).await?;
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
        release: &Archive<'_>,
    ) -> Result<tempfile::TempPath, Failure> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| Failure::Download)?;
        let mut response = self
            .cancellable(client.get(release.url).send())
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
        let cancelled = self.cancel.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        tokio::select! {
            output = work => Ok(output),
            () = cancelled => Err(Failure::Cancelled),
        }
    }
}

/// Removes what `root` holds under the names `remove` picks. Whatever cannot be
/// removed now goes with a later install.
fn remove_entries(root: &Path, remove: impl Fn(&str) -> bool) {
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
