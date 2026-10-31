//! Fakes for the tests of the storefronts' CDN requests: CDNs in memory that
//! answer as scripted, and a clock that runs the retry waits and stalls of a
//! request in virtual time while the work around it blocks the runtime. The
//! [`disk`] module holds what the same tests look at on disk.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use bytes::Bytes;
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream};
use reqwest::Url;
use tokio::{
    sync::Notify,
    time::{Instant, sleep, timeout},
};

use crate::transfer::{Answer, CdnClient, Transport};

#[cfg(unix)]
pub mod disk;

/// How long a task lets the others run, in real time, so that a request and
/// its answer have been handled.
const SETTLE: Duration = Duration::from_millis(20);
/// How near to the end of a wait [`VirtualTime::play`] leaps, and how far
/// past it a request may come.
const GUARD: Duration = Duration::from_millis(50);
const TOLERANCE: Duration = Duration::from_millis(100);
/// How far a [`VirtualTime::running_coarsely`] clock moves for each
/// millisecond of real time.
const COARSE_STEP: Duration = Duration::from_millis(50);
/// How far off the times a [`VirtualTime::running_coarsely`] clock measures
/// may be.
pub const COARSE_SLACK: Duration = Duration::from_secs(2);
/// How long a request may take to arrive in real time.
const ARRIVAL: Duration = Duration::from_secs(30);
/// How long a [`Reply::Gated`] reply waits for the requests it gates on.
pub const GATE_PATIENCE: Duration = Duration::from_secs(3);

/// A paused tokio clock under the test's control, for a paused current-thread
/// runtime such as `#[tokio::test(start_paused = true)]`. A task of the
/// runtime that never idles keeps the runtime from moving the clock by
/// itself, which it would do whenever the work around the request is not
/// running, such as while the disk worker of a download is.
pub struct VirtualTime {
    task: tokio::task::JoinHandle<()>,
}

impl VirtualTime {
    /// A clock that moves forward by 50 ms for each millisecond of real time,
    /// so waits of minutes pass in seconds. What waits on other work sees the
    /// clock run ahead of it by that step for each millisecond it takes: assert
    /// times within [`COARSE_SLACK`].
    pub fn running_coarsely() -> Self {
        let task = tokio::spawn(async move {
            loop {
                tokio::time::advance(COARSE_STEP).await;
                thread::sleep(Duration::from_millis(1));
            }
        });
        Self { task }
    }

    /// A clock that stands still until [`Self::play`] moves it.
    pub fn frozen() -> Self {
        let task = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
                thread::sleep(Duration::from_micros(500));
            }
        });
        Self { task }
    }

    /// Moves a frozen clock through the `waits` between the requests `cdn`
    /// takes: after each request, nothing comes until the wait is over and
    /// the next request comes within a tolerance of it, or at once after a
    /// wait of zero. Panics otherwise.
    ///
    /// Run it beside the work that makes the requests. Each wait must be
    /// the only timer the work has pending, as the clock leaps to its end.
    pub async fn play(&self, cdn: &Cdn, waits: &[Duration]) {
        for (index, wait) in waits.iter().enumerate() {
            let taken = index + 1;
            cdn.taken(taken).await;
            if wait.is_zero() {
                cdn.taken(taken + 1).await;
                continue;
            }
            // The work answers the request and starts its wait, while the
            // clock stands still.
            settle(SETTLE).await;
            let leap = wait.saturating_sub(GUARD);
            tokio::time::advance(leap).await;
            settle(SETTLE).await;
            assert_eq!(
                cdn.arrivals().len(),
                taken,
                "request {} came before {wait:?} had passed",
                taken + 1
            );
            let mut waited = leap;
            while cdn.arrivals().len() == taken {
                assert!(
                    waited <= *wait + TOLERANCE,
                    "request {} did not come within {waited:?}, the wait being {wait:?}",
                    taken + 1
                );
                let tick = Duration::from_millis(1);
                tokio::time::advance(tick).await;
                waited += tick;
                settle(Duration::from_millis(2)).await;
            }
        }
    }
}

impl Drop for VirtualTime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Runs `work` against a CDN that answers each request as `reply` says, while
/// the waits between the requests pass in virtual time as `waits` says, on a
/// [`VirtualTime::frozen`] clock. Returns what the work ended as with the
/// requests the CDN took, which are no more than the waits allow.
pub async fn played_against<T>(
    waits: &[Duration],
    reply: impl Fn(&Arrival) -> Reply + Send + Sync + 'static,
    work: impl AsyncFnOnce(&Cdn) -> T,
) -> (T, Vec<Arrival>) {
    let time = VirtualTime::frozen();
    let cdn = Cdn::in_memory(reply);
    let (ended, ()) = tokio::join!(work(&cdn), time.play(&cdn, waits));
    let arrivals = cdn.arrivals();
    assert_eq!(
        arrivals.len(),
        waits.len() + 1,
        "no request beyond the waits"
    );
    (ended, arrivals)
}

