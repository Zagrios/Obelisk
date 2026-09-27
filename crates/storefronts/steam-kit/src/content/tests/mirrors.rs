//! A depot spread over several CDN servers, each a local fixture that answers
//! its own way.

use super::*;

/// One file of `count` distinct chunks, with the routes that serve it.
fn game(count: usize) -> (Vec<Vec<u8>>, HashMap<String, Vec<u8>>) {
    let pieces: Vec<_> = (0..count)
        .map(|index| format!("chunk {index} of the game").into_bytes())
        .collect();
    let slices: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
    let routes = routes(file("game.bin", &slices), &slices);
    (pieces, routes)
}

/// Grants `token` to every server that asks, recording each host.
struct Grant {
    token: Option<&'static str>,
    hosts: Mutex<Vec<String>>,
}

impl Grant {
    fn new(token: Option<&'static str>) -> Self {
        Self {
            token,
            hosts: Mutex::new(Vec::new()),
        }
    }
}

impl CdnAuthorization for Grant {
    fn token<'a>(&'a self, host: &'a str) -> BoxFuture<'a, Option<String>> {
        self.hosts.lock().unwrap().push(host.into());
        Box::pin(async move { self.token.map(Into::into) })
    }
}

#[tokio::test]
async fn a_depot_spreads_its_chunks_over_every_server_that_serves_it() {
    let directory = tempfile::tempdir().unwrap();
    let (pieces, routes) = game(24);
    let first = CdnFixture::new(routes.clone());
    let second = CdnFixture::new(routes);
    let client = serving(&[("first.cdn", &first), ("second.cdn", &second)]);
    client
        .download_depot(
            &plan_from(directory.path(), &["first.cdn", "second.cdn"]),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read(directory.path().join("game.bin")).unwrap(),
        pieces.concat()
    );
    assert!(first.chunk_requests() > 0);
    assert!(second.chunk_requests() > 0);
    assert_eq!(first.chunk_requests() + second.chunk_requests(), 24);
}

#[tokio::test]
async fn a_failing_server_hands_its_requests_to_the_others() {
    let directory = tempfile::tempdir().unwrap();
    let (pieces, routes) = game(24);
    let failing = CdnFixture::answering(|_| (503, b"busy".to_vec(), Duration::ZERO));
    let healthy = CdnFixture::new(routes);
    let client = serving(&[("failing.cdn", &failing), ("healthy.cdn", &healthy)]);
    // Steam lists the failing server first: even the manifest moves on.
    client
        .download_depot(
            &plan_from(directory.path(), &["failing.cdn", "healthy.cdn"]),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read(directory.path().join("game.bin")).unwrap(),
        pieces.concat()
    );
    // Three failures in a row retire it for the rest of the depot.
    assert!(failing.requests.lock().unwrap().len() <= 3);
}

#[tokio::test]
async fn a_lone_server_is_tried_again_after_a_failure() {
    let directory = tempfile::tempdir().unwrap();
    let (pieces, routes) = game(4);
    let answered = std::sync::atomic::AtomicUsize::new(0);
    let fixture = CdnFixture::answering(move |path| {
        // Every third request, the manifest's first among them, meets a
        // transient failure.
        if answered.fetch_add(1, Ordering::Relaxed).is_multiple_of(3) {
            return (503, b"busy".to_vec(), Duration::ZERO);
        }
        let bytes = routes.get(path).cloned().unwrap_or_default();
        (200, bytes, Duration::ZERO)
    });
    fixture
        .client()
        .download_depot(&plan(directory.path()), &CancelToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        fs::read(directory.path().join("game.bin")).unwrap(),
        pieces.concat()
    );
}

#[tokio::test]
async fn an_unreachable_or_corrupt_server_does_not_fail_the_depot() {
    let directory = tempfile::tempdir().unwrap();
    let (pieces, routes) = game(12);
    let corrupt = CdnFixture::answering({
        let routes = routes.clone();
        move |path| {
            let mut bytes = routes.get(path).cloned().unwrap_or_default();
            if path.contains("/chunk/") {
                bytes.truncate(bytes.len() / 2);
            }
            (200, bytes, Duration::ZERO)
        }
    });
    let healthy = CdnFixture::new(routes);
    let client = serving(&[("corrupt.cdn", &corrupt), ("healthy.cdn", &healthy)]);
    client
        .download_depot(
            &plan_from(
                directory.path(),
                &["unreachable.cdn", "corrupt.cdn", "healthy.cdn"],
            ),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read(directory.path().join("game.bin")).unwrap(),
        pieces.concat()
    );
}

