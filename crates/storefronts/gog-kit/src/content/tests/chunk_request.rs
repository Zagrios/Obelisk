//! How a chunk request enters GOG's retry choreography, as its download shows
//! it against a CDN in memory: the bytes it delivers and counts, which
//! answers it waits out and for how long, a body that stalls, link renewal,
//! cancellation and the speed limit. Waits run in virtual time, played out one
//! by one or, for a body that stalls, running ahead of the download's disk
//! work. How a request itself times out and is held is tested once, in
//! `obelisk-core`.
use obelisk_core::testing::{
    Arrival, COARSE_SLACK, VirtualTime, assert_waits_roughly, millis, noise, played_against,
    seconds, serves, serves_after,
};

use super::*;

const PRODUCT: &str = "1207658924";

/// The waits of a chunk that fails for good on a failure that is no outage:
/// four requests, 250, 500 and 750 ms apart.
fn quick() -> Vec<Duration> {
    vec![millis(250), millis(500), millis(750)]
}

/// The waits of a chunk that fails to the end on an outage: the quick ones,
/// then each of the outage waits.
fn exhausted() -> Vec<Duration> {
    let mut waits = quick();
    waits.extend([1, 2, 4, 8, 15, 30].map(seconds));
    waits
}

/// A game of one file made of one chunk, whose CDN path is under `/store`.
struct Game {
    installed: Arc<Installed>,
    route: String,
    packed: Vec<u8>,
    bytes: Vec<u8>,
}

fn game(bytes: &[u8]) -> Game {
    let packed = compressed(bytes);
    let route = format!("/store/{}", galaxy_path(&md5_hex(&packed)).unwrap());
    let manifest: DepotManifest = serde_json::from_value(json!({"depot": {"items": [
        {"type": "DepotFile", "path": "Game\\data.bin", "chunks": [
            {"md5": md5_hex(bytes), "size": bytes.len(), "compressedMd5": md5_hex(&packed),
             "compressedSize": packed.len()}]},
    ]}}))
    .unwrap();
    let mut installed = Installed::default();
    installed.add("", PRODUCT, manifest.depot.items).unwrap();
    Game {
        installed: Arc::new(installed),
        route,
        packed,
        bytes: bytes.to_vec(),
    }
}

/// What a download of the game ended as, the folder it installed into and how
/// often its secure link was renewed.
struct Outcome {
    result: Result<DownloadSummary, GogError>,
    folder: tempfile::TempDir,
    renewals: usize,
}

impl Outcome {
    fn summary(&self) -> &DownloadSummary {
        self.result.as_ref().expect("an installed game")
    }

    fn file(&self) -> Vec<u8> {
        fs::read(
            self.folder
                .path()
                .join("gog-1207658924-windows/Game/data.bin"),
        )
        .unwrap()
    }
}

impl Game {
    /// A CDN that serves the chunk to every request.
    fn serves(&self) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
        serves(&self.route, &self.packed)
    }

    /// A CDN that answers `first` to the first `count` requests and serves
    /// the chunk to the rest.
    fn serves_after(
        &self,
        count: usize,
        first: Reply,
    ) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
        serves_after(&self.route, &self.packed, count, first)
    }

    /// Installs the game from the CDNs, in the order of their priority.
    async fn install(&self, cdns: &[&Cdn], cancel: &CancelToken) -> Outcome {
        self.install_with(reaching(cdns), cdns, cancel).await
    }

    /// `install`, through `client`, which reaches the CDNs.
    async fn install_with(
        &self,
        client: CdnClient,
        cdns: &[&Cdn],
        cancel: &CancelToken,
    ) -> Outcome {
        let endpoints: Vec<_> = cdns
            .iter()
            .enumerate()
            .map(|(index, fake)| Endpoint {
                url: format!("{}/store", fake.base()),
                url_format: String::new(),
                parameters: Default::default(),
                priority: 10 - index as i64,
                fallback_only: false,
            })
            .collect();
        let renewals = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&renewals);
        let links = Arc::new(SecureLinks::new(Arc::new(move |source| {
            assert_eq!(source, PRODUCT);
            counted.fetch_add(1, Ordering::AcqRel);
            let endpoints = endpoints.clone();
            async move { Ok(endpoints) }.boxed()
        })));
        let folder = tempfile::tempdir().unwrap();
        let result = download(
            client,
            links,
            Arc::clone(&self.installed),
            "build".into(),
            &folder.path().join("gog-1207658924-windows"),
            cancel,
            |_| {},
        )
        .await;
        Outcome {
            result,
            folder,
            renewals: renewals.load(Ordering::Acquire),
        }
    }

    /// Installs the game from a CDN that answers each request as `reply`
    /// says, while the waits between the requests pass in virtual time as
    /// `waits` says. Returns how it ended with the requests taken and the
    /// renewals of the link.
    async fn against(
        &self,
        waits: &[Duration],
        reply: impl Fn(&Arrival) -> Reply + Send + Sync + 'static,
    ) -> (Result<(), GogError>, Vec<Arrival>, usize) {
        let cancel = CancelToken::new();
        let (outcome, arrivals) = played_against(waits, reply, async |cdn| {
            self.install(&[cdn], &cancel).await
        })
        .await;
        (outcome.result.map(|_| ()), arrivals, outcome.renewals)
    }
}

