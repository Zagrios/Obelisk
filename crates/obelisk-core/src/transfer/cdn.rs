//! The request every storefront makes to its CDN.
//!
//! Epic, GOG and Steam differ in which URL they ask, which answers they give
//! another try and whom they count the bytes for. Everything else about the
//! request is the same, and lives here once: the headers, the deadline, the
//! stall limit, the status check, the body bound, the speed limit and the
//! byte counting.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::{
    FutureExt, Stream, StreamExt, TryStreamExt,
    future::BoxFuture,
    stream::{self, BoxStream},
};
use reqwest::Url;
use tokio::time::timeout;

use super::{
    BodyError, BodySize, CancelToken, Cancelled,
    body::read_body,
    pace::{self, Pace},
};

/// How long a chunk's server may send nothing before its request fails. A
/// whole-request timeout instead failed every chunk on a slow link: there,
/// the requests in flight share the bandwidth, and each takes longer than
/// any fixed bound while bytes still flow.
const CHUNK_STALL: Duration = Duration::from_secs(30);
/// How long a chunk request may take in all, so a server that trickles a
/// few bytes before each stall still lets its request go elsewhere.
const CHUNK_DEADLINE: Duration = Duration::from_secs(600);
/// The waits before a chunk's further attempts, once its quick retries
/// failed for want of the network or for a server error: about a minute in
/// all, so a Wi-Fi that drops or a Mac that changes networks is ridden out
/// instead of failing the Transfer.
pub const NETWORK_RETRY_WAITS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

/// A storefront's way to request what its CDN serves: whatever the store, a
/// request has the same headers, deadline, stall limit, speed limit and byte
/// counting.
#[derive(Clone)]
pub struct CdnClient {
    transport: Arc<dyn Transport>,
    pace: Arc<Pace>,
}

impl CdnClient {
    /// Requests go through `client` and are held to the player's speed limit.
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            transport: Arc::new(client),
            pace: pace::process(),
        }
    }

    /// GETs `url` once and returns its whole body, within `size`. Never
    /// retries: a failure says why, and the store decides what to do next.
    ///
    /// `received` is told the size of each part as it arrives, before the
    /// part is checked against `size`, so a counter includes the bytes of a
    /// refused body. The request fails once no answer or part comes within
    /// `CHUNK_STALL`, and in all after `CHUNK_DEADLINE`; the speed limit holds
    /// a part back outside that stall limit. A cancellation ends it at once,
    /// whatever is pending.
    pub async fn get(
        &self,
        url: Url,
        size: BodySize,
        cancel: &CancelToken,
        received: impl FnMut(u64),
    ) -> Result<Vec<u8>, CdnError> {
        timeout(CHUNK_DEADLINE, self.read(url, size, cancel, received))
            .await
            .unwrap_or(Err(CdnError::Network))
    }

    async fn read(
        &self,
        url: Url,
        size: BodySize,
        cancel: &CancelToken,
        received: impl FnMut(u64),
    ) -> Result<Vec<u8>, CdnError> {
        let answer = cancel
            .race(timeout(CHUNK_STALL, self.transport.open(url)))
            .await?
            .map_err(|_| CdnError::Network)?
            .map_err(|()| CdnError::Network)?;
        if !(200..300).contains(&answer.status) {
            return Err(CdnError::Http(answer.status));
        }
        let parts = self.pace.pace(unless_stalled(answer.parts, CHUNK_STALL));
        Ok(read_body(answer.length, parts, size, cancel, received).await?)
    }
}

#[cfg(any(test, feature = "testing"))]
impl CdnClient {
    /// Requests go through `transport`, held to a speed limit of the client's
    /// own, which is none until [`Self::limited_to`] sets one: the player's is
    /// left alone.
    pub(crate) fn over(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            pace: Arc::new(Pace::new()),
        }
    }

    /// This client with a speed limit of its own.
    pub fn limited_to(self, bytes_per_second: u64) -> Self {
        let pace = Pace::new();
        pace.limit(bytes_per_second);
        Self {
            pace: Arc::new(pace),
            ..self
        }
    }
}

/// Why a request got no body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdnError {
    /// Not sent, no answer within the stall limit, a body that stalled or
    /// broke, or a request that outlasted its deadline.
    Network,
    /// A status outside 2xx, redirects included; the body is not read.
    Http(u16),
    /// The advertised or streamed length broke the size, or an exact body
    /// ended short.
    Size,
    Cancelled,
}

