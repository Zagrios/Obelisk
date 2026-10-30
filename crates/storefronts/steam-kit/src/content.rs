//! Steam depot delivery: manifests, authenticated CDN chunks, verification and installation.
//!
//! The caller obtains entitlements, depot keys and request codes through Steam's CM.
//! Protocol references and format limits are recorded in docs/steam-content-notes.md.
//! Path confinement, locking, hashing and progress reporting come from `obelisk_core`.

mod format;
mod manifest_cache;
mod mirrors;
mod verification;

pub use manifest_cache::CachedManifest;

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::{StreamExt, future::BoxFuture};
use obelisk_core::{
    install::{
        self, InstallError, PARTIAL_DIRECTORY, apply_permissions, checked_file_path,
        ensure_directory, reject_link_or_special_file, safe_relative_path,
    },
    mac_build::Listing,
    transfer::{
        self, BodyError, BodySize, Cancelled, Chunks, Delivery, DownloadPhase, DownloadProgress,
        DownloadSummary, Interrupted, Pipeline, Reporter,
    },
};
use reqwest::{Client, Url};
use sha1::{Digest, Sha1};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::cm::ContentServer;
use format::{Chunk, Manifest, ManifestFile, decode_chunk, parse_manifest, unpack_zip};
use mirrors::{Mirrors, Ticket};

/// Shared with CM avatar hashes.
pub(crate) use obelisk_core::install::hex;
pub use obelisk_core::transfer::CancelToken;

const MAX_MANIFEST_BYTES: usize = 128 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
/// Chunk requests in flight per depot, across every server. One CDN
/// connection carries about a megabyte per second, so a fast link needs many.
/// Chunks are typically 1 MiB, so this also bounds the memory decoded chunks
/// hold.
const CHUNK_REQUESTS: usize = 64;
/// Chunks requested ahead of the writer: enough that requests in flight never
/// wait for it to prepare files.
const QUEUED_CHUNKS: usize = 2 * CHUNK_REQUESTS;
/// Decoded chunks waiting for the disk writer.
const WRITE_QUEUE: usize = 16;
/// Files prepared ahead of the writer, each holding its partial file open:
/// requests queue without opening partial files for the whole depot.
const PREPARED_FILES: usize = 32;
/// Chunks reach the writer as they arrive: it places each one itself.
fn pipeline() -> Pipeline {
    Pipeline {
        delivery: Delivery::AsFetched,
        fetches: CHUNK_REQUESTS,
        queue: WRITE_QUEUE,
    }
}

/// One download's aggregate network pace. Every concurrent chunk reserves its
/// bytes on the same timeline, so the limit applies to their total rather than
/// to each connection separately. `None` leaves the stream untouched.
struct RateLimiter {
    bytes_per_second: Option<u64>,
    next: Mutex<Instant>,
}

impl RateLimiter {
    fn from_environment() -> Self {
        let megabits = STEAM_LIMIT.load(std::sync::atomic::Ordering::Relaxed);
        Self {
            bytes_per_second: (megabits > 0).then_some(megabits.saturating_mul(125_000)),
            next: Mutex::new(Instant::now()),
        }
    }

    async fn acquire(&self, bytes: usize) {
        let Some(rate) = self.bytes_per_second else {
            return;
        };
        let duration = Duration::from_secs_f64(bytes as f64 / rate as f64);
        let wait_until = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            if *next < now {
                *next = now;
            }
            *next += duration;
            *next
        };
        tokio::time::sleep_until(wait_until.into()).await;
    }
}

static STEAM_LIMIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Maximum aggregate Steam content speed in decimal Mbps; zero is unlimited.
/// A content client snapshots this value when a download starts.
pub fn set_download_limit_mbps(value: u64) {
    STEAM_LIMIT.store(value.min(1000), std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_requests_share_one_budget() {
        let limiter = RateLimiter {
            bytes_per_second: Some(10_000),
            next: Mutex::new(Instant::now()),
        };
        let start = Instant::now();
        tokio::join!(
            limiter.acquire(1000),
            limiter.acquire(1000),
            limiter.acquire(1000)
        );
        assert!(start.elapsed() >= Duration::from_millis(190));
    }

    #[tokio::test]
    async fn unlimited_does_not_reserve_or_wait() {
        let next = Instant::now() + Duration::from_secs(60);
        let limiter = RateLimiter {
            bytes_per_second: None,
            next: Mutex::new(next),
        };
        tokio::time::timeout(Duration::from_millis(100), limiter.acquire(usize::MAX))
            .await
            .unwrap();
        assert_eq!(*limiter.next.lock().await, next);
    }
}

/// No Debug implementation: depot keys are credentials.
pub struct DepotDownload<'a> {
    pub depot_id: u32,
    pub manifest_id: u64,
    pub request_code: u64,
    pub depot_key: [u8; 32],
    /// The CDN servers allowed to serve the depot, in Steam's order of
    /// preference. HTTPS is always used.
    pub servers: Vec<ContentServer>,
    /// Grants the tokens servers may require; without it, a refusal is final.
    pub authorization: Option<&'a dyn CdnAuthorization>,
    pub destination: PathBuf,
    /// Already validated snapshot of the current manifest, if available locally.
    pub cached_manifest: Option<CachedManifest>,
}

