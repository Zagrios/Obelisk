//! Galaxy chunk delivery and atomic installation.
//!
//! The shared chunk engine and the blocking disk worker come from
//! `obelisk_core`, as do path confinement, locking, hashing and progress
//! reporting. Each file is assembled from its chunks in order: a chunk
//! already on disk, in the file's partial copy or at the same place in its
//! installed copy, is reused once its MD5 matches, so a cancelled Transfer
//! resumes and an update downloads only the chunks that changed.
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use md5::{Digest, Md5};
use obelisk_core::{
    install::{
        self, FileDigest, Md5Part, PARTIAL_DIRECTORY, apply_permissions, checked_file_path,
        ensure_directory, hex, safe_relative_path, unhex,
    },
    transfer::{
        self, BodyError, BodySize, CancelToken, Chunks, Delivery, DownloadPhase, DownloadProgress,
        DownloadSummary, Interrupted, Pipeline, Reporter,
    },
    verification::{self, VerificationError, VerificationFile},
};
use reqwest::Url;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::{
    GogError,
    manifest::{Chunk, Installed, InstalledFile, galaxy_path, relative_target},
};

/// Chunk requests in flight. Chunks reach 10 MiB and arrive in request
/// order, so one slow chunk holds up the ones after it: more in flight keep
/// the link busy meanwhile.
const CHUNK_CONCURRENCY: usize = 16;
/// Decoded chunks waiting for the disk worker, which writes them as they
/// come: few are enough, and each may hold 10 MiB.
const WRITE_QUEUE: usize = 4;
/// Chunks requested ahead of the disk worker: enough that requests in
/// flight never wait for it to plan files. Small files hold one chunk each,
/// so a count of files planned ahead left only a few requests in flight.
const QUEUED_CHUNKS: usize = 2 * CHUNK_CONCURRENCY;
/// Files whose chunks are requested ahead of the one being written, at most.
const FILE_PREFETCH: usize = 256;
const PIPELINE: Pipeline = Pipeline {
    delivery: Delivery::InOrder,
    fetches: CHUNK_CONCURRENCY,
    queue: WRITE_QUEUE,
};

/// One CDN a product's chunks come from, as its secure link describes it.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Endpoint {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub url_format: String,
    #[serde(default)]
    pub parameters: serde_json::Map<String, serde_json::Value>,
    /// Higher first.
    #[serde(default)]
    pub priority: i64,
    /// Tried only once every other endpoint failed.
    #[serde(default)]
    pub fallback_only: bool,
}

impl Endpoint {
    /// The URL of the chunk at `galaxy_path`: the endpoint's `path` parameter
    /// extended with it, or, for an endpoint without parameters such as the
    /// redistributables', its URL.
    pub fn chunk_url(&self, galaxy_path: &str) -> Result<Url, GogError> {
        if !self.parameters.contains_key("path") {
            return checked_url(&format!("{}/{galaxy_path}", self.url.trim_end_matches('/')));
        }
        let mut url = self.url_format.clone();
        for (name, value) in &self.parameters {
            let value = match value {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Number(value) => value.to_string(),
                serde_json::Value::Bool(value) => value.to_string(),
                _ => continue,
            };
            let value = match name.as_str() {
                "path" => format!("{}/{galaxy_path}", value.trim_end_matches('/')),
                _ => value,
            };
            url = url.replace(&format!("{{{name}}}"), &value);
        }
        if url.contains(['{', '}']) {
            return Err(GogError::Protocol);
        }
        checked_url(&url)
    }

    /// Whether the endpoint's signature lapsed, or lapses within a minute.
    fn expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|now| now.as_secs())
            .unwrap_or(0);
        self.parameters
            .get("expires_at")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .is_some_and(|expires| expires <= now + 60)
    }
}

/// Only HTTPS locations without embedded credentials.
pub(crate) fn checked_url(value: &str) -> Result<Url, GogError> {
    let url = Url::parse(value).map_err(|_| GogError::Protocol)?;
    let secure = url.scheme() == "https";
    // Tests serve their CDN on the loopback interface.
    #[cfg(test)]
    let secure = secure || url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    if !secure
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(GogError::Protocol);
    }
    Ok(url)
}

/// Renews a product's secure link with the account's credentials.
pub(crate) type Renew = Arc<
    dyn Fn(String) -> futures_util::future::BoxFuture<'static, Result<Vec<Endpoint>, GogError>>
        + Send
        + Sync,
>;

/// Each product's CDN endpoints, renewed once they lapse.
pub(crate) struct SecureLinks {
    links: Mutex<HashMap<String, Arc<[Endpoint]>>>,
    renew: Renew,
}

