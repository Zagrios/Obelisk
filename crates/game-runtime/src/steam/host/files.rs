use super::{Config, NativeStorage, Proof, licenses::LicenseCache};
use crate::{
    files::{private_directory, safe_storage_path},
    steam::Error,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) struct Prepared {
    pub directory: PathBuf,
    pub run_directory: PathBuf,
    pub profile: PathBuf,
    pub report: PathBuf,
    pub endpoints: [(String, String); 2],
    pub storage: Option<NativeStorage>,
    /// The account's kept Steam caches, when this launch uses them.
    pub licenses: Option<LicenseCache>,
}
impl Drop for Prepared {
    fn drop(&mut self) {
        // The host has stopped by now: `Host` stops it before it drops this,
        // and a launch that failed drops its host child first.
        if let Some(licenses) = &self.licenses {
            licenses.keep();
        }
        let _ = fs::remove_dir_all(&self.run_directory);
    }
}
impl Prepared {
    pub fn home(&self) -> PathBuf {
        self.storage
            .as_ref()
            .map_or_else(|| self.run_directory.join("home"), |s| s.home.clone())
    }
    pub fn steam_directory(&self) -> PathBuf {
        self.home().join(super::STEAM_DIRECTORY)
    }
    /// Starts the client from the account's kept caches, lets it write them
    /// in its home, and keeps them again when the launch ends.
    pub fn enable_licenses(&mut self, cache: &Path, steam_id: u64) -> Result<(), Error> {
        let licenses = LicenseCache::new(cache, steam_id, &self.steam_directory())?;
        licenses.seed()?;
        self.allow(&self.home(), licenses.folders())?;
        self.licenses = Some(licenses);
        Ok(())
    }
    /// Lets the host read all of `read` and write in each of `writes`.
    fn allow(&self, read: &Path, writes: &[PathBuf]) -> Result<(), Error> {
        use std::io::Write;
        let mut rules = format!(
            "\n(allow file-read* (subpath {}))\n(allow file-write*",
            quote(read)?
        );
        for path in writes {
            rules.push_str(&format!(" (subpath {})", quote(path)?));
        }
        rules.push_str(")\n");
        fs::OpenOptions::new()
            .append(true)
            .open(&self.profile)?
            .write_all(rules.as_bytes())?;
        Ok(())
    }
    pub fn enable_storage(&mut self, storage: &NativeStorage) -> Result<(), Error> {
        safe_storage_path(&storage.home).map_err(|error| Error::Cloud(error.into()))?;
        let steam = storage.steam_directory();
        let app = storage.cache_directory();
        for path in [app.join("remote"), steam.join("appcache")] {
            safe_storage_path(&path).map_err(|error| Error::Cloud(error.into()))?;
            private_directory(&path)?;
        }
        // The coordinator must publish a complete local projection before a
        // native host starts. Validate it without loading the game's SDK. SDK
        // quota/readback readiness now belongs to the game's own attachment.
        super::native_cache_files(app.clone(), storage.app_id, storage.platforms)?;
        self.allow(&storage.home, &[app, steam.join("appcache")])?;
        self.storage = Some(storage.clone());
        Ok(())
    }
    pub fn consumer_environment(&self, service: &str) -> std::collections::HashMap<String, String> {
        let mut environment: std::collections::HashMap<_, _> =
            self.endpoints.iter().cloned().collect();
        environment.insert(
            "DYLD_INSERT_LIBRARIES".into(),
            self.directory
                .join(crate::package::ADAPTER)
                .to_string_lossy()
                .into_owned(),
        );
        environment.insert("OBELISK_PRIVATE_STEAM_DISCOVERY".into(), service.into());
        environment
    }
}
pub(crate) fn prepare(
    config: &Config,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Prepared, Error> {
    if config.wine.is_none() && !config.steam_api.as_deref().is_some_and(Path::is_file) {
        return Err(Error::Unavailable);
    }
    let directory = crate::package::prepare(config, cancelled)?;
    let id = uuid::Uuid::new_v4();
    let run_directory = config.cache.join(format!("{}{id}", crate::package::LAUNCH));
    crate::package::remove_stale(&config.cache, &directory, &run_directory);
    let listeners = [
        std::net::TcpListener::bind("127.0.0.1:0")?,
        std::net::TcpListener::bind("127.0.0.1:0")?,
    ];
    // Refuse an unsupported or malformed SDK library before anything is written.
    // The selected slice itself is owned by Launch, which the supervisor reads.
    if let Some(library) = &config.steam_api {
        crate::supervisor::architecture::Architecture::preferred(library)?;
    }
    let prepared = Prepared {
        storage: None,
        licenses: None,
        directory,
        profile: run_directory.join("host.sb"),
        report: crate::diagnostics::launch_report(&config.diagnostics, config.app_id, id),
        run_directory,
        endpoints: [
            (
                "Steam3Master".into(),
                listeners[0].local_addr()?.to_string(),
            ),
            (
                "SteamClientService".into(),
                listeners[1].local_addr()?.to_string(),
            ),
        ],
    };
    let run_directory = &prepared.run_directory;
    private_directory(run_directory)?;
    private_directory(&run_directory.join("home"))?;
    private_directory(&run_directory.join("tmp"))?;
    private_directory(&config.diagnostics)?;
    fs::write(&prepared.profile, sandbox(config, &prepared.directory)?)?;
    Ok(prepared)
}
fn quote(path: &Path) -> Result<String, Error> {
    serde_json::to_string(path.to_str().ok_or(Error::Storage)?).map_err(|_| Error::Storage)
}
fn sandbox(config: &Config, directory: &Path) -> Result<String, Error> {
    let mut profile = format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (literal \"/dev/null\"))\n(deny file-read* (subpath {}) (subpath {}))\n(deny process-exec)\n(allow process-exec (literal {}))\n(deny mach-lookup (global-name-regex \".*[Ss]team.*\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\"))\n",
        quote(&config.home.join("Library/Application Support/Steam"))?,
        quote(
            &config
                .home
                .join("Library/Application Support/com.zagrios.obelisk")
        )?,
        quote(&directory.join(crate::package::HOST))?
    );
    // A fresh, read-only native generation cannot inspect the ordinary saves
    // owned by Rust. Permit only the pinned runtime under the real home. The
    // native host never receives root-0 API storage or any save projection.
    profile.push_str(&format!(
        "(deny file-read* (subpath {}))\n(allow file-read* (subpath {}))\n",
        quote(&config.home)?,
        quote(directory)?
    ));
    // realpath on the host executable needs metadata for each ancestor. Grant
    // those exact directories, without granting enumeration or save contents.
    for ancestor in directory.ancestors().skip(1) {
        profile.push_str(&format!(
            "(allow file-read-metadata (literal {}))\n",
            quote(ancestor)?
        ));
    }
    for root in &config.cloud_save_roots {
        profile.push_str(&format!("(deny file-read* (subpath {}))\n", quote(root)?));
    }
    Ok(profile)
}
/// Replaces `path` atomically without fsync: diagnostics and caches need no
/// durability, unlike [`obelisk_core::write_atomic`].
pub(super) fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().ok_or(Error::Storage)?)?;
    write(temporary.as_file_mut())?;
    temporary.persist(path).map_err(|_| Error::Storage)?;
    Ok(())
}

