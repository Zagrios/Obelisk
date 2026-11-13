use std::{
    collections::HashMap,
    os::unix::fs::PermissionsExt,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures_util::FutureExt;
use obelisk_core::testing::{Cdn, Reply, reaching};
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::manifest::{DepotManifest, Installed};

mod chunk_request;
mod publish;

/// A CDN in memory that serves fixed chunks by path.
fn serving(responses: HashMap<String, Vec<u8>>) -> Cdn {
    unavailable_for(responses, 0)
}

/// A CDN that answers its first `unavailable` requests with 503.
fn unavailable_for(responses: HashMap<String, Vec<u8>>, unavailable: usize) -> Cdn {
    Cdn::in_memory(move |arrival| {
        if arrival.index < unavailable {
            Reply::status(503, "")
        } else {
            responses
                .get(arrival.path())
                .map_or_else(|| Reply::status(404, ""), |body| Reply::ok(body.clone()))
        }
    })
}

fn md5_hex(bytes: &[u8]) -> String {
    hex(&Md5::digest(bytes))
}

fn compressed(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// A depot whose files are made of `chunks`, served by the CDN it returns.
struct Fixture {
    cdn: Cdn,
    /// How many of the CDN's requests `requests` already told of.
    told: AtomicUsize,
    installed: Arc<Installed>,
    chunks: HashMap<&'static str, (Vec<u8>, String)>,
}

fn fixture() -> Fixture {
    fixture_serving(|_, _| {})
}

/// `fixture`, with `edit` changing the body the CDN serves for a named chunk.
fn fixture_serving(edit: impl Fn(&str, &mut Vec<u8>)) -> Fixture {
    let parts: [(&str, &[u8]); 3] = [
        ("a", b"first part "),
        ("b", b"second"),
        ("tool", b"\xcf\xfa\xed\xfe tool"),
    ];
    let mut responses = HashMap::new();
    let mut chunks = HashMap::new();
    let mut described = HashMap::new();
    for (name, bytes) in parts {
        let packed = compressed(bytes);
        let path = format!("/store/{}", galaxy_path(&md5_hex(&packed)).unwrap());
        let mut served = packed.clone();
        edit(name, &mut served);
        responses.insert(path.clone(), served);
        chunks.insert(name, (bytes.to_vec(), path));
        described.insert(
            name,
            json!({"md5": md5_hex(bytes), "size": bytes.len(), "compressedMd5": md5_hex(&packed),
                   "compressedSize": packed.len()}),
        );
    }
    let manifest: DepotManifest = serde_json::from_value(json!({"depot": {"items": [
        {"type": "DepotFile", "path": "Game\\game.exe", "chunks": [described["a"], described["b"]]},
        {"type": "DepotFile", "path": "empty.txt", "chunks": []},
        {"type": "DepotFile", "path": "bin/tool", "chunks": [described["tool"]]},
        {"type": "DepotDirectory", "path": "saves"},
        {"type": "DepotLink", "path": "bin/current", "target": "Game/game.exe"},
    ]}}))
    .unwrap();
    let mut installed = Installed::default();
    installed
        .add("", "1207658924", manifest.depot.items)
        .unwrap();
    Fixture {
        cdn: serving(responses),
        told: AtomicUsize::new(0),
        installed: Arc::new(installed),
        chunks,
    }
}

impl Fixture {
    /// The CDN's URL for the product's chunks.
    fn base(&self) -> String {
        format!("{}/store", self.cdn.base())
    }

    fn client(&self) -> CdnClient {
        reaching(&[&self.cdn])
    }

    /// The paths of the requests the CDN took since this was last called.
    fn requests(&self) -> Vec<String> {
        let arrivals = self.cdn.arrivals();
        let told = self.told.swap(arrivals.len(), Ordering::AcqRel);
        arrivals[told..]
            .iter()
            .map(|arrival| arrival.path().to_owned())
            .collect()
    }

    /// Endpoints for the product, renewed on demand.
    fn links(&self) -> Arc<SecureLinks> {
        let base = self.base();
        Arc::new(SecureLinks::new(Arc::new(move |source| {
            assert_eq!(source, "1207658924");
            let endpoint = Endpoint {
                url: base.clone(),
                url_format: String::new(),
                parameters: Default::default(),
                priority: 10,
                fallback_only: false,
            };
            async move { Ok(vec![endpoint]) }.boxed()
        })))
    }

    async fn install(&self, root: &Path) -> Result<DownloadSummary, GogError> {
        download(
            self.client(),
            self.links(),
            self.installed.clone(),
            "build".into(),
            root,
            &CancelToken::new(),
            |_| {},
        )
        .await
    }
}

#[tokio::test(start_paused = true)]
async fn a_chunk_outlasts_a_cdn_unavailable_past_its_quick_retries() {
    let mut fixture = fixture();
    let (bytes, path) = fixture.chunks["a"].clone();
    let packed = compressed(&bytes);
    // Four answers of 503 used up every attempt before: three quick retries
    // after the first.
    fixture.cdn = unavailable_for([(path.clone(), packed)].into(), 4);
    let started = tokio::time::Instant::now();
    let head = read(
        &fixture.client(),
        &fixture.links(),
        &fixture.installed.files[0],
        5,
        &CancelToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(head, b"first");
    assert_eq!(fixture.requests(), vec![path; 5]);
    assert!(started.elapsed() >= transfer::NETWORK_RETRY_WAITS[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_s_first_bytes_are_read_from_the_chunks_that_hold_them() {
    let fixture = fixture();
    let http = fixture.client();
    let links = fixture.links();
    let file = &fixture.installed.files[0];
    let cancel = CancelToken::new();
    let head = read(&http, &links, file, 5, &cancel).await.unwrap();
    assert_eq!(head, b"first");
    assert_eq!(fixture.requests(), [fixture.chunks["a"].1.clone()]);
    let whole = read(&http, &links, file, 100, &cancel).await.unwrap();
    assert_eq!(whole, b"first part second");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transfer_installs_every_file_then_reuses_what_is_on_disk() {
    let fixture = fixture();
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("gog-1207658924-windows");

    let summary = fixture.install(&root).await.unwrap();
    assert_eq!(fixture.requests().len(), 3);
    assert_eq!(summary.reused_bytes, 0);
    assert_eq!(
        fs::read(root.join("Game/game.exe")).unwrap(),
        b"first part second"
    );
    assert_eq!(fs::read(root.join("empty.txt")).unwrap(), b"");
    assert!(root.join("saves").is_dir());
    let tool = fs::metadata(root.join("bin/tool"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(tool & 0o111, 0o111, "a Mach-O header makes a program");
    let game = fs::metadata(root.join("Game/game.exe"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(game & 0o111, 0);
    assert_eq!(
        fs::read_link(root.join("bin/current")).unwrap(),
        Path::new("../Game/game.exe")
    );
    assert!(
        !root
            .join(PARTIAL_DIRECTORY)
            .join("gog")
            .join("build")
            .exists()
    );

    let summary = fixture.install(&root).await.unwrap();
    assert!(
        fixture.requests().is_empty(),
        "a verified installation downloads nothing"
    );
    assert_eq!(summary.reused_bytes, summary.total_bytes);

    // The file's second chunk changed: only it is downloaded again.
    fs::write(root.join("Game/game.exe"), b"first part SECOND").unwrap();
    fixture.install(&root).await.unwrap();
    assert_eq!(fixture.requests(), [fixture.chunks["b"].1.clone()]);
    assert_eq!(
        fs::read(root.join("Game/game.exe")).unwrap(),
        b"first part second"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn assembling_a_file_never_shows_the_download_as_verifying_again() {
    let fixture = fixture();
    let folder = tempfile::tempdir().unwrap();
    let phases = Arc::new(StdMutex::new(Vec::new()));
    let captured = Arc::clone(&phases);
    download(
        fixture.client(),
        fixture.links(),
        fixture.installed.clone(),
        "build".into(),
        &folder.path().join("gog-1207658924-windows"),
        &CancelToken::new(),
        move |snapshot| captured.lock().unwrap().push(snapshot.phase),
    )
    .await
    .unwrap();
    let phases = phases.lock().unwrap();
    let downloading = phases
        .iter()
        .position(|phase| *phase == DownloadPhase::Downloading)
        .unwrap();
    assert!(
        phases[downloading..]
            .iter()
            .all(|phase| *phase == DownloadPhase::Downloading)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_chunk_fails_the_transfer() {
    let fixture = fixture();
    let (bytes, _) = &fixture.chunks["a"];
    let mut chunk: crate::manifest::Chunk = serde_json::from_value(json!({
        "md5": md5_hex(bytes), "size": bytes.len(), "compressedMd5": md5_hex(b"other"), "compressedSize": 5,
    }))
    .unwrap();
    assert!(matches!(
        decode_chunk(b"other", &chunk),
        Err(GogError::Integrity)
    ));
    chunk.compressed_md5 = md5_hex(b"other");
    chunk.compressed_size = 5;
    assert!(
        matches!(decode_chunk(b"other", &chunk), Err(GogError::Integrity)),
        "not a zlib stream"
    );
    let packed = compressed(bytes);
    chunk.compressed_md5 = md5_hex(&packed);
    chunk.compressed_size = packed.len() as u64;
    assert_eq!(decode_chunk(&packed, &chunk).unwrap(), *bytes);
}

/// Each chunk's file and decoded bytes, by the chunk's MD5.
type Decoded = HashMap<String, (usize, Vec<u8>)>;

/// `FILE_PREFETCH + 2` files of one chunk each, and what their chunks decode to.
fn one_chunk_files() -> (Arc<Installed>, Decoded) {
    let mut decoded = HashMap::new();
    let items: Vec<_> = (0..FILE_PREFETCH + 2)
        .map(|index| {
            let bytes = format!("file {index}").into_bytes();
            let packed = compressed(&bytes);
            let item = json!({"type": "DepotFile", "path": format!("file-{index:02}"), "chunks": [
                {"md5": md5_hex(&bytes), "size": bytes.len(), "compressedMd5": md5_hex(&packed),
                 "compressedSize": packed.len()},
            ]});
            decoded.insert(md5_hex(&bytes), (index, bytes));
            item
        })
        .collect();
    let manifest: DepotManifest =
        serde_json::from_value(json!({"depot": {"items": items}})).unwrap();
    let mut installed = Installed::default();
    installed
        .add("", "1207658924", manifest.depot.items)
        .unwrap();
    (Arc::new(installed), decoded)
}

/// The requests the disk side plans ahead of the one it writes, with one chunk
/// per file.
const WINDOW: usize = if FILE_PREFETCH < QUEUED_CHUNKS {
    FILE_PREFETCH
} else {
    QUEUED_CHUNKS
};

/// How long a scripted install may wait on either side before the test fails.
const WAIT: Duration = Duration::from_secs(5);

/// A disk side installing `one_chunk_files` with no engine behind it. The test
/// plays the network side: it takes `requests` and queues `outcomes`.
struct ScriptedInstall {
    disk: tokio::task::JoinHandle<Result<u64, GogError>>,
    requests: mpsc::UnboundedReceiver<Request>,
    outcomes: mpsc::Sender<ChunkResult>,
    decoded: Decoded,
}

impl ScriptedInstall {
    fn start(
        root: &Path,
        cancel: &CancelToken,
        progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Self {
        let (installed, decoded) = one_chunk_files();
        let (chunks, requests, outcomes) = Chunks::detached(PIPELINE.queue);
        let disk = DiskInstall::engine_side(
            root.to_owned(),
            installed,
            "build".into(),
            cancel.clone(),
            Reporter::new(DownloadProgress::default(), progress),
        );
        Self {
            disk: tokio::task::spawn_blocking(move || disk(chunks)),
            requests,
            outcomes,
            decoded,
        }
    }

    /// Takes the next `count` requests: each chunk's file and decoded bytes.
    async fn take(&mut self, count: usize) -> Vec<(usize, Vec<u8>)> {
        let mut taken = Vec::with_capacity(count);
        for _ in 0..count {
            let request = tokio::time::timeout(WAIT, self.requests.recv())
                .await
                .expect("the disk side requests its window")
                .unwrap();
            taken.push(self.decoded[&request.chunk.md5].clone());
        }
        taken
    }

    async fn outcome(self) -> Result<u64, GogError> {
        tokio::time::timeout(WAIT, self.disk)
            .await
            .expect("the disk side ends")
            .unwrap()
    }
}

fn partial_root(root: &Path) -> PathBuf {
    root.join(PARTIAL_DIRECTORY).join("gog").join("build")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_chunk_reports_its_own_error_not_protocol() {
    // Red check: with `plan_ahead` returning `Protocol` when `request` is
    // refused, as it did before the shared engine, this returned `Protocol`.
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let mut network = ScriptedInstall::start(&root, &CancelToken::new(), |_| {});

    let window = network.take(WINDOW).await;
    assert!(
        window.iter().map(|(index, _)| *index).eq(0..WINDOW),
        "the first files are planned in order"
    );
    // The network side stops here, as after a chunk that failed for good:
    // the disk side blocks on file 0, so its refill always meets the refusal.
    network.requests.close();
    for outcome in [
        Ok(window[0].1.clone()),
        Ok(window[1].1.clone()),
        Err(GogError::Http(404)),
    ] {
        network.outcomes.send(outcome).await.unwrap();
    }
    let result = network.outcome().await;

    assert!(
        matches!(result, Err(GogError::Http(404))),
        "{:?}",
        result.err()
    );
    assert_eq!(fs::read(root.join("file-00")).unwrap(), b"file 0");
    assert_eq!(fs::read(root.join("file-01")).unwrap(), b"file 1");
    assert!(!root.join("file-02").exists());
    assert!(partial_root(&root).is_dir(), "kept for the next attempt");
    assert!(install::lock_engine(&root).unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_transfer_reports_cancelled_not_protocol() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let cancel = CancelToken::new();
    let progress = {
        let cancel = cancel.clone();
        move |snapshot: DownloadProgress| {
            if snapshot.completed_files == 1 {
                cancel.cancel();
            }
        }
    };
    let mut network = ScriptedInstall::start(&root, &cancel, progress);

    let window = network.take(WINDOW).await;
    network
        .outcomes
        .send(Ok(window[0].1.clone()))
        .await
        .unwrap();
    // What the engine does on a cancellation. The disk side may already have
    // seen it and gone, so the send may find nobody.
    tokio::time::timeout(WAIT, cancel.cancelled())
        .await
        .expect("file 0's progress cancels");
    let _ = network.outcomes.send(Err(GogError::Cancelled)).await;
    network.requests.close();
    let result = network.outcome().await;

    assert!(
        matches!(result, Err(GogError::Cancelled)),
        "{:?}",
        result.err()
    );
    assert_eq!(fs::read(root.join("file-00")).unwrap(), b"file 0");
    assert!(partial_root(&root).is_dir(), "kept for the next attempt");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chunk_with_a_wrong_length_fails_as_integrity() {
    let fixture = fixture_serving(|name, body| {
        if name == "a" {
            body.push(0);
        }
    });
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("gog-1207658924-windows");

    let result = fixture.install(&root).await;

    assert!(
        matches!(result, Err(GogError::Integrity)),
        "{:?}",
        result.err()
    );
    let path = &fixture.chunks["a"].1;
    let asked = fixture
        .requests()
        .iter()
        .filter(|request| *request == path)
        .count();
    assert_eq!(asked, 4, "every round asks again before the chunk fails");
    assert!(!root.join("Game/game.exe").exists());

    // The decode's MD5 check would refuse this body too. Fetched alone, the
    // exact bound refuses its advertised length before a byte is read.
    let (bytes, path) = &fixture.chunks["a"];
    let url = Url::parse(&fixture.base()).unwrap().join(path).unwrap();
    let reporter = Reporter::new(DownloadProgress::default(), |_| {});
    let fetched = fetch(
        &fixture.client(),
        url,
        compressed(bytes).len() as u64,
        &CancelToken::new(),
        &reporter,
    )
    .await;
    assert!(matches!(fetched, Err(GogError::Integrity)));
    assert_eq!(reporter.snapshot().network_bytes, 0);
}

#[test]
fn chunk_urls_extend_the_endpoints_path() {
    let endpoint: Endpoint = serde_json::from_value(json!({
        "endpoint_name": "fastly",
        "url_format": "{base_url}/token=nva={expires_at}~dirs={dirs}~token={token}{path}",
        "parameters": {
            "base_url": "https://gog-cdn-fastly.gog.com",
            "path": "/content-system/v2/store/1207658924",
            "expires_at": 1_700_000_000,
            "dirs": 4,
            "token": "signed"
        },
        "priority": 10,
        "fallback_only": false
    }))
    .unwrap();
    assert_eq!(
        endpoint.chunk_url("ab/cd/abcd").unwrap().as_str(),
        "https://gog-cdn-fastly.gog.com/token=nva=1700000000~dirs=4~token=signed/content-system/v2/store/1207658924/ab/cd/abcd"
    );
    assert!(endpoint.expired());
    let open: Endpoint = serde_json::from_value(json!({
        "url": "https://gog-cdn-fastly.gog.com/content-system/v2/dependencies/store",
        "url_format": "https://gog-cdn-fastly.gog.com/content-system/v2/dependencies/store",
        "parameters": {}
    }))
    .unwrap();
    assert_eq!(
        open.chunk_url("ab/cd/abcd").unwrap().as_str(),
        "https://gog-cdn-fastly.gog.com/content-system/v2/dependencies/store/ab/cd/abcd"
    );
    let unknown: Endpoint = serde_json::from_value(json!({
        "url_format": "{base_url}{missing}{path}",
        "parameters": {"base_url": "https://cdn", "path": "/p"}
    }))
    .unwrap();
    assert!(unknown.chunk_url("ab/cd/abcd").is_err());
}

#[tokio::test]
async fn endpoints_put_fallbacks_last_and_renew_once_stale() {
    let renewals = Arc::new(StdMutex::new(0));
    let counted = renewals.clone();
    let links = SecureLinks::new(Arc::new(move |_| {
        *counted.lock().unwrap() += 1;
        let endpoints: Vec<Endpoint> = serde_json::from_value(json!([
            {"url": "https://fallback", "priority": 20, "fallback_only": true},
            {"url": "https://low", "priority": 1},
            {"url": "https://high", "priority": 10},
        ]))
        .unwrap();
        async move { Ok(endpoints) }.boxed()
    }));
    let order: Vec<_> = links
        .endpoints("1", false)
        .await
        .unwrap()
        .iter()
        .map(|endpoint| endpoint.url.clone())
        .collect();
    assert_eq!(order, ["https://high", "https://low", "https://fallback"]);
    links.endpoints("1", false).await.unwrap();
    assert_eq!(*renewals.lock().unwrap(), 1);
    links.endpoints("1", true).await.unwrap();
    assert_eq!(*renewals.lock().unwrap(), 2);
}