impl SecureLinks {
    pub fn new(renew: Renew) -> Self {
        Self {
            links: Mutex::new(HashMap::new()),
            renew,
        }
    }

    /// The product's endpoints, fetched again when they lapsed or `stale`.
    async fn endpoints(&self, product_id: &str, stale: bool) -> Result<Arc<[Endpoint]>, GogError> {
        let mut links = self.links.lock().await;
        match links.get(product_id) {
            Some(endpoints) if !stale && !endpoints.iter().any(Endpoint::expired) => {
                Ok(Arc::clone(endpoints))
            }
            _ => {
                let mut endpoints = (self.renew)(product_id.to_owned()).await?;
                if endpoints.is_empty() || endpoints.len() > 16 {
                    return Err(GogError::Protocol);
                }
                // Fallbacks last, then the highest priority first.
                endpoints.sort_by_key(|endpoint| (endpoint.fallback_only, -endpoint.priority));
                let endpoints: Arc<[Endpoint]> = endpoints.into();
                links.insert(product_id.to_owned(), Arc::clone(&endpoints));
                Ok(endpoints)
            }
        }
    }
}

/// What the disk worker asks the network for: one chunk, and the secure
/// link that serves it.
#[derive(Clone)]
struct Request {
    source: String,
    chunk: Chunk,
}

type ChunkResult = Result<Vec<u8>, GogError>;

/// Installs `installed`, already validated, into `destination` and returns
/// what the Transfer did.
pub(crate) async fn download(
    http: reqwest::Client,
    links: Arc<SecureLinks>,
    installed: Arc<Installed>,
    key: String,
    destination: &Path,
    cancel: &CancelToken,
    progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
) -> Result<DownloadSummary, GogError> {
    cancel.check()?;
    let reporter = Reporter::new(
        DownloadProgress {
            phase: DownloadPhase::Verifying,
            total_bytes: installed.total_bytes(),
            total_files: u32::try_from(installed.files.len()).map_err(|_| GogError::Protocol)?,
            ..Default::default()
        },
        progress,
    );
    let disk = DiskInstall::engine_side(
        destination.to_owned(),
        installed,
        key,
        cancel.clone(),
        Arc::clone(&reporter),
    );
    let reused_bytes = transfer::run_engine(PIPELINE, cancel, disk, |request| {
        fetch_chunk(&http, &links, request, cancel, &reporter)
    })
    .await?;
    Ok(reporter.summary(reused_bytes))
}

/// The first `bytes` of an installation's file, from the chunks that hold
/// them, without installing it.
pub(crate) async fn read(
    http: &reqwest::Client,
    links: &SecureLinks,
    installed: &InstalledFile,
    bytes: u64,
    cancel: &CancelToken,
) -> Result<Vec<u8>, GogError> {
    let reporter = Reporter::new(DownloadProgress::default(), |_| {});
    let mut output = Vec::new();
    for chunk in &installed.file.chunks {
        if output.len() as u64 >= bytes {
            break;
        }
        let request = Request {
            source: installed.source.clone(),
            chunk: chunk.clone(),
        };
        output.extend(fetch_chunk(http, links, request, cancel, &reporter).await?);
    }
    output.truncate(usize::try_from(bytes).unwrap_or(usize::MAX));
    Ok(output)
}

async fn fetch_chunk(
    http: &reqwest::Client,
    links: &SecureLinks,
    request: Request,
    cancel: &CancelToken,
    reporter: &Reporter,
) -> ChunkResult {
    let path = galaxy_path(&request.chunk.compressed_md5)?;
    let mut failure = GogError::Network;
    let mut stale = false;
    for attempt in 0_usize.. {
        cancel.check()?;
        let endpoints = links.endpoints(&request.source, stale).await?;
        stale = false;
        for endpoint in endpoints.iter() {
            let url = endpoint.chunk_url(&path)?;
            reporter.update(|status| status.phase = DownloadPhase::Downloading);
            match fetch(http, url, request.chunk.compressed_size, cancel, reporter).await {
                Ok(bytes) => {
                    let chunk = request.chunk.clone();
                    match cancel
                        .spawn_blocking(move || decode_chunk(&bytes, &chunk))
                        .await
                        .map_err(|_| GogError::Protocol)?
                    {
                        Ok(bytes) => return Ok(bytes),
                        Err(error) => failure = error,
                    }
                }
                Err(GogError::Cancelled) => return Err(GogError::Cancelled),
                // A lapsed signature: the next attempt renews the link.
                Err(error @ GogError::Http(401 | 403 | 410)) => {
                    stale = true;
                    failure = error;
                    break;
                }
                Err(error) => failure = error,
            }
        }
        // Three quick retries, then, while the network or the server fails,
        // about a minute more.
        let wait = match attempt.checked_sub(3) {
            None => Some(Duration::from_millis(250 * (attempt as u64 + 1))),
            Some(late)
                if matches!(failure, GogError::Network | GogError::Http(429 | 500..=599)) =>
            {
                transfer::NETWORK_RETRY_WAITS.get(late).copied()
            }
            Some(_) => None,
        };
        match wait {
            Some(wait) => cancel.race(tokio::time::sleep(wait)).await?,
            None => return Err(failure),
        }
    }
    Err(failure)
}

