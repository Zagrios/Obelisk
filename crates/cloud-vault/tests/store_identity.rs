use cloud_vault::*;
use std::{collections::BTreeSet, fs, path::Path};

const ACCOUNT_A: &str = "0123456789abcdef0123456789abcdef";
const ACCOUNT_B: &str = "abcdef0123456789abcdef0123456789";

fn steam() -> Scope {
    Scope {
        steam_id: 0x0110_0001_0000_0001,
        app_id: 10,
        cloud_app_id: 42,
    }
}

fn epic(account: &str, namespace: &str, app: &str) -> EpicScope {
    EpicScope {
        account_id: account.into(),
        namespace: namespace.into(),
        app_name: app.into(),
    }
}

fn open(root: &Path, scope: impl Into<StoreScope>) -> Vault {
    let mut vault = Vault::open(root, scope).unwrap();
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    vault
}

fn staged(vault: &Vault, bytes: &[u8]) -> Manifest {
    Manifest([(String::from("save.dat"), vault.stage(bytes, 2).unwrap())].into())
}

fn remote(files: &Manifest) -> RemoteView {
    RemoteView {
        revision: "fixture-view".into(),
        files: files.clone(),
    }
}

#[test]
fn existing_steam_journals_roundtrip_without_migration_or_path_changes() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let scope = steam();
    let directory = root.path().join(scope.steam_id.to_string()).join("42");
    fs::create_dir_all(&directory).unwrap();
    // This is the original Steam schema-2 shape, independent of StoreScope's serializer.
    let original = format!(
        r#"{{"schema":2,"scope":{{"steam_id":{0},"app_id":10,"cloud_app_id":42}},"baseline":null,"intent":null,"local":{{}},"pending":{{"id":"00000000-0000-4000-8000-000000000001","scope":{{"steam_id":{0},"app_id":11,"cloud_app_id":42}},"adapter":"steam-fixture","desired":{{}},"against":{{"revision":"known","files":{{}}}},"may_have_committed":true}},"applying":null,"game":"00000000-0000-4000-8000-000000000002"}}"#,
        scope.steam_id,
    );
    fs::write(directory.join("state.json"), &original).unwrap();
    fs::set_permissions(
        directory.join("state.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut vault = open(root.path(), scope);
    assert_eq!(
        StoreScope::from(scope).directory(root.path()).unwrap(),
        directory
    );
    assert_eq!(
        fs::read_to_string(directory.join("state.json")).unwrap(),
        original
    );
    assert!(vault.pending().unwrap().may_have_committed);
    assert_eq!(
        vault.pending().unwrap().scope,
        StoreScope::Steam(Scope {
            app_id: 11,
            ..scope
        })
    );
    vault
        .game_exited("00000000-0000-4000-8000-000000000002")
        .unwrap();
    vault.record_local(&Manifest::default()).unwrap();
    assert_eq!(
        fs::read_to_string(directory.join("state.json")).unwrap(),
        original.replace(
            r#""game":"00000000-0000-4000-8000-000000000002""#,
            r#""game":null"#
        )
    );
    vault.revoke().unwrap();
    let authorization = directory.parent().unwrap().join("authorization.json");
    assert_eq!(fs::read_to_string(authorization).unwrap(), "1");
    assert!(!root.path().join("steam").exists());
    assert!(!root.path().join("epic").exists());
}

#[test]
fn epic_storage_separates_accounts_namespaces_apps_case_and_tuple_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let scopes = [
        epic(ACCOUNT_A, "namespace", "game"),
        epic(ACCOUNT_B, "namespace", "game"),
        epic(ACCOUNT_A, "other", "game"),
        epic(ACCOUNT_A, "namespace", "Game"),
        epic(ACCOUNT_A, "a.b", "c"),
        epic(ACCOUNT_A, "a", "b.c"),
    ];
    let mut paths = BTreeSet::new();
    for (index, scope) in scopes.iter().enumerate() {
        let store = StoreScope::from(scope.clone());
        let path = store.directory(root.path()).unwrap();
        assert!(path.starts_with(root.path().join("epic").join(&scope.account_id)));
        assert!(paths.insert(path.clone()));
        let mut vault = open(root.path(), scope.clone());
        let files = staged(&vault, &[index as u8]);
        vault.record_local(&files).unwrap();
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(path.join("state.json")).unwrap()).unwrap();
        assert_eq!(persisted["scope"]["account_id"], scope.account_id);
        assert_eq!(persisted["scope"]["namespace"], scope.namespace);
        assert_eq!(persisted["scope"]["app_name"], scope.app_name);
        assert!(persisted["scope"].get("steam_id").is_none());
    }
    assert!(paths.insert(StoreScope::from(steam()).directory(root.path()).unwrap()));
    for (index, scope) in scopes.into_iter().enumerate() {
        let vault = open(root.path(), scope);
        assert_eq!(
            vault
                .read_content(&vault.local().unwrap().0["save.dat"])
                .unwrap(),
            [index as u8]
        );
    }
}

