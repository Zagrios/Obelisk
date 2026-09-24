use super::*;
use std::sync::{Arc, Mutex};

#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("busy")]
    Busy,
    #[error("cancelled")]
    Cancelled,
    #[error("engine failed")]
    Engine,
    #[error("{0}")]
    Install(InstallFailure),
}

impl From<InstallFailure> for Failure {
    fn from(error: InstallFailure) -> Self {
        Self::Install(error)
    }
}
impl From<Cancelled> for Failure {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}
impl From<TransferBusy> for Failure {
    fn from(_: TransferBusy) -> Self {
        Self::Busy
    }
}
impl TransferError for Failure {
    fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Receipt {
    build: u32,
}

const NAME: &str = "store-7-macos";

fn files(build: u32) -> InstalledFiles<Receipt> {
    InstalledFiles {
        receipt: Receipt { build },
        total_bytes: 30,
        network_bytes: 12,
        content_bytes: 10,
    }
}

/// Installs through the real sequence with an engine that writes one file.
async fn install_build(
    transfers: &Transfers<TransferProgress>,
    parent: &Path,
    build: u32,
    engine: impl FnOnce(&Path) -> Result<(), Failure>,
) -> Result<Receipt, Failure> {
    install(
        transfers,
        "7",
        Destination {
            parent,
            directory: NAME,
        },
        |_, _| async { Ok::<_, Failure>(()) },
        |(), root, _| async move {
            engine(&root)?;
            Ok(files(build))
        },
    )
    .await
}

fn read_receipt(root: &Path) -> Option<Receipt> {
    fs::read(root.join(RECEIPT_FILE))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn the_receipt_is_published_after_every_file_with_the_final_progress() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    let root = parent.path().canonicalize().unwrap().join(NAME);
    let receipt = install_build(&transfers, parent.path(), 1, |root| {
        assert!(
            !root.join(RECEIPT_FILE).exists(),
            "no receipt while files change"
        );
        fs::write(root.join("game"), b"files").map_err(|_| Failure::Engine)
    })
    .await
    .unwrap();
    assert_eq!(receipt, Receipt { build: 1 });
    assert_eq!(read_receipt(&root), Some(Receipt { build: 1 }));
    let progress = transfers.snapshot().unwrap();
    assert_eq!(progress.game, "7");
    assert_eq!(progress.state, TransferState::Complete);
    assert_eq!((progress.completed_bytes, progress.total_bytes), (30, 30));
    assert_eq!((progress.network_bytes, progress.content_bytes), (12, 10));
    assert_eq!(progress.message, "backend.transfer.complete");
}

#[tokio::test]
async fn a_failed_update_withdraws_the_old_receipt() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    let root = parent.path().canonicalize().unwrap().join(NAME);
    install_build(&transfers, parent.path(), 1, |_| Ok(()))
        .await
        .unwrap();
    assert!(read_receipt(&root).is_some());

    let failed = install_build(&transfers, parent.path(), 2, |_| Err(Failure::Engine)).await;

    assert!(matches!(failed, Err(Failure::Engine)));
    assert_eq!(
        read_receipt(&root),
        None,
        "a half-updated installation is not launchable"
    );
    let progress = transfers.snapshot().unwrap();
    assert_eq!(progress.state, TransferState::Failed);
    assert_eq!(progress.message, "engine failed");
}

#[tokio::test]
async fn an_engine_cancellation_is_the_transfers_outcome() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    let result = install_build(&transfers, parent.path(), 1, |_| Err(Failure::Cancelled)).await;
    assert!(matches!(result, Err(Failure::Cancelled)));
    assert_eq!(
        transfers.snapshot().unwrap().state,
        TransferState::Cancelled
    );
    assert_eq!(transfers.snapshot().unwrap().message, "cancelled");
}

#[tokio::test]
async fn preparation_runs_before_the_folder_or_its_lock_exists() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    let root = parent.path().canonicalize().unwrap().join(NAME);
    let seen = Arc::new(Mutex::new(None));
    let during = Arc::clone(&seen);
    install(
        &transfers,
        "7",
        Destination {
            parent: parent.path(),
            directory: NAME,
        },
        |prepared_root, _| async move {
            *during.lock().unwrap() = Some((prepared_root.clone(), prepared_root.exists()));
            Ok::<_, Failure>(())
        },
        |(), root, _| async move {
            assert!(
                matches!(lock(&root), Err(InstallFailure::Locked)),
                "files are written under the installation lock"
            );
            assert!(matches!(
                open::<Receipt>(&root),
                Err(InstallFailure::Locked)
            ));
            Ok(files(1))
        },
    )
    .await
    .unwrap();
    assert_eq!(*seen.lock().unwrap(), Some((root.clone(), false)));
    assert!(
        lock(&root).is_ok(),
        "the lock is released with the Transfer"
    );
}