#[tokio::test]
async fn a_chunk_is_asked_for_once_and_its_bytes_are_counted_once() {
    let game = game(&noise(300_000));
    let cdn = Cdn::in_memory(game.serves());
    let outcome = game.install(&[&cdn], &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    let summary = outcome.summary();
    assert_eq!(summary.downloaded_bytes, game.packed.len() as u64);
    assert_eq!(summary.content_bytes, game.packed.len() as u64);
    assert_eq!(summary.reused_bytes, 0);
    assert_eq!(outcome.renewals, 1);
    assert_eq!(cdn.arrivals().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_service_unavailable_chunk_is_waited_out_to_the_last_wait_then_fails_as_http() {
    let (result, _, renewals) = game(b"payload")
        .against(&exhausted(), |_| Reply::status(503, ""))
        .await;
    assert!(matches!(result, Err(GogError::Http(503))), "{result:?}");
    assert_eq!(renewals, 1, "an outage keeps the link");
}

#[tokio::test(start_paused = true)]
async fn a_too_many_requests_chunk_is_waited_out_like_a_server_error() {
    let (result, _, _) = game(b"payload")
        .against(&exhausted(), |_| Reply::status(429, ""))
        .await;
    assert!(matches!(result, Err(GogError::Http(429))), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn the_first_and_the_last_server_error_status_are_outages() {
    let game = game(b"payload");
    let (result, _, _) = game.against(&exhausted(), |_| Reply::status(500, "")).await;
    assert!(matches!(result, Err(GogError::Http(500))), "{result:?}");
    let (result, _, _) = game.against(&exhausted(), |_| Reply::status(599, "")).await;
    assert!(matches!(result, Err(GogError::Http(599))), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_connection_closed_without_an_answer_is_a_network_outage() {
    let (result, _, _) = game(b"payload")
        .against(&exhausted(), |_| Reply::Hangup)
        .await;
    assert!(matches!(result, Err(GogError::Network)), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_chunk_is_served_once_an_outage_passes() {
    let game = game(b"payload");
    let time = VirtualTime::frozen();
    let cdn = Cdn::in_memory(game.serves_after(4, Reply::status(503, "")));
    // Four failed requests, the quick retries and the first outage wait.
    let mut waits = quick();
    waits.push(seconds(1));
    let (cancel, cdns) = (CancelToken::new(), [&cdn]);
    let (outcome, ()) = tokio::join!(game.install(&cdns, &cancel), time.play(&cdn, &waits));
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(outcome.summary().downloaded_bytes, game.packed.len() as u64);
    assert_eq!(cdn.arrivals().len(), 5);
}

#[tokio::test(start_paused = true)]
async fn an_answer_that_is_no_outage_gets_its_quick_retries_and_no_more() {
    let game = game(b"payload");
    for status in [400, 404, 428, 430, 499, 600] {
        let (result, _, _) = game
            .against(&quick(), move |_| Reply::status(status, ""))
            .await;
        assert!(
            matches!(result, Err(GogError::Http(code)) if code == status),
            "{status}: {result:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_lapsed_link_is_renewed_before_each_further_request() {
    let game = game(b"payload");
    for status in [401, 403, 410] {
        let (result, _, renewals) = game
            .against(&quick(), move |_| Reply::status(status, ""))
            .await;
        assert!(
            matches!(result, Err(GogError::Http(code)) if code == status),
            "{status}: {result:?}"
        );
        assert_eq!(renewals, 4, "{status}: one link for the first request");
    }
}

#[tokio::test]
async fn a_renewed_link_serves_the_chunk_after_a_lapse() {
    let game = game(b"payload");
    let cdn = Cdn::in_memory(game.serves_after(1, Reply::status(403, "")));
    let outcome = game.install(&[&cdn], &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(cdn.arrivals().len(), 2);
    assert_eq!(outcome.renewals, 2);
}

#[tokio::test]
async fn a_failing_endpoint_hands_the_chunk_to_the_next_one_without_waiting() {
    let game = game(b"payload");
    let failing = Cdn::in_memory(|_| Reply::status(503, ""));
    let serving = Cdn::in_memory(game.serves());
    let outcome = game
        .install(&[&failing, &serving], &CancelToken::new())
        .await;
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(failing.arrivals().len(), 1);
    assert_eq!(serving.arrivals().len(), 1);
    assert!(serving.arrivals()[0].at - failing.arrivals()[0].at < millis(240));
}

#[tokio::test(start_paused = true)]
async fn a_corrupt_chunk_is_asked_for_again_and_fails_as_integrity() {
    let game = game(&noise(1_000));
    let mut corrupt = game.packed.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0xff;
    let (result, _, _) = game
        .against(&quick(), move |_| Reply::ok(corrupt.clone()))
        .await;
    assert!(matches!(result, Err(GogError::Integrity)), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_chunk_of_the_wrong_length_is_refused_unread_and_fails_as_integrity() {
    let game = game(&noise(1_000));
    let wrong = game.packed.len() + 1;
    let (result, _, _) = game
        .against(&quick(), move |_| Reply::Stalls {
            length: wrong,
            sent: Vec::new(),
        })
        .await;
    assert!(matches!(result, Err(GogError::Integrity)), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_body_that_stops_midway_fails_the_request_after_the_stall_and_counts_its_bytes() {
    let game = game(&noise(10_000));
    let half = game.packed.len() / 2;
    let stalled = Reply::Stalls {
        length: game.packed.len(),
        sent: game.packed[..half].to_vec(),
    };
    let _time = VirtualTime::running_coarsely();
    let cdn = Cdn::in_memory(game.serves_after(1, stalled));
    let outcome = game.install(&[&cdn], &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    assert_waits_roughly(&cdn.arrivals(), &[seconds(30) + millis(250)], COARSE_SLACK);
    let counted = (half + game.packed.len()) as u64;
    assert_eq!(outcome.summary().downloaded_bytes, counted);
    assert_eq!(outcome.summary().content_bytes, counted);
}

#[tokio::test]
async fn cancelling_ends_a_request_to_a_silent_server_at_once() {
    let game = game(b"payload");
    let cdn = Cdn::in_memory(|_| Reply::Silent);
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(millis(300)).await;
        trigger.cancel();
    });
    let outcome = tokio::time::timeout(seconds(5), game.install(&[&cdn], &cancel))
        .await
        .expect("a cancelled request ends without waiting for its stall");
    assert!(matches!(outcome.result, Err(GogError::Cancelled)));
    assert_eq!(cdn.arrivals().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_an_outage_wait_ends_the_chunk_without_another_request() {
    let game = game(b"payload");
    let time = VirtualTime::frozen();
    let cdn = Cdn::in_memory(|_| Reply::status(503, ""));
    let (cancel, cdns) = (CancelToken::new(), [&cdn]);
    let cancelling = async {
        // The fourth request is the last before the first outage wait, which
        // the clock then stands still in.
        time.play(&cdn, &quick()).await;
        cancel.cancel();
    };
    let (outcome, ()) = tokio::join!(game.install(&cdns, &cancel), cancelling);
    assert!(matches!(outcome.result, Err(GogError::Cancelled)));
    assert_eq!(cdn.arrivals().len(), 4);
}

#[tokio::test]
async fn the_speed_limit_paces_a_chunk_request() {
    let game = game(&noise(1_250_000));
    let cdn = Cdn::in_memory(game.serves());
    let limited = reaching(&[&cdn]).limited_to(625_000);
    let started = std::time::Instant::now();
    let outcome = game
        .install_with(limited, &[&cdn], &CancelToken::new())
        .await;
    // About 1.25 MB at 625 kB/s.
    assert!(started.elapsed() >= millis(1900), "{:?}", started.elapsed());
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(outcome.summary().downloaded_bytes, game.packed.len() as u64);
}
