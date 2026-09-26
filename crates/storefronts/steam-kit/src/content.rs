//! Steam depot delivery: manifests, authenticated CDN chunks, verification and installation.
//!
//! The caller obtains entitlements, depot keys and request codes through Steam's CM.
//! Protocol references and format limits are recorded in docs/steam-content-notes.md.
//! Path confinement, locking, hashing and progress reporting come from `sailover_core`.

mod format;
mod manifest_cache;
mod verification;

pub use manifest_cache::CachedManifest;

use std::{
    collections::{HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reqwest::{Client, Url};
use sailover_core::{
    install::{
        self, InstallError, PARTIAL_DIRECTORY, apply_permissions, checked_file_path,
        ensure_directory, reject_link_or_special_file, safe_relative_path,
    },
    transfer::{self, Cancelled, DownloadPhase, DownloadProgress, DownloadSummary, Reporter},
};
use sha1::{Digest, Sha1};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;
use tokio::sync::mpsc;

use format::{Chunk, Manifest, ManifestFile, decode_chunk, parse_manifest, unpack_zip};

/// Shared with CM avatar hashes.
pub(crate) use sailover_core::install::hex;
pub use sailover_core::transfer::CancelToken;

const MAX_MANIFEST_BYTES: usize = 128 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
/// CDN requests in flight per depot. Chunks are typically 1 MiB, so this also
/// bounds the memory held by decoded chunks waiting to be written.
const CHUNK_CONCURRENCY: usize = 16;
/// Keep upcoming files ready without opening partial files for the whole depot.
/// Chunk payloads remain bounded by the existing network pump and delivery queue.
const FILE_PREFETCH_WINDOW: usize = 8;

/// No Debug implementation: depot keys and CDN tokens are credentials.
pub struct DepotDownload {
    pub depot_id: u32,
    pub manifest_id: u64,
    pub request_code: u64,
    pub depot_key: [u8; 32],
    /// A CDN hostname, optionally including a port. HTTPS is always used.
    pub server: String,
    pub cdn_token: Option<String>,
    pub destination: PathBuf,
    /// Already validated snapshot of the current manifest, if available locally.
    pub cached_manifest: Option<CachedManifest>,
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
    #[cfg(test)]
    test_endpoint: Option<Url>,
}

type ChunkResult = Result<Vec<u8>, ContentError>;

impl ContentClient {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            #[cfg(test)]
            test_endpoint: None,
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

    /// The installation paths of the install scripts an installed depot carries,
    /// from the manifest its installation cached.
    pub async fn install_scripts(
        &self,
        destination: &Path,
        depot_id: u32,
        manifest_id: u64,
        key: &[u8; 32],
    ) -> Result<Vec<String>, ContentError> {
        let cached = self
            .cached_manifest(destination, depot_id, manifest_id, key)
            .await?
            .ok_or_else(|| invalid("backend.steam.content.manifestReadInterrupted"))?;
        Ok(cached
            .manifest
            .files
            .iter()
            .filter(|file| !file.is_directory() && file.flags & crate::install_script::FLAG != 0)
            .map(|file| file.name.clone())
            .collect())
    }

    /// Opt-in protocol smoke test: validates the smallest real file (up to 8 MiB)
    /// without installing a depot or writing any game files.
    #[cfg(test)]
    pub(crate) async fn verify_live_sample(
        &self,
        plan: &DepotDownload,
    ) -> Result<(u64, u32), ContentError> {
        let cancel = CancelToken::new();
        let path = format!(
            "depot/{}/manifest/{}/5/{}",
            plan.depot_id, plan.manifest_id, plan.request_code
        );
        let zipped = self
            .fetch(plan, &path, MAX_MANIFEST_BYTES, &cancel, |_| {})
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
                    && file.flags & 512 == 0
                    && file.link_target.is_empty()
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
            let encrypted = self
                .fetch(plan, &path, MAX_CHUNK_BYTES, &cancel, |_| {})
                .await?;
            let plaintext = decode_chunk(&encrypted, chunk, &plan.depot_key)?;
            size += plaintext.len() as u64;
            hasher.update(plaintext);
        }
        if size != file.size || hasher.finalize()[..] != file.sha {
            return Err(invalid("backend.steam.content.sampleHashMismatch"));
        }
        Ok((size, file.chunks.len() as u32))
    }

    /// Installs one authorized depot. Network work stays on the async runtime while
    /// every disk operation runs on the blocking pool, so a multi-GB depot does not
    /// pin a runtime worker. Existing complete files and verified partial chunks are
    /// reused on the next invocation. File replacement is atomic; an incomplete file
    /// never replaces an installed file.
    pub async fn download_depot(
        &self,
        plan: &DepotDownload,
        cancel: &CancelToken,
        progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Result<DownloadSummary, ContentError> {
        cancel.check()?;
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
            (cached.manifest, None)
        } else {
            let manifest_path = if plan.request_code == 0 {
                format!("depot/{}/manifest/{}/5", plan.depot_id, plan.manifest_id)
            } else {
                format!(
                    "depot/{}/manifest/{}/5/{}",
                    plan.depot_id, plan.manifest_id, plan.request_code
                )
            };
            let zipped = self
                .fetch(
                    plan,
                    &manifest_path,
                    MAX_MANIFEST_BYTES,
                    cancel,
                    |received| {
                        reporter.update(|status| status.network_bytes += received);
                    },
                )
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
        let total_bytes = manifest.files.iter().map(|file| file.size).sum();
        let total_files = u32::try_from(
            manifest
                .files
                .iter()
                .filter(|file| !file.is_directory())
                .count(),
        )
        .map_err(|_| invalid("backend.steam.content.tooManyFiles"))?;
        reporter.update(|status| {
            status.total_bytes = total_bytes;
            status.total_files = total_files;
            status.phase = DownloadPhase::Verifying;
            status.current_file.clear();
        });

        let (requests, request_queue) = mpsc::unbounded_channel();
        let (deliveries, results) = mpsc::channel(CHUNK_CONCURRENCY);
        let disk = DiskInstall {
            destination: plan.destination.clone(),
            depot_id: plan.depot_id,
            manifest_id: plan.manifest_id,
            archive,
            manifest,
            cancel: cancel.clone(),
            reporter: Arc::clone(&reporter),
            requests,
            results,
        };
        let (reused_bytes, ()) = tokio::join!(
            cancel.spawn_blocking(move || disk.run()),
            transfer::pump_chunks(
                request_queue,
                deliveries,
                CHUNK_CONCURRENCY,
                cancel,
                |chunk| self.fetch_chunk(plan, chunk, cancel, &reporter),
            ),
        );
        let reused_bytes = reused_bytes
            .map_err(|_| invalid("backend.steam.content.diskInstallationInterrupted"))??;
        Ok(reporter.summary(reused_bytes))
    }

    /// Three attempts per chunk. Authorization and missing-content responses are
    /// final, and cancellation is never retried. Decryption and decompression run
    /// on the blocking pool.
    async fn fetch_chunk(
        &self,
        plan: &DepotDownload,
        chunk: Chunk,
        cancel: &CancelToken,
        reporter: &Reporter,
    ) -> ChunkResult {
        let path = format!("depot/{}/chunk/{}", plan.depot_id, hex(&chunk.sha));
        let key = plan.depot_key;
        let mut last_error = None;
        for attempt in 0..3 {
            cancel.check()?;
            reporter.update(|status| status.phase = DownloadPhase::Downloading);
            let fetched = self
                .fetch(plan, &path, MAX_CHUNK_BYTES, cancel, |received| {
                    reporter.update(|status| {
                        status.network_bytes += received;
                        status.content_bytes += received;
                    });
                })
                .await;
            match fetched {
                Ok(encrypted) => {
                    let expected = chunk.clone();
                    let decoded = cancel
                        .spawn_blocking(move || decode_chunk(&encrypted, &expected, &key))
                        .await
                        .map_err(|_| invalid("backend.steam.content.chunkDecryptionInterrupted"))?;
                    match decoded {
                        Ok(bytes) => return Ok(bytes),
                        Err(error) => last_error = Some(error),
                    }
                }
                Err(error @ (ContentError::Cancelled | ContentError::Http(401 | 403 | 404))) => {
                    return Err(error);
                }
                Err(error) => last_error = Some(error),
            }
            if attempt < 2 {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))) => {}
                    () = cancel.cancelled() => return Err(ContentError::Cancelled),
                }
            }
        }
        Err(last_error.unwrap_or(ContentError::Network))
    }

    async fn fetch(
        &self,
        plan: &DepotDownload,
        path: &str,
        limit: usize,
        cancel: &CancelToken,
        mut on_bytes: impl FnMut(u64),
    ) -> Result<Vec<u8>, ContentError> {
        let url = content_url(plan, path)?;
        #[cfg(test)]
        let url = if let Some(endpoint) = &self.test_endpoint {
            let mut local = endpoint.join(path).expect("test URL");
            local.set_query(url.query());
            local
        } else {
            url
        };
        let operation = async {
            let mut response = self
                .client
                .get(url)
                .timeout(Duration::from_secs(45))
                .send()
                .await
                .map_err(|_| ContentError::Network)?;
            if !response.status().is_success() {
                return Err(ContentError::Http(response.status().as_u16()));
            }
            let advertised = response.content_length();
            if advertised.is_some_and(|size| size > limit as u64) {
                return Err(invalid("backend.steam.content.cdnResponseTooLarge"));
            }
            let mut bytes = Vec::with_capacity(advertised.map_or(0, |size| size as usize));
            while let Some(chunk) = response.chunk().await.map_err(|_| ContentError::Network)? {
                on_bytes(chunk.len() as u64);
                cancel.check()?;
                if bytes.len().saturating_add(chunk.len()) > limit {
                    return Err(invalid("backend.steam.content.cdnResponseTooLarge"));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        };
        tokio::select! {
            result = operation => result,
            () = cancel.cancelled() => Err(ContentError::Cancelled),
        }
    }
}

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
    requests: mpsc::UnboundedSender<Chunk>,
    results: mpsc::Receiver<ChunkResult>,
}

