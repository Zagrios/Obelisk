use super::*;

fn scope(account: u32) -> Scope {
    Scope {
        steam_id: 0x0110_0001_0000_0000 | account as u64,
        app_id: 10,
        cloud_app_id: 42,
    }
}

fn bindings(root: &Path) -> Bindings {
    ["one", "two", "deleted"]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                SelectedFile {
                    path: root.join(name),
                    platforms: 7,
                },
            )
        })
        .collect()
}

fn fixture(vault: &Vault) -> Manifest {
    Manifest(
        [
            ("one", b"new one".as_slice()),
            ("two", b"new two".as_slice()),
        ]
        .into_iter()
        .map(|(name, bytes)| (name.into(), vault.stage(bytes, 7).unwrap()))
        .collect(),
    )
}

#[test]
fn interrupted_multifile_download_recovers_with_its_preimage() {
    for interrupted_after in 0..=3 {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(root.path(), scope(1)).unwrap();
        let bindings = bindings(saves.path());
        for file in bindings.values() {
            fs::write(&file.path, b"old").unwrap();
        }
        let before = vault.capture(&bindings).unwrap();
        let after = fixture(&vault);
        let remote = RemoteView {
            revision: "known remote".into(),
            files: after.clone(),
        };
        vault
            .prepare_apply(
                &bindings,
                after.clone(),
                Effect::Download {
                    remote: remote.clone(),
                },
            )
            .unwrap();
        assert!(matches!(
            vault.recover_after(Some(interrupted_after)),
            Err(Error::Recovery)
        ));
        drop(vault);
        let mut vault = Vault::open(root.path(), scope(1)).unwrap();
        assert!(vault.needs_recovery());
        assert!(matches!(
            vault.game_started("00000000-0000-4000-8000-000000000001"),
            Err(Error::Recovery)
        ));
        vault.recover_local().unwrap();
        assert_eq!(vault.capture(&bindings).unwrap(), after);
        assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"new one");
        assert!(!saves.path().join("deleted").exists());
        assert_eq!(vault.local(), Some(&after));
        assert_eq!(vault.read_content(&before.0["one"]).unwrap(), b"old");
        assert_eq!(
            vault.decision(&after, &remote).unwrap(),
            Decision::Converged
        );
        assert!(vault.pending().is_none()); // Download itself queued no upload.
    }
}

#[test]
fn external_change_during_interrupted_apply_is_preserved_and_blocks_recovery() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let mut vault = Vault::open(root.path(), scope(1)).unwrap();
    let bindings = bindings(saves.path());
    for file in bindings.values() {
        fs::write(&file.path, b"old").unwrap();
    }
    let after = fixture(&vault);
    let remote = RemoteView {
        revision: "1".into(),
        files: after.clone(),
    };
    vault
        .prepare_apply(&bindings, after, Effect::Download { remote })
        .unwrap();
    assert!(vault.recover_after(Some(1)).is_err());
    fs::write(saves.path().join("two"), b"external edit").unwrap();
    drop(vault);
    let mut vault = Vault::open(root.path(), scope(1)).unwrap();
    assert!(matches!(vault.recover_local(), Err(Error::Stale)));
    assert_eq!(
        fs::read(saves.path().join("two")).unwrap(),
        b"external edit"
    );
    assert!(vault.needs_recovery());
}

#[test]
fn adopts_unowned_paths_but_never_reads_them_under_another_account() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let bindings = bindings(saves.path());
    fs::write(saves.path().join("one"), b"A private save").unwrap();
    let mut first = Vault::open(root.path(), scope(1)).unwrap();
    first.capture(&bindings).unwrap();
    drop(first);
    let mut second = Vault::open(root.path(), scope(2)).unwrap();
    assert!(matches!(
        second.capture(&bindings),
        Err(Error::Authentication)
    ));
    assert!(second.local().is_none());
}

