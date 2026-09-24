use cloud_vault::*;
use std::{
    collections::BTreeMap,
    fs,
    time::{Duration, Instant},
};

fn scope(account: u32, app: u32, cloud: u32) -> Scope {
    Scope {
        steam_id: 0x0110_0001_0000_0000 | account as u64,
        app_id: app,
        cloud_app_id: cloud,
    }
}

fn open(root: &std::path::Path, scope: Scope) -> Vault {
    let mut vault = Vault::open(root, scope).unwrap();
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    vault
}

fn manifest(value: Option<&[u8]>) -> Manifest {
    Manifest(
        value
            .map(|value| {
                (
                    "save.dat".into(),
                    FileVersion {
                        hash: ContentHash::of(value),
                        size: value.len() as u64,
                        platforms: 7,
                    },
                )
            })
            .into_iter()
            .collect(),
    )
}

fn remote(revision: &str, files: &Manifest) -> RemoteView {
    RemoteView {
        revision: revision.into(),
        files: files.clone(),
    }
}

fn stage(vault: &Vault, value: &[u8]) -> Manifest {
    Manifest(BTreeMap::from([(
        "save.dat".into(),
        vault.stage(value, 7).unwrap(),
    )]))
}

#[test]
fn compares_content_and_deletions_without_timestamps() {
    let old = manifest(Some(b"old"));
    let local = manifest(Some(b"local"));
    let other = manifest(Some(b"remote"));
    let empty = Manifest::default();
    let baseline = remote("10", &old);
    for (local, cloud, expected) in [
        (&old, &old, Decision::Converged),
        (&local, &old, Decision::KeepLocal),
        (&old, &other, Decision::Download),
        (&local, &other, Decision::Conflict),
        (&local, &local, Decision::Converged),
        (&empty, &old, Decision::KeepLocal),
        (&old, &empty, Decision::Download),
        (&empty, &other, Decision::Conflict),
    ] {
        assert_eq!(
            reconcile(Some(&baseline), local, &remote("11", cloud), None),
            expected
        );
    }
    assert_eq!(
        reconcile(None, &local, &remote("1", &other), None),
        Decision::Conflict
    );
    assert_eq!(
        reconcile(None, &empty, &remote("1", &other), None),
        Decision::Download
    );
}

#[test]
fn local_conflict_choice_is_protected_even_when_it_equals_the_old_baseline() {
    let old = manifest(Some(b"old"));
    let new = manifest(Some(b"new"));
    let baseline = remote("10", &old);
    let current = remote("20", &new);
    let intent = LocalIntent {
        against: current.clone(),
    };
    assert_eq!(
        reconcile(Some(&baseline), &old, &current, Some(&intent)),
        Decision::KeepLocal
    );
    assert_eq!(
        reconcile(Some(&baseline), &old, &remote("21", &new), Some(&intent)),
        Decision::Conflict
    );
}

#[test]
fn pending_bytes_outlive_the_latest_local_snapshot_and_require_acknowledgement_and_readback() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path(), scope(1, 10, 42));
    let desired = stage(&vault, b"pending");
    let empty = remote("1", &Manifest::default());
    let auth = vault.authorize(scope(1, 10, 42).steam_id).unwrap();
    let id = vault
        .queue_upload(&desired, &empty, "fixture-v1", auth)
        .unwrap();
    for n in 0..6 {
        let files = stage(&vault, &[n]);
        vault.record_local(&files).unwrap();
    }
    let permit = vault.begin_upload(&id, auth, &empty).unwrap();
    assert!(vault.pending().unwrap().may_have_committed);
    assert!(
        vault
            .complete_upload(&permit, false, &remote("2", &desired))
            .is_err()
    );
    assert!(vault.complete_upload(&permit, true, &empty).is_err());
    drop(vault);
    let mut vault = open(root.path(), scope(1, 10, 42));
    assert_eq!(
        vault.read_content(&desired.0["save.dat"]).unwrap(),
        b"pending"
    );
    assert!(vault.begin_upload(&id, auth, &empty).is_err());
    assert!(
        vault
            .reconcile_pending(&id, auth, false, &remote("2", &desired))
            .is_err()
    );
    vault
        .reconcile_pending(&id, auth, true, &remote("2", &desired))
        .unwrap();
    assert!(vault.pending().is_none());
    assert_eq!(
        vault.decision(&desired, &remote("2", &desired)).unwrap(),
        Decision::Converged
    );
}

