//! Voile, the Wine runtime Windows games run in. SailOver downloads the release
//! it pins on the first Windows launch; a development runtime that voile's
//! install-dev.sh put in `current` takes precedence.
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

/// The Voile release SailOver runs Windows games with.
#[derive(Debug, Deserialize)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
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
        (self.received.load(Ordering::Acquire), release().size)
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
        // Two launches never extract into the same folder.
        static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _installing = self.cancellable(INSTALLING.lock()).await?;
        if let Some(runtime) = located(root, release) {
            return Ok(runtime);
        }
        fs::create_dir_all(root)?;
        remove_entries(root, |name| name.starts_with(UNFINISHED));
        let downloaded;
        let archive = match archive {
            Some(archive) => {
                let (path, release_sha) = (archive.to_owned(), release.sha256.clone());
                let matches = tokio::task::spawn_blocking(move || {
                    crate::sha(&path).is_ok_and(|sha| sha == release_sha)
                })
                .await
                .map_err(|_| Failure::Storage)?;
                if !matches {
                    return Err(Failure::Damaged);
                }
                self.received.store(release.size, Ordering::Release);
                archive
            }
            None => {
                downloaded = self.download(root, release).await?;
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
        let runtime = extracted.path().join(format!("voile-{}", release.version));
        if !has_wine(&runtime) || version(&runtime).as_deref() != Some(release.version.as_str()) {
            return Err(Failure::Damaged);
        }
        let destination = root.join(&release.version);
        if destination.exists() {
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
