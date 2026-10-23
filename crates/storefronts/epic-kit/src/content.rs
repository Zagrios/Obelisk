//! Epic BuildPatch chunk delivery and atomic installation.
//!
//! The shared chunk engine and the blocking disk worker come from `obelisk_core`,
//! as do path confinement, locking, hashing and progress reporting.
//! Complete files and verified chunk caches survive cancellation and are reused.
mod verification;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use obelisk_core::{
    install::{
        self, PARTIAL_DIRECTORY, apply_permissions, checked_file_path, ensure_directory, hex,
        safe_relative_path,
    },
    transfer::{
        self, BodyError, BodySize, CancelToken, Chunks, Delivery, DownloadPhase, DownloadProgress,
        DownloadSummary, Interrupted, Pipeline, Reporter,
    },
};
use reqwest::{Client, Url};
use sha1::{Digest, Sha1};

use crate::{
    EpicError,
    auth::credential_free,
    content_manifest::{Chunk, ChunkGuid, Manifest, ManifestFile},
};

const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
/// Chunk requests in flight. Chunks are about 1 MiB and arrive in request
/// order, so one slow chunk holds up the ones after it: more in flight keep
/// the link busy meanwhile.
const CHUNK_CONCURRENCY: usize = 32;
/// Decoded chunks waiting for the disk worker.
const WRITE_QUEUE: usize = 16;
/// Chunks requested ahead of the disk worker: enough that requests in
/// flight never wait for it to plan files. Small files share chunks, so a
/// count of files planned ahead left only a few requests in flight.
const QUEUED_CHUNKS: usize = 2 * CHUNK_CONCURRENCY;
/// Files planned ahead at most, whatever their chunks.
const FILE_PREFETCH: usize = 256;
/// Fetched chunks kept in memory for the files after the one that fetched
/// them: Epic packs consecutive small files into shared chunks, and reading
/// each part back from the chunk cache kept the disk worker busy, on an
/// external disk most of the time.
const RECENT_CHUNK_BYTES: usize = 64 * 1024 * 1024;
const PIPELINE: Pipeline = Pipeline {
    delivery: Delivery::InOrder,
    fetches: CHUNK_CONCURRENCY,
    queue: WRITE_QUEUE,
};

#[derive(Clone)]
pub struct ContentClient {
    client: Client,
}

type ChunkResult = Result<Vec<u8>, EpicError>;

impl From<Interrupted> for EpicError {
    fn from(_: Interrupted) -> Self {
        Self::Protocol
    }
}

impl From<BodyError> for EpicError {
    fn from(error: BodyError) -> Self {
        match error {
            BodyError::Network => Self::Network,
            BodyError::Size => Self::Protocol,
            BodyError::Cancelled => Self::Cancelled,
        }
    }
}