#[test]
fn signout_fences_an_inflight_completion_and_never_rebinds_it_to_another_account() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path(), scope(1, 10, 42));
    let desired = stage(&vault, b"owned by A");
    let empty = remote("1", &Manifest::default());
    let auth = vault.authorize(scope(1, 10, 42).steam_id).unwrap();
    let id = vault
        .queue_upload(&desired, &empty, "fixture-v1", auth)
        .unwrap();
    let permit = vault.begin_upload(&id, auth, &empty).unwrap();
    vault.revoke().unwrap();
    assert!(matches!(
        vault.complete_upload(&permit, true, &remote("2", &desired)),
        Err(Error::Authentication)
    ));
    assert!(vault.authorize(scope(2, 10, 42).steam_id).is_err());
    drop(vault);
    let mut vault = open(root.path(), scope(1, 10, 42));
    assert!(
        vault
            .reconcile_pending(&id, auth, true, &remote("2", &desired))
            .is_err()
    );
    let fresh = vault.authorize(scope(1, 10, 42).steam_id).unwrap();
    vault
        .reconcile_pending(&id, fresh, true, &remote("2", &desired))
        .unwrap();
}

#[test]
fn missing_receipt_reopens_for_a_new_comparison_without_changing_the_baseline() {
    for outcome in ["unchanged", "committed", "other-device", "deleted"] {
        let root = tempfile::tempdir().unwrap();
        let account = scope(1, 10, 42);
        let mut vault = open(root.path(), account);
        let base = remote("1", &stage(&vault, b"original"));
        vault.confirm_equal(&base.files, &base).unwrap();
        let desired = if outcome == "deleted" {
            Manifest::default()
        } else {
            stage(&vault, b"new progress")
        };
        vault.record_local(&desired).unwrap();
        let auth = vault.authorize(account.steam_id).unwrap();
        let old = vault
            .queue_upload(&desired, &base, "fixture", auth)
            .unwrap();
        let old_permit = vault.begin_upload(&old, auth, &base).unwrap();
        drop(vault);

        let mut vault = open(root.path(), account);
        let observed = match outcome {
            "committed" => remote("2", &desired),
            "other-device" => remote("2", &stage(&vault, b"deck progress")),
            _ => base.clone(),
        };
        vault.recompare_pending(&old, auth, &observed).unwrap();
        assert!(vault.pending().is_none());
        assert_eq!(vault.baseline(), Some(&base));
        assert_eq!(vault.local(), Some(&desired));
        // A late acknowledgement from the retired local attempt is stale.
        assert!(
            vault
                .complete_upload(&old_permit, true, &remote("3", &desired))
                .is_err()
        );
        match outcome {
            "committed" => {
                assert_eq!(
                    vault.decision(&desired, &observed).unwrap(),
                    Decision::Converged
                );
                vault.confirm_equal(&desired, &observed).unwrap();
            }
            "other-device" => {
                assert_eq!(
                    vault.decision(&desired, &observed).unwrap(),
                    Decision::Conflict
                );
                assert!(
                    vault
                        .queue_upload(&desired, &observed, "fixture", auth)
                        .is_err()
                );
                vault.choose_local(&observed, &observed).unwrap();
            }
            _ => {
                assert_eq!(
                    vault.decision(&desired, &observed).unwrap(),
                    Decision::KeepLocal
                );
                let fresh = vault
                    .queue_upload(&desired, &observed, "fixture", auth)
                    .unwrap();
                assert_ne!(fresh, old);
                let permit = vault.begin_upload(&fresh, auth, &observed).unwrap();
                assert!(
                    vault
                        .complete_upload(&permit, false, &remote("3", &desired))
                        .is_err()
                );
                vault
                    .complete_upload(&permit, true, &remote("3", &desired))
                    .unwrap();
                assert!(vault.pending().is_none());
            }
        }
    }
}

#[test]
fn recompare_keeps_account_preferences_game_and_verified_content_guards() {
    let root = tempfile::tempdir().unwrap();
    let account = scope(1, 10, 42);
    let mut vault = open(root.path(), account);
    let desired = stage(&vault, b"local");
    let base = remote("1", &Manifest::default());
    let auth = vault.authorize(account.steam_id).unwrap();
    let id = vault
        .queue_upload(&desired, &base, "fixture", auth)
        .unwrap();
    vault.begin_upload(&id, auth, &base).unwrap();
    assert!(
        vault
            .recompare_pending("wrong-operation", auth, &base)
            .is_err()
    );
    let unavailable_bytes = remote("2", &manifest(Some(b"not downloaded")));
    assert!(
        vault
            .recompare_pending(&id, auth, &unavailable_bytes)
            .is_err()
    );
    vault.set_preferences(Preferences::default());
    assert!(matches!(
        vault.recompare_pending(&id, auth, &base),
        Err(Error::Preferences)
    ));
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let game = "00000000-0000-4000-8000-000000000001";
    vault.game_started(game).unwrap();
    assert!(matches!(
        vault.recompare_pending(&id, auth, &base),
        Err(Error::Busy)
    ));
    vault.game_exited(game).unwrap();
    vault.revoke().unwrap();
    assert!(matches!(
        vault.recompare_pending(&id, auth, &base),
        Err(Error::Authentication)
    ));
    assert_eq!(vault.pending().unwrap().id, id);
    assert_eq!(
        vault.read_content(&desired.0["save.dat"]).unwrap(),
        b"local"
    );
}