async fn fetch(
    http: &reqwest::Client,
    url: Url,
    expected: u64,
    cancel: &CancelToken,
    reporter: &Reporter,
) -> ChunkResult {
    // A server that stops sending fails the request, not a slow link: GOG's
    // chunks reach 10 MiB, and many share the bandwidth.
    // Chunks are compressed already: never gzip them again.
    let request = http
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .timeout(transfer::CHUNK_DEADLINE)
        .send();
    let response = cancel
        .race(tokio::time::timeout(transfer::CHUNK_STALL, request))
        .await?
        .map_err(|_| GogError::Network)?
        .map_err(|_| GogError::Network)?;
    if !response.status().is_success() {
        return Err(GogError::Http(response.status().as_u16()));
    }
    Ok(transfer::read_body(
        response.content_length(),
        transfer::unless_stalled(response.bytes_stream(), transfer::CHUNK_STALL),
        BodySize::Exactly(expected),
        cancel,
        |length| {
            reporter.update(|status| {
                status.network_bytes += length;
                status.content_bytes += length;
            });
        },
    )
    .await?)
}

/// A chunk's zlib stream, inflated within its size and checked by the MD5
/// of what it holds. The MD5 of the stream itself, which names it on the
/// CDN, would be another pass over every byte proving nothing more.
pub(crate) fn decode_chunk(bytes: &[u8], chunk: &Chunk) -> ChunkResult {
    if bytes.len() as u64 != chunk.compressed_size {
        return Err(GogError::Integrity);
    }
    let mut decoder = flate2::read::ZlibDecoder::new(bytes);
    let mut output = Vec::with_capacity(chunk.size as usize);
    decoder
        .by_ref()
        .take(chunk.size + 1)
        .read_to_end(&mut output)
        .map_err(|_| GogError::Integrity)?;
    if output.len() as u64 != chunk.size || !md5_is(&output, &chunk.md5) {
        return Err(GogError::Integrity);
    }
    Ok(output)
}

impl From<Interrupted> for GogError {
    fn from(_: Interrupted) -> Self {
        Self::Protocol
    }
}

impl From<BodyError> for GogError {
    fn from(error: BodyError) -> Self {
        match error {
            BodyError::Network => Self::Network,
            // The manifest names each chunk's exact size.
            BodyError::Size => Self::Integrity,
            BodyError::Cancelled => Self::Cancelled,
        }
    }
}

/// An installation's file as the shared verification scheduler reads it.
struct Verified<'a> {
    installed: &'a InstalledFile,
    parts: Vec<Md5Part>,
}

impl VerificationFile for Verified<'_> {
    fn name(&self) -> &str {
        &self.installed.path
    }
    fn size(&self) -> u64 {
        self.installed.file.size()
    }
    fn digest(&self) -> FileDigest<'_> {
        FileDigest::Md5Parts(&self.parts)
    }
}

impl From<VerificationError> for GogError {
    fn from(error: VerificationError) -> Self {
        match error {
            VerificationError::Cancelled => Self::Cancelled,
            VerificationError::Install(error) => error.into(),
            VerificationError::SchedulerFailed => Self::Protocol,
        }
    }
}

fn md5_is(bytes: &[u8], expected: &str) -> bool {
    unhex::<16>(expected).is_some_and(|expected| Md5::digest(bytes)[..] == expected)
}

fn parts(chunks: &[Chunk]) -> Result<Vec<Md5Part>, GogError> {
    chunks
        .iter()
        .map(|chunk| {
            Ok(Md5Part {
                size: chunk.size,
                md5: unhex(&chunk.md5).ok_or(GogError::Protocol)?,
            })
        })
        .collect()
}

/// Where one chunk of a file comes from.
#[derive(Clone)]
enum Source {
    /// The same place in a copy already on disk, whose MD5 matched.
    Reused(PathBuf),
    Download,
}

struct PlannedFile {
    index: usize,
    sources: Vec<Source>,
}