#[test]
fn symlinks_hardlinks_case_aliases_and_unmapped_remote_names_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let mut vault = Vault::open(root.path(), scope(1)).unwrap();
    let mut bindings = bindings(saves.path());
    fs::write(saves.path().join("one"), b"private").unwrap();
    std::os::unix::fs::symlink(saves.path().join("one"), saves.path().join("two")).unwrap();
    assert!(vault.capture(&bindings).is_err());
    fs::remove_file(saves.path().join("two")).unwrap();
    fs::hard_link(saves.path().join("one"), saves.path().join("two")).unwrap();
    assert!(vault.capture(&bindings).is_err());
    fs::remove_file(saves.path().join("two")).unwrap();
    bindings.insert(
        "alias".into(),
        SelectedFile {
            path: saves.path().join("ONE"),
            platforms: 7,
        },
    );
    assert!(vault.capture(&bindings).is_err());
    bindings.remove("alias");
    let files = Manifest(BTreeMap::from([(
        "../../escape".into(),
        vault.stage(b"bad", 7).unwrap(),
    )]));
    let remote = RemoteView {
        revision: "1".into(),
        files: files.clone(),
    };
    assert!(
        vault
            .prepare_apply(&bindings, files, Effect::Download { remote })
            .is_err()
    );
    assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"private");
}

#[test]
fn ownership_survives_deletion_and_unicode_filename_aliases() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let composed = saves.path().join("caf\u{e9}.save");
    let decomposed = saves.path().join("cafe\u{301}.save");
    fs::write(&composed, b"A private save").unwrap();
    let aliases = decomposed.exists();
    let first_binding = BTreeMap::from([(
        "save".into(),
        SelectedFile {
            path: composed.clone(),
            platforms: 7,
        },
    )]);
    let mut first = Vault::open(root.path(), scope(1)).unwrap();
    first.capture(&first_binding).unwrap();
    drop(first);
    fs::remove_file(&composed).unwrap();
    let second_binding = BTreeMap::from([(
        "save".into(),
        SelectedFile {
            path: decomposed,
            platforms: 7,
        },
    )]);
    let mut second = Vault::open(root.path(), scope(2)).unwrap();
    let result = second.capture(&second_binding);
    if aliases {
        assert!(matches!(result, Err(Error::Authentication)));
    } else {
        assert!(result.unwrap().0.is_empty());
    }
}

#[test]
fn local_edit_after_download_decision_requires_another_comparison() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let mut vault = Vault::open(root.path(), scope(1)).unwrap();
    let bindings = bindings(saves.path());
    fs::write(saves.path().join("one"), b"base").unwrap();
    let observed = vault.capture(&bindings).unwrap();
    let after = fixture(&vault);
    fs::write(saves.path().join("one"), b"new local edit").unwrap();
    let remote = RemoteView {
        revision: "2".into(),
        files: after.clone(),
    };
    let guard = vault.owners_guard().unwrap();
    let result = vault.prepare_checked_apply(
        &guard,
        &bindings,
        after,
        Effect::Download { remote },
        Some(&observed),
    );
    assert!(matches!(result, Err(Error::Stale)));
    assert_eq!(
        fs::read(saves.path().join("one")).unwrap(),
        b"new local edit"
    );
    assert!(!vault.needs_recovery());
}

#[test]
fn interrupted_current_snapshot_restoration_retains_the_baseline_and_rejects_external_edits() {
    for interrupted_after in 0..=3 {
        let root = tempfile::tempdir().unwrap();
        let saves = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(root.path(), scope(1)).unwrap();
        let bindings = bindings(saves.path());
        let desired = fixture(&vault);
        vault.record_local(&desired).unwrap();
        let remote = RemoteView {
            revision: "last-confirmed".into(),
            files: Manifest::default(),
        };
        vault.confirm_equal(&Manifest::default(), &remote).unwrap();
        for file in bindings.values() {
            fs::write(&file.path, b"reinstalled preimage").unwrap();
        }
        let guard = vault.owners_guard().unwrap();
        vault.prepare_local_restore(&guard, &bindings).unwrap();
        assert_eq!(vault.local(), Some(&desired));
        assert!(matches!(
            vault.recover_with(&guard, Some(interrupted_after)),
            Err(Error::Recovery)
        ));
        drop(guard);
        drop(vault);
        let mut vault = Vault::open(root.path(), scope(1)).unwrap();
        if interrupted_after == 0 {
            fs::write(saves.path().join("one"), b"unrelated edit").unwrap();
            assert!(vault.recover_local().is_err());
            assert_eq!(
                fs::read(saves.path().join("one")).unwrap(),
                b"unrelated edit"
            );
            assert_eq!(vault.baseline(), Some(&remote));
            continue;
        }
        vault.recover_local().unwrap();
        assert_eq!(vault.local(), Some(&desired));
        assert_eq!(vault.baseline(), Some(&remote));
        assert_eq!(fs::read(saves.path().join("one")).unwrap(), b"new one");
        assert_eq!(fs::read(saves.path().join("two")).unwrap(), b"new two");
        assert!(!saves.path().join("deleted").exists());
    }
}
