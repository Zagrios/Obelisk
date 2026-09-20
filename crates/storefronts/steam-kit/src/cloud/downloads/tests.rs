use super::*;
use sha1::{Digest, Sha1};
use std::cell::Cell;

fn files(count: usize) -> Vec<CloudFile> {
    (0..count)
        .map(|i| CloudFile {
            name: i.to_string(),
            sha1: crate::content::hex(&Sha1::digest(b"save")),
            size: 4,
            timestamp: 1,
            persist_state: 0,
            platforms: 7,
            reupload_requested: false,
        })
        .collect()
}

#[derive(Default)]
struct Plans<'a> {
    prepared: Option<&'a Cell<usize>>,
    revoke: Option<&'a Cell<bool>>,
}
impl DownloadPlans for Plans<'_> {
    type Plan = ();
    fn prepare(&mut self, _: CloudFile) -> impl Future<Output = Result<(), CloudError>> + Send {
        if let Some(count) = self.prepared {
            count.set(count.get() + 1);
        }
        if let Some(authorized) = self.revoke {
            authorized.set(false);
        }
        std::future::ready(Ok(()))
    }
}

struct Active<'a>(&'a Cell<usize>);
impl<'a> Active<'a> {
    fn new(active: &'a Cell<usize>, peak: &Cell<usize>) -> Self {
        active.set(active.get() + 1);
        peak.set(peak.get().max(active.get()));
        Self(active)
    }
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[tokio::test]
async fn overlaps_http_reads_with_bounded_concurrency_and_delivers_each_file_once() {
    let active = Cell::new(0);
    let peak = Cell::new(0);
    let prepared = Cell::new(0);
    let mut received = std::collections::BTreeSet::new();
    download_files(
        &files(11),
        &mut Plans {
            prepared: Some(&prepared),
            ..Default::default()
        },
        |_, ()| async {
            let _active = Active::new(&active, &peak);
            tokio::task::yield_now().await;
            Ok(b"save".to_vec())
        },
        &|| true,
        |file, bytes| {
            assert_eq!(bytes, b"save");
            assert!(received.insert(file.name.clone()));
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(prepared.get(), 11);
    assert_eq!(received.len(), 11);
    assert_eq!(peak.get(), MAX_DOWNLOADS);
    assert_eq!(active.get(), 0);
}

#[tokio::test]
async fn large_files_are_not_started_together_and_dropping_cancels_every_reader() {
    let active = Cell::new(0);
    let peak = Cell::new(0);
    let prepared = Cell::new(0);
    for large in [false, true] {
        let mut files = files(8);
        if large {
            for file in &mut files {
                file.size = BATCH_BYTES as u32 + 1;
            }
        }
        peak.set(0);
        prepared.set(0);
        let mut plans = Plans {
            prepared: Some(&prepared),
            ..Default::default()
        };
        let operation = download_files(
            &files,
            &mut plans,
            |_, ()| async {
                let _active = Active::new(&active, &peak);
                std::future::pending::<()>().await;
                Ok(Vec::new())
            },
            &|| true,
            |_, _| panic!("cancelled read must not deliver bytes"),
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), operation)
                .await
                .is_err()
        );
        let expected = if large { 1 } else { MAX_DOWNLOADS };
        assert_eq!(peak.get(), expected);
        assert_eq!(prepared.get(), expected);
        assert_eq!(active.get(), 0);
    }
}

#[tokio::test]
async fn revocation_corruption_and_receiver_failure_stop_the_batch() {
    let authorized = Cell::new(true);
    let requests = Cell::new(0);
    let result = download_files(
        &files(8),
        &mut Plans {
            revoke: Some(&authorized),
            ..Default::default()
        },
        |_, ()| async {
            requests.set(requests.get() + 1);
            Ok(b"save".to_vec())
        },
        &|| authorized.get(),
        |_, _| panic!("revoked transfer"),
    )
    .await;
    assert!(matches!(result, Err(CloudError::Revoked)));
    assert_eq!(requests.get(), 0);

    let result = download_files(
        &files(8),
        &mut Plans::default(),
        |_, ()| async { Ok(b"bad!".to_vec()) },
        &|| true,
        |_, _| panic!("unverified bytes"),
    )
    .await;
    assert!(matches!(result, Err(CloudError::Integrity)));

    requests.set(0);
    let result = download_files(
        &files(8),
        &mut Plans {
            prepared: Some(&requests),
            ..Default::default()
        },
        |_, ()| async { Ok(b"save".to_vec()) },
        &|| true,
        |_, _| Err(CloudError::Storage),
    )
    .await;
    assert!(matches!(result, Err(CloudError::Storage)));
    assert_eq!(requests.get(), MAX_DOWNLOADS);
}
