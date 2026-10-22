//! Cancellation and progress vocabulary shared by storefront download engines.
mod body;
mod engine;
mod lifecycle;
mod window;
pub use body::{
    BodyError, BodySize, CHUNK_DEADLINE, CHUNK_STALL, NETWORK_RETRY_WAITS, read_body,
    unless_stalled,
};
pub use engine::{Chunks, Delivery, Interrupted, Pipeline, run_engine};
pub use lifecycle::{TransferBusy, TransferContext, TransferStatus, Transfers};
pub use window::Window;

use std::{
    future::Future,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use tokio::sync::Notify;

/// Cancels a download from any thread. Synchronous checks stay a cheap atomic
/// load; pending CDN requests are woken immediately instead of polling.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<CancelState>);

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    notify: Notify,
    workers: AtomicUsize,
    workers_finished: Notify,
}

/// Returned by [`CancelToken::check`]; storefront errors convert it into their
/// own cancellation variant so `cancel.check()?` reads the same everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// Completes once cancelled, including when cancellation happened earlier.
    pub async fn cancelled(&self) {
        let notified = self.0.notify.notified();
        tokio::pin!(notified);
        // Register before re-reading the flag so a cancel between the two is not lost.
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    /// Runs `work` unless this token is cancelled first, including earlier.
    /// A cancellation drops `work` at once, so a CDN request or a retry pause
    /// stalled on the network ends without waiting for its timeout. When both
    /// are ready, the cancellation wins.
    pub async fn race<T>(&self, work: impl Future<Output = T>) -> Result<T, Cancelled> {
        tokio::select! {
            biased;
            () = self.cancelled() => Err(Cancelled),
            value = work => Ok(value),
        }
    }

    /// Registers work before it reaches the blocking pool. A dropped async
    /// caller cannot release its transfer slot while this work still runs.
    /// The worker must check this token at its normal cancellation points.
    pub fn spawn_blocking<F, R>(&self, work: F) -> tokio::task::JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.0.workers.fetch_add(1, Ordering::AcqRel);
        let worker = BlockingWorker(Arc::clone(&self.0));
        tokio::task::spawn_blocking(move || {
            let _worker = worker;
            work()
        })
    }

    /// Cancels the token when the guard drops before it is disarmed. Dropping
    /// an async task does not stop its `spawn_blocking` work: the guard wakes
    /// its network wait and lets its next disk check stop before a new
    /// attempt can acquire the lock.
    pub fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop(Some(self.clone()))
    }

    fn workers_finished(&self) -> bool {
        self.0.workers.load(Ordering::Acquire) == 0
    }

    async fn wait_for_workers(&self) {
        loop {
            let notified = self.0.workers_finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.workers_finished() {
                return;
            }
            notified.await;
        }
    }
}

struct BlockingWorker(Arc<CancelState>);

impl Drop for BlockingWorker {
    fn drop(&mut self) {
        if self.0.workers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.workers_finished.notify_waiters();
        }
    }
}

/// Returned by [`CancelToken::cancel_on_drop`].
pub struct CancelOnDrop(Option<CancelToken>);