/// Where a depot's files come from, to read a few of them before it is
/// installed. No Debug implementation: depot keys are credentials.
pub struct DepotSource<'a> {
    pub depot_id: u32,
    pub manifest_id: u64,
    pub request_code: u64,
    pub depot_key: [u8; 32],
    pub servers: &'a [ContentServer],
    pub authorization: Option<&'a dyn CdnAuthorization>,
}

/// A depot's files as its checked manifest lists them, whose bytes come from
/// its servers without being installed.
pub struct DepotFiles<'a> {
    content: &'a ContentClient,
    mirrors: Mirrors<'a>,
    depot_id: u32,
    depot_key: [u8; 32],
    manifest: Arc<Manifest>,
    archive: Vec<u8>,
}

/// Grants the CDN authorization a server may require before serving a depot.
pub trait CdnAuthorization: Sync {
    /// A token for `host`, or `None` when Steam grants none.
    fn token<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Option<String>>;
}

/// One installed depot.
#[derive(Debug)]
pub struct DepotInstallation {
    pub summary: DownloadSummary,
    /// The installation paths of the install scripts its manifest flags.
    pub install_scripts: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ContentError {
    #[error("backend.steam.content.cancelled")]
    Cancelled,
    #[error("backend.steam.content.network")]
    Network,
    #[error("{}", crate::message::format("backend.steam.content.http", &[.0.to_string()]))]
    Http(u16),
    #[error("{}", crate::message::format("backend.steam.content.invalid", std::slice::from_ref(.0)))]
    InvalidData(String),
    #[error("{}", crate::message::format("backend.steam.content.unsafePath", std::slice::from_ref(.0)))]
    UnsafePath(String),
    #[error("{}", crate::message::format("backend.steam.content.unsupportedSymlink", std::slice::from_ref(.0)))]
    UnsupportedSymlink(String),
    #[error("backend.steam.content.locked")]
    AlreadyDownloading,
    #[error("{}", crate::message::format("backend.steam.content.io", &[.0.to_string()]))]
    Io(#[from] std::io::Error),
}

impl From<Cancelled> for ContentError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<Interrupted> for ContentError {
    fn from(_: Interrupted) -> Self {
        invalid("backend.steam.content.diskInstallationInterrupted")
    }
}

impl From<BodyError> for ContentError {
    fn from(error: BodyError) -> Self {
        match error {
            BodyError::Network => Self::Network,
            BodyError::Size => invalid("backend.steam.content.cdnResponseTooLarge"),
            BodyError::Cancelled => Self::Cancelled,
        }
    }
}

impl From<InstallError> for ContentError {
    fn from(error: InstallError) -> Self {
        match error {
            InstallError::Cancelled => Self::Cancelled,
            InstallError::UnsafePath(path) => Self::UnsafePath(path),
            InstallError::Io(error) => Self::Io(error),
        }
    }
}

#[derive(Clone)]
pub struct ContentClient {
    client: Client,
    limiter: Arc<RateLimiter>,
    /// Local servers standing in for CDN hosts; any other host is unreachable.
    #[cfg(test)]
    test_endpoints: HashMap<String, Url>,
}

impl ContentClient {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            limiter: Arc::new(RateLimiter::from_environment()),
            #[cfg(test)]
            test_endpoints: HashMap::new(),
        }
    }

    /// Reads and validates the cache without issuing a CDN or authentication request.
    pub async fn cached_manifest(
        &self,
        destination: &Path,
        depot_id: u32,
        manifest_id: u64,
        key: &[u8; 32],
    ) -> Result<Option<CachedManifest>, ContentError> {
        let destination = destination.to_owned();
        let key = *key;
        tokio::task::spawn_blocking(move || {
            manifest_cache::load(&destination, depot_id, manifest_id, &key)
        })
        .await
        .map_err(|_| invalid("backend.steam.content.manifestReadInterrupted"))?
    }

