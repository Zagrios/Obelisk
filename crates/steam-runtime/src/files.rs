use crate::{Config, Error};
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
}
impl Drop for Prepared {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.run_directory);
    }
}
impl Prepared {
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
        environment.insert("FORGE_PRIVATE_STEAM_DISCOVERY".into(), service.into());
        environment
    }
}
pub(crate) fn private_directory(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(Error::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub(crate) fn prepare(
    config: &Config,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Prepared, Error> {
    if !config.steam_api.is_file() {
        return Err(Error::Unavailable);
    }
    let directory = crate::package::prepare(config, cancelled)?;
    let id = uuid::Uuid::new_v4();
    let run_directory = config.cache.join(format!("launch-{id}"));
    let listeners = [
        std::net::TcpListener::bind("127.0.0.1:0")?,
        std::net::TcpListener::bind("127.0.0.1:0")?,
    ];
    let prepared = Prepared {
        directory,
        profile: run_directory.join("host.sb"),
        report: config
            .diagnostics
            .join(format!("launch-{}-{id}.json", config.app_id)),
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
                .join("Library/Application Support/com.zagrios.forge")
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
/// Durability is explicit: session recovery requires fsync, diagnostics do not.
#[derive(Clone, Copy)]
pub(crate) enum Durability {
    Atomic,
    FileSync,
}

pub(crate) fn atomic_write(
    path: &Path,
    durability: Durability,
    write: impl FnOnce(&mut std::fs::File) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().ok_or(Error::Storage)?)?;
    write(temporary.as_file_mut())?;
    if matches!(durability, Durability::FileSync) {
        temporary.as_file().sync_all()?;
    }
    temporary.persist(path).map_err(|_| Error::Storage)?;
    Ok(())
}

pub(crate) fn report(path: &Path, proof: &crate::Proof) -> Result<(), Error> {
    atomic_write(path, Durability::Atomic, |file| {
        serde_json::to_writer_pretty(file, proof).map_err(|_| Error::Storage)
    })
}
pub(crate) fn failure_report(
    config: &Config,
    error: &Error,
    events: &[crate::protocol::Event],
) -> Result<(), Error> {
    private_directory(&config.diagnostics)?;
    let path = config.diagnostics.join(format!(
        "failed-{}-{}.json",
        config.app_id,
        uuid::Uuid::new_v4()
    ));
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
        let Some(root) = std::env::var_os("FORGE_SANDBOX_ACCESS_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        assert!(root.join("home/cache/runtime").canonicalize().is_ok());
        assert!(fs::read(root.join("home/saves/slot.dat")).is_err());
        assert!(fs::read(root.join("home/Library/Application Support/Steam/fixture")).is_err());
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
            steam_api: root.join("unused"),
            home,
            app_id: 42,
            cloud_save_roots: vec![],
        };
        let executable = std::env::current_exe().unwrap();
        let profile = format!(
            "{}\n(allow process-exec (literal {}))",
            sandbox(&config, &runtime).unwrap(),
            quote(&executable).unwrap()
        );
        let result = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile])
            .arg(executable)
            .args(["files::atomic_tests::sandbox_access_child", "--exact"])
            .env("FORGE_SANDBOX_ACCESS_FIXTURE", &root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
    }

    #[test]
    fn incomplete_atomic_writes_preserve_the_previous_file_and_remove_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.json");
        for durability in [Durability::Atomic, Durability::FileSync] {
            fs::write(&path, b"previous").unwrap();
            let result = atomic_write(&path, durability, |file| {
                file.write_all(b"incomplete")?;
                Err(Error::Storage)
            });
            assert!(result.is_err());
            assert_eq!(fs::read(&path).unwrap(), b"previous");
            assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
            atomic_write(&path, durability, |file| {
                file.write_all(b"complete")?;
                Ok(())
            })
            .unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"complete");
        }
    }
}