struct DiskInstall {
    destination: PathBuf,
    installed: Arc<Installed>,
    /// Names this build's partial files.
    key: String,
    cancel: CancelToken,
    reporter: Arc<Reporter>,
    chunks: Chunks<Request, Vec<u8>, GogError>,
    planned: std::collections::VecDeque<PlannedFile>,
    next_file: usize,
}

impl DiskInstall {
    /// The disk side of one engine run, from its first file.
    fn engine_side(
        destination: PathBuf,
        installed: Arc<Installed>,
        key: String,
        cancel: CancelToken,
        reporter: Arc<Reporter>,
    ) -> impl FnOnce(Chunks<Request, Vec<u8>, GogError>) -> Result<u64, GogError> + Send + 'static
    {
        move |chunks| {
            Self {
                destination,
                installed,
                key,
                cancel,
                reporter,
                chunks,
                planned: Default::default(),
                next_file: 0,
            }
            .run()
        }
    }

    /// Returns the number of verified bytes reused from disk.
    fn run(mut self) -> Result<u64, GogError> {
        self.cancel.check()?;
        fs::create_dir_all(&self.destination)?;
        let root = fs::canonicalize(&self.destination)?;
        let _lock = install::lock_engine(&root)?.ok_or(GogError::AlreadyDownloading)?;
        let installed = Arc::clone(&self.installed);
        let partial_relative = PathBuf::from(PARTIAL_DIRECTORY).join("gog").join(&self.key);
        let partial_root = ensure_directory(&root, &partial_relative)?;
        let checked = installed
            .files
            .iter()
            .map(|installed| {
                Ok(Verified {
                    installed,
                    parts: parts(&installed.file.chunks)?,
                })
            })
            .collect::<Result<Vec<_>, GogError>>()?;
        let verified = verification::verify_files(&root, &checked, &self.cancel, &self.reporter)?;
        let mut reused_bytes = verified.reused_bytes;
        for directory in &installed.directories {
            ensure_directory(&root, &safe_relative_path(directory)?)?;
        }
        self.plan_ahead(&verified, &checked, &partial_root, &root)?;
        let mut ensured_parents: HashSet<PathBuf> = HashSet::new();
        for (index, (file, valid)) in checked.iter().zip(&verified.valid).enumerate() {
            self.cancel.check()?;
            let relative = safe_relative_path(&file.installed.path)?;
            let parent = relative.parent().unwrap_or(Path::new(""));
            if ensured_parents.insert(parent.to_path_buf()) {
                ensure_directory(&root, parent)?;
            }
            let target = checked_file_path(&root, &relative)?;
            if *valid {
                apply_permissions(&target, is_executable(&file.installed.file, &target)?)?;
                continue;
            }
            self.reporter.update(|status| {
                status.current_file.clone_from(&file.installed.path);
            });
            let plan = self.planned.pop_front().ok_or(GogError::Protocol)?;
            if plan.index != index {
                return Err(GogError::Protocol);
            }
            reused_bytes += self.assemble(file, &plan, &partial_root, &target)?;
            self.reporter.update(|status| status.completed_files += 1);
            self.plan_ahead(&verified, &checked, &partial_root, &root)?;
        }
        self.cancel.check()?;
        for link in &installed.links {
            place_link(&root, &link.path, &link.target)?;
        }
        // Every file is in place: durable before the receipt says so, and
        // recorded whole for the next Transfer to skip.
        verification::complete(&root, &checked)?;
        // Partial copies stay only after a failure or a cancellation.
        fs::remove_dir_all(partial_root)?;
        Ok(reused_bytes)
    }