    /// Fetches and checks a depot's manifest, to read some of its files.
    pub async fn depot_files<'a>(
        &'a self,
        source: DepotSource<'a>,
        cancel: &CancelToken,
    ) -> Result<DepotFiles<'a>, ContentError> {
        let mirrors = Mirrors::new(source.servers, source.authorization)?;
        let path = manifest_path(source.depot_id, source.manifest_id, source.request_code);
        let (zipped, _) = mirrors
            .fetch(&path, None, 0, cancel, |url| {
                self.fetch(url, MAX_MANIFEST_BYTES, cancel, None, |_| {})
            })
            .await?;
        let key = source.depot_key;
        let (manifest, archive) = cancel
            .spawn_blocking(move || {
                let manifest = parse_manifest(&unpack_zip(&zipped, MAX_MANIFEST_BYTES)?, &key)?;
                validate_manifest_files(&manifest.files)?;
                Ok::<_, ContentError>((manifest, zipped))
            })
            .await
            .map_err(|_| invalid("backend.steam.content.manifestParseInterrupted"))??;
        if manifest.depot_id != source.depot_id || manifest.manifest_id != source.manifest_id {
            return Err(invalid("backend.steam.content.manifestDepotMismatch"));
        }
        Ok(DepotFiles {
            content: self,
            mirrors,
            depot_id: source.depot_id,
            depot_key: key,
            manifest: Arc::new(manifest),
            archive,
        })
    }

    /// Opt-in protocol smoke test: validates the smallest real file (up to 8 MiB)
    /// without installing a depot or writing any game files.
    #[cfg(test)]
    pub(crate) async fn verify_live_sample(
        &self,
        plan: &DepotDownload<'_>,
    ) -> Result<(u64, u32), ContentError> {
        let cancel = CancelToken::new();
        let mirrors = Mirrors::new(&plan.servers, plan.authorization)?;
        let path = format!(
            "depot/{}/manifest/{}/5/{}",
            plan.depot_id, plan.manifest_id, plan.request_code
        );
        let (zipped, _) = mirrors
            .fetch(&path, None, 0, &cancel, |url| {
                self.fetch(url, MAX_MANIFEST_BYTES, &cancel, None, |_| {})
            })
            .await?;
        let bytes = unpack_zip(&zipped, MAX_MANIFEST_BYTES)?;
        let manifest = parse_manifest(&bytes, &plan.depot_key)?;
        if manifest.depot_id != plan.depot_id || manifest.manifest_id != plan.manifest_id {
            return Err(invalid("backend.steam.content.manifestDepotMismatch"));
        }
        let file = manifest
            .files
            .iter()
            .filter(|file| {
                !file.is_directory()
                    && !file.is_link()
                    && file.size > 0
                    && file.size <= 8 * 1024 * 1024
            })
            .min_by_key(|file| file.size)
            .ok_or_else(|| invalid("backend.steam.content.noSampleFile"))?;
        validate_manifest_files(std::slice::from_ref(file))?;
        let mut hasher = Sha1::new();
        let mut size = 0_u64;
        for chunk in &file.chunks {
            let path = format!("depot/{}/chunk/{}", plan.depot_id, hex(&chunk.sha));
            let (mut encrypted, _) = mirrors
                .fetch(&path, None, 0, &cancel, |url| {
                    self.fetch(url, MAX_CHUNK_BYTES, &cancel, None, |_| {})
                })
                .await?;
            let plaintext = decode_chunk(&mut encrypted, chunk, &plan.depot_key)?;
            size += plaintext.len() as u64;
            hasher.update(plaintext);
        }
        if size != file.size || hasher.finalize()[..] != file.sha {
            return Err(invalid("backend.steam.content.sampleHashMismatch"));
        }
        Ok((size, file.chunks.len() as u32))
    }

    /// Installs one authorized depot, its chunks spread over every server the
    /// plan names. Network work stays on the async runtime while every disk
    /// operation runs on the blocking pool, so a multi-GB depot does not pin a
    /// runtime worker. Existing complete files and verified partial chunks are
    /// reused on the next invocation. File replacement is atomic; an incomplete
    /// file never replaces an installed file.
    pub async fn download_depot(
        &self,
        plan: &DepotDownload<'_>,
        cancel: &CancelToken,
        progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Result<DepotInstallation, ContentError> {
        cancel.check()?;
        let mirrors = Mirrors::new(&plan.servers, plan.authorization)?;
        let reporter = Reporter::new(
            DownloadProgress {
                current_file: "backend.steam.content.fetchingManifest".into(),
                ..Default::default()
            },
            progress,
        );
        let cached = match &plan.cached_manifest {
            Some(cached) => Some(cached.clone()),
            None => {
                self.cached_manifest(
                    &plan.destination,
                    plan.depot_id,
                    plan.manifest_id,
                    &plan.depot_key,
                )
                .await?
            }
        };
        cancel.check()?;
        // A cached manifest was validated when it was loaded; only a fresh
        // download must be checked in full before any installation file exists.
        let (manifest, archive) = if let Some(cached) = cached {
            (cached.manifest, cached.archive)
        } else {
            let manifest_path = manifest_path(plan.depot_id, plan.manifest_id, plan.request_code);
            let reporter = &reporter;
            // Every chunk waits on the manifest: it is raced like the oldest chunk.
            let ticket = mirrors.ticket();
            let (zipped, _) = mirrors
                .fetch(&manifest_path, Some(&ticket), 0, cancel, |url| {
                    self.fetch(url, MAX_MANIFEST_BYTES, cancel, None, |received| {
                        reporter.update(|status| status.network_bytes += received);
                    })
                })
                .await?;
            let key = plan.depot_key;
            let manifest = cancel
                .spawn_blocking(move || {
                    let manifest_bytes = unpack_zip(&zipped, MAX_MANIFEST_BYTES)?;
                    let manifest = parse_manifest(&manifest_bytes, &key)?;
                    validate_manifest_files(&manifest.files)?;
                    Ok::<_, ContentError>((manifest, zipped))
                })
                .await
                .map_err(|_| invalid("backend.steam.content.manifestParseInterrupted"))?;
            let (manifest, zipped) = manifest?;
            (Arc::new(manifest), Some(zipped))
        };
        if manifest.depot_id != plan.depot_id || manifest.manifest_id != plan.manifest_id {
            return Err(invalid("backend.steam.content.manifestDepotMismatch"));
        }
        // A link holds no content, whatever size its entry gives.
        let total_bytes = manifest
            .files
            .iter()
            .filter(|file| !file.is_link())
            .map(|file| file.size)
            .sum();
        let total_files = u32::try_from(
            manifest
                .files
                .iter()
                .filter(|file| !file.is_directory() && !file.is_link())
                .count(),
        )
        .map_err(|_| invalid("backend.steam.content.tooManyFiles"))?;
        reporter.update(|status| {
            status.total_bytes = total_bytes;
            status.total_files = total_files;
            status.phase = DownloadPhase::Verifying;
            status.current_file.clear();
        });
        let install_scripts = manifest
            .files
            .iter()
            .filter(|file| {
                !file.is_directory()
                    && !file.is_link()
                    && file.flags & crate::install_script::FLAG != 0
            })
            .map(|file| file.name.clone())
            .collect();

        let disk = DiskInstall::engine_side(
            plan.destination.clone(),
            plan.depot_id,
            plan.manifest_id,
            archive,
            manifest,
            cancel.clone(),
            Arc::clone(&reporter),
        );
        let reused_bytes =
            transfer::run_engine(pipeline(), cancel, disk, |(place, chunk): ChunkRequest| {
                // Requests are created in manifest order and taken up in that order.
                let ticket = mirrors.ticket();
                let fetched = self.fetch_chunk(&mirrors, ticket, plan, chunk, cancel, &reporter);
                async move { fetched.await.map(|bytes| (place, bytes)) }
            })
            .await?;
        Ok(DepotInstallation {
            summary: reporter.summary(reused_bytes),
            install_scripts,
        })
    }

    /// Fetches one chunk through the depot's servers and decodes it. Chunk
    /// bytes count once, from the response that answered; a corrupt response
    /// moves the chunk to another server. Decryption and decompression run on
    /// the blocking pool.
    async fn fetch_chunk(
        &self,
        mirrors: &Mirrors<'_>,
        ticket: Ticket<'_>,
        plan: &DepotDownload<'_>,
        chunk: Chunk,
        cancel: &CancelToken,
        reporter: &Reporter,
    ) -> Result<Vec<u8>, ContentError> {
        let path = format!("depot/{}/chunk/{}", plan.depot_id, hex(&chunk.sha));
        let key = plan.depot_key;
        let chunk = &chunk;
        let limiter = self.limiter.as_ref();
        reporter.update(|status| status.phase = DownloadPhase::Downloading);
        let (bytes, received) = mirrors
            .fetch(
                &path,
                Some(&ticket),
                u64::from(chunk.compressed_size),
                cancel,
                |url| async move {
                    let (mut encrypted, received) = self
                        .fetch(url, MAX_CHUNK_BYTES, cancel, Some(limiter), |received| {
                            reporter.update(|status| status.network_bytes += received);
                        })
                        .await?;
                    let expected = chunk.clone();
                    let decoded = cancel
                        .spawn_blocking(move || decode_chunk(&mut encrypted, &expected, &key))
                        .await
                        .map_err(|_| {
                            invalid("backend.steam.content.chunkDecryptionInterrupted")
                        })??;
                    Ok((decoded, received))
                },
            )
            .await?;
        reporter.update(|status| status.content_bytes += received);
        Ok(bytes)
    }

    /// Returns the response body with its size, which measures the server.
    async fn fetch(
        &self,
        url: Url,
        limit: usize,
        cancel: &CancelToken,
        limiter: Option<&RateLimiter>,
        on_bytes: impl FnMut(u64),
    ) -> Result<(Vec<u8>, u64), ContentError> {
        #[cfg(test)]
        let url = if self.test_endpoints.is_empty() {
            url
        } else {
            let Some(endpoint) = url
                .host_str()
                .and_then(|host| self.test_endpoints.get(host))
            else {
                return Err(ContentError::Network);
            };
            let mut local = endpoint.join(url.path()).expect("test URL");
            local.set_query(url.query());
            local
        };
        // A server that stops sending fails the request, not a slow link.
        let request = self
            .client
            .get(url)
            // Chunks are compressed already: never gzip them again.
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .timeout(
                if limiter.is_some_and(|limit| limit.bytes_per_second.is_some()) {
                    Duration::from_secs(7 * 24 * 60 * 60)
                } else {
                    transfer::CHUNK_DEADLINE
                },
            )
            .send();
        let response = cancel
            .race(tokio::time::timeout(transfer::CHUNK_STALL, request))
            .await?
            .map_err(|_| ContentError::Network)?
            .map_err(|_| ContentError::Network)?;
        if !response.status().is_success() {
            return Err(ContentError::Http(response.status().as_u16()));
        }
        let advertised = response.content_length();
        let parts = transfer::unless_stalled(response.bytes_stream(), transfer::CHUNK_STALL).then(
            |part| async {
                if let (Ok(bytes), Some(limiter)) = (&part, limiter) {
                    limiter.acquire(bytes.len()).await;
                }
                part
            },
        );
        let bytes = transfer::read_body(
            advertised,
            parts,
            BodySize::AtMost(limit as u64),
            cancel,
            on_bytes,
        )
        .await?;
        let size = bytes.len() as u64;
        Ok((bytes, size))
    }
}

