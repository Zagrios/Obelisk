//! One upload: queued, recorded by the storefront, sent, then counted only
//! with a readback that holds the saves.
use super::*;

fn scope() -> Scope {
    Scope {
        steam_id: 0x0110_0001_0000_0001,
        app_id: 10,
        cloud_app_id: 42,
    }
}

fn open(root: &std::path::Path) -> Vault {
    let mut vault = Vault::open(root, scope()).unwrap();
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    vault.set_clock(|| 1_000);
    vault
}

fn staged(vault: &Vault, bytes: &[u8]) -> Manifest {
    Manifest(BTreeMap::from([(
        "save".into(),
        vault.stage(bytes, 7).unwrap(),
    )]))
}

fn remote(revision: &str, files: &Manifest) -> RemoteView {
    RemoteView {
        revision: revision.into(),
        files: files.clone(),
    }
}

/// The sends below never wait, so polling once completes them.
fn now<T>(future: impl std::future::Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => value,
        std::task::Poll::Pending => panic!("the fixture send never waits"),
    }
}

fn upload(
    vault: &mut Vault,
    local: &Manifest,
    before: &RemoteView,
    readback: Result<RemoteView>,
) -> Result<&'static str> {
    now(vault.upload(
        scope().steam_id,
        local,
        before,
        "fixture",
        || Ok(()),
        async |_| readback.map(|view| (view, "sent")),
    ))
}

#[test]
fn an_upload_is_recorded_before_it_may_commit_and_counts_with_its_readback() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    let local = staged(&vault, b"new progress");
    let before = remote("1", &Manifest::default());
    let mut recorded = false;

    let sent = now(vault.upload(
        scope().steam_id,
        &local,
        &before,
        "fixture",
        || {
            recorded = true;
            Ok::<_, Error>(())
        },
        async |vault| {
            assert!(
                vault
                    .pending()
                    .is_some_and(|pending| pending.may_have_committed),
                "the send happens after the upload may commit"
            );
            Ok((remote("2", &local), "sent"))
        },
    ))
    .unwrap();

    assert!(recorded);
    assert_eq!(sent, "sent");
    assert!(vault.pending().is_none());
    assert_eq!(vault.baseline(), Some(&remote("2", &local)));
    assert_eq!(vault.synced_at(), Some(1_000));
}

#[test]
fn a_failed_record_stops_the_upload_before_it_may_commit() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    let local = staged(&vault, b"new progress");
    let before = remote("1", &Manifest::default());

    let result = now(vault.upload(
        scope().steam_id,
        &local,
        &before,
        "fixture",
        || Err(Error::Busy),
        async |_| -> Result<(RemoteView, ())> { panic!("nothing is sent without its record") },
    ));

    assert!(matches!(result, Err(Error::Busy)));
    assert!(
        vault
            .pending()
            .is_some_and(|pending| !pending.may_have_committed)
    );
}

#[test]
fn an_upload_without_a_matching_readback_stays_pending() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    let local = staged(&vault, b"new progress");
    let before = remote("1", &Manifest::default());
    let other = remote("2", &staged(&vault, b"other device"));

    assert!(matches!(
        upload(&mut vault, &local, &before, Ok(other)),
        Err(Error::Stale)
    ));
    assert!(
        vault
            .pending()
            .is_some_and(|pending| pending.may_have_committed)
    );

    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    let local = staged(&vault, b"new progress");
    assert!(matches!(
        upload(
            &mut vault,
            &local,
            &before,
            Err(Error::Io(std::io::ErrorKind::TimedOut.into()))
        ),
        Err(Error::Io(_))
    ));
    assert!(
        vault
            .pending()
            .is_some_and(|pending| pending.may_have_committed)
    );
    assert_eq!(vault.synced_at(), None);
}

#[test]
fn settling_counts_a_settled_upload_and_otherwise_leaves_a_fresh_comparison() {
    for settled in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let mut vault = open(root.path());
        let local = staged(&vault, b"new progress");
        let before = remote("1", &Manifest::default());
        let _ = upload(&mut vault, &local, &before, Err(Error::Stale));
        let current = remote("2", &local);

        vault
            .settle_pending(scope().steam_id, &current, settled)
            .unwrap();

        assert!(vault.pending().is_none());
        assert_eq!(vault.baseline(), settled.then_some(&current));
        assert_eq!(vault.synced_at(), settled.then_some(1_000));
    }
}

#[test]
fn settling_without_an_earlier_upload_needs_recovery() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    assert!(matches!(
        vault.settle_pending(scope().steam_id, &remote("1", &Manifest::default()), true),
        Err(Error::Recovery)
    ));
}