impl CdnError {
    /// Whether the network or a server is failing rather than the chunk bad:
    /// `Network` and `Http(429 | 500..=599)`. A chunk that failed so is worth
    /// [`NETWORK_RETRY_WAITS`].
    pub fn is_outage(self) -> bool {
        matches!(self, Self::Network)
            || matches!(self, Self::Http(status) if is_outage_status(status))
    }
}

/// Whether an HTTP `status` says a server is failing rather than the chunk
/// bad: 429 and 5xx. A kit's own error tells an outage by this and its
/// network variant.
pub fn is_outage_status(status: u16) -> bool {
    matches!(status, 429 | 500..=599)
}

impl From<Cancelled> for CdnError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<BodyError> for CdnError {
    fn from(error: BodyError) -> Self {
        match error {
            BodyError::Network => Self::Network,
            BodyError::Size => Self::Size,
            BodyError::Cancelled => Self::Cancelled,
        }
    }
}

/// What a [`CdnClient`] sends its requests through: the network, or in tests
/// a CDN in memory.
pub(crate) trait Transport: Send + Sync {
    /// Sends the GET and resolves once the head of the answer arrives. A
    /// transport error is opaque, as in [`read_body`].
    fn open(&self, url: Url) -> BoxFuture<'static, Result<Answer, ()>>;
}

/// The head of an answer, and its body still to come.
pub(crate) struct Answer {
    pub status: u16,
    pub length: Option<u64>,
    pub parts: BoxStream<'static, Result<Bytes, ()>>,
}

impl Transport for reqwest::Client {
    fn open(&self, url: Url) -> BoxFuture<'static, Result<Answer, ()>> {
        let request = self
            .get(url)
            // Chunks are compressed already: never gzip them again.
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            // Replaces the client's own total timeout, which would end a slow
            // chunk: `CdnClient::get` holds the deadline itself.
            .timeout(CHUNK_DEADLINE)
            .send();
        async move {
            let response = request.await.map_err(drop)?;
            Ok(Answer {
                status: response.status().as_u16(),
                length: response.content_length(),
                parts: response.bytes_stream().map_err(drop).boxed(),
            })
        }
        .boxed()
    }
}

/// `parts`, with an error once no part arrived for `stall`: a connection
/// that stopped delivering fails that soon, while a slow one keeps going
/// however long its body takes. A stall and a transport error are alike to
/// [`read_body`], which stops at either.
fn unless_stalled(
    parts: BoxStream<'static, Result<Bytes, ()>>,
    stall: Duration,
) -> impl Stream<Item = Result<Bytes, ()>> {
    stream::unfold(parts, move |mut parts| async move {
        match timeout(stall, parts.next()).await {
            Ok(Some(part)) => Some((part, parts)),
            Ok(None) => None,
            Err(_) => Some((Err(()), parts)),
        }
    })
}

#[cfg(test)]
mod tests {
    use tokio::time::Instant;

    use super::*;
    use crate::testing::{Cdn, Reply, reaching, seconds};

    fn url(cdn: &Cdn) -> Url {
        Url::parse(&format!("{}/chunk", cdn.base())).unwrap()
    }