#[test]
fn epic_revocation_fences_the_account_across_games_without_revoking_other_accounts() {
    let root = tempfile::tempdir().unwrap();
    let original_scope = epic(ACCOUNT_A, "namespace", "game");
    let mut vault = open(root.path(), original_scope.clone());
    let files = staged(&vault, b"pending Epic save");
    let empty = remote(&Manifest::default());
    assert!(matches!(
        vault.authorize(ACCOUNT_B),
        Err(Error::Authentication)
    ));
    assert!(matches!(
        vault.authorize(steam().steam_id),
        Err(Error::Authentication)
    ));
    let authorization = vault.authorize(ACCOUNT_A).unwrap();
    let id = vault
        .queue_upload(&files, &empty, "epic", authorization)
        .unwrap();
    let permit = vault.begin_upload(&id, authorization, &empty).unwrap();
    drop(vault);
    open(
        root.path(),
        epic(ACCOUNT_A, "other-namespace", "other-game"),
    )
    .revoke()
    .unwrap();
    let mut vault = open(root.path(), original_scope.clone());
    assert!(matches!(
        vault.complete_upload(&permit, true, &remote(&files)),
        Err(Error::Authentication)
    ));
    let fresh = vault.authorize(ACCOUNT_A).unwrap();
    drop(vault);
    open(root.path(), epic(ACCOUNT_B, "namespace", "game"))
        .revoke()
        .unwrap();
    open(root.path(), steam()).revoke().unwrap();
    let mut vault = open(root.path(), original_scope);
    vault
        .reconcile_pending(&id, fresh, true, &remote(&files))
        .unwrap();
    assert!(vault.pending().is_none());
    assert_eq!(vault.baseline().unwrap().files, files);
}

#[test]
fn shared_save_projection_preserves_both_steam_and_epic_account_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let path = saves.path().join("save.dat");
    fs::write(&path, b"Steam save").unwrap();
    let bindings = Bindings::from([(
        "save.dat".into(),
        SelectedFile {
            path: path.clone(),
            platforms: 2,
        },
    )]);
    let mut vault = open(root.path(), steam());
    vault.activate(&bindings).unwrap();
    let steam_files = vault.capture(&bindings).unwrap();
    vault.record_local(&steam_files).unwrap();
    drop(vault);

    let epic_scope = epic(ACCOUNT_A, "namespace", "game");
    let mut vault = open(root.path(), epic_scope.clone());
    assert!(vault.activate(&bindings).unwrap());
    assert!(!path.exists());
    fs::write(&path, b"Epic save").unwrap();
    let epic_files = vault.capture(&bindings).unwrap();
    vault.record_local(&epic_files).unwrap();
    drop(vault);

    let mut vault = open(root.path(), steam());
    assert!(vault.activate(&bindings).unwrap());
    assert_eq!(fs::read(&path).unwrap(), b"Steam save");
    assert_eq!(
        vault.read_content(&steam_files.0["save.dat"]).unwrap(),
        b"Steam save"
    );
    drop(vault);
    let mut vault = open(root.path(), epic_scope);
    assert!(vault.activate(&bindings).unwrap());
    assert_eq!(fs::read(&path).unwrap(), b"Epic save");
    assert_eq!(
        vault.read_content(&epic_files.0["save.dat"]).unwrap(),
        b"Epic save"
    );
}

#[test]
fn epic_scopes_reject_unsafe_identity_and_mixed_store_journals() {
    let root = tempfile::tempdir().unwrap();
    for scope in [
        epic("../account", "namespace", "game"),
        epic(&ACCOUNT_A.to_uppercase(), "namespace", "game"),
        epic(ACCOUNT_A, "../namespace", "game"),
        epic(ACCOUNT_A, "namespace", ".."),
        epic(ACCOUNT_A, "namespace", "game\0other"),
    ] {
        assert!(matches!(
            Vault::open(root.path(), scope),
            Err(Error::Invalid)
        ));
    }
    let mut mixed = serde_json::to_value(steam()).unwrap();
    mixed["account_id"] = ACCOUNT_A.into();
    mixed["namespace"] = "namespace".into();
    mixed["app_name"] = "game".into();
    assert!(serde_json::from_value::<StoreScope>(mixed).is_err());
}

fn binding(path: &Path) -> Bindings {
    Bindings::from([(
        "save.dat".into(),
        SelectedFile {
            path: path.to_path_buf(),
            platforms: 2,
        },
    )])
}