impl DepotFiles<'_> {
    /// The checked manifest, for the installation of the depot to start from
    /// instead of fetching it again.
    pub fn into_manifest(self) -> ((u32, u64), CachedManifest) {
        let identity = (self.depot_id, self.manifest.manifest_id);
        let manifest = CachedManifest {
            manifest: self.manifest,
            archive: Some(self.archive),
        };
        (identity, manifest)
    }

    /// Adds the depot's files and links to `listing`, each file named by
    /// `depot` and its place in the manifest. Steam keeps a link's target
    /// from the link's own folder.
    pub fn list(&self, listing: &mut Listing<(usize, usize)>, depot: usize) {
        for (index, file) in self.manifest.files.iter().enumerate() {
            if file.is_link() {
                let folder = file.name.rsplit_once('/').map_or("", |(folder, _)| folder);
                listing.link(&file.name, &format!("{folder}/{}", file.link_target));
            } else if !file.is_directory() {
                listing.file(&file.name, (depot, index), file.size);
            }
        }
    }

    /// The first `bytes` of the manifest's file `index`, from the chunks that
    /// hold them.
    pub async fn read(
        &self,
        index: usize,
        bytes: u64,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, ContentError> {
        let file = self
            .manifest
            .files
            .get(index)
            .ok_or_else(|| invalid("backend.steam.content.noSampleFile"))?;
        let mut output = Vec::new();
        for chunk in &file.chunks {
            if output.len() as u64 >= bytes {
                break;
            }
            let path = format!("depot/{}/chunk/{}", self.depot_id, hex(&chunk.sha));
            let key = self.depot_key;
            let (plaintext, _) = self
                .mirrors
                .fetch(
                    &path,
                    None,
                    u64::from(chunk.compressed_size),
                    cancel,
                    |url| async move {
                        let (mut encrypted, received) = self
                            .content
                            .fetch(url, MAX_CHUNK_BYTES, cancel, None, |_| {})
                            .await?;
                        let expected = chunk.clone();
                        let decoded = cancel
                            .spawn_blocking(move || decode_chunk(&mut encrypted, &expected, &key))
                            .await
                            .map_err(|_| {
                                invalid("backend.steam.content.chunkDecryptionInterrupted")
                            })??;
                        Ok((decoded, received))
                    },
                )
                .await?;
            output.extend(plaintext);
        }
        output.truncate(usize::try_from(bytes).unwrap_or(usize::MAX));
        Ok(output)
    }
}

