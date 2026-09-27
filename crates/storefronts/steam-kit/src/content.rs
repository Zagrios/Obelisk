//! Steam depot delivery: manifests, authenticated CDN chunks, verification and installation.
//!
//! The caller obtains entitlements, depot keys and request codes through Steam's CM.
//! Protocol references and format limits are recorded in docs/steam-content-notes.md.
//! Path confinement, locking, hashing and progress reporting come from `sailover_core`.

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
    time::Duration,
};

use futures_util::future::BoxFuture;
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

use crate::cm::ContentServer;
use format::{Chunk, Manifest, ManifestFile, decode_chunk, parse_manifest, unpack_zip};
use mirrors::{Mirrors, Ticket};

/// Shared with CM avatar hashes.
pub(crate) use sailover_core::install::hex;
pub use sailover_core::transfer::CancelToken;

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

/// Grants the CDN authorization a server may require before serving a depot.
pub trait CdnAuthorization: Sync {
    /// A token for `host`, or `None` when Steam grants none.
    fn token<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Option<String>>;
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
    /// Local servers standing in for CDN hosts; any other host is unreachable.
    #[cfg(test)]
    test_endpoints: HashMap<String, Url>,
}

impl ContentClient {
    pub fn new(client: Client) -> Self {
        Self {
            client,
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
                self.fetch(url, MAX_MANIFEST_BYTES, &cancel, |_| {})
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
            let (encrypted, _) = mirrors
                .fetch(&path, None, 0, &cancel, |url| {
                    self.fetch(url, MAX_CHUNK_BYTES, &cancel, |_| {})
                })
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
    ) -> Result<DownloadSummary, ContentError> {
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
            let reporter = &reporter;
            // Every chunk waits on the manifest: it is raced like the oldest chunk.
            let ticket = mirrors.ticket();
            let (zipped, _) = mirrors
                .fetch(&manifest_path, Some(&ticket), 0, cancel, |url| {
                    self.fetch(url, MAX_MANIFEST_BYTES, cancel, |received| {
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
        let (deliveries, results) = mpsc::channel(WRITE_QUEUE);
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
            transfer::pump_chunks_unordered(
                request_queue,
                deliveries,
                CHUNK_REQUESTS,
                cancel,
                |(place, chunk)| {
                    // Requests are created in manifest order.
                    let ticket = mirrors.ticket();
                    let fetched =
                        self.fetch_chunk(&mirrors, ticket, plan, chunk, cancel, &reporter);
                    async move { fetched.await.map(|bytes| (place, bytes)) }
                },
            ),
        );
        let reused_bytes = reused_bytes
            .map_err(|_| invalid("backend.steam.content.diskInstallationInterrupted"))??;
        Ok(reporter.summary(reused_bytes))
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
        reporter.update(|status| status.phase = DownloadPhase::Downloading);
        let (bytes, received) = mirrors
            .fetch(
                &path,
                Some(&ticket),
                u64::from(chunk.compressed_size),
                cancel,
                |url| async move {
                    let (encrypted, received) = self
                        .fetch(url, MAX_CHUNK_BYTES, cancel, |received| {
                            reporter.update(|status| status.network_bytes += received);
                        })
                        .await?;
                    let expected = chunk.clone();
                    let decoded = cancel
                        .spawn_blocking(move || decode_chunk(&encrypted, &expected, &key))
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
        mut on_bytes: impl FnMut(u64),
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
            let size = bytes.len() as u64;
            Ok((bytes, size))
        };
        tokio::select! {
            result = operation => result,
            () = cancel.cancelled() => Err(ContentError::Cancelled),
        }
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
/// A requested chunk's decoded bytes, delivered as soon as they arrive.
type ChunkResult = Result<(ChunkPlace, Vec<u8>), ContentError>;

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
    requests: mpsc::UnboundedSender<ChunkRequest>,
    results: mpsc::Receiver<ChunkResult>,
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
        self.hash_present(Some((index, bytes)))
    }

    /// Hashes the chunks present from where hashing stopped. The chunk just
    /// written is hashed from memory; one that arrived early is read back.
    fn hash_present(&mut self, written: Option<(usize, &[u8])>) -> Result<(), ContentError> {
        let mut buffer = Vec::new();
        while self.present.get(self.hashed) == Some(&true) {
            match written {
                Some((index, bytes)) if index == self.hashed => self.hasher.update(bytes),
                _ => {
                    let chunk = &self.file.chunks[self.hashed];
                    buffer.resize(chunk.original_size as usize, 0);
                    self.partial.seek(SeekFrom::Start(chunk.offset))?;
                    self.partial.read_exact(&mut buffer)?;
                    self.hasher.update(&buffer);
                }
            }
            self.hashed += 1;
        }
        Ok(())
    }
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
        // Files being assembled, by manifest index.
        let mut assembling = HashMap::new();
        // Chunks requested for them and not written yet.
        let mut queued = 0;
        loop {
            // Requests follow manifest order. Files are prepared until enough
            // of their chunks are queued; valid files and directories do not
            // count.
            while assembling.len() < PREPARED_FILES && queued < QUEUED_CHUNKS {
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
            let (place, bytes) = self
                .results
                .blocking_recv()
                .ok_or_else(|| invalid("backend.steam.content.chunkDownloadInterrupted"))??;
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
        if let Some(directory) = pending_directory {
            File::open(directory)?.sync_all()?;
        }
        Ok(reused_bytes)
    }

    /// Inspect the partial file once and request only its missing chunks. Keep
    /// its open handle until its last chunk is written.
    fn prepare<'a>(
        &self,
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
            if self.requests.send((place, chunk.clone())).is_err() {
                // The pump may already have queued a terminal CDN error. Let
                // the writer receive that result instead of hiding it behind an
                // interrupted-prefetch error.
                break;
            }
        }
        let mut prepared = PreparedFile {
            file,
            relative,
            target,
            partial_path,
            partial,
            missing: present.iter().filter(|present| !**present).count(),
            present,
            hasher: Sha1::new(),
            hashed: 0,
            reused_bytes,
        };
        prepared.hash_present(None)?;
        Ok(prepared)
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
        // finished earlier, and traversing a valid file is one chmod.
        self.reporter.update(|status| {
            status.phase = DownloadPhase::Verifying;
            status.current_file.clone_from(&file.name);
        });
        partial.sync_all()?;
        drop(partial);
        if hashed != present.len() || hasher.finalize()[..] != file.sha {
            return Err(invalid("backend.steam.content.assembledHashMismatch"));
        }
        apply_permissions(&partial_path, is_executable(file))?;
        // Re-check parents before publishing; never follow existing symlinks under root.
        checked_file_path(root, &relative)?;
        let directory = target.parent().unwrap_or(root).to_path_buf();
        fs::rename(&partial_path, target)?;
        if pending_directory.as_ref() != Some(&directory)
            && let Some(previous) = pending_directory.replace(directory)
        {
            File::open(previous)?.sync_all()?;
        }
        self.reporter.update(|status| status.completed_files += 1);
        Ok(reused_bytes)
    }
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

#[cfg(test)]
mod tests;
