//! Epic BuildPatch chunk delivery and atomic installation.
//!
//! The bounded network pump and blocking disk worker follow Steam's installer;
//! path confinement, locking, hashing and progress reporting come from `forge_core`.
//! Complete files and verified chunk caches survive cancellation and are reused.
mod verification;

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use forge_core::{
    install::{
        self, PARTIAL_DIRECTORY, apply_permissions, checked_file_path, ensure_directory, hex,
        safe_relative_path, try_lock_file,
    },
    transfer::{self, CancelToken, DownloadPhase, DownloadProgress, DownloadSummary, Reporter},
};
use reqwest::{Client, Url};
use sha1::{Digest, Sha1};
use tokio::sync::mpsc;

use crate::{
    EpicError,
    content_manifest::{Chunk, ChunkGuid, Manifest, ManifestFile},
};

const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_CONCURRENCY: usize = 6;

#[derive(Clone)]
pub struct ContentClient {
    client: Client,
}

type ChunkResult = Result<Vec<u8>, EpicError>;

// Dropping an async task does not stop spawn_blocking. Wake its network wait
// and let its next disk check stop before a new attempt can acquire the lock.
struct CancellationGuard(Option<CancelToken>);
impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if let Some(cancel) = &self.0 {
            cancel.cancel();
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
            if !secure
                || !base_url.username().is_empty()
                || base_url.password().is_some()
                || base_url.fragment().is_some()
            {
                return Err(EpicError::Protocol);
            }
        }
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
        let (requests, receiver) = mpsc::unbounded_channel();
        let (deliveries, results) = mpsc::channel(CHUNK_CONCURRENCY);
        let disk = DiskInstall {
            destination: destination.to_owned(),
            manifest,
            cancel: cancel.clone(),
            reporter: Arc::clone(&reporter),
            requests,
            results,
        };
        let mut cancellation_guard = CancellationGuard(Some(cancel.clone()));
        let (reused_bytes, ()) = tokio::join!(
            tokio::task::spawn_blocking(move || disk.run()),
            transfer::pump_chunks(receiver, deliveries, CHUNK_CONCURRENCY, cancel, |chunk| {
                self.fetch_chunk(feature_level, base_urls, chunk, cancel, &reporter)
            }),
        );
        cancellation_guard.0.take();
        let reused_bytes = reused_bytes.map_err(|_| EpicError::Protocol)??;
        Ok(reporter.summary(reused_bytes))
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
                        match tokio::task::spawn_blocking(move || decode_chunk(&bytes, &expected))
                            .await
                            .map_err(|_| EpicError::Protocol)?
                        {
                            Ok(bytes) => return Ok(bytes),
                            Err(error @ EpicError::UnsupportedContent) => return Err(error),
                            Err(error) => failure = error,
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
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))) => {},
                        () = cancel.cancelled() => return Err(EpicError::Cancelled),
                    }
                }
            }
        }
        Err(failure)
    }

    async fn fetch(&self, url: &Url, cancel: &CancelToken, reporter: &Reporter) -> ChunkResult {
        let operation = async {
            let mut response = self
                .client
                .get(url.clone())
                .timeout(Duration::from_secs(45))
                .send()
                .await
                .map_err(|_| EpicError::Network)?;
            if !response.status().is_success() {
                return Err(EpicError::Http(response.status().as_u16()));
            }
            let advertised = response.content_length();
            if advertised.is_some_and(|size| size > (MAX_CHUNK_BYTES + 98) as u64) {
                return Err(EpicError::Protocol);
            }
            let mut bytes = Vec::with_capacity(advertised.unwrap_or(0) as usize);
            while let Some(part) = response.chunk().await.map_err(|_| EpicError::Network)? {
                reporter.update(|status| {
                    status.network_bytes += part.len() as u64;
                    status.content_bytes += part.len() as u64;
                });
                cancel.check()?;
                if bytes.len().saturating_add(part.len()) > MAX_CHUNK_BYTES + 98 {
                    return Err(EpicError::Protocol);
                }
                bytes.extend_from_slice(&part);
            }
            Ok(bytes)
        };
        tokio::select! {
            result = operation => result,
            () = cancel.cancelled() => Err(EpicError::Cancelled),
        }
    }
}

