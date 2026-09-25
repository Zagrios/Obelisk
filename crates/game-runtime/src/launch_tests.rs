use super::*;
use std::fs;

fn credentials() -> Credentials {
    Credentials {
        steam_id: 0x011000010000002a,
        account_name: "fixture".into(),
        refresh_token: "fixture-token".into(),
    }
}

fn fixture(root: &std::path::Path) -> (Config, files::Prepared) {
    let config = Config {
        helpers: root.into(),
        package: root.join("unused.zip"),
        cache: root.into(),
        diagnostics: root.into(),
        steam_api: root.join("libsteam_api.dylib"),
        wine: None,
        home: root.into(),
        app_id: 42,
        cloud_save_roots: vec![],
    };
    let prepared = tests::prepared(root);
    fs::write(&prepared.profile, "(version 1)\n(deny default)\n").unwrap();
    (config, prepared)
}

/// Announces authentication and readiness, then holds stdin so the host stays alive.
fn ready_host(_: &files::Prepared, _: &Config, _: &str) -> Result<process::Child, Error> {
    Ok(tests::child(
        r#"printf '%s\n' '{"event":"authenticated"}'; printf '{"event":"ready","app_id":42,"host_pid":%s,"ticket_bytes":178}\n' "$$"; cat >/dev/null"#,
    ))
}

#[test]
fn host_readiness_admits_an_unknown_sdk_and_reports_it_as_unchecked() {
    let root = tempfile::tempdir().unwrap();
    let (config, prepared) = fixture(root.path());
    let runtime = Launch::new(credentials())
        .prepare_runtime(
            &config,
            None,
            &mut Vec::new(),
            credentials(),
            prepared,
            ready_host,
        )
        .unwrap();
    assert_eq!(runtime.environment()["Steam3Master"], "127.0.0.1:1");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(report["host_account_verified"], true);
    assert_eq!(report["host_license_verified"], true);
    assert_eq!(report["sdk_checked"], false);
    runtime.stopped(None);
}

fn storage(root: &std::path::Path) -> (NativeStorage, PathBuf) {
    let storage = NativeStorage {
        home: root.join("native-home"),
        account_id: credentials().steam_id,
        app_id: 42,
    };
    let cache = storage.cache_directory();
    fs::create_dir_all(cache.join("remote")).unwrap();
    let paths = steam_kit::cloud::SdkCloudPaths::new(cache.clone(), 42).unwrap();
    let save = paths.destination("slot.sav", 7).unwrap().unwrap();
    fs::write(&save, b"reconciled save").unwrap();
    paths
        .prepare_index(&[steam_kit::cloud::LocalCloudFile {
            name: "slot.sav".into(),
            path: save,
            platforms: 7,
        }])
        .unwrap();
    (storage, cache)
}

#[test]
fn local_storage_is_validated_before_host_start_without_an_sdk_consumer() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (config, prepared) = fixture(&root);
    let (storage, cache) = storage(&root);
    let index = fs::read(cache.join("remotecache.vdf")).unwrap();
    let launch = Launch::new(credentials());
    let runtime = launch
        .prepare_runtime(
            &config,
            Some(storage.clone()),
            &mut Vec::new(),
            credentials(),
            prepared,
            ready_host,
        )
        .unwrap();
    // The host ran against the prepared projection, not a scratch home.
    assert_eq!(runtime._prepared.home(), storage.home);
    assert!(
        process::Child::host_command(&runtime._prepared, &config, "fixture")
            .get_envs()
            .any(|(key, value)| key == "FORGE_STORAGE_METADATA"
                && value == Some(std::ffi::OsStr::new("1")))
    );
    assert_eq!(native_cache_files(cache.clone(), 42).unwrap().len(), 1);
    runtime.stopped(None);
    drop(runtime);
    assert_eq!(
        fs::read(cache.join("remote/slot.sav")).unwrap(),
        b"reconciled save"
    );
    assert_eq!(fs::read(cache.join("remotecache.vdf")).unwrap(), index);
}

#[test]
fn invalid_or_wrong_account_storage_never_starts_the_host() {
    for invalid in [
        "missing",
        "malformed",
        "wrong-app",
        "unindexed",
        "symlink",
        "wrong-account",
    ] {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let (config, prepared) = fixture(&root);
        let (mut storage, cache) = storage(&root);
        let index = cache.join("remotecache.vdf");
        match invalid {
            "missing" => fs::remove_file(&index).unwrap(),
            "malformed" => fs::write(&index, b"invalid index").unwrap(),
            "wrong-app" => {
                let text = fs::read_to_string(&index).unwrap();
                fs::write(&index, text.replacen("\"42\"", "\"43\"", 1)).unwrap();
            }
            "unindexed" => fs::write(cache.join("remote/unindexed.sav"), b"preserve").unwrap(),
            "symlink" => {
                fs::rename(&index, cache.join("original.vdf")).unwrap();
                std::os::unix::fs::symlink("original.vdf", &index).unwrap();
            }
            "wrong-account" => storage.account_id += 1,
            _ => unreachable!(),
        }
        let launch = Launch::new(credentials());
        let result = launch.prepare_runtime(
            &config,
            Some(storage),
            &mut Vec::new(),
            credentials(),
            prepared,
            |_, _, _| panic!("host started with {invalid} storage"),
        );
        assert!(matches!(
            result,
            Err(Error::Storage | Error::InvalidSession)
        ));
        assert_eq!(
            fs::read(cache.join("remote/slot.sav")).unwrap(),
            b"reconciled save"
        );
    }
}

#[test]
fn native_authentication_and_ownership_failures_still_block_admission() {
    for reason in ["authentication", "ownership"] {
        let root = tempfile::tempdir().unwrap();
        let (config, prepared) = fixture(root.path());
        let result = Launch::new(credentials()).prepare_runtime(
            &config,
            None,
            &mut Vec::new(),
            credentials(),
            prepared,
            |_, _, _| {
                Ok(tests::child(&format!(
                    r#"printf '{{"event":"failed","reason":"{reason}"}}\n'; cat >/dev/null"#
                )))
            },
        );
        assert!(matches!(
            (reason, result),
            ("authentication", Err(Error::Authentication)) | ("ownership", Err(Error::Ownership))
        ));
        assert!(!root.path().join("report.json").exists());
    }
}