struct PreparedFile<'a> {
    file: &'a ManifestFile,
    relative: PathBuf,
    target: PathBuf,
    partial_path: PathBuf,
    partial: File,
    reusable: Vec<bool>,
}

impl DiskInstall {
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
        let mut prepared = VecDeque::new();
        loop {
            // Requests retain manifest order across files, so the existing
            // ordered pump can feed the writer without changing chunk identity.
            // Valid files and directories do not occupy the preparation window.
            while prepared.len() < FILE_PREFETCH_WINDOW {
                let Some((index, file)) = remaining.next() else {
                    break;
                };
                self.cancel.check()?;
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
                    apply_permissions(&target, is_executable(file))?;
                    continue;
                }
                prepared.push_back(self.prepare(&partial_root, file, relative, target)?);
            }
            let Some(next) = prepared.pop_front() else {
                break;
            };
            self.cancel.check()?;
            // Only a file this pass actually repairs is reported: verification
            // finished above, and traversing a valid file is one chmod.
            self.reporter.update(|status| {
                status.phase = DownloadPhase::Verifying;
                status.current_file.clone_from(&next.file.name);
            });
            let directory = next.target.parent().unwrap_or(&root).to_path_buf();
            reused_bytes += self.assemble(next, &root)?;
            if pending_directory.as_ref() != Some(&directory)
                && let Some(previous) = pending_directory.replace(directory)
            {
                File::open(previous)?.sync_all()?;
            }
            self.reporter.update(|status| status.completed_files += 1);
        }
        if let Some(directory) = pending_directory {
            File::open(directory)?.sync_all()?;
        }
        Ok(reused_bytes)
    }

    /// Inspect the partial file once and enqueue only its missing chunks. Keep
    /// its open handle and reuse bitmap until the ordered writer reaches it.
    fn prepare<'a>(
        &self,
        partial_root: &Path,
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
        let mut reusable = vec![false; file.chunks.len()];
        let mut buffer = Vec::new();
        for (index, chunk) in file.chunks.iter().enumerate() {
            self.cancel.check()?;
            if chunk.offset + u64::from(chunk.original_size) > existing {
                continue;
            }
            buffer.clear();
            buffer.resize(chunk.original_size as usize, 0);
            partial.seek(SeekFrom::Start(chunk.offset))?;
            partial.read_exact(&mut buffer)?;
            reusable[index] = format::verify_chunk(&buffer, chunk).is_ok();
        }
        for (chunk, _) in file
            .chunks
            .iter()
            .zip(&reusable)
            .filter(|(_, reused)| !**reused)
        {
            self.cancel.check()?;
            if self.requests.send(chunk.clone()).is_err() {
                // The pump may already have queued a terminal CDN error for an
                // earlier file. Let assembly consume that ordered result instead
                // of hiding it behind an interrupted-prefetch error.
                break;
            }
        }
        Ok(PreparedFile {
            file,
            relative,
            target,
            partial_path,
            partial,
            reusable,
        })
    }

    /// Reuses verified chunks already in the partial file, writes fetched chunks
    /// in manifest order and publishes the file atomically. A file assembled only
    /// from fresh chunks is hashed as it is written; one that reused on-disk
    /// chunks is read back so the check covers what actually landed on disk.
    fn assemble(&mut self, prepared: PreparedFile<'_>, root: &Path) -> Result<u64, ContentError> {
        let PreparedFile {
            file,
            relative,
            target,
            partial_path,
            mut partial,
            reusable,
        } = prepared;
        let hash_while_writing = !reusable.contains(&true);
        let mut hasher = Sha1::new();
        let mut reused_bytes = 0;
        for (chunk, reused) in file.chunks.iter().zip(&reusable) {
            self.cancel.check()?;
            if *reused {
                reused_bytes += u64::from(chunk.original_size);
            } else {
                let bytes = self
                    .results
                    .blocking_recv()
                    .ok_or_else(|| invalid("backend.steam.content.chunkDownloadInterrupted"))??;
                partial.seek(SeekFrom::Start(chunk.offset))?;
                partial.write_all(&bytes)?;
                if hash_while_writing {
                    hasher.update(&bytes);
                }
            }
            let size = u64::from(chunk.original_size);
            self.reporter
                .update(|status| status.completed_bytes += size);
        }
        self.cancel.check()?;
        self.reporter
            .update(|status| status.phase = DownloadPhase::Verifying);
        partial.sync_all()?;
        drop(partial);
        let valid = if hash_while_writing {
            hasher.finalize()[..] == file.sha
        } else {
            verify_file(&partial_path, file, &self.cancel)?
        };
        if !valid {
            return Err(invalid("backend.steam.content.assembledHashMismatch"));
        }
        apply_permissions(&partial_path, is_executable(file))?;
        // Re-check parents before publishing; never follow existing symlinks under root.
        checked_file_path(root, &relative)?;
        fs::rename(&partial_path, target)?;
        Ok(reused_bytes)
    }
}