fn session() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[test]
fn a_running_steam_game_leaves_epic_cloud_work_on_other_saves_available() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let steam_save = saves.path().join("steam.dat");
    let epic_save = saves.path().join("epic.dat");
    fs::write(&steam_save, b"Steam progress").unwrap();
    fs::write(&epic_save, b"Epic progress").unwrap();
    let mut steam_vault = open(root.path(), steam());
    steam_vault.activate(&binding(&steam_save)).unwrap();
    steam_vault.capture(&binding(&steam_save)).unwrap();
    let game = session();
    steam_vault.game_started(&game).unwrap();

    let mut epic_vault = open(root.path(), epic(ACCOUNT_A, "namespace", "game"));
    assert!(!epic_vault.activate(&binding(&epic_save)).unwrap());
    let local = epic_vault.capture(&binding(&epic_save)).unwrap();
    let authorization = epic_vault.authorize(ACCOUNT_A).unwrap();
    epic_vault
        .queue_upload(&local, &remote(&Manifest::default()), "epic", authorization)
        .unwrap();

    assert_eq!(epic_vault.pending().unwrap().desired, local);
    assert_eq!(steam_vault.game_session(), Some(game.as_str()));
    assert_eq!(fs::read(&steam_save).unwrap(), b"Steam progress");
}

#[test]
fn epic_cannot_take_saves_from_a_steam_game_that_holds_its_vault() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let save = saves.path().join("save.dat");
    fs::write(&save, b"Steam progress").unwrap();
    let mut steam_vault = open(root.path(), steam());
    steam_vault.activate(&binding(&save)).unwrap();
    steam_vault.capture(&binding(&save)).unwrap();
    let game = session();
    steam_vault.game_started(&game).unwrap();
    let owners = fs::read(root.path().join("owners.json")).unwrap();

    let mut epic_vault = open(root.path(), epic(ACCOUNT_A, "namespace", "game"));
    let epic_files = staged(&epic_vault, b"Epic progress");
    epic_vault.record_local(&epic_files).unwrap();
    assert!(matches!(
        epic_vault.activate(&binding(&save)),
        Err(Error::OtherStoreBusy)
    ));

    assert_eq!(fs::read(&save).unwrap(), b"Steam progress");
    assert_eq!(fs::read(root.path().join("owners.json")).unwrap(), owners);
    steam_vault.game_exited(&game).unwrap();
    drop(steam_vault);
    assert!(epic_vault.activate(&binding(&save)).unwrap());
    assert_eq!(fs::read(&save).unwrap(), b"Epic progress");
}

#[test]
fn steam_garbage_collection_never_touches_epic_blobs() {
    let root = tempfile::tempdir().unwrap();
    let mut epic_vault = open(root.path(), epic(ACCOUNT_A, "namespace", "game"));
    let orphan = epic_vault.stage(b"unreferenced Epic bytes", 2).unwrap();
    let mut steam_vault = open(root.path(), steam());
    steam_vault.stage(b"unreferenced Steam bytes", 2).unwrap();

    assert_eq!(steam_vault.collect_garbage(&[]).unwrap(), 1);

    assert_eq!(
        epic_vault.read_content(&orphan).unwrap(),
        b"unreferenced Epic bytes"
    );
    assert_eq!(epic_vault.collect_garbage(&[]).unwrap(), 1);
    assert!(epic_vault.read_content(&orphan).is_err());
}

#[test]
fn restoring_current_local_snapshot_preserves_remote_baseline_and_local_choice() {
    let root = tempfile::tempdir().unwrap();
    let saves = tempfile::tempdir().unwrap();
    let path = saves.path().join("save.dat");
    let bindings = Bindings::from([(
        "save.dat".into(),
        SelectedFile {
            path: path.clone(),
            platforms: 2,
        },
    )]);
    let mut vault = open(root.path(), epic(ACCOUNT_A, "namespace", "game"));
    let baseline = staged(&vault, b"confirmed cloud");
    let confirmed = remote(&baseline);
    vault.confirm_equal(&baseline, &confirmed).unwrap();
    fs::write(&path, b"archived progress").unwrap();
    let local = vault.capture(&bindings).unwrap();
    let remote_change = remote(&staged(&vault, b"other device"));
    vault.choose_local(&remote_change, &remote_change).unwrap();
    fs::remove_file(&path).unwrap();
    vault.restore_local(&bindings).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"archived progress");
    assert_eq!(vault.local(), Some(&local));
    assert_eq!(vault.baseline(), Some(&confirmed));
    assert_eq!(
        vault.decision(&local, &remote_change).unwrap(),
        Decision::KeepLocal
    );
    assert!(vault.pending().is_none());
}