#[test]
fn retry_retains_an_explicit_local_choice_and_rejects_new_remote_divergence() {
    let root = tempfile::tempdir().unwrap();
    let account = scope(1, 10, 42);
    let mut vault = open(root.path(), account);
    let local = stage(&vault, b"deliberate local choice");
    let shown = remote("2", &stage(&vault, b"cloud"));
    vault.choose_local(&shown, &shown).unwrap();
    let auth = vault.authorize(account.steam_id).unwrap();
    let id = vault.queue_upload(&local, &shown, "fixture", auth).unwrap();
    vault.begin_upload(&id, auth, &shown).unwrap();
    vault.recompare_pending(&id, auth, &shown).unwrap();
    assert_eq!(vault.decision(&local, &shown).unwrap(), Decision::KeepLocal);
    let newer = remote("3", &stage(&vault, b"new deck progress"));
    assert_eq!(vault.decision(&local, &newer).unwrap(), Decision::Conflict);
}

#[test]
fn remote_change_after_local_choice_requires_a_new_decision() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path(), scope(1, 10, 42));
    let local = stage(&vault, b"local");
    let shown = remote("1", &manifest(Some(b"remote")));
    let current = remote("2", &manifest(Some(b"new remote")));
    assert!(vault.choose_local(&shown, &current).is_err());
    vault.choose_local(&shown, &shown).unwrap();
    let auth = vault.authorize(scope(1, 10, 42).steam_id).unwrap();
    let id = vault
        .queue_upload(&local, &shown, "fixture-v1", auth)
        .unwrap();
    assert!(vault.begin_upload(&id, auth, &current).is_err());
    assert_eq!(vault.pending().unwrap().id, id);
}

#[test]
fn journal_and_blob_corruption_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path(), scope(1, 10, 42));
    let files = stage(&vault, b"good");
    vault.record_local(&files).unwrap();
    let directory = root
        .path()
        .join(scope(1, 10, 42).steam_id.to_string())
        .join("42");
    fs::write(
        directory
            .join("blobs")
            .join(files.0["save.dat"].hash.as_str()),
        b"evil",
    )
    .unwrap();
    assert!(matches!(
        vault.read_content(&files.0["save.dat"]),
        Err(Error::Integrity)
    ));
    drop(vault);
    fs::write(directory.join("state.json"), b"{truncated").unwrap();
    assert!(Vault::open(root.path(), scope(1, 10, 42)).is_err());
}

#[test]
fn game_guard_survives_signout_and_reopening_until_the_same_session_exits() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path(), scope(1, 10, 42));
    let session = "00000000-0000-4000-8000-000000000001";
    vault.game_started(session).unwrap();
    vault.revoke().unwrap();
    assert!(matches!(vault.capture(&Bindings::new()), Err(Error::Busy)));
    drop(vault);
    let mut vault = open(root.path(), scope(1, 10, 42));
    assert!(vault.game_exited("different session").is_err());
    assert!(matches!(vault.capture(&Bindings::new()), Err(Error::Busy)));
    vault.game_exited(session).unwrap();
    vault.capture(&Bindings::new()).unwrap();
}

#[test]
fn retries_make_three_attempts_with_one_then_two_seconds_between_failures() {
    let start = Instant::now();
    let mut budget = RetryBudget::new(start);
    assert_eq!(
        budget.poll(start),
        Retry::Attempt {
            remaining: Duration::from_secs(120)
        }
    );
    budget.failed(start + Duration::from_secs(2), false);
    for seconds in [3, 5] {
        assert_eq!(
            budget.poll(start + Duration::from_secs(seconds - 1)),
            Retry::Wait(Duration::from_secs(1))
        );
        assert_eq!(
            budget.poll(start + Duration::from_secs(seconds)),
            Retry::Attempt {
                remaining: Duration::from_secs(120 - seconds)
            }
        );
        budget.failed(start + Duration::from_secs(seconds), false);
    }
    assert_eq!(budget.poll(start + Duration::from_secs(5)), Retry::Defer);
}

