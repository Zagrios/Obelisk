//! Valve's Steam components, the libraries Obelisk's Steam host loads. Obelisk
//! ships none of them: a Steam game's Transfer downloads the public packages
//! the runtime manifest pins from Valve's CDN before the game's files, and a
//! launch that still misses them downloads them first. Only the files the
//! manifest names are kept, in one payload in the Steam runtime cache, and
//! `package::prepare` extracts it from there. Never searches for Steam,
//! executes an installer or accesses account data.
use crate::{
    package::{self, Manifest, PayloadFile, SourcePackage},
    voile::remove_entries,
};
use obelisk_core::transfer::CancelToken;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

/// Unfinished downloads in the cache start with this, and the next install
/// removes them. `package::prepare` stages under another prefix.
const UNFINISHED: &str = ".download-";
/// Every payload's name starts with this; a newer manifest replaces the
/// payloads of the ones before it.
const PAYLOAD: &str = "valve-";

/// Two installs never write the same payload.
static INSTALLING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Where the payload of the pinned manifest lives in `cache`.
pub fn payload(cache: &Path) -> PathBuf {
    payload_in(cache, package::manifest())
}

fn payload_in(cache: &Path, manifest: &Manifest) -> PathBuf {
    cache.join(format!("{PAYLOAD}{}.zip", manifest.version))
}

/// Whether `payload` lists every file the pinned manifest names, at its size.
/// It reads the payload's central directory only, never inflates a file:
/// the install hashes every file it keeps, and `package::prepare` every file
/// it extracts.
pub fn installed(payload: &Path) -> bool {
    listed(payload, &package::manifest().files)
}