    /// Plans the next few files to repair: which chunks a copy on disk
    /// already holds, and requests the rest, in the order they are written.
    /// The target's leading chunks that `verified` matched are not read again.
    fn plan_ahead(
        &mut self,
        verified: &verification::Verified,
        files: &[Verified<'_>],
        partial_root: &Path,
        root: &Path,
    ) -> Result<(), GogError> {
        while self.planned.len() < FILE_PREFETCH
            && self.requested() < QUEUED_CHUNKS
            && self.next_file < files.len()
            && !self.chunks.refused()
        {
            self.cancel.check()?;
            let index = self.next_file;
            self.next_file += 1;
            if verified.valid[index] {
                continue;
            }
            let file = &files[index].installed;
            let partial = checked_file_path(partial_root, Path::new(&partial_name(&file.path)))?;
            let target = checked_file_path(root, &safe_relative_path(&file.path)?)?;
            let mut sources = Vec::with_capacity(file.file.chunks.len());
            let mut offset = 0;
            for (part, chunk) in file.file.chunks.iter().enumerate() {
                self.cancel.check()?;
                let holds =
                    |copy: &Path| copy_chunk(copy, offset, chunk, &mut io::sink()).unwrap_or(false);
                let reused = if holds(&partial) {
                    Some(&partial)
                } else if part < verified.matched_parts[index] || holds(&target) {
                    Some(&target)
                } else {
                    None
                };
                if let Some(copy) = reused {
                    sources.push(Source::Reused(copy.clone()));
                } else if self.chunks.request(Request {
                    source: file.source.clone(),
                    chunk: chunk.clone(),
                }) {
                    sources.push(Source::Download);
                } else {
                    // Every remaining place receives the outcome that stopped
                    // the network side: the file stays fully planned, so it is
                    // never published short.
                    sources.resize(file.file.chunks.len(), Source::Download);
                    break;
                }
                offset += chunk.size;
            }
            self.planned.push_back(PlannedFile { index, sources });
        }
        Ok(())
    }

    /// The chunks the planned files requested, none of which has arrived:
    /// the file being written was planned too and left the plan already.
    fn requested(&self) -> usize {
        self.planned
            .iter()
            .flat_map(|plan| &plan.sources)
            .filter(|source| matches!(source, Source::Download))
            .count()
    }

    /// Writes the file's chunks in order into a fresh partial copy, checks
    /// each one, then publishes the file atomically.
    fn assemble(
        &mut self,
        file: &Verified<'_>,
        plan: &PlannedFile,
        partial_root: &Path,
        target: &Path,
    ) -> Result<u64, GogError> {
        let name = partial_name(&file.installed.path);
        let partial = checked_file_path(partial_root, Path::new(&name))?;
        let next = checked_file_path(partial_root, Path::new(&format!("{name}.next")))?;
        let mut output = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&next)?;
        let mut reused_bytes = 0;
        let mut offset = 0;
        for (chunk, source) in file.installed.file.chunks.iter().zip(&plan.sources) {
            self.cancel.check()?;
            match source {
                Source::Download => {
                    self.reporter
                        .update(|status| status.phase = DownloadPhase::Downloading);
                    output.write_all(&self.chunks.receive()?)?;
                }
                Source::Reused(copy) => {
                    reused_bytes += chunk.size;
                    if !copy_chunk(copy, offset, chunk, &mut output)? {
                        return Err(GogError::Integrity);
                    }
                }
            }
            offset += chunk.size;
            // A partial copy that lasts across a cancellation keeps what was
            // written: the next attempt reuses it.
            self.reporter
                .update(|status| status.completed_bytes += chunk.size);
        }
        self.cancel.check()?;
        install::write_out(&output)?;
        drop(output);
        fs::rename(&next, &partial)?;
        apply_permissions(&partial, is_executable(&file.installed.file, &partial)?)?;
        self.cancel.check()?;
        fs::rename(&partial, target)?;
        install::write_out(&File::open(target.parent().ok_or(GogError::Protocol)?)?)?;
        Ok(reused_bytes)
    }
}

/// The partial copy of the file at `path`.
fn partial_name(path: &str) -> String {
    format!("{}.part", hex(&Md5::digest(path.to_lowercase().as_bytes())))
}

/// Streams the chunk at `offset` in `copy` into `output` in 64 KiB reads;
/// `false` when `copy` is too short to hold it or its MD5 differs.
fn copy_chunk(
    copy: &Path,
    offset: u64,
    chunk: &Chunk,
    output: &mut impl Write,
) -> Result<bool, GogError> {
    let expected: [u8; 16] = unhex(&chunk.md5).ok_or(GogError::Protocol)?;
    let mut file = match File::open(copy) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() < offset + chunk.size {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut hasher = Md5::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = chunk.size;
    while remaining > 0 {
        let count = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        file.read_exact(&mut buffer[..count])?;
        hasher.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    Ok(hasher.finalize()[..] == expected)
}

/// Places the link at `path` to `target`, both from the installation's root,
/// replacing whatever an earlier build left there. The link is relative, so
/// it keeps its target wherever the installation goes.
fn place_link(root: &Path, path: &str, target: &str) -> Result<(), GogError> {
    let relative = safe_relative_path(path)?;
    safe_relative_path(target)?;
    install::place_link(root, &relative, Path::new(&relative_target(path, target)))?;
    Ok(())
}

/// Galaxy flags executables, but some builds miss the flag on helpers their
/// game starts: programs are also recognised by their header.
fn is_executable(file: &crate::manifest::File, path: &Path) -> std::io::Result<bool> {
    Ok(file.executable() || install::starts_as_program(path)?)
}

#[cfg(test)]
mod tests;