#[test]
fn slow_requests_keep_the_retry_delay_and_never_extend_the_deadline() {
    let start = Instant::now();
    let mut budget = RetryBudget::new(start);
    budget.poll(start);
    budget.failed(start, false);
    assert!(matches!(
        budget.poll(start + Duration::from_secs(40)),
        Retry::Attempt { .. }
    ));
    budget.failed(start + Duration::from_secs(40), false);
    assert_eq!(
        budget.poll(start + Duration::from_secs(40)),
        Retry::Wait(Duration::from_secs(2))
    );
    assert_eq!(budget.poll(start + Duration::from_secs(120)), Retry::Defer);
    let mut auth_failure = RetryBudget::new(start);
    auth_failure.poll(start);
    auth_failure.failed(start, true);
    assert_eq!(auth_failure.poll(start), Retry::Defer);
}

#[test]
fn unavailable_or_disabled_preferences_never_admit_a_transfer() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = Vault::open(root.path(), scope(1, 10, 42)).unwrap();
    let desired = stage(&vault, b"local");
    let empty = remote("1", &Manifest::default());
    let auth = vault.authorize(scope(1, 10, 42).steam_id).unwrap();
    for preferences in [
        Preferences::default(),
        Preferences {
            account_enabled: Some(false),
            game_enabled: Some(true),
        },
        Preferences {
            account_enabled: Some(true),
            game_enabled: None,
        },
    ] {
        vault.set_preferences(preferences);
        assert!(matches!(
            vault.queue_upload(&desired, &empty, "fixture-v1", auth),
            Err(Error::Preferences)
        ));
    }
    assert!(vault.pending().is_none());
}

#[test]
fn revocation_fences_other_cloud_stores_for_the_same_account() {
    let root = tempfile::tempdir().unwrap();
    let mut first = open(root.path(), scope(1, 10, 42));
    let files = stage(&first, b"A save");
    let empty = remote("1", &Manifest::default());
    let auth = first.authorize(scope(1, 10, 42).steam_id).unwrap();
    let id = first
        .queue_upload(&files, &empty, "fixture-v1", auth)
        .unwrap();
    drop(first);
    let mut other = open(root.path(), scope(1, 20, 50));
    other.revoke().unwrap();
    drop(other);
    let mut first = open(root.path(), scope(1, 10, 42));
    assert!(matches!(
        first.begin_upload(&id, auth, &empty),
        Err(Error::Authentication)
    ));
}

#[test]
fn failed_journal_write_requires_reopening_before_more_decisions() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path(), scope(1, 10, 42));
    let files = stage(&vault, b"save");
    vault.record_local(&files).unwrap();
    let state_path = root
        .path()
        .join(scope(1, 10, 42).steam_id.to_string())
        .join("42/state.json");
    let original = fs::read(&state_path).unwrap();
    let original_permissions = fs::metadata(&state_path).unwrap().permissions();
    fs::remove_file(&state_path).unwrap();
    std::os::unix::fs::symlink("missing", &state_path).unwrap();
    assert!(vault.record_local(&files).is_err());
    fs::remove_file(&state_path).unwrap();
    fs::write(&state_path, original).unwrap();
    fs::set_permissions(&state_path, original_permissions).unwrap();
    assert!(matches!(vault.record_local(&files), Err(Error::Recovery)));
    drop(vault);
    let mut vault = open(root.path(), scope(1, 10, 42));
    vault.record_local(&files).unwrap();
}

#[test]
fn garbage_collection_retains_pending_bytes_and_conflict_choice_is_fenced() {
    let root = tempfile::tempdir().unwrap();
    let mut vault = open(root.path(), scope(1, 10, 42));
    let desired = stage(&vault, b"pending");
    let initial = remote("1", &Manifest::default());
    let auth = vault.authorize(scope(1, 10, 42).steam_id).unwrap();
    let id = vault
        .queue_upload(&desired, &initial, "fixture-v1", auth)
        .unwrap();
    for n in 0..6 {
        let files = stage(&vault, &[n]);
        vault.record_local(&files).unwrap();
    }
    assert_eq!(vault.collect_garbage(&[]).unwrap(), 5);
    assert_eq!(
        vault.read_content(&desired.0["save.dat"]).unwrap(),
        b"pending"
    );
    let current = remote("2", &manifest(Some(b"other device")));
    assert!(
        vault
            .resolve_pending_conflict(&id, &initial, &current, true)
            .is_err()
    );
    vault
        .resolve_pending_conflict(&id, &current, &current, true)
        .unwrap();
    let permit = vault.begin_upload(&id, auth, &current).unwrap();
    assert!(
        vault
            .resolve_pending_conflict(&id, &current, &current, false)
            .is_err()
    );
    vault
        .complete_upload(&permit, true, &remote("3", &desired))
        .unwrap();
    assert!(vault.pending().is_none());
}