#[tokio::test]
async fn a_server_that_refuses_the_depot_serves_it_once_authorized() {
    let directory = tempfile::tempdir().unwrap();
    let (pieces, routes) = game(4);
    let fixture = CdnFixture::answering(move |target| {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        match routes.get(path) {
            Some(bytes) if query == "token=granted" => (200, bytes.clone(), Duration::ZERO),
            _ => (403, b"forbidden".to_vec(), Duration::ZERO),
        }
    });
    let grant = Grant::new(Some("?token=granted"));
    let mut plan = plan(directory.path());
    plan.authorization = Some(&grant);
    fixture
        .client()
        .download_depot(&plan, &CancelToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        fs::read(directory.path().join("game.bin")).unwrap(),
        pieces.concat()
    );
    assert_eq!(*grant.hosts.lock().unwrap(), [CDN_HOST]);
}

#[tokio::test]
async fn a_depot_every_server_refuses_fails_as_forbidden() {
    let directory = tempfile::tempdir().unwrap();
    let refuse = || CdnFixture::answering(|_| (403, b"forbidden".to_vec(), Duration::ZERO));
    let (first, second) = (refuse(), refuse());
    let client = serving(&[("first.cdn", &first), ("second.cdn", &second)]);
    // Each server gets one token; refusing it too retires the server.
    let grant = Grant::new(Some("token=refused"));
    let mut plan = plan_from(directory.path(), &["first.cdn", "second.cdn"]);
    plan.authorization = Some(&grant);
    let result = client
        .download_depot(&plan, &CancelToken::new(), |_| {})
        .await;
    assert!(matches!(result, Err(ContentError::Http(403))));
    assert_eq!(*grant.hosts.lock().unwrap(), ["first.cdn", "second.cdn"]);
    // Without authorization, the first refusal is final.
    plan.authorization = None;
    let result = client
        .download_depot(&plan, &CancelToken::new(), |_| {})
        .await;
    assert!(matches!(result, Err(ContentError::Http(403))));
}

#[tokio::test]
async fn a_chunk_every_server_reports_missing_fails_the_depot() {
    let directory = tempfile::tempdir().unwrap();
    let (_, mut routes) = game(1);
    routes.retain(|path, _| !path.contains("/chunk/"));
    let first = CdnFixture::new(routes.clone());
    let second = CdnFixture::new(routes);
    let client = serving(&[("first.cdn", &first), ("second.cdn", &second)]);
    let result = client
        .download_depot(
            &plan_from(directory.path(), &["first.cdn", "second.cdn"]),
            &CancelToken::new(),
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(ContentError::Http(404))));
    assert_eq!(first.chunk_requests() + second.chunk_requests(), 2);
}

#[tokio::test]
async fn a_chunk_the_writer_waits_on_is_raced_on_a_faster_server() {
    let directory = tempfile::tempdir().unwrap();
    let (pieces, routes) = game(24);
    // A busy server: it answers the manifest at once, then holds every chunk.
    let slow = CdnFixture::answering({
        let routes = routes.clone();
        move |path| {
            let delay = if path.contains("/chunk/") {
                Duration::from_secs(60)
            } else {
                Duration::ZERO
            };
            let bytes = routes.get(path).cloned().unwrap_or_default();
            (200, bytes, delay)
        }
    });
    let fast = CdnFixture::new(routes);
    let client = serving(&[("slow.cdn", &slow), ("fast.cdn", &fast)]);
    let started = Instant::now();
    client
        .download_depot(
            &plan_from(directory.path(), &["slow.cdn", "fast.cdn"]),
            &CancelToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "chunks held by the busy server must come from the fast one"
    );
    assert_eq!(
        fs::read(directory.path().join("game.bin")).unwrap(),
        pieces.concat()
    );
    assert!(slow.chunk_requests() > 0, "the busy server was measured");
}