/// Lets the other tasks of the runtime run for `real` time.
async fn settle(real: Duration) {
    let until = std::time::Instant::now() + real;
    while std::time::Instant::now() < until {
        tokio::task::yield_now().await;
        thread::sleep(Duration::from_micros(500));
    }
}

pub fn millis(count: u64) -> Duration {
    Duration::from_millis(count)
}

pub fn seconds(count: u64) -> Duration {
    Duration::from_secs(count)
}

/// `length` bytes that no compression shrinks, the same ones every time.
pub fn noise(length: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// One request a [`Cdn`] took, as it arrived.
#[derive(Clone, Debug)]
pub struct Arrival {
    /// Arrival order, from 0.
    pub index: usize,
    /// The path and query.
    pub target: String,
    /// When it arrived, since the CDN was made, on tokio's clock.
    pub at: Duration,
}

impl Arrival {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }
}

/// How a [`Cdn`] answers one request.
#[derive(Clone, Debug)]
pub enum Reply {
    /// A complete answer, in one part.
    Whole { status: u16, body: Vec<u8> },
    /// Promises `length` bytes and sends `sent`, then holds the connection
    /// without sending more.
    Stalls { length: usize, sent: Vec<u8> },
    /// Sends `body` in pieces of `piece` bytes, the first at once and then one
    /// every `every`, and ends a wait after the last.
    Trickles {
        body: Vec<u8>,
        piece: usize,
        every: Duration,
    },
    /// Takes the request and never answers.
    Silent,
    /// Drops the connection at once.
    Hangup,
    /// The reply, once the wait is over.
    After(Duration, Box<Reply>),
    /// The reply, once `arrivals` requests have arrived or [`GATE_PATIENCE`]
    /// has passed.
    Gated { arrivals: usize, then: Box<Reply> },
}

impl Reply {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self::status(200, body)
    }

    pub fn status(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::Whole {
            status,
            body: body.into(),
        }
    }
}

/// A handler for [`Cdn::in_memory`] that serves `body` at `route` to every
/// request and finds nothing at any other path.
pub fn serves(route: &str, body: &[u8]) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
    let (route, body) = (route.to_owned(), body.to_vec());
    move |arrival| {
        if arrival.path() == route {
            Reply::ok(body.clone())
        } else {
            Reply::status(404, "")
        }
    }
}

/// The same, once `first` has been the reply to each of the first `count`
/// requests.
pub fn serves_after(
    route: &str,
    body: &[u8],
    count: usize,
    first: Reply,
) -> impl Fn(&Arrival) -> Reply + Send + Sync + 'static {
    let then = serves(route, body);
    move |arrival| {
        if arrival.index < count {
            first.clone()
        } else {
            then(arrival)
        }
    }
}

type Handler = dyn Fn(&Arrival) -> Reply + Send + Sync;

/// A CDN in memory, which a client built by [`reaching`] reaches by its host.
/// It takes every request as an [`Arrival`] and answers as its handler says.
pub struct Cdn {
    host: String,
    served: Arc<Served>,
}

struct Served {
    handler: Box<Handler>,
    arrivals: Mutex<Vec<Arrival>>,
    arrived: Notify,
    made: Instant,
}

