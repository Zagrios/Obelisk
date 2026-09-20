use super::*;
use std::sync::Arc;

struct Fixture {
    _root: tempfile::TempDir,
    config: Config,
    files: Vec<PayloadFile>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            cloud_save_roots: vec![],
            helpers: root.path().join("helpers"),
            package: root.path().join("runtime.zip"),
            cache: root.path().join("cache"),
            diagnostics: root.path().join("logs"),
            home: root.path().join("home"),
            steam_api: root.path().join("libsteam_api.dylib"),
            app_id: 42,
        };
        fs::create_dir_all(&config.helpers).unwrap();
        fs::create_dir(&config.home).unwrap();
        fs::write(&config.steam_api, b"fixture SDK").unwrap();
        for name in HELPERS {
            fs::write(config.helpers.join(name), name).unwrap();
        }
        executable(&config.helpers.join(HELPERS[0]));
        let data = b"fixture runtime bytes";
        let files = [
            "steamclient.dylib",
            "libaudio.dylib",
            "Frameworks/Breakpad.framework/Versions/A/Breakpad",
        ]
        .into_iter()
        .map(|path| PayloadFile {
            path: path.into(),
            package: "fixture".into(),
            size: data.len() as u64,
            sha256: format!("{:x}", Sha256::digest(data)),
        })
        .collect::<Vec<_>>();
        write_zip(
            &config.package,
            files
                .iter()
                .map(|entry| (entry.path.as_str(), data.as_slice()))
                .collect(),
        );
        Self {
            _root: root,
            config,
            files,
        }
    }
    fn prepare(&self) -> Result<PathBuf, Error> {
        prepare_with(&self.config, &self.files, &AtomicBool::new(false))
    }
    fn contents(&self) -> Vec<String> {
        fs::read_dir(&self.config.cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }
}
fn executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
fn write_zip(path: &Path, entries: Vec<(&str, &[u8])>) {
    let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
    for (name, bytes) in entries {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}
#[test]
fn compiled_manifest_matches_native_abi_and_contains_seven_libraries_plus_signed_metadata() {
    let value = manifest();
    assert_eq!(value.files.len(), 8);
    assert_eq!(value.packages.len(), 3);
    assert_eq!(
        value
            .files
            .iter()
            .find(|file| file.path == "steamclient.dylib")
            .unwrap()
            .sha256,
        crate::client_sha256()
    );
    for entry in &value.files {
        assert!(
            Path::new(&entry.path)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        );
        assert!(
            value
                .packages
                .iter()
                .any(|source| source.name == entry.package)
        );
        assert_eq!(entry.sha256.len(), 64);
    }
    assert_eq!(
        value
            .files
            .iter()
            .map(|entry| &entry.path)
            .collect::<BTreeSet<_>>()
            .len(),
        8
    );
}
#[test]
fn first_preparation_needs_only_bundled_package_and_helpers_then_reuses_cache() {
    let fixture = Fixture::new();
    assert!(
        !fixture
            .config
            .home
            .join("Library/Application Support/Steam")
            .exists()
    );
    let directory = fixture.prepare().unwrap();
    assert_eq!(fixture.contents().len(), 1);
    fs::remove_file(&fixture.config.package).unwrap();
    assert_eq!(fixture.prepare().unwrap(), directory);
}
#[test]
fn missing_package_fails_without_publishing_a_runtime() {
    let fixture = Fixture::new();
    fs::remove_file(&fixture.config.package).unwrap();
    assert!(matches!(fixture.prepare(), Err(Error::Unavailable)));
    assert!(fixture.contents().is_empty());
}
#[test]
fn corrupt_payload_cleans_staging_and_never_publishes() {
    let fixture = Fixture::new();
    let bad = b"different bytes......";
    assert_eq!(bad.len() as u64, fixture.files[0].size);
    write_zip(
        &fixture.config.package,
        fixture
            .files
            .iter()
            .map(|entry| (entry.path.as_str(), bad.as_slice()))
            .collect(),
    );
    assert!(matches!(fixture.prepare(), Err(Error::VersionMismatch)));
    assert!(fixture.contents().is_empty());
}
#[test]
fn incomplete_truncated_and_unexpected_archives_are_rejected() {
    let fixture = Fixture::new();
    for entries in [
        vec![("steamclient.dylib", b"fixture runtime bytes".as_slice())],
        vec![
            ("../outside", b"fixture runtime bytes".as_slice()),
            ("libaudio.dylib", b"fixture runtime bytes"),
            (
                "Frameworks/Breakpad.framework/Versions/A/Breakpad",
                b"fixture runtime bytes",
            ),
        ],
    ] {
        write_zip(&fixture.config.package, entries);
        assert!(fixture.prepare().is_err());
        assert!(fixture.contents().is_empty());
    }
    fs::write(&fixture.config.package, b"PK incomplete").unwrap();
    assert!(fixture.prepare().is_err());
    assert!(fixture.contents().is_empty());
    assert!(!fixture._root.path().join("outside").exists());
}
#[test]
fn missing_changed_and_unexpected_cached_files_are_repaired_without_overwriting() {
    for damage in 0..3 {
        let fixture = Fixture::new();
        let old = fixture.prepare().unwrap();
        match damage {
            0 => fs::remove_file(old.join("libaudio.dylib")).unwrap(),
            1 => fs::write(old.join("steamclient.dylib"), b"changed").unwrap(),
            _ => fs::write(old.join("libvideo.dylib"), b"unexpected loader dependency").unwrap(),
        }
        let new = fixture.prepare().unwrap();
        assert_ne!(new, old);
        assert!(old.is_dir());
        assert_eq!(fixture.prepare().unwrap(), new);
    }
}
#[test]
fn helper_upgrade_uses_bundled_payload_and_preserves_previous_generation() {
    let fixture = Fixture::new();
    let old = fixture.prepare().unwrap();
    fs::write(fixture.config.helpers.join(HELPERS[0]), b"new helper").unwrap();
    let new = fixture.prepare().unwrap();
    assert_ne!(old, new);
    assert_eq!(
        fs::read(old.join(HELPERS[0])).unwrap(),
        HELPERS[0].as_bytes()
    );
    assert_eq!(fs::read(new.join(HELPERS[0])).unwrap(), b"new helper");
}
#[test]
fn interrupted_staging_is_ignored_and_concurrent_preparations_publish_complete_generations() {
    let fixture = Arc::new(Fixture::new());
    fs::create_dir_all(fixture.config.cache.join(".staging-interrupted")).unwrap();
    fs::write(
        fixture
            .config
            .cache
            .join(".staging-interrupted/steamclient.dylib"),
        b"partial",
    )
    .unwrap();
    let other = fixture.clone();
    let worker = std::thread::spawn(move || other.prepare().unwrap());
    let first = fixture.prepare().unwrap();
    let second = worker.join().unwrap();
    for directory in [first, second] {
        assert_eq!(
            fs::read(directory.join("steamclient.dylib")).unwrap(),
            b"fixture runtime bytes"
        );
        assert!(directory.join(HELPERS[0]).is_file());
    }
    // Never delete another preparation's staging directory.
    assert!(fixture.config.cache.join(".staging-interrupted").exists());
}
#[test]
fn cancellation_before_and_during_copy_is_observed() {
    let fixture = Fixture::new();
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        prepare_with(&fixture.config, &fixture.files, &cancelled),
        Err(Error::Cancelled)
    ));
    assert!(!fixture.config.cache.exists());
    struct CancelReader<'a>(&'a AtomicBool);
    impl Read for CancelReader<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.0.store(true, Ordering::Release);
            bytes[0] = 1;
            Ok(1)
        }
    }
    cancelled.store(false, Ordering::Release);
    assert!(matches!(
        copy_verified(
            CancelReader(&cancelled),
            std::io::sink(),
            &fixture.files[0],
            &cancelled
        ),
        Err(Error::Cancelled)
    ));
}
#[cfg(unix)]
#[test]
fn symlinked_cache_components_are_never_used() {
    let fixture = Fixture::new();
    let old = fixture.prepare().unwrap();
    let external = fixture._root.path().join("outside");
    fs::rename(old.join("Frameworks"), &external).unwrap();
    std::os::unix::fs::symlink(&external, old.join("Frameworks")).unwrap();
    let repaired = fixture.prepare().unwrap();
    assert_ne!(old, repaired);
    assert!(
        fs::symlink_metadata(repaired.join("Frameworks"))
            .unwrap()
            .is_dir()
    );
    assert!(external.is_dir());
}
#[cfg(unix)]
#[test]
fn archive_symlinks_are_rejected() {
    let fixture = Fixture::new();
    let mut zip = zip::ZipWriter::new(File::create(&fixture.config.package).unwrap());
    for entry in &fixture.files {
        zip.add_symlink(
            &entry.path,
            "/outside",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    }
    zip.finish().unwrap();
    assert!(fixture.prepare().is_err());
    assert!(fixture.contents().is_empty());
}
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "Requires FORGE_STEAM_BUNDLE pointing at a built app; no account or game is used"]
fn bundled_runtime_loads_without_steam_installation_access() {
    let bundle = PathBuf::from(std::env::var_os("FORGE_STEAM_BUNDLE").expect("FORGE_STEAM_BUNDLE"));
    let root = tempfile::tempdir().unwrap();
    let config = Config {
        cloud_save_roots: vec![],
        helpers: bundle.join("Contents/Helpers/SteamNative"),
        package: bundle.join("Contents/Resources/SteamRuntime/runtime.zip"),
        cache: root.path().join("cache"),
        diagnostics: root.path().join("logs"),
        home: root.path().join("home"),
        steam_api: root.path().join("unused"),
        app_id: 0,
    };
    let directory = prepare(&config, &AtomicBool::new(false)).unwrap();
    let executable = directory.join(HELPERS[0]);
    let actual_home = std::env::var_os("HOME").unwrap();
    let steam = PathBuf::from(actual_home).join("Library/Application Support/Steam");
    let profile = format!(
        "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (literal \"/dev/null\"))(deny file-read* (subpath {}))(deny process-exec)(allow process-exec (literal {}))",
        serde_json::to_string(steam.to_str().unwrap()).unwrap(),
        serde_json::to_string(executable.to_str().unwrap()).unwrap()
    );
    let status = std::process::Command::new("/usr/bin/sandbox-exec")
        .args(["-p", &profile])
        .arg(executable)
        .arg("verify-runtime")
        .arg(directory.join("steamclient.dylib"))
        .env_clear()
        .env("HOME", &config.home)
        .env("TMPDIR", root.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "minimal packaged runtime must load without Steam, network or writes"
    );
    assert_eq!(
        prepare(&config, &AtomicBool::new(false)).unwrap(),
        directory
    );
}

