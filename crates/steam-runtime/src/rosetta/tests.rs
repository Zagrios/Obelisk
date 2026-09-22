use super::*;
use std::{cell::Cell, io::Read, time::Duration};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn resume_between_state_read_and_await_is_not_lost() {
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("available");
    let gate = Gate::new(helper.clone(), |path| Ok(path.exists()));
    let requested = Cell::new(false);
    let pending = Cell::new(false);
    let waited = tokio::time::timeout(
        Duration::from_secs(2),
        gate.wait(
            || {
                requested.set(true);
                pending.set(true);
                Ok::<_, Error>(())
            },
            || {
                let observed = pending.get();
                if observed {
                    // Resume after reading the old state but before awaiting.
                    std::fs::write(&helper, b"available").unwrap();
                    pending.set(false);
                    gate.wake();
                }
                Ok(observed)
            },
        ),
    )
    .await
    .expect("resume notification was lost")
    .unwrap();
    assert!(waited && requested.get());
    assert!(!pending.get());
}

#[tokio::test]
async fn cancellation_during_the_initial_probe_preserves_the_provider_error() {
    let root = tempfile::tempdir().unwrap();
    let helper = root.path().join("probe.sock");
    let listener = tokio::net::UnixListener::bind(&helper).unwrap();
    let gate = Gate::new(helper, |path| {
        let mut connection = std::os::unix::net::UnixStream::connect(path)?;
        connection.read_exact(&mut [0])?;
        Ok(true)
    });
    let cancelled = Cell::new(false);
    let waiting = gate.wait(
        || panic!("cancelled preparation published an installation request"),
        || {
            if cancelled.get() {
                Err(crate::epic::Error::Cancelled)
            } else {
                Ok(false)
            }
        },
    );
    let cancel = async {
        let (mut connection, _) = listener.accept().await.unwrap();
        cancelled.set(true);
        gate.wake();
        connection.write_all(&[1]).await.unwrap();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(waiting, cancel)
    })
    .await
    .expect("cancelled probe did not complete");
    assert!(matches!(result, Err(crate::epic::Error::Cancelled)));
}
