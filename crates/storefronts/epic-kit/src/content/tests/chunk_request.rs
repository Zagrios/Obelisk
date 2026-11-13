//! How a chunk request enters Epic's retry choreography, as its download shows
//! it against a CDN in memory: the bytes it delivers and counts, which
//! answers it waits out and for how long, a body that stalls, cancellation
//! and the speed limit. Waits run in virtual time, played out one by one or,
//! for a body that stalls, running ahead of the download's disk work. How a
//! request itself times out and is held is tested once, in `obelisk-core`.
use obelisk_core::testing::{
    Arrival, COARSE_SLACK, Cdn, Reply, VirtualTime, assert_waits_roughly, millis, noise,
    played_against, reaching, seconds, serves, serves_after,
};

use super::*;

/// The waits of a mirror's three requests.
fn quick() -> Vec<Duration> {
    vec![millis(250), millis(500)]
}

/// The waits of a chunk that fails to the end: a mirror gets three requests,
/// 250 ms and 500 ms apart, and again after each of the outage waits.
fn exhausted() -> Vec<Duration> {
    let outage = [1, 2, 4, 8, 15, 30];
    let mut waits = Vec::new();
    for late in 0..=outage.len() {
        waits.extend([millis(250), millis(500)]);
        if let Some(wait) = outage.get(late) {
            waits.push(seconds(*wait));
        }
    }
    waits
}

/// A game of one file made of one chunk.
struct Game {
    manifest: Arc<Manifest>,
    route: String,
    wire: Vec<u8>,
    bytes: Vec<u8>,
}

fn game(bytes: &[u8]) -> Game {
    let (chunk, wire) = chunk(bytes, 1, false);
    let file = ManifestFile {
        filename: "Game/data.bin".into(),
        symlink_target: None,
        hash: Sha1::digest(bytes).into(),
        flags: 0,
        parts: vec![ChunkPart {
            guid: chunk.guid,
            offset: 0,
            size: bytes.len() as u32,
        }],
    };
    let route = format!("/{}", chunk.relative_path(18));
    Game {
        manifest: Arc::new(manifest(
            "test-game",
            vec![file],
            [(chunk.guid, chunk)].into(),
        )),
        route,
        wire,
        bytes: bytes.to_vec(),
    }
}

/// What a download of the game ended as, and the folder it installed into.
struct Outcome {
    result: Result<DownloadSummary, EpicError>,
    folder: tempfile::TempDir,
}

impl Outcome {
    fn summary(&self) -> &DownloadSummary {
        self.result.as_ref().expect("an installed game")
    }

    fn file(&self) -> Vec<u8> {
        fs::read(self.folder.path().join("Game/data.bin")).unwrap()
    }
}

impl Game {
    /// A CDN that serves the chunk to every request.
    fn serves(&self) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
        serves(&self.route, &self.wire)
    }

    /// A CDN that answers `first` to the first `count` requests and serves
    /// the chunk to the rest.
    fn serves_after(
        &self,
        count: usize,
        first: Reply,
    ) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
        serves_after(&self.route, &self.wire, count, first)
    }

    async fn install(&self, cdns: &[&Cdn], cancel: &CancelToken) -> Outcome {
        let content = ContentClient {
            cdn: reaching(cdns),
        };
        self.install_with(&content, cdns, cancel).await
    }

    /// `install`, through `content`, which reaches the CDNs.
    async fn install_with(
        &self,
        content: &ContentClient,
        cdns: &[&Cdn],
        cancel: &CancelToken,
    ) -> Outcome {
        let folder = tempfile::tempdir().unwrap();
        let bases: Vec<_> = cdns
            .iter()
            .map(|cdn| Url::parse(&format!("{}/", cdn.base())).unwrap())
            .collect();
        let result = content
            .download(
                Arc::clone(&self.manifest),
                &bases,
                folder.path(),
                cancel,
                |_| {},
            )
            .await;
        Outcome { result, folder }
    }

    /// Installs the game from a CDN that answers each request as `reply`
    /// says, while the waits between the requests pass in virtual time as
    /// `waits` says. Returns how it ended with the requests taken.
    async fn against(
        &self,
        waits: &[Duration],
        reply: impl Fn(&Arrival) -> Reply + Send + Sync + 'static,
    ) -> (Result<(), EpicError>, Vec<Arrival>) {
        let cancel = CancelToken::new();
        let (installed, arrivals) = played_against(waits, reply, async |cdn| {
            self.install(&[cdn], &cancel).await
        })
        .await;
        (installed.result.map(|_| ()), arrivals)
    }
}