/// The CDN path of a depot's manifest; a request code of 0 is left out.
fn manifest_path(depot_id: u32, manifest_id: u64, request_code: u64) -> String {
    match request_code {
        0 => format!("depot/{depot_id}/manifest/{manifest_id}/5"),
        code => format!("depot/{depot_id}/manifest/{manifest_id}/5/{code}"),
    }
}

/// Where a chunk belongs: its file's index in the manifest and its own index
/// in that file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChunkPlace {
    file: usize,
    chunk: usize,
}

type ChunkRequest = (ChunkPlace, Chunk);
/// The writer's end of the depot's engine.
type DepotChunks = Chunks<ChunkRequest, (ChunkPlace, Vec<u8>), ContentError>;

/// Everything that touches the disk for one depot, on a blocking thread: the
/// destination lock, manifest cache, verification, assembly and atomic publication.
struct DiskInstall {
    destination: PathBuf,
    depot_id: u32,
    manifest_id: u64,
    archive: Option<Vec<u8>>,
    manifest: Arc<Manifest>,
    cancel: CancelToken,
    reporter: Arc<Reporter>,
    chunks: DepotChunks,
}

/// A file being assembled in its partial file. Its chunks arrive in any
/// order and each is written in place; the file is hashed in order as far as
/// its chunks are present, so a chunk arriving late holds up only its file.
struct PreparedFile<'a> {
    file: &'a ManifestFile,
    relative: PathBuf,
    target: PathBuf,
    partial_path: PathBuf,
    partial: File,
    /// Chunks in the partial file: verified from an earlier run, or written.
    present: Vec<bool>,
    /// Chunks requested and not written yet.
    missing: usize,
    hasher: Sha1,
    /// Chunks hashed so far, from the start of the file.
    hashed: usize,
    /// Holds a chunk read back from the partial file.
    buffer: Vec<u8>,
    reused_bytes: u64,
}