    /// The whole of `cdn`'s answer, with how long it took.
    async fn get(
        client: &CdnClient,
        cdn: &Cdn,
        size: BodySize,
    ) -> (Result<Vec<u8>, CdnError>, Duration) {
        let started = Instant::now();
        let body = client
            .get(url(cdn), size, &CancelToken::new(), |_| {})
            .await;
        (body, started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn a_success_status_returns_the_body() {
        for status in [200, 204] {
            let cdn = Cdn::in_memory(move |_| Reply::status(status, "body"));
            let (body, _) = get(&reaching(&[&cdn]), &cdn, BodySize::AtMost(16)).await;
            assert_eq!(body, Ok(b"body".to_vec()), "{status}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn any_other_status_fails_with_it_and_the_body_is_not_read() {
        for status in [301, 401, 404, 429, 503] {
            let cdn = Cdn::in_memory(move |_| Reply::status(status, "body"));
            let mut counted = 0;
            let body = reaching(&[&cdn])
                .get(
                    url(&cdn),
                    BodySize::AtMost(16),
                    &CancelToken::new(),
                    |length| counted += length,
                )
                .await;
            assert_eq!(body, Err(CdnError::Http(status)));
            assert_eq!(counted, 0, "{status}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_answers_fails_after_the_stall() {
        let cdn = Cdn::in_memory(|_| Reply::Silent);
        let (body, took) = get(&reaching(&[&cdn]), &cdn, BodySize::AtMost(16)).await;
        assert_eq!(body, Err(CdnError::Network));
        assert_eq!(took, CHUNK_STALL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_connection_and_an_unknown_host_fail_at_once() {
        let cdn = Cdn::in_memory(|_| Reply::Hangup);
        let (body, took) = get(&reaching(&[&cdn]), &cdn, BodySize::AtMost(16)).await;
        assert_eq!((body, took), (Err(CdnError::Network), Duration::ZERO));
        let (body, took) = get(&reaching(&[]), &cdn, BodySize::AtMost(16)).await;
        assert_eq!((body, took), (Err(CdnError::Network), Duration::ZERO));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancellation_ends_the_wait_for_an_answer_at_once() {
        let cdn = Cdn::in_memory(|_| Reply::Silent);
        let cancel = CancelToken::new();
        let started = Instant::now();
        let cancelling = async {
            tokio::time::sleep(seconds(1)).await;
            cancel.cancel();
        };
        let client = reaching(&[&cdn]);
        let (body, ()) = tokio::join!(
            client.get(url(&cdn), BodySize::AtMost(16), &cancel, |_| {}),
            cancelling
        );
        assert_eq!(body, Err(CdnError::Cancelled));
        assert_eq!(started.elapsed(), seconds(1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_body_that_stops_fails_after_the_stall_having_counted_what_came() {
        let cdn = Cdn::in_memory(|_| Reply::Stalls {
            length: 8,
            sent: b"abc".to_vec(),
        });
        let mut counted = Vec::new();
        let started = Instant::now();
        let body = reaching(&[&cdn])
            .get(
                url(&cdn),
                BodySize::AtMost(16),
                &CancelToken::new(),
                |length| counted.push(length),
            )
            .await;
        assert_eq!(body, Err(CdnError::Network));
        assert_eq!(started.elapsed(), CHUNK_STALL);
        assert_eq!(counted, [3]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancellation_ends_a_body_that_stopped_at_once() {
        let cdn = Cdn::in_memory(|_| Reply::Stalls {
            length: 8,
            sent: b"abc".to_vec(),
        });
        let cancel = CancelToken::new();
        let started = Instant::now();
        let cancelling = async {
            tokio::time::sleep(seconds(5)).await;
            cancel.cancel();
        };
        let client = reaching(&[&cdn]);
        let (body, ()) = tokio::join!(
            client.get(url(&cdn), BodySize::AtMost(16), &cancel, |_| {}),
            cancelling
        );
        assert_eq!(body, Err(CdnError::Cancelled));
        assert_eq!(started.elapsed(), seconds(5));
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_body_is_read_however_long_it_takes() {
        // A part every half stall, thirty times.
        let cdn = Cdn::in_memory(|_| Reply::Trickles {
            body: vec![7; 60],
            piece: 2,
            every: CHUNK_STALL / 2,
        });
        let (body, took) = get(&reaching(&[&cdn]), &cdn, BodySize::Exactly(60)).await;
        assert_eq!(body.map(|body| body.len()), Ok(60));
        assert!(took > CHUNK_STALL * 10, "slower than any fixed bound");
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_that_outlasts_its_deadline_fails_though_bytes_keep_flowing() {
        // A byte every 7 s never stalls, and would take over 20 minutes.
        let cdn = Cdn::in_memory(|_| Reply::Trickles {
            body: vec![7; 200],
            piece: 1,
            every: seconds(7),
        });
        let (body, took) = get(&reaching(&[&cdn]), &cdn, BodySize::AtMost(200)).await;
        assert_eq!(body, Err(CdnError::Network));
        assert_eq!(took, CHUNK_DEADLINE);
    }

    #[tokio::test(start_paused = true)]
    async fn a_length_that_breaks_the_size_is_refused_before_anything_is_counted() {
        for (reply, size) in [
            (Reply::ok(b"abcde"), BodySize::AtMost(4)),
            (Reply::ok(b"abcd"), BodySize::Exactly(5)),
            (Reply::ok(b"abcdef"), BodySize::Exactly(5)),
        ] {
            let cdn = Cdn::in_memory(move |_| reply.clone());
            let mut counted = 0;
            let body = reaching(&[&cdn])
                .get(url(&cdn), size, &CancelToken::new(), |length| {
                    counted += length
                })
                .await;
            assert_eq!(body, Err(CdnError::Size), "{size:?}");
            assert_eq!(counted, 0, "{size:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_streamed_excess_is_counted_before_it_is_refused() {
        // The server promised 3 bytes and sends 6.
        let cdn = Cdn::in_memory(|_| Reply::Stalls {
            length: 3,
            sent: b"abcdef".to_vec(),
        });
        let mut counted = Vec::new();
        let body = reaching(&[&cdn])
            .get(
                url(&cdn),
                BodySize::AtMost(4),
                &CancelToken::new(),
                |length| counted.push(length),
            )
            .await;
        assert_eq!(body, Err(CdnError::Size));
        assert_eq!(counted, [6]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancellation_from_the_counter_stops_before_the_part_is_kept() {
        let cdn = Cdn::in_memory(|_| Reply::ok(b"abcdef"));
        let cancel = CancelToken::new();
        let mut counted = Vec::new();
        let body = reaching(&[&cdn])
            .get(url(&cdn), BodySize::AtMost(16), &cancel, |length| {
                counted.push(length);
                cancel.cancel();
            })
            .await;
        assert_eq!(body, Err(CdnError::Cancelled));
        assert_eq!(counted, [6]);
    }

    /// A body that the lowest speed limit holds for longer than a stall.
    fn heavy() -> (Vec<u8>, Duration) {
        let held = CHUNK_STALL + seconds(2);
        let body = vec![0; (pace::MIN_SPEED_LIMIT * held.as_secs()) as usize];
        (body, held)
    }

    #[tokio::test(start_paused = true)]
    async fn the_speed_limit_holds_a_part_for_as_long_as_it_takes_and_that_is_no_stall() {
        let (body, held) = heavy();
        let cdn = Cdn::in_memory(move |_| Reply::ok(body.clone()));
        let client = reaching(&[&cdn]).limited_to(pace::MIN_SPEED_LIMIT);
        let (body, took) = get(&client, &cdn, BodySize::AtMost(u64::MAX)).await;
        assert_eq!(
            body.map(|body| body.len() as u64),
            Ok(pace::MIN_SPEED_LIMIT * held.as_secs())
        );
        assert_eq!(took, held);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_speed_limit_a_part_passes_at_once() {
        let (body, _) = heavy();
        let cdn = Cdn::in_memory(move |_| Reply::ok(body.clone()));
        let (body, took) = get(&reaching(&[&cdn]), &cdn, BodySize::AtMost(u64::MAX)).await;
        assert!(body.is_ok());
        assert_eq!(took, Duration::ZERO);
        let limited = reaching(&[&cdn]).limited_to(pace::MIN_SPEED_LIMIT);
        let _ = get(&limited, &cdn, BodySize::AtMost(u64::MAX)).await;
        let (_, took) = get(&reaching(&[&cdn]), &cdn, BodySize::AtMost(u64::MAX)).await;
        assert_eq!(
            took,
            Duration::ZERO,
            "a limit of its own leaves other clients alone"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_requests_of_a_client_share_one_speed_limit() {
        let second = vec![0; pace::MIN_SPEED_LIMIT as usize];
        let cdn = Cdn::in_memory(move |_| Reply::ok(second.clone()));
        let client = reaching(&[&cdn]).limited_to(pace::MIN_SPEED_LIMIT);
        let started = Instant::now();
        let other_client = client.clone();
        let (first, other) = tokio::join!(
            get(&client, &cdn, BodySize::AtMost(u64::MAX)),
            get(&other_client, &cdn, BodySize::AtMost(u64::MAX)),
        );
        assert!(first.0.is_ok() && other.0.is_ok());
        assert_eq!(started.elapsed(), seconds(2), "a second of bytes each");
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancellation_ends_a_part_the_speed_limit_holds_at_once() {
        let (body, _) = heavy();
        let cdn = Cdn::in_memory(move |_| Reply::ok(body.clone()));
        let client = reaching(&[&cdn]).limited_to(pace::MIN_SPEED_LIMIT);
        let cancel = CancelToken::new();
        let started = Instant::now();
        let cancelling = async {
            tokio::time::sleep(seconds(1)).await;
            cancel.cancel();
        };
        let (body, ()) = tokio::join!(
            client.get(url(&cdn), BodySize::AtMost(u64::MAX), &cancel, |_| {}),
            cancelling
        );
        assert_eq!(body, Err(CdnError::Cancelled));
        assert_eq!(started.elapsed(), seconds(1));
    }

    #[test]
    fn network_and_server_errors_are_outages_and_the_rest_are_not() {
        for outage in [
            CdnError::Network,
            CdnError::Http(429),
            CdnError::Http(500),
            CdnError::Http(503),
            CdnError::Http(599),
        ] {
            assert!(outage.is_outage(), "{outage:?}");
        }
        for failure in [
            CdnError::Http(400),
            CdnError::Http(401),
            CdnError::Http(403),
            CdnError::Http(404),
            CdnError::Http(410),
            CdnError::Http(428),
            CdnError::Http(600),
            CdnError::Size,
            CdnError::Cancelled,
        ] {
            assert!(!failure.is_outage(), "{failure:?}");
        }
    }
}