fn content_url(plan: &DepotDownload, path: &str) -> Result<Url, ContentError> {
    // CM supplies a host, not an arbitrary URL. Prevent credentials/query injection.
    if plan.server.is_empty() || plan.server.contains(['/', '?', '#', '@', '\\']) {
        return Err(invalid("backend.steam.content.invalidCdnAddress"));
    }
    let mut url = Url::parse(&format!("https://{}/", plan.server))
        .map_err(|_| invalid("backend.steam.content.invalidCdnAddress"))?;
    url.set_path(path);
    url.set_query(
        plan.cdn_token
            .as_deref()
            .map(|token| token.trim_start_matches('?')),
    );
    Ok(url)
}

fn invalid(reason: &str) -> ContentError {
    ContentError::InvalidData(reason.into())
}

fn is_executable(file: &ManifestFile) -> bool {
    file.flags & 32 != 0
}

fn validate_manifest_files(files: &[ManifestFile]) -> Result<(), ContentError> {
    let mut paths = HashSet::new();
    let mut total_size = 0_u64;
    for file in files {
        let path = safe_relative_path(&file.name)?;
        if !paths.insert(path.to_string_lossy().to_lowercase()) {
            return Err(invalid("backend.steam.content.conflictingPaths"));
        }
        if file.flags & 512 != 0 || !file.link_target.is_empty() {
            return Err(ContentError::UnsupportedSymlink(file.name.clone()));
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

fn verify_file(
    path: &Path,
    expected: &ManifestFile,
    cancel: &CancelToken,
) -> Result<bool, ContentError> {
    Ok(install::verify_sha1_file(
        path,
        expected.size,
        &expected.sha,
        cancel,
    )?)
}

#[cfg(test)]
mod tests;