impl CancelOnDrop {
    /// The work finished: dropping the guard cancels nothing.
    pub fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = &self.0 {
            cancel.cancel();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadPhase {
    #[default]
    Preparing,
    Verifying,
    Downloading,
}

/// Cumulative counters for one engine invocation, published on every change.
#[derive(Clone, Debug, Default)]
pub struct DownloadProgress {
    pub phase: DownloadPhase,
    /// Verified, installed or reusable uncompressed bytes.
    pub completed_bytes: u64,
    /// Inspection progress during the initial file scan, including bytes whose
    /// hash is still pending. Separate from reusable bytes; absent during repair.
    pub verification_bytes: Option<u64>,
    pub total_bytes: u64,
    /// Compressed bytes received during this invocation, including manifests.
    pub network_bytes: u64,
    /// Compressed chunk bytes received, excluding manifests and disk reads.
    pub content_bytes: u64,
    pub completed_files: u32,
    pub total_files: u32,
    pub current_file: String,
}

#[derive(Clone, Debug)]
pub struct DownloadSummary {
    pub downloaded_bytes: u64,
    pub content_bytes: u64,
    pub reused_bytes: u64,
    pub completed_files: u32,
    pub total_bytes: u64,
}

/// Lifecycle of one account-scoped transfer as presented to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferState {
    Preparing,
    Verifying,
    Downloading,
    /// The installation moves into the games folder.
    Moving,
    Complete,
    Cancelled,
    Failed,
}

impl From<DownloadPhase> for TransferState {
    fn from(phase: DownloadPhase) -> Self {
        match phase {
            DownloadPhase::Preparing => Self::Preparing,
            DownloadPhase::Verifying => Self::Verifying,
            DownloadPhase::Downloading => Self::Downloading,
        }
    }
}

/// Serializes progress so every snapshot handed to the caller is consistent and
/// monotonic, whether the change came from the network pump or the disk thread.
/// The callback runs inline under the lock, so a caller that cancels from within
/// it is observed before the next chunk is written.
pub struct Reporter {
    status: Mutex<DownloadProgress>,
    callback: Box<dyn Fn(DownloadProgress) + Send + Sync>,
}

impl Reporter {
    /// Publishes `initial` once so observers see the starting state immediately.
    pub fn new(
        initial: DownloadProgress,
        callback: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Arc<Self> {
        let reporter = Arc::new(Self {
            status: Mutex::new(initial),
            callback: Box::new(callback),
        });
        reporter.update(|_| {});
        reporter
    }

    pub fn update(&self, change: impl FnOnce(&mut DownloadProgress)) {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        change(&mut status);
        (self.callback)(status.clone());
    }

    pub fn snapshot(&self) -> DownloadProgress {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn summary(&self, reused_bytes: u64) -> DownloadSummary {
        let status = self.snapshot();
        DownloadSummary {
            downloaded_bytes: status.network_bytes,
            content_bytes: status.content_bytes,
            reused_bytes,
            completed_files: status.completed_files,
            total_bytes: status.total_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_reports_the_flag_and_cancellation_is_shared_by_every_clone() {
        let cancel = CancelToken::new();
        let clone = cancel.clone();
        assert!(cancel.check().is_ok());
        assert!(!clone.is_cancelled());
        clone.cancel();
        assert!(cancel.is_cancelled());
        assert!(matches!(cancel.check(), Err(Cancelled)));
    }

    #[tokio::test]
    async fn waiting_on_a_token_cancelled_earlier_returns_instead_of_hanging() {
        let cancel = CancelToken::new();
        cancel.cancel();
        // `cancelled` registers before re-reading the flag, so a cancellation
        // that already happened cannot be missed by a later waiter.
        cancel.cancelled().await;
    }

    #[tokio::test]
    async fn a_pending_waiter_wakes_on_a_cancellation_from_another_task() {
        let cancel = CancelToken::new();
        let waiter = cancel.clone();
        let woken = tokio::spawn(async move { waiter.cancelled().await });
        tokio::task::yield_now().await;
        cancel.cancel();
        woken.await.unwrap();
    }

    #[test]
    fn the_reporter_publishes_its_initial_snapshot_once_before_new_returns() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let reporter = Reporter::new(
            DownloadProgress {
                total_files: 3,
                ..DownloadProgress::default()
            },
            move |snapshot| sink.lock().unwrap().push(snapshot.total_files),
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [3],
            "observers see the starting state"
        );
        reporter.update(|status| status.completed_files = 1);
        assert_eq!(*seen.lock().unwrap(), [3, 3]);
        assert_eq!(reporter.snapshot().completed_files, 1);
    }

    #[test]
    fn a_cancellation_from_inside_the_callback_is_visible_when_update_returns() {
        // The callback runs inline under the lock, so an engine that cancels
        // from within it is observed before the next chunk is written.
        let cancel = CancelToken::new();
        let inner = cancel.clone();
        let reporter = Reporter::new(DownloadProgress::default(), move |status| {
            if status.completed_bytes > 0 {
                inner.cancel();
            }
        });
        assert!(!cancel.is_cancelled());
        reporter.update(|status| status.completed_bytes = 1);
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn the_summary_reports_network_bytes_as_downloaded_and_keeps_reused_separate() {
        let reporter = Reporter::new(DownloadProgress::default(), |_| {});
        reporter.update(|status| {
            status.network_bytes = 900;
            status.content_bytes = 800;
            status.completed_files = 4;
            status.total_bytes = 5_000;
        });
        let summary = reporter.summary(1_200);
        assert_eq!(summary.downloaded_bytes, 900);
        assert_eq!(summary.content_bytes, 800);
        assert_eq!(summary.reused_bytes, 1_200);
        assert_eq!(summary.completed_files, 4);
        assert_eq!(summary.total_bytes, 5_000);
    }

    #[tokio::test]
    async fn race_returns_cancelled_for_a_token_cancelled_earlier_or_while_work_is_pending() {
        let cancel = CancelToken::new();
        let pending = cancel.race(std::future::pending::<()>());
        let cancelling = async {
            tokio::task::yield_now().await;
            cancel.cancel();
        };
        let (raced, ()) = tokio::join!(pending, cancelling);
        assert_eq!(raced, Err(Cancelled));
        // Cancelled earlier, the cancellation wins even over finished work.
        assert_eq!(cancel.race(async { 1 }).await, Err(Cancelled));
    }

    #[tokio::test]
    async fn race_returns_the_work_when_it_finishes_first() {
        let cancel = CancelToken::new();
        assert_eq!(cancel.race(async { 7 }).await, Ok(7));
        assert!(!cancel.is_cancelled());
    }
}