#[tokio::test]
async fn a_cancelled_preparation_touches_no_file() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    let result = install(
        &transfers,
        "7",
        Destination {
            parent: parent.path(),
            directory: NAME,
        },
        |_, cancel| async move {
            cancel.cancel();
            Ok::<_, Failure>(())
        },
        |(), _, _| async { panic!("no file after a cancelled preparation") },
    )
    .await;
    assert!(matches!(result, Err::<Receipt, _>(Failure::Cancelled)));
    assert!(!parent.path().join(NAME).exists());
}

#[tokio::test]
async fn the_destination_is_confined_before_preparation() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    let attempt = |parent: PathBuf, directory: &'static str| {
        let transfers = &transfers;
        async move {
            install(
                transfers,
                "7",
                Destination {
                    parent: &parent,
                    directory,
                },
                |_, _| async { panic!("an unsafe destination is refused before preparation") },
                |(), _, _| async { Ok::<_, Failure>(files(1)) },
            )
            .await
            .map(|_: Receipt| ())
        }
    };
    let missing = parent.path().join("missing");
    assert!(matches!(
        attempt(missing, NAME).await,
        Err(Failure::Install(InstallFailure::ParentMissing))
    ));
    assert!(matches!(
        attempt(PathBuf::from("relative"), NAME).await,
        Err(Failure::Install(InstallFailure::ParentMissing))
    ));
    for directory in ["", ".", "..", "a/b", "a\\b", ".forge-content"] {
        assert!(
            matches!(
                attempt(parent.path().to_owned(), directory).await,
                Err(Failure::Install(InstallFailure::UnsafeDirectory(_)))
            ),
            "{directory:?}"
        );
    }
    fs::write(parent.path().join("file"), b"").unwrap();
    assert!(matches!(
        attempt(parent.path().to_owned(), "file").await,
        Err(Failure::Install(InstallFailure::DestinationIsFile(_)))
    ));
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), parent.path().join("link")).unwrap();
        assert!(matches!(
            attempt(parent.path().to_owned(), "link").await,
            Err(Failure::Install(InstallFailure::DestinationIsLink(_)))
        ));
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_linked_lock_or_receipt_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target");
    fs::write(&target, b"unchanged").unwrap();
    let root = parent.path().canonicalize().unwrap().join(NAME);
    fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(&target, root.join(LOCK_FILE)).unwrap();
    let transfers = Transfers::default();
    assert!(matches!(
        install_build(&transfers, parent.path(), 1, |_| Ok(())).await,
        Err(Failure::Install(InstallFailure::UnsafeLock(_)))
    ));
    fs::remove_file(root.join(LOCK_FILE)).unwrap();
    fs::hard_link(&target, root.join(RECEIPT_FILE)).unwrap();
    assert!(matches!(
        install_build(&transfers, parent.path(), 1, |_| Ok(())).await,
        Err(Failure::Install(InstallFailure::UnsafeReceipt(_)))
    ));
    fs::remove_file(root.join(RECEIPT_FILE)).unwrap();
    // A link substituted while files are written is refused at publication.
    let substituted = target.clone();
    assert!(matches!(
        install_build(&transfers, parent.path(), 1, move |root| {
            std::os::unix::fs::symlink(&substituted, root.join(RECEIPT_FILE))
                .map_err(|_| Failure::Engine)
        })
        .await,
        Err(Failure::Install(InstallFailure::UnsafeReceipt(_)))
    ));
    assert_eq!(fs::read(&target).unwrap(), b"unchanged");
}

#[tokio::test]
async fn an_installation_opens_with_its_receipt_under_the_lock() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    let root = parent.path().canonicalize().unwrap().join(NAME);
    install_build(&transfers, parent.path(), 3, |_| Ok(()))
        .await
        .unwrap();

    let opened = open::<Receipt>(&parent.path().join(NAME)).unwrap();
    assert_eq!(opened.root, root);
    assert_eq!(opened.receipt, Some(Receipt { build: 3 }));
    assert!(matches!(lock(&root), Err(InstallFailure::Locked)));
    let busy = install_build(&transfers, parent.path(), 4, |_| Ok(())).await;
    assert!(matches!(
        busy,
        Err(Failure::Install(InstallFailure::Locked))
    ));
    assert_eq!(
        read_receipt(&root),
        Some(Receipt { build: 3 }),
        "a refused Transfer withdraws nothing"
    );
    drop(opened);

    fs::write(root.join(RECEIPT_FILE), b"not json").unwrap();
    assert_eq!(open::<Receipt>(&root).unwrap().receipt, None);
    assert!(matches!(
        open::<Receipt>(&parent.path().join("missing")),
        Err(InstallFailure::Io(_))
    ));
}

#[tokio::test]
async fn an_up_to_date_installation_reports_no_downloaded_content() {
    let parent = tempfile::tempdir().unwrap();
    let transfers = Transfers::default();
    install(
        &transfers,
        "7",
        Destination {
            parent: parent.path(),
            directory: NAME,
        },
        |_, _| async { Ok::<_, Failure>(()) },
        |(), _, _| async {
            Ok(InstalledFiles {
                content_bytes: 0,
                ..files(1)
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(
        transfers.snapshot().unwrap().message,
        "backend.transfer.upToDate"
    );
}