impl PreparedFile<'_> {
    /// Writes a chunk in place, then hashes on as far as the file is present.
    fn write(&mut self, index: usize, bytes: &[u8]) -> Result<(), ContentError> {
        let chunk = self
            .file
            .chunks
            .get(index)
            .filter(|_| !self.present[index])
            .ok_or_else(|| invalid("backend.steam.content.chunkDownloadInterrupted"))?;
        self.partial.seek(SeekFrom::Start(chunk.offset))?;
        self.partial.write_all(bytes)?;
        self.present[index] = true;
        self.missing -= 1;
        self.hash_present(index, bytes)
    }

    /// Hashes the chunks present from where hashing stopped. The chunk just
    /// written is hashed from memory; one that arrived early is read back.
    fn hash_present(&mut self, written: usize, bytes: &[u8]) -> Result<(), ContentError> {
        while self.present.get(self.hashed) == Some(&true) {
            if self.hashed == written {
                self.hasher.update(bytes);
            } else {
                let chunk = &self.file.chunks[self.hashed];
                self.buffer.resize(chunk.original_size as usize, 0);
                self.partial.seek(SeekFrom::Start(chunk.offset))?;
                self.partial.read_exact(&mut self.buffer)?;
                self.hasher.update(&self.buffer);
            }
            self.hashed += 1;
        }
        Ok(())
    }
}

