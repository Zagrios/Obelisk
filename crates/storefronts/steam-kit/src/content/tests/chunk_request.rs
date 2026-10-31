//! How a chunk request enters Steam's retry choreography, as its depot
//! download shows it against a CDN in memory: the bytes it delivers and
//! counts, which answers it waits out and for how long, a body that stalls,
//! cancellation and the speed limit. How a request itself times out and is
//! held is tested once, in `obelisk-core`.
//!
//! Steam's servers time their rests and races with the real clock, so a
//! virtual one cannot play out a ladder of attempts: those tests run in real
//! time and stop at the first outage wait. A depot with a single server and
//! no second attempt to rest or race for may run ahead of the download's disk
//! work, as a stalled body does.
use std::sync::atomic::AtomicUsize;

use obelisk_core::testing::{
    Arrival, COARSE_SLACK, VirtualTime, assert_waits, assert_waits_roughly, millis, noise,
    reaching_as, seconds,
};

use super::*;

/// How far off the waits measured in real time may be.
const SLACK: Duration = Duration::from_millis(500);
const MANIFEST: &str = "/depot/42/manifest/99/5/123";

/// The waits of a chunk asked for five times, each wait twice the one
/// before, and again after the first outage wait.
fn quick() -> Vec<Duration> {
    vec![millis(250), millis(500), millis(1000), millis(2000)]
}

/// A depot of one file made of one chunk.
struct Game {
    manifest: Vec<u8>,
    route: String,
    wire: Vec<u8>,
    bytes: Vec<u8>,
}

fn game(bytes: &[u8]) -> Game {
    let mut served = routes(file("Game/data.bin", &[bytes]), &[bytes]);
    let route = format!("/depot/42/chunk/{}", hex(&Sha1::digest(bytes)));
    Game {
        manifest: served.remove(MANIFEST).unwrap(),
        wire: served.remove(&route).unwrap(),
        route,
        bytes: bytes.to_vec(),
    }
}

/// What a download of the depot ended as, and the folder it installed into.
struct Outcome {
    result: Result<DepotInstallation, ContentError>,
    folder: tempfile::TempDir,
}

impl Outcome {
    fn summary(&self) -> &DownloadSummary {
        &self.result.as_ref().expect("an installed depot").summary
    }

    fn file(&self) -> Vec<u8> {
        fs::read(self.folder.path().join("Game/data.bin")).unwrap()
    }
}

impl Game {
    /// A CDN that serves the manifest to every request for it, and answers
    /// the chunk requests as `chunk` says, given how many came before.
    fn script(
        &self,
        chunk: impl Fn(usize) -> Reply + Send + Sync + 'static,
    ) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
        let (manifest, route) = (self.manifest.clone(), self.route.clone());
        let asked = AtomicUsize::new(0);
        move |arrival| {
            if arrival.path() == MANIFEST {
                Reply::ok(manifest.clone())
            } else if arrival.path() == route {
                chunk(asked.fetch_add(1, Ordering::AcqRel))
            } else {
                Reply::status(404, "")
            }
        }
    }

    /// A CDN that serves the chunk to every request.
    fn serves(&self) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
        self.serves_after(0, Reply::Hangup)
    }

    /// A CDN that answers `first` to the first `count` chunk requests and
    /// serves the chunk to the rest.
    fn serves_after(
        &self,
        count: usize,
        first: Reply,
    ) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
        let wire = self.wire.clone();
        self.script(move |asked| {
            if asked < count {
                first.clone()
            } else {
                Reply::ok(wire.clone())
            }
        })
    }

    /// Downloads the depot from `cdn` through `servers` hosts that all
    /// stand for it.
    async fn install(&self, servers: usize, cdn: &Cdn, cancel: &CancelToken) -> Outcome {
        self.install_with(servers, cdn, cancel, |client| client)
            .await
    }

    /// `install`, with the client changed by `change`.
    async fn install_with(
        &self,
        servers: usize,
        cdn: &Cdn,
        cancel: &CancelToken,
        change: impl FnOnce(CdnClient) -> CdnClient,
    ) -> Outcome {
        let hosts: Vec<_> = (0..servers)
            .map(|index| format!("cdn{index}.test"))
            .collect();
        let hosts: Vec<_> = hosts.iter().map(String::as_str).collect();
        let reached: Vec<_> = hosts.iter().map(|host| (*host, cdn)).collect();
        let content = ContentClient {
            cdn: change(reaching_as(&reached)),
        };
        let folder = tempfile::tempdir().unwrap();
        let result = content
            .download_depot(&plan_from(folder.path(), &hosts), cancel, |_| {})
            .await;
        Outcome { result, folder }
    }
}