struct DiskInstall {
    destination: PathBuf,
    manifest: Arc<Manifest>,
    cancel: CancelToken,
    reporter: Arc<Reporter>,
    requests: mpsc::UnboundedSender<Chunk>,
    results: mpsc::Receiver<ChunkResult>,
}

impl DiskInstall {
    /// Returns the number of verified bytes reused from disk.
    fn run(mut self) -> Result<u64, EpicError> {
        self.cancel.check()?;
        fs::create_dir_all(&self.destination)?;
        let root = fs::canonicalize(&self.destination)?;
        let cache = ensure_directory(&root, Path::new(PARTIAL_DIRECTORY))?;
        let lock_path = checked_file_path(&cache, Path::new("install.lock"))?;
        let _lock = try_lock_file(&lock_path)?.ok_or(EpicError::AlreadyDownloading)?;
        let manifest = Arc::clone(&self.manifest);
        let partial_relative = PathBuf::from(PARTIAL_DIRECTORY)
            .join("epic")
            .join(manifest_key(&manifest));
        let partial_root = ensure_directory(&root, &partial_relative)?;
        let chunk_root = ensure_directory(&partial_root, Path::new("chunks"))?;
        let verified = verification::verify_files(
            &root,
            &manifest.files,
            &self.cancel,
            |bytes, files, name| {
                self.reporter.update(|status| {
                    status.completed_bytes = bytes;
                    status.completed_files = files;
                    status.current_file.replace_range(.., name);
                });
            },
        )?;
        let mut reused_bytes: u64 = manifest
            .files
            .iter()
            .zip(&verified)
            .filter(|(_, valid)| **valid)
            .map(|(file, _)| file.size())
            .sum();
        let mut remaining_chunks = HashMap::new();
        for (file, valid) in manifest.files.iter().zip(&verified) {
            if !valid {
                for part in &file.parts {
                    *remaining_chunks.entry(part.guid).or_insert(0_usize) += 1;
                }
            }
        }
        // Parents are created and checked once per distinct directory; every
        // file's full path is still confined right before it is written.
        let mut ensured_parents: HashSet<PathBuf> = HashSet::new();
        for (file, valid) in manifest.files.iter().zip(verified) {
            self.cancel.check()?;
            let relative = safe_relative_path(&file.filename)?;
            let parent = relative.parent().unwrap_or(Path::new(""));
            if !ensured_parents.contains(parent) {
                ensure_directory(&root, parent)?;
                ensured_parents.insert(parent.to_path_buf());
            }
            let target = checked_file_path(&root, &relative)?;
            if valid {
                apply_permissions(&target, is_executable(file))?;
                continue;
            }
            self.reporter.update(|status| {
                status.phase = DownloadPhase::Verifying;
                status.current_file.clone_from(&file.filename);
            });
            reused_bytes += self.assemble(file, &partial_root, &chunk_root, &root, &relative)?;
            // A published file can be verified directly next time. Keep only
            // chunks that are still required by unfinished files.
            for part in &file.parts {
                let remaining = remaining_chunks
                    .get_mut(&part.guid)
                    .ok_or(EpicError::Protocol)?;
                *remaining -= 1;
                if *remaining == 0 {
                    let chunk = manifest.chunks.get(&part.guid).ok_or(EpicError::Protocol)?;
                    let path = checked_file_path(&chunk_root, Path::new(&chunk_key(chunk)))?;
                    fs::remove_file(path)?;
                }
            }
            self.reporter.update(|status| status.completed_files += 1);
        }
        self.cancel.check()?;
        // Keep caches on failure/cancellation only, avoiding a second permanent copy of the game.
        fs::remove_dir_all(partial_root)?;
        Ok(reused_bytes)
    }