impl Cdn {
    /// A CDN that answers each request as `handler` says.
    pub fn in_memory(handler: impl Fn(&Arrival) -> Reply + Send + Sync + 'static) -> Self {
        static MADE: AtomicUsize = AtomicUsize::new(0);
        Self {
            host: format!("cdn-{}.test", MADE.fetch_add(1, Ordering::Relaxed)),
            served: Arc::new(Served {
                handler: Box::new(handler),
                arrivals: Mutex::new(Vec::new()),
                arrived: Notify::new(),
                made: Instant::now(),
            }),
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// `https://host`, without a trailing slash.
    pub fn base(&self) -> String {
        format!("https://{}", self.host)
    }

    /// Returns once `count` requests have arrived, however the runtime's clock
    /// stands: it waits in real time, by letting the other tasks run.
    pub async fn taken(&self, count: usize) {
        let until = std::time::Instant::now() + ARRIVAL;
        while self.arrivals().len() < count {
            assert!(
                std::time::Instant::now() < until,
                "request {count} never came, only {} did",
                self.arrivals().len()
            );
            settle(Duration::from_millis(1)).await;
        }
    }

    /// Every request taken so far, in the order they arrived.
    pub fn arrivals(&self) -> Vec<Arrival> {
        self.served.lock().clone()
    }
}

impl Served {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Arrival>> {
        self.arrivals.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn take(&self, target: String) -> Result<Answer, ()> {
        let arrival = {
            let mut arrivals = self.lock();
            let arrival = Arrival {
                index: arrivals.len(),
                target,
                at: self.made.elapsed(),
            };
            arrivals.push(arrival.clone());
            arrival
        };
        self.arrived.notify_waiters();
        self.answer((self.handler)(&arrival)).await
    }

    async fn answer(&self, mut reply: Reply) -> Result<Answer, ()> {
        loop {
            match reply {
                Reply::Whole { status, body } => {
                    let length = body.len() as u64;
                    let part = (!body.is_empty()).then(|| Ok(Bytes::from(body)));
                    return Ok(Answer {
                        status,
                        length: Some(length),
                        parts: stream::iter(part).boxed(),
                    });
                }
                Reply::Stalls { length, sent } => {
                    let part = (!sent.is_empty()).then(|| Ok(Bytes::from(sent)));
                    return Ok(Answer {
                        status: 200,
                        length: Some(length as u64),
                        parts: stream::iter(part).chain(stream::pending()).boxed(),
                    });
                }
                Reply::Trickles { body, piece, every } => {
                    let length = body.len() as u64;
                    let pieces: Vec<Bytes> =
                        body.chunks(piece).map(Bytes::copy_from_slice).collect();
                    let parts = stream::unfold(0, move |index| {
                        let piece = pieces.get(index).cloned();
                        async move {
                            if index > 0 {
                                sleep(every).await;
                            }
                            piece.map(|piece| (Ok(piece), index + 1))
                        }
                    });
                    return Ok(Answer {
                        status: 200,
                        length: Some(length),
                        parts: parts.boxed(),
                    });
                }
                Reply::Silent => std::future::pending().await,
                Reply::Hangup => return Err(()),
                Reply::After(wait, then) => {
                    sleep(wait).await;
                    reply = *then;
                }
                Reply::Gated { arrivals, then } => {
                    let _ = timeout(GATE_PATIENCE, self.arrivals_reach(arrivals)).await;
                    reply = *then;
                }
            }
        }
    }

    async fn arrivals_reach(&self, count: usize) {
        loop {
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            if self.lock().len() >= count {
                return;
            }
            arrived.await;
        }
    }
}

/// What a client built by [`reaching`] sends its requests through: CDNs in
/// memory, found by host. A host no CDN answers to is unreachable.
struct Memory {
    cdns: HashMap<String, Arc<Served>>,
}

impl Transport for Memory {
    fn open(&self, url: Url) -> BoxFuture<'static, Result<Answer, ()>> {
        let Some(served) = url.host_str().and_then(|host| self.cdns.get(host)) else {
            return std::future::ready(Err(())).boxed();
        };
        let served = Arc::clone(served);
        let target = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        async move { served.take(target).await }.boxed()
    }
}

/// A client whose CDN hosts are `cdns`; any other host is unreachable. It
/// is held to a speed limit of its own, which is none until
/// [`CdnClient::limited_to`] sets one.
pub fn reaching(cdns: &[&Cdn]) -> CdnClient {
    let hosts: Vec<_> = cdns.iter().map(|cdn| (cdn.host(), *cdn)).collect();
    reaching_as(&hosts)
}

/// The same, with each CDN answering to the host named, such as the servers a
/// storefront names by themselves.
pub fn reaching_as(hosts: &[(&str, &Cdn)]) -> CdnClient {
    let cdns = hosts
        .iter()
        .map(|(host, cdn)| ((*host).to_owned(), Arc::clone(&cdn.served)))
        .collect();
    CdnClient::over(Arc::new(Memory { cdns }))
}

/// The waits between consecutive `requests`.
fn waits(requests: &[Arrival]) -> Vec<Duration> {
    requests
        .windows(2)
        .map(|pair| pair[1].at.saturating_sub(pair[0].at))
        .collect()
}

/// Asserts that each wait between `requests` lasted its `expected` time and no
/// more than `slack` longer, and that there are no further requests.
pub fn assert_waits(requests: &[Arrival], expected: &[Duration], slack: Duration) {
    assert_waits_within(requests, expected, Duration::ZERO, slack);
}

/// Asserts that each wait between `requests` lasted its `expected` time,
/// give or take `slack`, and that there are no further requests. For a
/// [`VirtualTime::running_coarsely`] clock, whose readings are a step off.
pub fn assert_waits_roughly(requests: &[Arrival], expected: &[Duration], slack: Duration) {
    assert_waits_within(requests, expected, slack, slack);
}

fn assert_waits_within(
    requests: &[Arrival],
    expected: &[Duration],
    below: Duration,
    above: Duration,
) {
    let observed = waits(requests);
    assert_eq!(
        observed.len(),
        expected.len(),
        "{} requests, waits {observed:?}",
        requests.len()
    );
    for (index, (observed, expected)) in observed.iter().zip(expected).enumerate() {
        assert!(
            *observed + below >= *expected && *observed <= *expected + above,
            "wait {index} lasted {observed:?}, expected {expected:?} (-{below:?}, +{above:?}): {:?}",
            waits(requests)
        );
    }
}
