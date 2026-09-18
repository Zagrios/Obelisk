use super::*;
use std::{future::pending, time::Duration};
use tokio::{sync::oneshot, task::JoinHandle};

#[derive(Clone, Debug)]
struct Progress {
    game: u32,
    phase: TransferState,
    bytes: u64,
    error: Option<String>,
}

impl Progress {
    fn new(game: u32) -> Self {
        Self {
            game,
            phase: TransferState::Preparing,
            bytes: 0,
            error: None,
        }
    }
}

impl TransferStatus for Progress {
    fn finish(&mut self, state: TransferState, error: Option<String>) {
        self.phase = state;
        self.error = error;
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum Failure {
    #[error("busy")]
    Busy,
    #[error("cancelled")]
    Cancelled,
    #[error("disk failure")]
    Disk,
}

impl From<TransferBusy> for Failure {
    fn from(_: TransferBusy) -> Self {
        Self::Busy
    }
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("transfer lifecycle did not settle")
}

#[tokio::test]
async fn abandoned_transfer_keeps_admission_until_disk_exit_and_rejects_old_progress() {
    let transfers = Arc::new(Transfers::default());
    let (release, blocked) = oneshot::channel();
    let (started, running) = oneshot::channel();
    let (expose, worker) = oneshot::channel();
    let owner = Arc::clone(&transfers);
    let task = tokio::spawn(async move {
        owner
            .run(Progress::new(1), |context| async move {
                let late = context.clone();
                let work = context.cancel_token().spawn_blocking(move || {
                    started.send(()).unwrap();
                    blocked.blocking_recv().unwrap();
                    assert!(late.cancel_token().is_cancelled());
                    late.update(|progress| progress.bytes = 999);
                });
                expose
                    .send((context, work))
                    .unwrap_or_else(|_| panic!("test receiver lost"));
                pending::<Result<(), Failure>>().await
            })
            .await
    });
    let (late, work) = bounded(worker).await.unwrap();
    bounded(running).await.unwrap();
    task.abort();
    assert!(bounded(task).await.unwrap_err().is_cancelled());
    assert!(late.cancel_token().is_cancelled());
    assert_eq!(
        transfers
            .run(Progress::new(2), |_| async { Ok::<_, Failure>(()) })
            .await,
        Err(Failure::Busy)
    );
    assert_eq!(
        transfers.snapshot().unwrap().phase,
        TransferState::Preparing
    );
    release.send(()).unwrap();
    bounded(work).await.unwrap();
    let cancelled = transfers.snapshot().unwrap();
    assert_eq!(cancelled.phase, TransferState::Cancelled);
    assert_eq!(cancelled.bytes, 0);
    transfers
        .run(Progress::new(2), |context| async move {
            late.update(|progress| {
                progress.game = 1;
                progress.bytes = 999;
            });
            context.update(|progress| progress.bytes = 42);
            Ok::<_, Failure>(())
        })
        .await
        .unwrap();
    let completed = transfers.snapshot().unwrap();
    assert_eq!(completed.game, 2);
    assert_eq!(completed.bytes, 42);
    assert_eq!(completed.phase, TransferState::Complete);
}

#[tokio::test]
async fn explicit_cancellation_drains_workers_before_returning_or_admitting_next_transfer() {
    let transfers = Arc::new(Transfers::default());
    let (release, blocked) = oneshot::channel();
    let (expose, worker) = oneshot::channel::<JoinHandle<()>>();
    let (ending, ended) = oneshot::channel();
    let owner = Arc::clone(&transfers);
    let task = tokio::spawn(async move {
        owner
            .run(Progress::new(1), |context| async move {
                let work = context.cancel_token().spawn_blocking(move || {
                    blocked.blocking_recv().unwrap();
                });
                expose.send(work).unwrap();
                context.cancel_token().cancelled().await;
                context.update(|progress| progress.bytes = 999);
                ending.send(()).unwrap();
                Err::<(), _>(Failure::Cancelled)
            })
            .await
    });
    let work = bounded(worker).await.unwrap();
    transfers.cancel();
    bounded(ended).await.unwrap();
    assert!(!task.is_finished());
    assert_eq!(
        transfers
            .run(Progress::new(2), |_| async { Ok::<_, Failure>(()) })
            .await,
        Err(Failure::Busy)
    );
    release.send(()).unwrap();
    bounded(work).await.unwrap();
    assert_eq!(bounded(task).await.unwrap(), Err(Failure::Cancelled));
    let snapshot = transfers.snapshot().unwrap();
    assert_eq!(snapshot.phase, TransferState::Cancelled);
    assert_eq!(snapshot.bytes, 0);
    assert_eq!(snapshot.error.as_deref(), Some("cancelled"));
    transfers
        .run(Progress::new(2), |_| async { Ok::<_, Failure>(()) })
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_transfer_preserves_error_and_late_callbacks_cannot_overwrite_next_attempt() {
    let transfers = Transfers::default();
    let (expose, context) = oneshot::channel();
    assert_eq!(
        transfers
            .run(Progress::new(1), |context| async move {
                context.update(|progress| progress.bytes = 7);
                expose
                    .send(context)
                    .unwrap_or_else(|_| panic!("test receiver lost"));
                Err::<(), _>(Failure::Disk)
            })
            .await,
        Err(Failure::Disk)
    );
    let failed = transfers.snapshot().unwrap();
    assert_eq!(failed.phase, TransferState::Failed);
    assert_eq!(failed.error.as_deref(), Some("disk failure"));
    assert_eq!(failed.bytes, 7);
    let old = context.await.unwrap();
    transfers
        .run(Progress::new(2), |_| async {
            old.update(|progress| progress.bytes = 999);
            assert_eq!(transfers.snapshot().unwrap().bytes, 0);
            Ok::<_, Failure>(())
        })
        .await
        .unwrap();
    old.update(|progress| progress.phase = TransferState::Downloading);
    assert_eq!(transfers.snapshot().unwrap().phase, TransferState::Complete);
}

#[tokio::test]
async fn worker_panic_releases_admission_and_failure_remains_observable() {
    let transfers = Transfers::default();
    assert_eq!(
        transfers
            .run(Progress::new(1), |context| async move {
                let failed = context
                    .cancel_token()
                    .spawn_blocking(|| panic!("worker failure"));
                assert!(bounded(failed).await.unwrap_err().is_panic());
                Err::<(), _>(Failure::Disk)
            })
            .await,
        Err(Failure::Disk)
    );
    assert_eq!(transfers.snapshot().unwrap().phase, TransferState::Failed);
    transfers
        .run(Progress::new(2), |_| async { Ok::<_, Failure>(()) })
        .await
        .unwrap();
}

#[tokio::test]
async fn abandonment_waits_for_every_worker_not_only_the_first_one() {
    let transfers = Arc::new(Transfers::default());
    let (first_release, first_wait) = oneshot::channel();
    let (last_release, last_wait) = oneshot::channel();
    let (expose, workers) = oneshot::channel();
    let owner = Arc::clone(&transfers);
    let task = tokio::spawn(async move {
        owner
            .run(Progress::new(1), |context| async move {
                let first = context.cancel_token().spawn_blocking(move || {
                    first_wait.blocking_recv().unwrap();
                });
                let last = context.cancel_token().spawn_blocking(move || {
                    last_wait.blocking_recv().unwrap();
                });
                expose.send((first, last)).unwrap();
                pending::<Result<(), Failure>>().await
            })
            .await
    });
    let (first, last) = bounded(workers).await.unwrap();
    task.abort();
    assert!(bounded(task).await.unwrap_err().is_cancelled());
    first_release.send(()).unwrap();
    bounded(first).await.unwrap();
    assert_eq!(
        transfers
            .run(Progress::new(2), |_| async { Ok::<_, Failure>(()) })
            .await,
        Err(Failure::Busy)
    );
    last_release.send(()).unwrap();
    bounded(last).await.unwrap();
    assert_eq!(
        transfers.snapshot().unwrap().phase,
        TransferState::Cancelled
    );
    transfers
        .run(Progress::new(2), |_| async { Ok::<_, Failure>(()) })
        .await
        .unwrap();
}