impl DiskInstall {
    /// The writer for one depot's engine, ready to run on its blocking thread.
    fn engine_side(
        destination: PathBuf,
        depot_id: u32,
        manifest_id: u64,
        archive: Option<Vec<u8>>,
        manifest: Arc<Manifest>,
        cancel: CancelToken,
        reporter: Arc<Reporter>,
    ) -> impl FnOnce(DepotChunks) -> Result<u64, ContentError> + Send + 'static {
        move |chunks| {
            Self {
                destination,
                depot_id,
                manifest_id,
                archive,
                manifest,
                cancel,
                reporter,
                chunks,
            }
            .run()
        }
    }

    /// Returns the number of verified bytes reused from disk.
    fn run(mut self) -> Result<u64, ContentError> {
        self.cancel.check()?;
        fs::create_dir_all(&self.destination)?;
        let root = fs::canonicalize(&self.destination)?;
        // A per-destination lock also prevents conflicting depots from being installed together.
        let _lock = install::lock_engine(&root)?.ok_or(ContentError::AlreadyDownloading)?;
        if let Some(archive) = self.archive.take() {
            self.cancel.check()?;
            manifest_cache::store(&root, self.depot_id, self.manifest_id, &archive)?;
        }
        let partial_relative = PathBuf::from(PARTIAL_DIRECTORY)
            .join(self.depot_id.to_string())
            .join(self.manifest_id.to_string());
        let partial_root = ensure_directory(&root, &partial_relative)?;
        let manifest = Arc::clone(&self.manifest);
        // Readers finish before the repair loop can replace any game files.
        // Reusable bytes include only valid whole files; inspection progress is
        // reported separately while a large file is still being hashed.
        let verified =
            verification::verify_files(&root, &manifest.files, &self.cancel, &self.reporter)?;
        let mut reused_bytes = verified.reused_bytes;

        // Directory entries are synced once per run of files sharing a parent
        // rather than after every rename; a lost rename is repaired next run
        // from the partial file that is still on disk.
        let mut pending_directory: Option<PathBuf> = None;
        // Parents are created and checked once per distinct directory; every
        // file's full path is still confined right before it is written.
        let mut ensured_parents: HashSet<PathBuf> = HashSet::new();
        let mut remaining = manifest.files.iter().enumerate();
        // Files being assembled, by manifest index.
        let mut assembling = HashMap::new();
        // Chunks requested for them and not written yet.
        let mut queued = 0;
        loop {
            // Requests follow manifest order. Files are prepared until enough
            // of their chunks are queued; valid files and directories do not
            // count. After a refused request, the file that held it is
            // assembling and receives the outcome: preparing more is wasted.
            while assembling.len() < PREPARED_FILES
                && queued < QUEUED_CHUNKS
                && !self.chunks.refused()
            {
                let Some((index, file)) = remaining.next() else {
                    break;
                };
                self.cancel.check()?;
                if file.is_link() {
                    continue;
                }
                let relative = safe_relative_path(&file.name)?;
                if file.is_directory() {
                    ensure_directory(&root, &relative)?;
                    ensured_parents.insert(relative);
                    continue;
                }
                let parent = relative.parent().unwrap_or(Path::new(""));
                if !ensured_parents.contains(parent) {
                    ensure_directory(&root, parent)?;
                    ensured_parents.insert(parent.to_path_buf());
                }
                let target = checked_file_path(&root, &relative)?;
                if verified.valid[index] {
                    apply_permissions(&target, is_executable(file, &target)?)?;
                    continue;
                }
                let prepared = self.prepare(&partial_root, index, file, relative, target)?;
                if prepared.missing == 0 {
                    reused_bytes += self.publish(prepared, &root, &mut pending_directory)?;
                } else {
                    queued += prepared.missing;
                    assembling.insert(index, prepared);
                }
            }
            if assembling.is_empty() {
                break;
            }
            self.cancel.check()?;
            let (place, bytes) = self.chunks.receive()?;
            let file: &mut PreparedFile<'_> = assembling
                .get_mut(&place.file)
                .ok_or_else(|| invalid("backend.steam.content.chunkDownloadInterrupted"))?;
            file.write(place.chunk, &bytes)?;
            queued -= 1;
            let size = bytes.len() as u64;
            self.reporter
                .update(|status| status.completed_bytes += size);
            if file.missing == 0
                && let Some(file) = assembling.remove(&place.file)
            {
                reused_bytes += self.publish(file, &root, &mut pending_directory)?;
            }
        }
        // Links come once every file is in place. Validation kept every file
        // and folder from sitting behind one.
        for file in manifest.files.iter().filter(|file| file.is_link()) {
            self.cancel.check()?;
            install::place_link(
                &root,
                &safe_relative_path(&file.name)?,
                Path::new(&file.link_target),
            )?;
        }
        if let Some(directory) = pending_directory {
            install::write_out(&File::open(directory)?)?;
        }
        // Every file is in place: durable before the receipt says so, and
        // recorded whole for the next Transfer to skip.
        verification::complete(&root, &manifest.files)?;
        Ok(reused_bytes)
    }

    /// Inspect the partial file once and request only its missing chunks. Keep
    /// its open handle until its last chunk is written.
    fn prepare<'a>(
        &mut self,
        partial_root: &Path,
        index: usize,
        file: &'a ManifestFile,
        relative: PathBuf,
        target: PathBuf,
    ) -> Result<PreparedFile<'a>, ContentError> {
        let partial_name = format!("{}.part", hex(&Sha1::digest(file.name.as_bytes())));
        let partial_path = partial_root.join(partial_name);
        reject_link_or_special_file(&partial_path)?;
        let mut partial = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&partial_path)?;
        let existing = partial.metadata()?.len();
        partial.set_len(file.size)?;
        let mut present = vec![false; file.chunks.len()];
        let mut buffer = Vec::new();
        // The present chunks at the start of the file are hashed as they are
        // verified, rather than read again.
        let mut hasher = Sha1::new();
        let mut hashed = 0;
        for (index, chunk) in file.chunks.iter().enumerate() {
            self.cancel.check()?;
            if chunk.offset + u64::from(chunk.original_size) > existing {
                continue;
            }
            buffer.clear();
            buffer.resize(chunk.original_size as usize, 0);
            partial.seek(SeekFrom::Start(chunk.offset))?;
            partial.read_exact(&mut buffer)?;
            present[index] = format::verify_chunk(&buffer, chunk).is_ok();
            if present[index] && index == hashed {
                hasher.update(&buffer);
                hashed += 1;
            }
        }
        let reused_bytes = file
            .chunks
            .iter()
            .zip(&present)
            .filter(|(_, present)| **present)
            .map(|(chunk, _)| u64::from(chunk.original_size))
            .sum();
        self.reporter
            .update(|status| status.completed_bytes += reused_bytes);
        for (chunk_index, chunk) in file.chunks.iter().enumerate() {
            if present[chunk_index] {
                continue;
            }
            self.cancel.check()?;
            let place = ChunkPlace {
                file: index,
                chunk: chunk_index,
            };
            if !self.chunks.request((place, chunk.clone())) {
                // The writer receives the outcome that stopped the network side.
                break;
            }
        }
        Ok(PreparedFile {
            file,
            relative,
            target,
            partial_path,
            partial,
            missing: present.iter().filter(|present| !**present).count(),
            present,
            hasher,
            hashed,
            buffer,
            reused_bytes,
        })
    }

    /// Checks a fully assembled file's hash and publishes it atomically.
    /// Returns the bytes it reused from an earlier run.
    fn publish(
        &mut self,
        prepared: PreparedFile<'_>,
        root: &Path,
        pending_directory: &mut Option<PathBuf>,
    ) -> Result<u64, ContentError> {
        let PreparedFile {
            file,
            relative,
            target,
            partial_path,
            partial,
            present,
            hasher,
            hashed,
            reused_bytes,
            ..
        } = prepared;
        self.cancel.check()?;
        // Only a file this pass actually repairs is reported: verification
        // finished earlier, and traversing a valid file is one chmod. The
        // phase stays the network side's: chunks still arrive for other files.
        self.reporter
            .update(|status| status.current_file.clone_from(&file.name));
        install::write_out(&partial)?;
        drop(partial);
        if hashed != present.len() || hasher.finalize()[..] != file.sha {
            return Err(invalid("backend.steam.content.assembledHashMismatch"));
        }
        apply_permissions(&partial_path, is_executable(file, &partial_path)?)?;
        // Re-check parents before publishing; never follow existing symlinks under root.
        checked_file_path(root, &relative)?;
        let directory = target.parent().unwrap_or(root).to_path_buf();
        fs::rename(&partial_path, target)?;
        if pending_directory.as_ref() != Some(&directory)
            && let Some(previous) = pending_directory.replace(directory)
        {
            install::write_out(&File::open(previous)?)?;
        }
        self.reporter.update(|status| status.completed_files += 1);
        Ok(reused_bytes)
    }
}

