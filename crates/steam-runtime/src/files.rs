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
                .join(crate::package::HELPERS[1])
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
    Ok(format!(
        "(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write* (literal \"/dev/null\"))\n(deny file-read* (subpath {}) (subpath {}))\n(deny process-exec)\n(allow process-exec (literal {}))\n(deny mach-lookup (global-name-regex \".*[Ss]team.*\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\"))\n",
        quote(&config.home.join("Library/Application Support/Steam"))?,
        quote(
            &config
                .home
                .join("Library/Application Support/com.zagrios.forge")
        )?,
        quote(&directory.join(crate::package::HELPERS[0]))?
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