fn listed(payload: &Path, files: &[PayloadFile]) -> bool {
    package::archive(payload, files).is_ok()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Failure {
    /// The network failed, or Valve's CDN refused the request for a reason
    /// that may pass, such as a server error or a rate limit.
    #[error("backend.steam.components.downloadFailed")]
    Download,
    /// Valve no longer serves a package the manifest pins: only a newer
    /// Obelisk, which pins the current ones, can launch Steam games.
    #[error("backend.steam.components.unavailable")]
    Unavailable,
    #[error("backend.steam.components.damaged")]
    Damaged,
    #[error("backend.steam.components.storageFailed")]
    Storage,
    #[error("backend.steam.components.cancelled")]
    Cancelled,
}
impl From<std::io::Error> for Failure {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}
impl From<crate::steam::Error> for Failure {
    fn from(error: crate::steam::Error) -> Self {
        match error {
            crate::steam::Error::Cancelled => Self::Cancelled,
            crate::steam::Error::Storage => Self::Storage,
            _ => Self::Damaged,
        }
    }
}

/// One install of the pinned payload, which the player can cancel and watch.
#[derive(Default)]
pub struct Install {
    received: AtomicU64,
    /// The size of every package being downloaded, once known.
    total: AtomicU64,
    cancel: CancelToken,
    /// Stops the payload's assembly, which runs off the async runtime.
    stopped: Arc<AtomicBool>,
}

impl Install {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.stopped.store(true, Ordering::Release);
        self.cancel.cancel();
    }

    /// The bytes downloaded so far, and the size of every package.
    pub fn progress(&self) -> (u64, u64) {
        let total = self.total.load(Ordering::Acquire);
        let total = if total == 0 {
            size(package::manifest())
        } else {
            total
        };
        (self.received.load(Ordering::Acquire), total)
    }

    /// Downloads the pinned packages into `payload` unless it already holds
    /// every file intact, then removes the payloads of other manifests.
    pub async fn run(&self, payload: &Path) -> Result<(), Failure> {
        let client = client().map_err(|_| Failure::Download)?;
        self.install(&client, package::manifest(), payload).await
    }

    async fn install(
        &self,
        client: &reqwest::Client,
        manifest: &Manifest,
        payload: &Path,
    ) -> Result<(), Failure> {
        let _installing = self.cancellable(INSTALLING.lock()).await?;
        let (path, files) = (payload.to_owned(), manifest.files.clone());
        let intact = tokio::task::spawn_blocking(move || {
            package::verify(&path, &files, &AtomicBool::new(false)).is_ok()
        })
        .await
        .map_err(|_| Failure::Storage)?;
        if intact {
            return Ok(());
        }
        self.total.store(size(manifest), Ordering::Release);
        let cache = payload.parent().ok_or(Failure::Storage)?.to_owned();
        let unfinished = cache.clone();
        tokio::task::spawn_blocking(move || {
            crate::files::private_directory(&unfinished).map_err(|_| Failure::Storage)?;
            remove_entries(&unfinished, |name| name.starts_with(UNFINISHED));
            Ok::<_, Failure>(())
        })
        .await
        .map_err(|_| Failure::Storage)??;
        // Goes away with every package once the payload is in place, or on
        // any failure.
        let downloads = tempfile::Builder::new()
            .prefix(UNFINISHED)
            .tempdir_in(&cache)?;
        let mut packages = Vec::new();
        for source in &manifest.packages {
            let path = downloads.path().join(format!("{}.zip", source.sha256));
            self.download(client, source, &path).await?;
            packages.push((source.name.clone(), path));
        }
        let assembled = downloads.path().join("payload.zip");
        let (output, files, stopped) = (
            assembled.clone(),
            manifest.files.clone(),
            self.stopped.clone(),
        );
        // Not raced against cancellation: the assembly stops at its next
        // read once cancelled, before `downloads` goes away under it.
        tokio::task::spawn_blocking(move || assemble(&packages, &files, &output, &stopped))
            .await
            .map_err(|_| Failure::Storage)??;
        if self.cancel.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        let payload = payload.to_owned();
        tokio::task::spawn_blocking(move || {
            fs::rename(&assembled, &payload)?;
            File::open(&cache)?.sync_all()?;
            // No launch reads another manifest's payload: `package::prepare`
            // only reads the one its Locations name.
            let kept = payload.file_name().and_then(|name| name.to_str());
            remove_entries(&cache, |name| {
                name.starts_with(PAYLOAD) && name.ends_with(".zip") && kept != Some(name)
            });
            Ok::<_, Failure>(())
        })
        .await
        .map_err(|_| Failure::Storage)??;
        Ok(())
    }

    /// Downloads `source` into `path`, checking its size and SHA-256.
    async fn download(
        &self,
        client: &reqwest::Client,
        source: &SourcePackage,
        path: &Path,
    ) -> Result<(), Failure> {
        let mut response = self
            .cancellable(client.get(&source.url).send())
            .await?
            .map_err(|_| Failure::Download)?;
        let status = response.status();
        // Only a missing or moved package means the pin is gone: redirects
        // are not followed, since a pinned package lives at its URL. Any
        // other refusal, such as a rate limit, may pass with a retry.
        if status == StatusCode::NOT_FOUND || status == StatusCode::GONE || status.is_redirection()
        {
            return Err(Failure::Unavailable);
        }
        if !status.is_success() {
            return Err(Failure::Download);
        }
        let base = self.received.load(Ordering::Acquire);
        let mut file = File::create(path)?;
        let mut digest = Sha256::new();
        let mut size = 0_u64;
        while let Some(bytes) = self
            .cancellable(response.chunk())
            .await?
            .map_err(|_| Failure::Download)?
        {
            size += bytes.len() as u64;
            if size > source.size {
                return Err(Failure::Damaged);
            }
            digest.update(&bytes);
            file.write_all(&bytes)?;
            self.received.store(base + size, Ordering::Release);
        }
        if size != source.size || format!("{:x}", digest.finalize()) != source.sha256 {
            return Err(Failure::Damaged);
        }
        Ok(())
    }

    /// Stops waiting for `work` as soon as the install is cancelled.
    async fn cancellable<T>(&self, work: impl Future<Output = T>) -> Result<T, Failure> {
        self.cancel.race(work).await.map_err(|_| Failure::Cancelled)
    }
}

/// HTTPS to Valve's CDN only, without redirects, and bounded in time.
fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .build()
}

/// The size of every package `manifest` pins.
fn size(manifest: &Manifest) -> u64 {
    manifest.packages.iter().map(|source| source.size).sum()
}

/// Writes into `output` a payload of the `files` the downloaded `packages`
/// hold, each bounded and hash-checked by the manifest even for malformed ZIP
/// metadata, then checks the payload as `package::prepare` will read it.
fn assemble(
    packages: &[(String, PathBuf)],
    files: &[PayloadFile],
    output: &Path,
    stopped: &AtomicBool,
) -> Result<(), Failure> {
    let mut payload = File::create(output)?;
    {
        let mut writer = zip::ZipWriter::new(&mut payload);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6))
            .unix_permissions(0o644);
        for (name, path) in packages {
            let mut archive =
                zip::ZipArchive::new(File::open(path)?).map_err(|_| Failure::Damaged)?;
            for entry in files.iter().filter(|entry| &entry.package == name) {
                let mut file = archive.by_name(&entry.path).map_err(|_| Failure::Damaged)?;
                if file.size() != entry.size || !file.is_file() {
                    return Err(Failure::Damaged);
                }
                writer
                    .start_file(&entry.path, options)
                    .map_err(|_| Failure::Storage)?;
                package::copy_verified(&mut file, &mut writer, entry, stopped)?;
            }
        }
        writer.finish().map_err(|_| Failure::Storage)?;
    }
    payload.sync_all()?;
    package::verify(output, files, stopped)?;
    Ok(())
}

#[cfg(test)]
mod tests;