fn invalid(reason: &str) -> ContentError {
    ContentError::InvalidData(reason.into())
}

/// Some depots flag only the launch executable: OpenFront's Electron helpers
/// come unflagged. Programs are then recognised by their header, a Mach-O
/// binary or a script.
fn is_executable(file: &ManifestFile, path: &Path) -> std::io::Result<bool> {
    Ok(file.flags & 32 != 0 || install::starts_as_program(path)?)
}

fn validate_manifest_files(files: &[ManifestFile]) -> Result<(), ContentError> {
    let mut paths = HashSet::new();
    let mut total_size = 0_u64;
    for file in files {
        let path = safe_relative_path(&file.name)?;
        if !paths.insert(path.to_string_lossy().to_lowercase()) {
            return Err(invalid("backend.steam.content.conflictingPaths"));
        }
        if file.is_link() {
            if !link_stays_inside(&path, &file.link_target) {
                return Err(ContentError::UnsupportedSymlink(file.name.clone()));
            }
            continue;
        }
        if file.is_directory() {
            if file.size != 0 || !file.chunks.is_empty() {
                return Err(invalid("backend.steam.content.directoryContainsData"));
            }
            continue;
        }
        if file.sha.len() != 20 {
            return Err(invalid("backend.steam.content.invalidFileChecksum"));
        }
        total_size = total_size
            .checked_add(file.size)
            .ok_or_else(|| invalid("backend.steam.content.invalidDepotSize"))?;
        let mut offset = 0_u64;
        for chunk in &file.chunks {
            if chunk.sha.len() != 20
                || chunk.original_size == 0
                || chunk.original_size as usize > MAX_CHUNK_BYTES
                || chunk.compressed_size as usize > MAX_CHUNK_BYTES
                || chunk.offset != offset
            {
                return Err(invalid("backend.steam.content.invalidChunkLayout"));
            }
            offset = offset
                .checked_add(u64::from(chunk.original_size))
                .ok_or_else(|| invalid("backend.steam.content.invalidChunkSize"))?;
        }
        if offset != file.size {
            return Err(invalid("backend.steam.content.missingChunks"));
        }
    }
    // Nothing sits inside a file, or behind a link.
    let files_only: HashSet<_> = files
        .iter()
        .filter(|file| !file.is_directory())
        .map(|file| {
            safe_relative_path(&file.name).map(|path| path.to_string_lossy().to_lowercase())
        })
        .collect::<Result<_, _>>()?;
    for file in files {
        let path = safe_relative_path(&file.name)?;
        if path
            .ancestors()
            .skip(1)
            .any(|parent| files_only.contains(&parent.to_string_lossy().to_lowercase()))
        {
            return Err(invalid("backend.steam.content.fileUsedAsDirectory"));
        }
    }
    Ok(())
}

/// Whether the link at `path` leads inside the installation. Steam keeps a
/// target as the link holds it, from the link's own folder. Only a target
/// that climbs first, then descends, is followed: past a step that may be
/// another link, `..` could leave the installation.
fn link_stays_inside(path: &Path, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') {
        return false;
    }
    let mut destination: Vec<_> = path
        .parent()
        .into_iter()
        .flat_map(Path::iter)
        .filter_map(|part| part.to_str())
        .collect();
    let mut descending = false;
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." if !descending => {
                if destination.pop().is_none() {
                    return false;
                }
            }
            ".." => return false,
            part => {
                descending = true;
                destination.push(part);
            }
        }
    }
    destination.is_empty() || safe_relative_path(&destination.join("/")).is_ok()
}

#[cfg(test)]
mod tests;
