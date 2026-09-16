//! Cancellation and progress vocabulary shared by storefront download engines.
use std::{
    future::Future,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

use futures_util::{StreamExt, stream};
use tokio::sync::{Notify, mpsc};

/// Cancels a download from any thread. Synchronous checks stay a cheap atomic
/// load; pending CDN requests are woken immediately instead of polling.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<CancelState>);

#[derive(Debug, Default)]
struct CancelState {
    cancelled: AtomicBool,
    notify: Notify,
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

/// Fetches requested chunks with bounded concurrency and delivers them in
/// request order, so a disk thread can write and hash sequentially. Stops as
/// soon as the disk side goes away, a chunk fails for good, or cancellation is
/// requested; the latter is delivered so a disk thread blocked on the queue wakes.
pub async fn pump_chunks<C, T, E, F>(
    requests: mpsc::UnboundedReceiver<C>,
    deliveries: mpsc::Sender<Result<T, E>>,
    concurrency: usize,
    cancel: &CancelToken,
    fetch: impl Fn(C) -> F,
) where
    F: Future<Output = Result<T, E>>,
    E: From<Cancelled>,
{
    let requests = stream::unfold(requests, |mut requests| async move {
        requests.recv().await.map(|chunk| (chunk, requests))
    });
    let fetched = requests.map(fetch).buffered(concurrency);
    tokio::pin!(fetched);
    loop {
        let next = tokio::select! {
            result = fetched.next() => result,
            () = deliveries.closed() => return,
            () = cancel.cancelled() => {
                let _ = deliveries.send(Err(Cancelled.into())).await;
                return;
            }
        };
        let Some(result) = next else { return };
        let failed = result.is_err();
        if deliveries.send(result).await.is_err() || failed {
            return;
        }
    }
}
