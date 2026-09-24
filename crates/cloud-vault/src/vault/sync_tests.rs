//! Last Cloud sync: the moment local saves and Cloud saves were confirmed
//! identical. Capturing or choosing a version does not count.
use super::*;
use std::fs;

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

fn binding(directory: &std::path::Path) -> Bindings {
    Bindings::from([(
        "save".into(),
        SelectedFile {
            path: directory.join("save"),
            platforms: 7,
        },
    )])
}

#[test]
fn confirming_that_local_and_cloud_saves_are_equal_records_the_last_cloud_sync() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    let files = staged(&vault, b"same on both sides");
    vault.record_local(&files).unwrap();

    vault.confirm_equal(&files, &remote("1", &files)).unwrap();
    vault.confirm_equal(&files, &remote("2", &files)).unwrap();

    assert_eq!(vault.synced_at(), Some(1_000));
    assert_eq!(vault.syncs(), 2);
}

#[test]
fn choosing_cloud_while_the_archived_local_version_differs_is_not_a_sync() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    vault
        .record_local(&staged(&vault, b"exit version"))
        .unwrap();
    let cloud = staged(&vault, b"other device");

    vault.confirm_equal(&cloud, &remote("2", &cloud)).unwrap();

    assert_eq!(vault.baseline().unwrap().files, cloud);
    assert_eq!(vault.synced_at(), None);
}

#[test]
fn a_verified_upload_records_the_last_cloud_sync() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    let local = staged(&vault, b"new progress");
    let before = remote("1", &Manifest::default());
    let authorization = vault.authorize(scope().steam_id).unwrap();
    let id = vault
        .queue_upload(&local, &before, "fixture", authorization)
        .unwrap();
    let permit = vault.begin_upload(&id, authorization, &before).unwrap();
    assert_eq!(vault.synced_at(), None);

    vault
        .complete_upload(&permit, true, &remote("2", &local))
        .unwrap();

    assert_eq!(vault.synced_at(), Some(1_000));
}

#[test]
fn settling_an_uncertain_upload_is_a_sync_only_when_the_readback_matches() {
    for readback_matches in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let mut vault = open(root.path());
        let local = staged(&vault, b"new progress");
        let before = remote("1", &Manifest::default());
        let authorization = vault.authorize(scope().steam_id).unwrap();
        let id = vault
            .queue_upload(&local, &before, "fixture", authorization)
            .unwrap();
        vault.begin_upload(&id, authorization, &before).unwrap();
        let readback = if readback_matches {
            remote("2", &local)
        } else {
            remote("2", &staged(&vault, b"other device"))
        };

        vault
            .reconcile_pending(&id, authorization, true, &readback)
            .unwrap();

        assert_eq!(vault.synced_at(), readback_matches.then_some(1_000));
    }
}

#[test]
fn applying_a_newer_cloud_version_records_the_last_cloud_sync() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let bindings = binding(saves.path());
    fs::write(saves.path().join("save"), b"base").unwrap();
    let mut vault = open(root.path());
    let base = vault.capture(&bindings).unwrap();
    vault.confirm_equal(&base, &remote("1", &base)).unwrap();
    vault.set_clock(|| 2_000);
    let newer = remote("2", &staged(&vault, b"newer on another device"));

    vault
        .apply_download(&bindings, &newer, &newer, false)
        .unwrap();

    assert_eq!(
        fs::read(saves.path().join("save")).unwrap(),
        b"newer on another device"
    );
    assert_eq!(vault.synced_at(), Some(2_000));
}

#[test]
fn captures_local_choices_retries_and_restores_are_not_syncs() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let bindings = binding(saves.path());
    fs::write(saves.path().join("save"), b"base").unwrap();
    let mut vault = open(root.path());
    let base = vault.capture(&bindings).unwrap();
    let confirmed = remote("1", &base);
    vault.confirm_equal(&base, &confirmed).unwrap();
    vault.set_clock(|| 9_999);
    let authorization = vault.authorize(scope().steam_id).unwrap();

    fs::write(saves.path().join("save"), b"local edit").unwrap();
    let edited = vault.capture(&bindings).unwrap();
    let other = remote("2", &staged(&vault, b"other device"));
    vault.choose_local(&other, &other).unwrap();
    let id = vault
        .queue_upload(&edited, &other, "fixture", authorization)
        .unwrap();
    vault.recompare_pending(&id, authorization, &other).unwrap();
    let id = vault
        .queue_upload(&edited, &other, "fixture", authorization)
        .unwrap();
    vault
        .resolve_pending_conflict(&id, &other, &other, false)
        .unwrap();
    fs::remove_file(saves.path().join("save")).unwrap();
    vault.restore_local(&bindings).unwrap();

    assert_eq!(fs::read(saves.path().join("save")).unwrap(), b"local edit");
    assert_eq!(vault.synced_at(), Some(1_000));
    assert_eq!(vault.syncs(), 1);
}

#[test]
fn journals_without_a_last_cloud_sync_are_written_unchanged_and_a_recorded_one_survives() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path());
    let files = staged(&vault, b"save");
    vault.record_local(&files).unwrap();
    let path = vault.directory.join("state.json");
    let written: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(written.get("synced_at").is_none());
    drop(vault);

    let mut vault = open(root.path());
    assert_eq!(vault.synced_at(), None);
    vault.confirm_equal(&files, &remote("1", &files)).unwrap();
    drop(vault);

    let vault = Vault::open(root.path(), scope()).unwrap();
    assert_eq!(vault.synced_at(), Some(1_000));
}