impl ContentClient {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    pub async fn download(
        &self,
        manifest: Arc<Manifest>,
        base_urls: &[Url],
        destination: &Path,
        cancel: &CancelToken,
        progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Result<DownloadSummary, EpicError> {
        cancel.check()?;
        validate_manifest(&manifest)?;
        check_base_urls(base_urls)?;
        let reporter = Reporter::new(
            DownloadProgress {
                phase: DownloadPhase::Verifying,
                total_bytes: manifest.files.iter().map(ManifestFile::size).sum(),
                total_files: u32::try_from(manifest.files.len())
                    .map_err(|_| EpicError::Protocol)?,
                ..Default::default()
            },
            progress,
        );
        let feature_level = manifest.feature_level;
        let disk = DiskInstall::engine_side(
            destination.to_owned(),
            manifest,
            cancel.clone(),
            Arc::clone(&reporter),
        );
        let reused_bytes = transfer::run_engine(PIPELINE, cancel, disk, |chunk| {
            self.fetch_chunk(feature_level, base_urls, chunk, cancel, &reporter)
        })
        .await?;
        Ok(reporter.summary(reused_bytes))
    }

    /// The first `bytes` of the manifest's `file`, from the chunks that hold
    /// them, without installing it.
    pub(crate) async fn read(
        &self,
        manifest: &Manifest,
        base_urls: &[Url],
        file: &ManifestFile,
        bytes: u64,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, EpicError> {
        check_base_urls(base_urls)?;
        let reporter = Reporter::new(DownloadProgress::default(), |_| {});
        let mut output = Vec::new();
        for part in &file.parts {
            if output.len() as u64 >= bytes {
                break;
            }
            let chunk = manifest
                .chunks
                .get(&part.guid)
                .ok_or(EpicError::Protocol)?
                .clone();
            let window = self
                .fetch_chunk(manifest.feature_level, base_urls, chunk, cancel, &reporter)
                .await?;
            let start = part.offset as usize;
            output.extend_from_slice(
                window
                    .get(start..start + part.size as usize)
                    .ok_or(EpicError::Protocol)?,
            );
        }
        output.truncate(usize::try_from(bytes).unwrap_or(usize::MAX));
        Ok(output)
    }

    async fn fetch_chunk(
        &self,
        feature_level: u32,
        base_urls: &[Url],
        chunk: Chunk,
        cancel: &CancelToken,
        reporter: &Reporter,
    ) -> ChunkResult {
        let mut failure = EpicError::Network;
        // Every mirror in turn; then, while the network or the servers fail,
        // every mirror again after each of about a minute's waits.
        for late in 0_usize.. {
            for base_url in base_urls {
                let mut url = base_url
                    .join(&chunk.relative_path(feature_level))
                    .map_err(|_| EpicError::Protocol)?;
                // Signed manifest query parameters apply to the manifest only.
                // Account authorization is never attached to CDN chunk requests.
                url.set_query(None);
                for attempt in 0..3 {
                    cancel.check()?;
                    reporter.update(|status| status.phase = DownloadPhase::Downloading);
                    match self.fetch(&url, cancel, reporter).await {
                        Ok(bytes) => {
                            let expected = chunk.clone();
                            let received = cfg!(debug_assertions).then(|| bytes.clone());
                            match cancel
                                .spawn_blocking(move || decode_chunk(&bytes, &expected))
                                .await
                                .map_err(|_| EpicError::Protocol)?
                            {
                                Ok(bytes) => return Ok(bytes),
                                Err(error) => {
                                    if let Some(received) = &received {
                                        crate::details::diagnostic::write_chunk(
                                            &url, &chunk, received, &error,
                                        );
                                    }
                                    if matches!(error, EpicError::UnsupportedContent) {
                                        return Err(error);
                                    }
                                    failure = error;
                                }
                            }
                        }
                        Err(EpicError::Cancelled) => return Err(EpicError::Cancelled),
                        Err(error @ EpicError::Http(401 | 403 | 404)) => {
                            // A rejected or absent chunk is final for this mirror,
                            // but another current delivery URL may still serve it.
                            failure = error;
                            break;
                        }
                        Err(error) => failure = error,
                    }
                    if attempt < 2 {
                        cancel
                            .race(tokio::time::sleep(Duration::from_millis(
                                250 * (attempt + 1),
                            )))
                            .await?;
                    }
                }
            }
            let outage = matches!(
                failure,
                EpicError::Network | EpicError::Http(429 | 500..=599)
            );
            match transfer::NETWORK_RETRY_WAITS.get(late) {
                Some(wait) if outage => cancel.race(tokio::time::sleep(*wait)).await?,
                _ => break,
            }
        }
        Err(failure)
    }

    async fn fetch(&self, url: &Url, cancel: &CancelToken, reporter: &Reporter) -> ChunkResult {
        // A server that stops sending fails the request, not a slow link.
        let request = self
            .client
            .get(url.clone())
            // Chunks are compressed already: never gzip them again.
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .timeout(transfer::CHUNK_DEADLINE)
            .send();
        let response = cancel
            .race(tokio::time::timeout(transfer::CHUNK_STALL, request))
            .await?
            .map_err(|_| EpicError::Network)?
            .map_err(|_| EpicError::Network)?;
        if !response.status().is_success() {
            return Err(EpicError::Http(response.status().as_u16()));
        }
        // At most, never exactly `chunk.file_size`: that size is only an
        // estimate (see decode_chunk).
        Ok(transfer::read_body(
            response.content_length(),
            transfer::unless_stalled(response.bytes_stream(), transfer::CHUNK_STALL),
            BodySize::AtMost((MAX_CHUNK_BYTES + 98) as u64),
            cancel,
            |count| {
                reporter.update(|status| {
                    status.network_bytes += count;
                    status.content_bytes += count;
                });
            },
        )
        .await?)
    }
}

struct DiskInstall {
    destination: PathBuf,
    manifest: Arc<Manifest>,
    cancel: CancelToken,
    reporter: Arc<Reporter>,
    chunks: Chunks<Chunk, Vec<u8>, EpicError>,
    prefetch: FilePrefetch,
}

#[derive(Clone, Copy)]
enum ChunkState {
    Cached,
    Pending,
    /// Written by this run; `verified` once read back and hashed.
    Stored {
        verified: bool,
    },
    /// Fetched by this run for the one file that needs it, and kept in
    /// memory only.
    Held,
}

struct PlannedFile {
    index: usize,
    reused: HashSet<ChunkGuid>,
    // A previous file owns these queued chunks: they must have arrived when
    // this file is reached. Their cached copy is checked if it is read.
    delivered_earlier: HashSet<ChunkGuid>,
}

#[derive(Default)]
struct FilePrefetch {
    next_file: usize,
    files: VecDeque<PlannedFile>,
    chunks: HashMap<ChunkGuid, ChunkState>,
    pending: VecDeque<ChunkGuid>,
    recent: RecentChunks,
    /// Parts of the unfinished files that each chunk still serves.
    remaining: HashMap<ChunkGuid, usize>,
}

/// The chunks this run fetched last, verified as they decoded, newest last,
/// within `RECENT_CHUNK_BYTES`; the newest always stays.
#[derive(Default)]
struct RecentChunks {
    chunks: VecDeque<(ChunkGuid, Vec<u8>)>,
    bytes: usize,
}

impl RecentChunks {
    fn get(&self, guid: &ChunkGuid) -> Option<&[u8]> {
        self.chunks
            .iter()
            .rev()
            .find(|(kept, _)| kept == guid)
            .map(|(_, bytes)| &bytes[..])
    }

    fn keep(&mut self, guid: ChunkGuid, bytes: Vec<u8>) {
        self.bytes += bytes.len();
        self.chunks.push_back((guid, bytes));
        while self.bytes > RECENT_CHUNK_BYTES && self.chunks.len() > 1 {
            if let Some((_, oldest)) = self.chunks.pop_front() {
                self.bytes -= oldest.len();
            }
        }
    }
}

impl DiskInstall {
    /// The disk side of one engine run, from its start state.
    fn engine_side(
        destination: PathBuf,
        manifest: Arc<Manifest>,
        cancel: CancelToken,
        reporter: Arc<Reporter>,
    ) -> impl FnOnce(Chunks<Chunk, Vec<u8>, EpicError>) -> Result<u64, EpicError> + Send + 'static
    {
        move |chunks| {
            Self {
                destination,
                manifest,
                cancel,
                reporter,
                chunks,
                prefetch: FilePrefetch::default(),
            }
            .run()
        }
    }

    /// Returns the number of verified bytes reused from disk.
    fn run(mut self) -> Result<u64, EpicError> {
        self.cancel.check()?;
        fs::create_dir_all(&self.destination)?;
        let root = fs::canonicalize(&self.destination)?;
        let _lock = install::lock_engine(&root)?.ok_or(EpicError::AlreadyDownloading)?;
        let manifest = Arc::clone(&self.manifest);
        let partial_relative = PathBuf::from(PARTIAL_DIRECTORY)
            .join("epic")
            .join(manifest_key(&manifest));
        let partial_root = ensure_directory(&root, &partial_relative)?;
        let chunk_root = ensure_directory(&partial_root, Path::new("chunks"))?;
        let verified =
            verification::verify_files(&root, &manifest.files, &self.cancel, &self.reporter)?;
        let mut reused_bytes = verified.reused_bytes;
        for (file, valid) in manifest.files.iter().zip(&verified.valid) {
            if !valid {
                for part in &file.parts {
                    *self.prefetch.remaining.entry(part.guid).or_insert(0) += 1;
                }
            }
        }
        self.prefetch_files(&verified.valid, &chunk_root)?;
        // Parents are created and checked once per distinct directory; every
        // file's full path is still confined right before it is written.
        let mut ensured_parents: HashSet<PathBuf> = HashSet::new();
        for (index, (file, valid)) in manifest.files.iter().zip(&verified.valid).enumerate() {
            self.cancel.check()?;
            let relative = safe_relative_path(&file.filename)?;
            let parent = relative.parent().unwrap_or(Path::new(""));
            if !ensured_parents.contains(parent) {
                ensure_directory(&root, parent)?;
                ensured_parents.insert(parent.to_path_buf());
            }
            let target = checked_file_path(&root, &relative)?;
            if *valid {
                apply_permissions(&target, is_executable(file, &target)?)?;
                continue;
            }
            // The phase stays the network side's: chunks still arrive for
            // the files after this one.
            self.reporter
                .update(|status| status.current_file.clone_from(&file.filename));
            let plan = self.prefetch.files.pop_front().ok_or(EpicError::Protocol)?;
            if plan.index != index {
                return Err(EpicError::Protocol);
            }
            reused_bytes +=
                self.assemble(file, &plan, &partial_root, &chunk_root, &root, &relative)?;
            // A published file can be verified directly next time. Keep only
            // chunks that are still required by unfinished files.
            for part in &file.parts {
                let remaining = self
                    .prefetch
                    .remaining
                    .get_mut(&part.guid)
                    .ok_or(EpicError::Protocol)?;
                *remaining -= 1;
                // A chunk held in memory only has no cached copy.
                if *remaining == 0
                    && !matches!(
                        self.prefetch.chunks.remove(&part.guid),
                        Some(ChunkState::Held)
                    )
                {
                    let chunk = manifest.chunks.get(&part.guid).ok_or(EpicError::Protocol)?;
                    fs::remove_file(checked_file_path(
                        &chunk_root,
                        Path::new(&chunk_key(chunk)),
                    )?)?;
                }
            }
            self.reporter.update(|status| status.completed_files += 1);
            self.prefetch_files(&verified.valid, &chunk_root)?;
        }
        self.cancel.check()?;
        // Every file is in place: durable before the receipt says so, and
        // recorded whole for the next Transfer to skip.
        verification::complete(&root, &manifest.files)?;
        // Keep caches on failure/cancellation only, avoiding a second permanent copy of the game.
        fs::remove_dir_all(partial_root)?;
        Ok(reused_bytes)
    }

    /// Plans the next repair files, until `QUEUED_CHUNKS` chunks wait for
    /// the network, while the current file is written and published. Requests remain in first-consumer order, including chunks
    /// shared across files, so InOrder delivery needs no reordering.
    fn prefetch_files(&mut self, valid: &[bool], chunk_root: &Path) -> Result<(), EpicError> {
        while self.prefetch.files.len() < FILE_PREFETCH
            && self.prefetch.pending.len() < QUEUED_CHUNKS
            && self.prefetch.next_file < self.manifest.files.len()
            && !self.chunks.refused()
        {
            self.cancel.check()?;
            let index = self.prefetch.next_file;
            self.prefetch.next_file += 1;
            if valid[index] {
                continue;
            }
            let mut plan = PlannedFile {
                index,
                reused: HashSet::new(),
                delivered_earlier: HashSet::new(),
            };
            let mut seen = HashSet::new();
            for part in &self.manifest.files[index].parts {
                self.cancel.check()?;
                if !seen.insert(part.guid) {
                    continue;
                }
                match self.prefetch.chunks.get(&part.guid) {
                    Some(ChunkState::Pending) => {
                        plan.reused.insert(part.guid);
                        plan.delivered_earlier.insert(part.guid);
                        continue;
                    }
                    // Verified on disk earlier in this run and not yet evicted;
                    // the entry is dropped with the file at every removal site.
                    Some(ChunkState::Cached | ChunkState::Stored { verified: true }) => {
                        plan.reused.insert(part.guid);
                        continue;
                    }
                    // Still in memory, as it decoded: assembly checks the
                    // cached copy only if it has to read it.
                    Some(ChunkState::Stored { verified: false })
                        if self.prefetch.recent.get(&part.guid).is_some() =>
                    {
                        plan.reused.insert(part.guid);
                        continue;
                    }
                    Some(ChunkState::Stored { verified: false } | ChunkState::Held) | None => (),
                }
                let chunk = self
                    .manifest
                    .chunks
                    .get(&part.guid)
                    .ok_or(EpicError::Protocol)?;
                let path = checked_file_path(chunk_root, Path::new(&chunk_key(chunk)))?;
                if verify_file(
                    &path,
                    u64::from(chunk.window_size),
                    &chunk.sha1,
                    &self.cancel,
                )? {
                    self.prefetch
                        .chunks
                        .entry(part.guid)
                        .and_modify(|state| *state = ChunkState::Stored { verified: true })
                        .or_insert(ChunkState::Cached);
                    plan.reused.insert(part.guid);
                } else {
                    if !self.chunks.request(chunk.clone()) {
                        // The network side stopped and its outcome is queued
                        // ahead of this request. Keep this partial plan so
                        // assembly reaches it before any unsent chunk.
                        break;
                    }
                    self.prefetch.pending.push_back(part.guid);
                    self.prefetch.chunks.insert(part.guid, ChunkState::Pending);
                }
            }
            self.prefetch.files.push_back(plan);
        }
        Ok(())
    }

    /// Assembles and publishes one file. A file left unfinished, by a
    /// cancellation or a failure, writes the chunks it held in memory only to
    /// the chunk cache, so the next attempt resumes from them.
    fn assemble(
        &mut self,
        file: &ManifestFile,
        plan: &PlannedFile,
        partial_root: &Path,
        chunk_root: &Path,
        root: &Path,
        relative: &Path,
    ) -> Result<u64, EpicError> {
        let assembled = self.assemble_file(file, plan, partial_root, chunk_root, root, relative);
        if assembled.is_err() {
            for part in &file.parts {
                // Best effort: the failure that ended the file is the one
                // reported. A chunk written is no longer held, so a chunk of
                // several parts is written once.
                if let (Some(ChunkState::Held), Some(chunk), Some(bytes)) = (
                    self.prefetch.chunks.get(&part.guid),
                    self.manifest.chunks.get(&part.guid),
                    self.prefetch.recent.get(&part.guid),
                ) && let Ok(path) = checked_file_path(chunk_root, Path::new(&chunk_key(chunk)))
                    && fs::write(path, bytes).is_ok()
                {
                    self.prefetch
                        .chunks
                        .insert(part.guid, ChunkState::Stored { verified: false });
                }
            }
        }
        assembled
    }

    /// Reuses verified cached chunks, writes each fresh chunk that another
    /// file needs to the cache, and each straight into the partial file from
    /// memory, hashes what was assembled and publishes the file atomically.
    /// Only chunks that were already on disk, or that left memory, are read
    /// back.
    fn assemble_file(
        &mut self,
        file: &ManifestFile,
        plan: &PlannedFile,
        partial_root: &Path,
        chunk_root: &Path,
        root: &Path,
        relative: &Path,
    ) -> Result<u64, EpicError> {
        let manifest = Arc::clone(&self.manifest);
        for guid in &plan.delivered_earlier {
            if !matches!(
                self.prefetch.chunks.get(guid),
                Some(ChunkState::Stored { .. })
            ) {
                return Err(EpicError::Protocol);
            }
        }
        let mut delivered = HashSet::new();
        let partial_name = format!("{}.part", hex(&Sha1::digest(file.filename.as_bytes())));
        let partial_path = checked_file_path(partial_root, Path::new(&partial_name))?;
        let mut partial = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&partial_path)?;
        let mut reused_bytes = 0;
        let mut hash = Sha1::new();
        let mut buffer = [0_u8; 64 * 1024];
        // The chunks of a file whose windows fit in memory together, and
        // that no other file needs, stay in memory only: writing them to the
        // chunk cache too doubled the disk's work. A file left unfinished
        // writes them there then; see `assemble`.
        let mut uses: HashMap<ChunkGuid, usize> = HashMap::new();
        for part in &file.parts {
            *uses.entry(part.guid).or_insert(0) += 1;
        }
        let windows: usize = uses
            .keys()
            .filter_map(|guid| manifest.chunks.get(guid))
            .map(|chunk| chunk.window_size as usize)
            .sum();
        let held: HashSet<ChunkGuid> = uses
            .iter()
            .filter(|(guid, count)| {
                windows <= RECENT_CHUNK_BYTES && self.prefetch.remaining.get(*guid) == Some(*count)
            })
            .map(|(guid, _)| *guid)
            .collect();
        // The chunks fetched last stay in memory for the parts that follow
        // them; anything older is read back from the chunk cache, checked
        // first when this run wrote it.
        for part in &file.parts {
            self.cancel.check()?;
            let chunk = manifest.chunks.get(&part.guid).ok_or(EpicError::Protocol)?;
            if !plan.reused.contains(&part.guid) && delivered.insert(part.guid) {
                if self.prefetch.pending.front() != Some(&part.guid) {
                    return Err(EpicError::Protocol);
                }
                let bytes = self.chunks.receive()?;
                self.cancel.check()?;
                // A cached chunk is checked by size and SHA-1 before reuse, so a
                // torn write after a crash only costs one more download.
                let state = if held.contains(&part.guid) {
                    ChunkState::Held
                } else {
                    let path = checked_file_path(chunk_root, Path::new(&chunk_key(chunk)))?;
                    fs::write(path, &bytes)?;
                    ChunkState::Stored { verified: false }
                };
                self.prefetch.pending.pop_front();
                self.prefetch.chunks.insert(part.guid, state);
                self.prefetch.recent.keep(part.guid, bytes);
            }
            let start = part.offset as usize;
            let end = start + part.size as usize;
            match self.prefetch.recent.get(&part.guid) {
                Some(bytes) => {
                    let slice = bytes.get(start..end).ok_or(EpicError::Protocol)?;
                    partial.write_all(slice)?;
                    hash.update(slice);
                }
                None => {
                    let path = checked_file_path(chunk_root, Path::new(&chunk_key(chunk)))?;
                    if let Some(ChunkState::Stored { verified: false }) =
                        self.prefetch.chunks.get(&part.guid)
                    {
                        if !verify_file(
                            &path,
                            u64::from(chunk.window_size),
                            &chunk.sha1,
                            &self.cancel,
                        )? {
                            return Err(EpicError::Integrity);
                        }
                        self.prefetch
                            .chunks
                            .insert(part.guid, ChunkState::Stored { verified: true });
                    }
                    let mut source = File::open(path)?;
                    source.seek(SeekFrom::Start(u64::from(part.offset)))?;
                    let mut remaining = part.size as usize;
                    while remaining > 0 {
                        self.cancel.check()?;
                        let count = remaining.min(buffer.len());
                        source.read_exact(&mut buffer[..count])?;
                        partial.write_all(&buffer[..count])?;
                        hash.update(&buffer[..count]);
                        remaining -= count;
                    }
                }
            }
            if plan.reused.contains(&part.guid) {
                reused_bytes += u64::from(part.size);
            }
            self.reporter
                .update(|status| status.completed_bytes += u64::from(part.size));
        }
        self.cancel.check()?;
        if hash.finalize()[..] != file.hash {
            return Err(EpicError::Integrity);
        }
        install::write_out(&partial)?;
        drop(partial);
        apply_permissions(&partial_path, is_executable(file, &partial_path)?)?;
        self.cancel.check()?;
        let target = checked_file_path(root, relative)?;
        fs::rename(&partial_path, &target)?;
        install::write_out(&File::open(target.parent().unwrap_or(root))?)?;
        Ok(reused_bytes)
    }
}

/// Manifests flag only the launch executable of some builds: Prison Architect's
/// Paradox bootstrap runs an unflagged installer beside it. Programs are then
/// recognised by their header, a Mach-O binary or a script.
fn is_executable(file: &ManifestFile, path: &Path) -> std::io::Result<bool> {
    Ok(file.flags & 4 != 0 || install::starts_as_program(path)?)
}

fn manifest_key(manifest: &Manifest) -> String {
    let mut hash = Sha1::new();
    hash.update(manifest.app_name.as_bytes());
    hash.update([0]);
    hash.update(manifest.build_version.as_bytes());
    for file in &manifest.files {
        hash.update(file.filename.as_bytes());
        hash.update(file.hash);
    }
    hex(&hash.finalize())
}

fn chunk_key(chunk: &Chunk) -> String {
    let [a, b, c, d] = chunk.guid;
    format!(
        "{a:08x}{b:08x}{c:08x}{d:08x}-{}-{:016x}.chunk",
        hex(&chunk.sha1),
        chunk.hash
    )
}

pub(crate) fn validate_manifest(manifest: &Manifest) -> Result<(), EpicError> {
    let mut paths = HashSet::new();
    let mut total_size = 0_u64;
    for file in &manifest.files {
        let relative = safe_relative_path(&file.filename)?;
        if file
            .symlink_target
            .as_ref()
            .is_some_and(|target| !target.is_empty())
        {
            return Err(EpicError::UnsupportedContent);
        }
        if !paths.insert(relative.to_string_lossy().to_lowercase()) {
            return Err(EpicError::Protocol);
        }
        total_size = total_size
            .checked_add(file.size())
            .ok_or(EpicError::Protocol)?;
        for part in &file.parts {
            let chunk = manifest.chunks.get(&part.guid).ok_or(EpicError::Protocol)?;
            if chunk.guid != part.guid
                || chunk.window_size == 0
                || chunk.window_size as usize > MAX_CHUNK_BYTES
                || chunk.file_size > (MAX_CHUNK_BYTES + 98) as u64
                || chunk.sha1 == [0; 20]
                || part.size == 0
                || u64::from(part.offset) + u64::from(part.size) > u64::from(chunk.window_size)
            {
                return Err(EpicError::Protocol);
            }
        }
    }
    // Reject a file that is also the parent directory of another file.
    for file in &manifest.files {
        let path = safe_relative_path(&file.filename)?;
        for parent in path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            if paths.contains(&parent.to_string_lossy().to_lowercase()) {
                return Err(EpicError::Protocol);
            }
        }
    }
    Ok(())
}

