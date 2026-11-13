//! The speed the player may limit every storefront's downloads to.
//!
//! One pace holds the whole process: the requests of a Transfer reserve their
//! bytes on one timeline, so the limit holds for their total rather than for
//! each connection, and a new limit applies to the parts still to come.

use std::{
    sync::{
        Arc, LazyLock, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{Stream, StreamExt};
use tokio::time::{Instant, sleep_until};

static PACE: LazyLock<Arc<Pace>> = LazyLock::new(|| Arc::new(Pace::new()));

/// The lowest limit that holds. The requests in flight divide a limit among
/// them and each must still end within the deadline of a CDN request, which a
/// lower one could not allow.
pub(super) const MIN_SPEED_LIMIT: u64 = 625_000;

/// Limits what every storefront downloads, in bytes per second; zero lifts the
/// limit and a limit below the lowest that holds is raised to it. It applies
/// at once, to the Transfer under way too.
pub fn set_speed_limit(bytes_per_second: u64) {
    PACE.limit(bytes_per_second);
}

/// The pace of the player's speed limit.
pub(super) fn process() -> Arc<Pace> {
    Arc::clone(&PACE)
}

pub(super) struct Pace {
    /// Zero for no limit.
    bytes_per_second: AtomicU64,
    /// When the bytes reserved so far have all passed.
    next: Mutex<Option<Instant>>,
}

impl Pace {
    pub(super) const fn new() -> Self {
        Self {
            bytes_per_second: AtomicU64::new(0),
            next: Mutex::new(None),
        }
    }

    pub(super) fn limit(&self, bytes_per_second: u64) {
        let bytes_per_second = match bytes_per_second {
            0 => 0,
            limit => limit.max(MIN_SPEED_LIMIT),
        };
        self.bytes_per_second
            .store(bytes_per_second, Ordering::Relaxed);
        // What was reserved at the old pace is no debt at the new one.
        *self.lock() = None;
    }

    /// `parts`, each held back until its bytes fit the limit.
    pub(super) fn pace<B: AsRef<[u8]>, X>(
        &self,
        parts: impl Stream<Item = Result<B, X>>,
    ) -> impl Stream<Item = Result<B, X>> {
        parts.then(move |part| async move {
            if let Ok(bytes) = &part {
                self.pass(bytes.as_ref().len()).await;
            }
            part
        })
    }

    /// Returns once `bytes` more may pass. A pause is no credit: after one,
    /// the bytes do not pass at once.
    async fn pass(&self, bytes: usize) {
        let rate = self.bytes_per_second.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let passed = {
            let mut next = self.lock();
            let now = Instant::now();
            let start = next.map_or(now, |next| next.max(now));
            let passed = start + Duration::from_secs_f64(bytes as f64 / rate as f64);
            *next = Some(passed);
            passed
        };
        sleep_until(passed).await;
    }

    fn lock(&self) -> MutexGuard<'_, Option<Instant>> {
        self.next.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use futures_util::stream;

    use super::*;

    /// The lowest limit, and the bytes that pass in a tenth of a second at it.
    const RATE: u64 = MIN_SPEED_LIMIT;
    const TENTH: usize = (RATE / 10) as usize;

    fn limited(bytes_per_second: u64) -> Pace {
        let pace = Pace::new();
        pace.limit(bytes_per_second);
        pace
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_requests_share_one_pace() {
        let pace = limited(RATE);
        let start = Instant::now();
        tokio::join!(pace.pass(TENTH), pace.pass(TENTH), pace.pass(TENTH));
        assert_eq!(start.elapsed(), Duration::from_millis(300));
    }

    #[tokio::test(start_paused = true)]
    async fn no_limit_neither_waits_nor_reserves() {
        let pace = limited(0);
        let start = Instant::now();
        pace.pass(usize::MAX).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert_eq!(*pace.lock(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pause_does_not_let_the_next_bytes_through_at_once() {
        let pace = limited(RATE);
        pace.pass(TENTH).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let start = Instant::now();
        pace.pass(TENTH).await;
        pace.pass(TENTH).await;
        assert_eq!(start.elapsed(), Duration::from_millis(200));
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_limit_applies_at_once_and_forgets_what_the_old_one_reserved() {
        let pace = limited(RATE);
        // Reserves ten seconds, then gives up waiting for them.
        let _ = tokio::time::timeout(Duration::from_millis(1), pace.pass(100 * TENTH)).await;
        pace.limit(2 * RATE);
        let start = Instant::now();
        pace.pass(TENTH).await;
        assert_eq!(start.elapsed(), Duration::from_millis(50));
    }

    #[tokio::test(start_paused = true)]
    async fn a_limit_below_the_lowest_that_holds_is_raised_to_it() {
        let pace = limited(1);
        let start = Instant::now();
        pace.pass(MIN_SPEED_LIMIT as usize).await;
        assert_eq!(start.elapsed(), Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_paced_stream_holds_each_part_and_passes_a_failure_on() {
        let pace = limited(RATE);
        let parts = stream::iter([Ok(vec![0; TENTH]), Ok(vec![1; TENTH]), Err("lost")]);
        let start = Instant::now();
        let paced: Vec<_> = pace.pace(parts).collect().await;
        assert_eq!(start.elapsed(), Duration::from_millis(200));
        assert_eq!(paced, [Ok(vec![0; TENTH]), Ok(vec![1; TENTH]), Err("lost")]);
    }
}