    /// Reuses verified cached chunks, writes each fresh chunk to the cache and
    /// straight into the partial file from memory, hashes what was assembled
    /// and publishes the file atomically. Only chunks that were already on
    /// disk, or that an earlier part of this file fetched, are read back.
    fn assemble(
        &mut self,
        file: &ManifestFile,
        partial_root: &Path,
        chunk_root: &Path,
        root: &Path,
        relative: &Path,
    ) -> Result<u64, EpicError> {
        let manifest = Arc::clone(&self.manifest);
        let mut needed = Vec::new();
        let mut seen = HashSet::new();
        let mut reused = HashSet::new();
        for part in &file.parts {
            self.cancel.check()?;
            if !seen.insert(part.guid) {
                continue;
            }
            let chunk = manifest.chunks.get(&part.guid).ok_or(EpicError::Protocol)?;
            let path = checked_file_path(chunk_root, Path::new(&chunk_key(chunk)))?;
            if verify_file(
                &path,
                u64::from(chunk.window_size),
                &chunk.sha1,
                &self.cancel,
            )? {
                reused.insert(part.guid);
            } else {
                needed.push(chunk);
            }
        }
        for chunk in &needed {
            self.requests
                .send((*chunk).clone())
                .map_err(|_| EpicError::Protocol)?;
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
        // The most recently fetched chunk stays in memory for the parts that
        // follow it; anything older is read back from the chunk cache.
        let mut current: Option<(ChunkGuid, Vec<u8>)> = None;
        for part in &file.parts {
            self.cancel.check()?;
            let chunk = manifest.chunks.get(&part.guid).ok_or(EpicError::Protocol)?;
            if !reused.contains(&part.guid) && delivered.insert(part.guid) {
                let bytes = self.results.blocking_recv().ok_or(EpicError::Protocol)??;
                self.cancel.check()?;
                // A cached chunk is checked by size and SHA-1 before reuse, so a
                // torn write after a crash only costs one more download.
                let path = checked_file_path(chunk_root, Path::new(&chunk_key(chunk)))?;
                fs::write(path, &bytes)?;
                current = Some((part.guid, bytes));
            }
            let start = part.offset as usize;
            let end = start + part.size as usize;
            match &current {
                Some((guid, bytes)) if *guid == part.guid => {
                    let slice = bytes.get(start..end).ok_or(EpicError::Protocol)?;
                    partial.write_all(slice)?;
                    hash.update(slice);
                }
                _ => {
                    let path = checked_file_path(chunk_root, Path::new(&chunk_key(chunk)))?;
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
            if reused.contains(&part.guid) {
                reused_bytes += u64::from(part.size);
            }
            self.reporter
                .update(|status| status.completed_bytes += u64::from(part.size));
        }
        self.cancel.check()?;
        if hash.finalize()[..] != file.hash {
            return Err(EpicError::Integrity);
        }
        partial.sync_all()?;
        drop(partial);
        apply_permissions(&partial_path, is_executable(file))?;
        self.cancel.check()?;
        let target = checked_file_path(root, relative)?;
        fs::rename(&partial_path, &target)?;
        File::open(target.parent().unwrap_or(root))?.sync_all()?;
        Ok(reused_bytes)
    }
}

fn is_executable(file: &ManifestFile) -> bool {
    file.flags & 4 != 0
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

fn decode_chunk(bytes: &[u8], chunk: &Chunk) -> ChunkResult {
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
    if header_size != expected_header
        || bytes.len() != header_size + compressed_size
        || chunk.file_size != bytes.len() as u64
    {
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
        let mut decoder = flate2::read::ZlibDecoder::new(payload);
        let mut output = Vec::with_capacity(chunk.window_size as usize);
        decoder
            .by_ref()
            .take(u64::from(chunk.window_size) + 1)
            .read_to_end(&mut output)
            .map_err(|_| EpicError::Integrity)?;
        if decoder.total_in() != payload.len() as u64 {
            return Err(EpicError::Integrity);
        }
        output
    } else {
        payload.to_vec()
    };
    let sha: [u8; 20] = Sha1::digest(&decoded).into();
    if decoded.len() != chunk.window_size as usize
        || sha != chunk.sha1
        || hash_type & 2 != 0 && bytes[41..61] != sha
        || hash_type & 1 != 0 && header_hash != chunk.hash
        || rolling_hash(&decoded) != chunk.hash
    {
        return Err(EpicError::Integrity);
    }
    Ok(decoded)
}

/// BuildPatch's byte-table rolling hash, using the reflected CRC64 polynomial.
fn rolling_hash(bytes: &[u8]) -> u64 {
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
