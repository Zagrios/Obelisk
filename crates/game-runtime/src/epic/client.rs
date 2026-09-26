//! Starting an Epic Game session. Observation and commands are the shared
//! supervisor client's.
use super::*;
use crate::supervisor::storage;
use std::path::Path;

/// Readies a Windows build for its Game session: its prefix, the prerequisites
/// its installation names, and the folder of Wine's log. This runs before any
/// short-lived game credential exists, since a prerequisite may take minutes.
pub fn prepare_windows(config: &Configuration, runtime: PathBuf, logs: &Path) -> Result<Wine> {
    let prefix = config.prefix.clone().ok_or(Error::Unsupported)?;
    if config.platform != "Windows" {
        return Err(Error::Unsupported);
    }
    let wine = crate::wine::Config { runtime, prefix };
    crate::wine::prepare(&wine, &config.home)?;
    let prerequisite = epic_kit::client::installed_prerequisite(
        &config.namespace,
        &config.catalog_item_id,
        &config.app_name,
        &config.platform,
        &config.install,
    )?;
    let prerequisites = crate::wine::epic_prerequisites(&config.install, prerequisite.as_ref());
    crate::wine::install_prerequisites(&wine, &config.home, &prerequisites)?;
    crate::files::private_directory(logs)?;
    Ok(Wine {
        runtime: wine.runtime,
        log: logs.join(format!("wine-{}.log", config.app_name)),
    })
}

/// `wine` is how a Windows build runs, and must be absent for a Mac build.
pub fn start(
    config: Configuration,
    account_id: String,
    exchange_code: String,
    wine: Option<Wine>,
    cancel: CancelToken,
) -> Result<Client> {
    config.validate(&account_id)?;
    if wine.is_some() != config.prefix.is_some() {
        return Err(Error::Unsupported);
    }
    let request = Request {
        config: config.clone(),
        id: uuid::Uuid::new_v4().to_string(),
        account_id,
        exchange_code,
        environment: crate::native::environment(&config.home),
        wine,
    };
    let id = request.id.clone();
    let account_id = request.account_id.clone();
    let store = Store {
        namespace: config.namespace.clone(),
        app_name: config.app_name.clone(),
    };
    Ok(Client::start_with(
        &config.session_directory,
        &config.helper,
        id,
        Client::supervisor_command(&config.helper),
        supervisor::client::inspect,
        |pipe| storage::send(pipe, &request),
        |ack| ack.account_id == account_id && ack.store == store,
        || cancel.is_cancelled(),
    )?)
}