#[test]
fn equal_size_and_timestamp_tampering_is_still_detected() {
    let fixture = Fixture::new();
    let old = fixture.prepare().unwrap();
    let path = old.join("steamclient.dylib");
    let before = fs::metadata(&path).unwrap();
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    (&file)
        .write_all(&vec![b'x'; before.len() as usize])
        .unwrap();
    file.set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), before.len());
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        before.modified().unwrap()
    );
    assert_ne!(fixture.prepare().unwrap(), old);
}

#[cfg(unix)]
#[test]
fn tree_validation_rejects_root_file_and_nested_parent_symlinks() {
    for component in [
        "steamclient.dylib",
        "Frameworks/Breakpad.framework/Versions",
        "",
    ] {
        let fixture = Fixture::new();
        let old = fixture.prepare().unwrap();
        // This focused tree contains exactly the payload inventory.
        for helper in HELPERS {
            fs::remove_file(old.join(helper)).unwrap();
        }
        verify_tree(&old, &fixture.files, &AtomicBool::new(false)).unwrap();
        let path = if component.is_empty() {
            old.clone()
        } else {
            old.join(component)
        };
        let external = fixture._root.path().join("external");
        fs::rename(&path, &external).unwrap();
        std::os::unix::fs::symlink(&external, &path).unwrap();
        assert!(verify_tree(&old, &fixture.files, &AtomicBool::new(false)).is_err());
        assert!(external.exists());
    }
}