/// Chunks come from at most eight HTTPS delivery URLs, without credentials.
fn check_base_urls(base_urls: &[Url]) -> Result<(), EpicError> {
    if base_urls.is_empty() || base_urls.len() > 8 {
        return Err(EpicError::Protocol);
    }
    for base_url in base_urls {
        let secure = base_url.scheme() == "https";
        #[cfg(test)]
        let secure = secure
            || base_url.scheme() == "http"
                && base_url
                    .host_str()
                    .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "[::1]"));
        if !secure || !credential_free(base_url) {
            return Err(EpicError::Protocol);
        }
    }
    Ok(())
}

pub(crate) fn decode_chunk(bytes: &[u8], chunk: &Chunk) -> ChunkResult {
    let u32_at = |offset: usize| -> Result<u32, EpicError> {
        bytes
            .get(offset..offset + 4)
            .and_then(|part| part.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or(EpicError::Protocol)
    };
    if u32_at(0)? != 0xB1FE3AA2 {
        return Err(EpicError::Protocol);
    }
    let version = u32_at(4)?;
    let header_size = u32_at(8)? as usize;
    let expected_header = match version {
        1 => 41,
        2 => 62,
        3 => 66,
        4 => 98,
        _ => return Err(EpicError::Protocol),
    };
    let compressed_size = u32_at(12)? as usize;
    // The manifest's chunk file size is only an estimate: older manifests
    // list sizes for an older header, such as Unrailed's, 4 bytes short of
    // the version 3 header the CDN serves. The decoded window's SHA-1, from
    // the manifest, proves the chunk.
    if header_size != expected_header || bytes.len() != header_size + compressed_size {
        return Err(EpicError::Protocol);
    }
    for (index, word) in chunk.guid.iter().enumerate() {
        if u32_at(16 + 4 * index)? != *word {
            return Err(EpicError::Integrity);
        }
    }
    let header_hash = u64::from_le_bytes(
        bytes
            .get(32..40)
            .ok_or(EpicError::Protocol)?
            .try_into()
            .map_err(|_| EpicError::Protocol)?,
    );
    let stored_as = *bytes.get(40).ok_or(EpicError::Protocol)?;
    if stored_as & !1 != 0 {
        // Encrypted BuildPatch chunks require separate entitlement secrets.
        return Err(EpicError::UnsupportedContent);
    }
    let hash_type = if version >= 2 { bytes[61] } else { 1 };
    if hash_type == 0
        || hash_type & !3 != 0
        || version >= 3 && u32_at(62)? != chunk.window_size
        || version >= 4 && bytes[66..82].iter().any(|byte| *byte != 0)
    {
        return Err(EpicError::Protocol);
    }
    let payload = &bytes[header_size..];
    let decoded = if stored_as & 1 != 0 {
        crate::content_manifest::inflate(payload, chunk.window_size as usize)
            .ok_or(EpicError::Integrity)?
    } else {
        payload.to_vec()
    };
    // The manifest's SHA-1 proves the window: its rolling hash, another
    // pass over every byte, adds nothing to it, and only the header's copy
    // is compared.
    let sha: [u8; 20] = Sha1::digest(&decoded).into();
    if decoded.len() != chunk.window_size as usize
        || sha != chunk.sha1
        || hash_type & 2 != 0 && bytes[41..61] != sha
        || hash_type & 1 != 0 && header_hash != chunk.hash
    {
        return Err(EpicError::Integrity);
    }
    Ok(decoded)
}

/// BuildPatch's byte-table rolling hash, using the reflected CRC64 polynomial.
pub(crate) fn rolling_hash(bytes: &[u8]) -> u64 {
    const TABLE: [u64; 256] = {
        let mut table = [0; 256];
        let mut index = 0;
        while index < 256 {
            let mut value = index as u64;
            let mut bit = 0;
            while bit < 8 {
                value = (value >> 1)
                    ^ if value & 1 != 0 {
                        0xC96C5795D7870F42
                    } else {
                        0
                    };
                bit += 1;
            }
            table[index] = value;
            index += 1;
        }
        table
    };
    bytes.iter().fold(0, |hash, byte| {
        hash.rotate_left(1) ^ TABLE[usize::from(*byte)]
    })
}

fn verify_file(
    path: &Path,
    size: u64,
    expected: &[u8; 20],
    cancel: &CancelToken,
) -> Result<bool, EpicError> {
    Ok(install::verify_sha1_file(path, size, expected, cancel)?)
}

#[cfg(test)]
mod tests;
