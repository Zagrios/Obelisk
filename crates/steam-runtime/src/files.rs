use crate::{CLIENT_SHA256, Config, Error};
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
                .join("libforge_steam_discovery_adapter.dylib")
                .to_string_lossy()
                .into_owned(),
        );
        environment.insert("FORGE_PRIVATE_STEAM_DISCOVERY".into(), service.into());
        environment
    }
}
fn private_directory(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub(crate) fn prepare(config: &Config) -> Result<Prepared, Error> {
    if !config.source.join("steamclient.dylib").is_file()
        || !config.helpers.join("forge-steam-host").is_file()
        || !config
            .helpers
            .join("libforge_steam_discovery_adapter.dylib")
            .is_file()
        || !config.steam_api.is_file()
    {
        return Err(Error::Unavailable);
    }
    if crate::sha(&config.source.join("steamclient.dylib"))? != CLIENT_SHA256 {
        return Err(Error::VersionMismatch);
    }
    let helper_hash = crate::sha(&config.helpers.join("forge-steam-host"))?;
    let adapter_hash = crate::sha(
        &config
            .helpers
            .join("libforge_steam_discovery_adapter.dylib"),
    )?;
    private_directory(&config.cache)?;
    let directory = config.cache.join(format!(
        "{}-{}-{}",
        &CLIENT_SHA256[..12],
        &helper_hash[..12],
        &adapter_hash[..12]
    ));
    if !directory.exists() {
        let temporary = config
            .cache
            .join(format!(".staging-{}", uuid::Uuid::new_v4()));
        private_directory(&temporary)?;
        let result = (|| {
            for name in [
                "steamclient.dylib",
                "libtier0_s.dylib",
                "libvstdlib_s.dylib",
                "libaudio.dylib",
                "crashhandler.dylib",
            ] {
                fs::copy(config.source.join(name), temporary.join(name))?;
            }
            copy_tree(
                &config.source.join("Frameworks/Breakpad.framework"),
                &temporary.join("Frameworks/Breakpad.framework"),
            )?;
            for name in ["forge-steam-host", "libforge_steam_discovery_adapter.dylib"] {
                fs::copy(config.helpers.join(name), temporary.join(name))?;
            }
            fs::rename(&temporary, &directory)?;
            Ok::<_, Error>(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result?;
    }
    if crate::sha(&directory.join("steamclient.dylib"))? != CLIENT_SHA256
        || crate::sha(&directory.join("forge-steam-host"))? != helper_hash
        || crate::sha(&directory.join("libforge_steam_discovery_adapter.dylib"))? != adapter_hash
    {
        return Err(Error::VersionMismatch);
    }
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
fn copy_tree(source: &Path, target: &Path) -> Result<(), Error> {
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(source)?, target)?;
        #[cfg(not(unix))]
        return Err(Error::Unavailable);
    } else if source.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        fs::copy(source, target)?;
    }
    Ok(())
}
fn quote(path: &Path) -> Result<String, Error> {
    serde_json::to_string(path.to_str().ok_or(Error::Storage)?).map_err(|_| Error::Storage)
}
fn sandbox(config: &Config, directory: &Path) -> Result<String, Error> {
    Ok(format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (literal \"/dev/null\"))\n(deny file-read* (subpath {}) (subpath {}))\n(deny process-exec)\n(allow process-exec (literal {}))\n(deny mach-lookup (global-name-regex \".*[Ss]team.*\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\"))\n",
        quote(&config.home.join("Library/Application Support/Steam"))?,
        quote(
            &config
                .home
                .join("Library/Application Support/com.zagrios.forge")
        )?,
        quote(&directory.join("forge-steam-host"))?
    ))
}
pub(crate) fn report(path: &Path, proof: &crate::Proof) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(proof).map_err(|_| Error::Storage)?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)?;
    Ok(())
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