pub(super) fn report(path: &Path, proof: &Proof) -> Result<(), Error> {
    atomic_write(path, |file| {
        serde_json::to_writer_pretty(file, proof).map_err(|_| Error::Storage)
    })
}
pub(super) fn failure_report(
    config: &Config,
    error: &Error,
    events: &[crate::protocol::Event],
) -> Result<(), Error> {
    private_directory(&config.diagnostics)?;
    let path = crate::diagnostics::failure_report(
        &config.diagnostics,
        config.app_id,
        uuid::Uuid::new_v4(),
    );
    let report =
        serde_json::json!({"app_id": config.app_id, "error": error.to_string(), "events": events});
    fs::write(
        path,
        serde_json::to_vec_pretty(&report).map_err(|_| Error::Storage)?,
    )?;
    Ok(())
}

#[cfg(test)]
mod atomic_tests {
    use super::*;
    use std::io::Write;

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_access_child() {
        let Some(root) = std::env::var_os("OBELISK_SANDBOX_ACCESS_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        assert!(root.join("home/cache/runtime").canonicalize().is_ok());
        assert!(fs::read(root.join("home/saves/slot.dat")).is_err());
        assert!(fs::read(root.join("home/Library/Application Support/Steam/fixture")).is_err());
        if std::env::var_os("OBELISK_SANDBOX_STORAGE_FIXTURE").is_some() {
            let steam = root.join("home/sdk/Library/Application Support/Steam");
            assert!(fs::write(steam.join("userdata/42/42/remote/slot"), b"allowed").is_ok());
            assert!(fs::write(steam.join("userdata/42/43/slot"), b"wrong app").is_err());
            assert!(fs::write(steam.join("userdata/43/42/slot"), b"wrong account").is_err());
            assert!(fs::write(steam.join("config/loginusers.vdf"), b"credentials").is_err());
        }
        if let Some(steam) = std::env::var_os("OBELISK_SANDBOX_LICENSES_FIXTURE") {
            let steam = PathBuf::from(steam);
            assert_eq!(
                fs::read(steam.join("appcache/packageinfo.vdf")).unwrap(),
                b"kept"
            );
            assert!(fs::write(steam.join("appcache/packageinfo.vdf"), b"allowed").is_ok());
            assert!(fs::write(steam.join("userdata/42/config/licensecache"), b"allowed").is_ok());
            assert!(fs::write(steam.join("userdata/43/config/licensecache"), b"wrong").is_err());
            assert!(fs::write(steam.join("config/loginusers.vdf"), b"credentials").is_err());
        }
    }

    /// A launch of the runtime in `directory`, running in `run`.
    #[cfg(target_os = "macos")]
    fn launch(directory: &Path, run: &Path) -> Prepared {
        fs::create_dir_all(run).unwrap();
        Prepared {
            directory: directory.into(),
            run_directory: run.into(),
            profile: run.join("profile"),
            report: run.join("report"),
            endpoints: Default::default(),
            storage: None,
            licenses: None,
        }
    }

    /// Runs the access child under a sandbox profile that `sandbox-exec` takes
    /// with `flag`, and checks that it passed.
    #[cfg(target_os = "macos")]
    fn run_sandboxed(
        flag: &str,
        profile: &std::ffi::OsStr,
        root: &Path,
        fixtures: &[(&str, &std::ffi::OsStr)],
    ) {
        let result = std::process::Command::new("/usr/bin/sandbox-exec")
            .arg(flag)
            .arg(profile)
            .arg(std::env::current_exe().unwrap())
            .args([
                "steam::host::files::atomic_tests::sandbox_access_child",
                "--exact",
            ])
            .env("OBELISK_SANDBOX_ACCESS_FIXTURE", root)
            .envs(fixtures.iter().copied())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(result.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_allows_runtime_path_resolution_but_denies_save_contents() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let home = root.join("home");
        let runtime = home.join("cache/runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(home.join("saves")).unwrap();
        fs::create_dir_all(home.join("Library/Application Support/Steam")).unwrap();
        fs::write(home.join("saves/slot.dat"), b"fixture save").unwrap();
        fs::write(
            home.join("Library/Application Support/Steam/fixture"),
            b"fixture installation",
        )
        .unwrap();
        let config = Config {
            helpers: runtime.clone(),
            package: root.join("unused"),
            cache: home.join("cache"),
            diagnostics: root.join("logs"),
            steam_api: None,
            wine: None,
            home,
            app_id: 42,
            cloud_save_roots: vec![],
        };
        let profile = format!(
            "{}\n(allow process-exec (literal {}))",
            sandbox(&config, &runtime).unwrap(),
            quote(&std::env::current_exe().unwrap()).unwrap()
        );
        run_sandboxed("-p", std::ffi::OsStr::new(&profile), &root, &[]);
        let storage = NativeStorage {
            home: config.home.join("sdk"),
            account_id: 0x011000010000002a,
            app_id: 42,
            platforms: 2,
        };
        let steam = storage.home.join("Library/Application Support/Steam");
        for path in ["userdata/42/43", "userdata/43/42", "config"] {
            fs::create_dir_all(steam.join(path)).unwrap();
        }
        steam_kit::cloud::SdkCloudPaths::new(steam.join("userdata/42/42"), 42, 2)
            .unwrap()
            .prepare_index(&[])
            .unwrap();
        let mut prepared = launch(&runtime, &root.join("run"));
        fs::write(&prepared.profile, &profile).unwrap();
        prepared.enable_storage(&storage).unwrap();
        run_sandboxed(
            "-f",
            prepared.profile.as_os_str(),
            &root,
            &[("OBELISK_SANDBOX_STORAGE_FIXTURE", std::ffi::OsStr::new("1"))],
        );

        // A launch's own home sits under the player's, like the real one: the
        // client reads and writes its caches there and nothing else.
        let mut launch = launch(&runtime, &config.cache.join("launch-licenses"));
        let id = 0x011000010000002a;
        let kept = root.join("kept");
        let cache = LicenseCache::new(&kept, id, &launch.steam_directory()).unwrap();
        fs::write(cache.kept().join("packageinfo.vdf"), b"kept").unwrap();
        fs::write(&launch.profile, &profile).unwrap();
        launch.enable_licenses(&kept, id).unwrap();
        let steam = launch.steam_directory();
        for path in ["userdata/43/config", "config"] {
            fs::create_dir_all(steam.join(path)).unwrap();
        }
        run_sandboxed(
            "-f",
            launch.profile.as_os_str(),
            &root,
            &[("OBELISK_SANDBOX_LICENSES_FIXTURE", steam.as_os_str())],
        );
    }

    #[test]
    fn only_a_mac_game_needs_its_steam_api_library_to_start_the_host() {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config {
            helpers: root.path().join("helpers"),
            package: root.path().join("unused"),
            cache: root.path().join("cache"),
            diagnostics: root.path().join("logs"),
            steam_api: None,
            wine: None,
            home: root.path().join("home"),
            app_id: 42,
            cloud_save_roots: vec![],
        };
        // A cancelled launch stops at the runtime package, after the library.
        let cancelled = std::sync::atomic::AtomicBool::new(true);

        assert!(matches!(
            prepare(&config, &cancelled),
            Err(Error::Unavailable)
        ));
        config.wine = Some(crate::wine::tests::config(root.path()));
        assert!(matches!(
            prepare(&config, &cancelled),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn incomplete_atomic_writes_preserve_the_previous_file_and_remove_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.json");
        fs::write(&path, b"previous").unwrap();
        let result = atomic_write(&path, |file| {
            file.write_all(b"incomplete")?;
            Err(Error::Storage)
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        atomic_write(&path, |file| {
            file.write_all(b"complete")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete");
    }
}
