//! Starting an Epic Game session. Observation and commands are the shared
//! supervisor client's.
use super::*;
use crate::supervisor::storage;

/// `wine` is how a Windows build runs, from `prepare_windows`, and must be
/// absent for a Mac build.
pub fn start(
    config: Configuration,
    account_id: String,
    exchange_code: String,
    wine: Option<crate::wine::Config>,
    cancel: CancelToken,
) -> Result<Client> {
    config.validate(&account_id)?;
    if !config.runs_in(wine.as_ref()) {
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