#[tokio::test]
async fn a_chunk_is_asked_for_once_and_its_bytes_are_counted_once() {
    let game = game(&noise(300_000));
    let cdn = Cdn::in_memory(game.serves());
    let outcome = game.install(&[&cdn], &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    let summary = outcome.summary();
    assert_eq!(summary.downloaded_bytes, game.wire.len() as u64);
    assert_eq!(summary.content_bytes, game.wire.len() as u64);
    assert_eq!(summary.reused_bytes, 0);
    assert_eq!(cdn.arrivals().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_service_unavailable_chunk_is_waited_out_to_the_last_wait_then_fails_as_http() {
    let (result, _) = game(b"payload")
        .against(&exhausted(), |_| Reply::status(503, ""))
        .await;
    assert!(matches!(result, Err(EpicError::Http(503))), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_too_many_requests_chunk_is_waited_out_like_a_server_error() {
    let (result, _) = game(b"payload")
        .against(&exhausted(), |_| Reply::status(429, ""))
        .await;
    assert!(matches!(result, Err(EpicError::Http(429))), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn the_first_and_the_last_server_error_status_are_outages() {
    let game = game(b"payload");
    let (result, _) = game.against(&exhausted(), |_| Reply::status(500, "")).await;
    assert!(matches!(result, Err(EpicError::Http(500))), "{result:?}");
    let (result, _) = game.against(&exhausted(), |_| Reply::status(599, "")).await;
    assert!(matches!(result, Err(EpicError::Http(599))), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_connection_closed_without_an_answer_is_a_network_outage() {
    let (result, _) = game(b"payload")
        .against(&exhausted(), |_| Reply::Hangup)
        .await;
    assert!(matches!(result, Err(EpicError::Network)), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_chunk_is_served_once_an_outage_passes() {
    let game = game(b"payload");
    let time = VirtualTime::frozen();
    let cdn = Cdn::in_memory(game.serves_after(3, Reply::status(503, "")));
    // The three requests of the first round, then the outage's first wait.
    let waits = [millis(250), millis(500), seconds(1)];
    let (cancel, cdns) = (CancelToken::new(), [&cdn]);
    let (outcome, ()) = tokio::join!(game.install(&cdns, &cancel), time.play(&cdn, &waits));
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(outcome.summary().downloaded_bytes, game.wire.len() as u64);
    assert_eq!(cdn.arrivals().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn an_answer_that_is_no_outage_gets_its_quick_retries_and_no_more() {
    let game = game(b"payload");
    for status in [400, 428, 430, 499, 600] {
        let (result, _) = game
            .against(&quick(), move |_| Reply::status(status, ""))
            .await;
        assert!(
            matches!(result, Err(EpicError::Http(code)) if code == status),
            "{status}: {result:?}"
        );
    }
}

#[tokio::test]
async fn a_mirror_that_rejects_or_lacks_a_chunk_is_given_up_at_once() {
    let game = game(b"payload");
    for status in [401, 403, 404] {
        let alone = Cdn::in_memory(move |_| Reply::status(status, ""));
        let outcome = game.install(&[&alone], &CancelToken::new()).await;
        assert!(
            matches!(outcome.result, Err(EpicError::Http(code)) if code == status),
            "{status}: {:?}",
            outcome.result
        );
        assert_eq!(alone.arrivals().len(), 1, "{status}: asked once");

        let refusing = Cdn::in_memory(move |_| Reply::status(status, ""));
        let serving = Cdn::in_memory(game.serves());
        let outcome = game
            .install(&[&refusing, &serving], &CancelToken::new())
            .await;
        assert_eq!(outcome.file(), game.bytes, "{status}");
        assert_eq!(refusing.arrivals().len(), 1);
        assert_eq!(serving.arrivals().len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn a_failing_mirror_gets_three_requests_before_the_next_mirror_is_asked() {
    let game = game(b"payload");
    let time = VirtualTime::frozen();
    let failing = Cdn::in_memory(|_| Reply::status(503, ""));
    let serving = Cdn::in_memory(game.serves());
    let (cancel, waits, cdns) = (CancelToken::new(), quick(), [&failing, &serving]);
    let (outcome, ()) = tokio::join!(game.install(&cdns, &cancel), time.play(&failing, &waits));
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(failing.arrivals().len(), 3);
    assert_eq!(serving.arrivals().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_corrupt_chunk_is_asked_for_again_and_fails_as_integrity() {
    let game = game(&noise(1_000));
    let mut corrupt = game.wire.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let (result, _) = game
        .against(&quick(), move |_| Reply::ok(corrupt.clone()))
        .await;
    assert!(matches!(result, Err(EpicError::Integrity)), "{result:?}");
}

#[tokio::test]
async fn a_corrupt_answer_is_counted_and_the_next_good_one_installs() {
    let game = game(&noise(1_000));
    let mut corrupt = game.wire.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let cdn = Cdn::in_memory(game.serves_after(1, Reply::ok(corrupt)));
    let outcome = game.install(&[&cdn], &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    let wire = game.wire.len() as u64;
    assert_eq!(outcome.summary().downloaded_bytes, 2 * wire);
    assert_eq!(outcome.summary().content_bytes, 2 * wire);
}

#[tokio::test(start_paused = true)]
async fn a_chunk_that_announces_more_than_the_bound_is_refused_unread() {
    let announced = 64 * 1024 * 1024 + 98 + 1;
    let (result, _) = game(b"payload")
        .against(&quick(), move |_| Reply::Stalls {
            length: announced,
            sent: Vec::new(),
        })
        .await;
    assert!(matches!(result, Err(EpicError::Protocol)), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn a_body_that_stops_midway_fails_the_request_after_the_stall_and_counts_its_bytes() {
    let game = game(&noise(10_000));
    let half = game.wire.len() / 2;
    let stalled = Reply::Stalls {
        length: game.wire.len(),
        sent: game.wire[..half].to_vec(),
    };
    let _time = VirtualTime::running_coarsely();
    let cdn = Cdn::in_memory(game.serves_after(1, stalled));
    let outcome = game.install(&[&cdn], &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    assert_waits_roughly(&cdn.arrivals(), &[seconds(30) + millis(250)], COARSE_SLACK);
    let counted = (half + game.wire.len()) as u64;
    assert_eq!(outcome.summary().downloaded_bytes, counted);
    assert_eq!(outcome.summary().content_bytes, counted);
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_an_outage_wait_ends_the_chunk_without_another_request() {
    let game = game(b"payload");
    let time = VirtualTime::frozen();
    let cdn = Cdn::in_memory(|_| Reply::status(503, ""));
    let (cancel, cdns) = (CancelToken::new(), [&cdn]);
    let cancelling = async {
        // The third request is the last before the first outage wait, which
        // the clock then stands still in.
        time.play(&cdn, &quick()).await;
        cancel.cancel();
    };
    let (outcome, ()) = tokio::join!(game.install(&cdns, &cancel), cancelling);
    assert!(matches!(outcome.result, Err(EpicError::Cancelled)));
    assert_eq!(cdn.arrivals().len(), 3);
}

#[tokio::test]
async fn the_speed_limit_paces_a_chunk_request() {
    let game = game(&noise(1_250_000));
    let cdn = Cdn::in_memory(game.serves());
    let limited = ContentClient {
        cdn: reaching(&[&cdn]).limited_to(625_000),
    };
    let started = std::time::Instant::now();
    let outcome = game
        .install_with(&limited, &[&cdn], &CancelToken::new())
        .await;
    // 1.25 MB at 625 kB/s.
    assert!(started.elapsed() >= millis(1900), "{:?}", started.elapsed());
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(outcome.summary().downloaded_bytes, game.wire.len() as u64);
}