#[tokio::test]
async fn a_chunk_is_asked_for_once_and_its_bytes_are_counted() {
    let game = game(&noise(300_000));
    let cdn = Cdn::in_memory(game.serves());
    let outcome = game.install(1, &cdn, &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    let summary = outcome.summary();
    // The manifest counts as downloaded, not as content.
    assert_eq!(
        summary.downloaded_bytes,
        (game.manifest.len() + game.wire.len()) as u64
    );
    assert_eq!(summary.content_bytes, game.wire.len() as u64);
    assert_eq!(summary.reused_bytes, 0);
    let arrivals = cdn.arrivals();
    assert_eq!(arrivals.len(), 2);
    assert_eq!(arrivals[1].path(), game.route);
}

/// Downloads the depot from a CDN on the real clock, through a server for
/// each of six attempts, and returns how it ended with the waits between the
/// chunk requests.
async fn timed(game: &Game, reply: impl Fn(usize) -> Reply + Send + Sync + 'static) -> Timed {
    let cdn = Cdn::in_memory(game.script(reply));
    let outcome = game.install(6, &cdn, &CancelToken::new()).await;
    let arrivals = cdn.arrivals();
    assert_eq!(arrivals[0].path(), MANIFEST);
    Timed {
        outcome,
        arrivals: arrivals[1..].to_vec(),
    }
}

struct Timed {
    outcome: Outcome,
    arrivals: Vec<Arrival>,
}

#[tokio::test]
async fn an_outage_is_waited_out_to_its_first_wait_before_the_chunk_is_served() {
    let game = game(b"payload");
    let wire = game.wire.clone();
    let after_failures = |failure: Reply| {
        let wire = wire.clone();
        move |asked: usize| {
            if asked < 5 {
                failure.clone()
            } else {
                Reply::ok(wire.clone())
            }
        }
    };
    // The statuses at the edges of the outages, and a connection that closes,
    // all at once: each waits in its own depot.
    let runs = futures_util::future::join_all(
        [
            Reply::status(429, ""),
            Reply::status(500, ""),
            Reply::status(503, ""),
            Reply::status(599, ""),
            Reply::Hangup,
        ]
        .map(|failure| timed(&game, after_failures(failure))),
    )
    .await;
    let mut waits = quick();
    waits.push(seconds(1));
    for run in runs {
        assert_eq!(run.outcome.file(), game.bytes);
        assert_waits(&run.arrivals, &waits, SLACK);
    }
}

#[tokio::test]
async fn an_answer_that_is_no_outage_gets_its_quick_retries_and_no_more() {
    let game = game(b"payload");
    let statuses = [400, 428, 430, 499, 600];
    let runs = futures_util::future::join_all(
        statuses.map(|status| timed(&game, move |_| Reply::status(status, ""))),
    )
    .await;
    for (status, run) in statuses.into_iter().zip(runs) {
        assert!(
            matches!(run.outcome.result, Err(ContentError::Http(code)) if code == status),
            "{status}: {:?}",
            run.outcome.result.err()
        );
        assert_waits(&run.arrivals, &quick(), SLACK);
    }
}

#[tokio::test]
async fn a_chunk_every_server_reports_missing_is_given_up_without_a_wait() {
    let game = game(b"payload");
    let cdn = Cdn::in_memory(game.script(|_| Reply::status(404, "")));
    let outcome = game.install(1, &cdn, &CancelToken::new()).await;
    assert!(
        matches!(outcome.result, Err(ContentError::Http(404))),
        "{:?}",
        outcome.result.err()
    );
    // The manifest, then the chunk twice: two reports of it missing.
    let arrivals = cdn.arrivals();
    assert_eq!(arrivals.len(), 3);
    assert!(arrivals[2].at - arrivals[1].at < millis(240));
}

#[tokio::test]
async fn a_server_that_refuses_the_depot_is_given_up_without_a_wait() {
    let game = game(b"payload");
    for status in [401, 403] {
        let cdn = Cdn::in_memory(game.script(move |_| Reply::status(status, "")));
        let outcome = game.install(1, &cdn, &CancelToken::new()).await;
        assert!(
            matches!(outcome.result, Err(ContentError::Http(code)) if code == status),
            "{status}: {:?}",
            outcome.result.err()
        );
        assert_eq!(
            cdn.arrivals().len(),
            2,
            "{status}: the manifest and the chunk"
        );
    }
}

#[tokio::test]
async fn a_corrupt_chunk_is_asked_for_again_and_fails_as_invalid() {
    let game = game(&noise(1_000));
    let mut corrupt = game.wire.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0xff;
    let run = timed(&game, move |_| Reply::ok(corrupt.clone())).await;
    assert!(
        matches!(run.outcome.result, Err(ContentError::InvalidData(_))),
        "{:?}",
        run.outcome.result.err()
    );
    assert_waits(&run.arrivals, &quick(), SLACK);
}

#[tokio::test]
async fn a_corrupt_answer_is_counted_but_not_as_content_and_the_next_good_one_installs() {
    let game = game(&noise(1_000));
    let mut corrupt = game.wire.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0xff;
    let cdn = Cdn::in_memory(game.serves_after(1, Reply::ok(corrupt)));
    let outcome = game.install(2, &cdn, &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    let wire = game.wire.len() as u64;
    assert_eq!(
        outcome.summary().downloaded_bytes,
        game.manifest.len() as u64 + 2 * wire
    );
    assert_eq!(
        outcome.summary().content_bytes,
        wire,
        "bytes count once as content, from the response that answered"
    );
}

#[tokio::test]
async fn a_chunk_that_announces_more_than_the_bound_is_refused_unread() {
    let game = game(b"payload");
    let announced = 64 * 1024 * 1024 + 1;
    let run = timed(&game, move |_| Reply::Stalls {
        length: announced,
        sent: Vec::new(),
    })
    .await;
    assert!(
        matches!(run.outcome.result, Err(ContentError::InvalidData(_))),
        "{:?}",
        run.outcome.result.err()
    );
    assert_waits(&run.arrivals, &quick(), SLACK);
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
    let outcome = game.install(1, &cdn, &CancelToken::new()).await;
    assert_eq!(outcome.file(), game.bytes);
    assert_waits_roughly(
        &cdn.arrivals()[1..],
        &[seconds(30) + millis(250)],
        COARSE_SLACK,
    );
    let summary = outcome.summary();
    assert_eq!(
        summary.downloaded_bytes,
        (game.manifest.len() + half + game.wire.len()) as u64
    );
    assert_eq!(
        summary.content_bytes,
        game.wire.len() as u64,
        "the answering response's bytes, not the stalled one's"
    );
}

#[tokio::test]
async fn cancelling_ends_a_request_to_a_silent_server_at_once() {
    let game = game(b"payload");
    let cdn = Cdn::in_memory(game.script(|_| Reply::Silent));
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(millis(300)).await;
        trigger.cancel();
    });
    let outcome = tokio::time::timeout(seconds(5), game.install(1, &cdn, &cancel))
        .await
        .expect("a cancelled request ends without waiting for its stall");
    assert!(matches!(outcome.result, Err(ContentError::Cancelled)));
    assert_eq!(cdn.arrivals().len(), 2, "the manifest and the chunk");
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_an_outage_wait_ends_the_chunk_without_another_request() {
    let game = game(b"payload");
    let time = VirtualTime::frozen();
    let cdn = Cdn::in_memory(game.script(|_| Reply::status(503, "")));
    let cancel = CancelToken::new();
    // The manifest and the chunk twice: the second failure is followed by the
    // wait of 500 ms, which the clock then stands still in.
    let waits = [Duration::ZERO, millis(250)];
    let cancelling = async {
        time.play(&cdn, &waits).await;
        cancel.cancel();
    };
    let (outcome, ()) = tokio::join!(game.install(4, &cdn, &cancel), cancelling);
    assert!(matches!(outcome.result, Err(ContentError::Cancelled)));
    assert_eq!(cdn.arrivals().len(), 3);
}

#[tokio::test]
async fn the_speed_limit_paces_a_chunk_request() {
    let game = game(&noise(1_250_000));
    let cdn = Cdn::in_memory(game.serves());
    let started = std::time::Instant::now();
    let outcome = game
        .install_with(1, &cdn, &CancelToken::new(), |client| {
            client.limited_to(625_000)
        })
        .await;
    // 1.25 MB of chunk at 625 kB/s.
    assert!(started.elapsed() >= millis(1900), "{:?}", started.elapsed());
    assert_eq!(outcome.file(), game.bytes);
    assert_eq!(outcome.summary().content_bytes, game.wire.len() as u64);
}
