//! Presentation of the Rust-owned Epic supervisor and Cloud decisions.
use crate::{EpicError, EpicSession, GameSession, SailOverLocations};
use game_runtime::epic;
use std::{path::Path, sync::Arc};

impl From<epic::Error> for EpicError {
    fn from(error: epic::Error) -> Self {
        Self::Service {
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicCloudConfiguration {
    pub root: String,
    pub home: String,
    pub install: String,
    pub session_directory: String,
    pub helper: String,
    pub namespace: String,
    pub catalog_item_id: String,
    pub app_name: String,
    pub platform: String,
    pub language: String,
    /// A Windows build's Wine prefix, where its saves live.
    pub prefix: Option<String>,
}
impl From<EpicCloudConfiguration> for epic::Configuration {
    fn from(c: EpicCloudConfiguration) -> Self {
        Self {
            root: c.root.into(),
            home: c.home.into(),
            install: c.install.into(),
            session_directory: c.session_directory.into(),
            helper: c.helper.into(),
            namespace: c.namespace,
            catalog_item_id: c.catalog_item_id,
            app_name: c.app_name,
            platform: c.platform,
            language: c.language,
            prefix: c.prefix.map(Into::into),
        }
    }
}
impl From<epic::Configuration> for EpicCloudConfiguration {
    fn from(c: epic::Configuration) -> Self {
        Self {
            root: c.root.to_string_lossy().into_owned(),
            home: c.home.to_string_lossy().into_owned(),
            install: c.install.to_string_lossy().into_owned(),
            session_directory: c.session_directory.to_string_lossy().into_owned(),
            helper: c.helper.to_string_lossy().into_owned(),
            namespace: c.namespace,
            catalog_item_id: c.catalog_item_id,
            app_name: c.app_name,
            platform: c.platform,
            language: c.language,
            prefix: c.prefix.map(|prefix| prefix.to_string_lossy().into_owned()),
        }
    }
}
/// Every Epic supervisor call parks on IPC, so it runs off the async runtime.
/// A lost join is indistinguishable from a lost helper: both are `Session`.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> epic::Result<T> + Send + 'static,
) -> Result<T, EpicError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| epic::Error::Session)?
        .map_err(Into::into)
}

struct Admission {
    cancel: sailover_core::transfer::CancelToken,
    client: Arc<std::sync::Mutex<Option<Arc<GameSession>>>>,
    armed: bool,
}
impl Drop for Admission {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
            if let Some(client) = &*self.client.lock().unwrap() {
                client.cancel_preparation();
            }
        }
    }
}
#[uniffi::export(async_runtime = "tokio")]
impl EpicSession {
    pub async fn launch_game(
        &self,
        configuration: EpicCloudConfiguration,
        locations: SailOverLocations,
    ) -> Result<Arc<GameSession>, EpicError> {
        let cancel = sailover_core::transfer::CancelToken::new();
        let client_slot = Arc::new(std::sync::Mutex::new(None));
        let mut admission = Admission {
            cancel: cancel.clone(),
            client: Arc::clone(&client_slot),
            armed: true,
        };
        let configuration: epic::Configuration = configuration.into();
        // A Windows build runs in Wine, readied before any exchange code exists.
        let wine = match configuration.prefix.is_some() {
            false => None,
            true => {
                let configuration = configuration.clone();
                Some(
                    blocking(move || {
                        let runtime = game_runtime::voile::runtime(Path::new(&locations.voile))
                            .ok_or(epic::Error::WineUnavailable)?;
                        epic::prepare_windows(
                            &configuration,
                            runtime,
                            Path::new(&locations.epic_diagnostics),
                        )
                    })
                    .await?,
                )
            }
        };
        let account = self.client.credentials().await?.account_id;
        let exchange = self.client.exchange_code().await?;
        if self.client.credentials().await?.account_id != account {
            return Err(EpicError::Unauthorized);
        }
        let session = tokio::task::spawn_blocking(move || {
            let session = GameSession::epic(epic::start(
                configuration,
                account,
                exchange,
                wine,
                cancel.clone(),
            )?);
            *client_slot.lock().unwrap() = Some(Arc::clone(&session));
            if cancel.is_cancelled() {
                session.cancel_preparation();
            }
            Ok::<_, epic::Error>(session)
        })
        .await
        .map_err(|_| epic::Error::Session)??;
        // Completion transfers cancellation to the observer's explicit controls.
        admission.armed = false;
        Ok(session)
    }
    pub async fn archive_cloud(
        &self,
        configuration: EpicCloudConfiguration,
    ) -> Result<(), EpicError> {
        epic::archive(configuration.into(), &self.client)
            .await
            .map_err(Into::into)
    }
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn recover_epic_game_session(
    session_directory: String,
    helper: String,
) -> Result<Option<Arc<GameSession>>, EpicError> {
    blocking(move || {
        Ok(
            epic::Client::recover(Path::new(&session_directory), Path::new(&helper))?
                .map(GameSession::epic),
        )
    })
    .await
}
#[uniffi::export(async_runtime = "tokio")]
pub async fn epic_cloud_removed(
    configuration: EpicCloudConfiguration,
    account_id: String,
) -> Result<(), EpicError> {
    blocking(move || epic::removed(&configuration.into(), &account_id)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn failed_snapshot(error: &str) -> Value {
        json!({
            "version": 2,
            "id": "fixture-session",
            "account_id": "fixture-account",
            "store": {"namespace": "fixture-namespace", "app_name": "fixture-game"},
            "phase": "Failed",
            "cloud": {"phase": "Failed", "decision_id": 1, "allowed_choices": []},
            "error": error,
            "exit_code": null,
            "revision": 2,
            "supervisor": {
                "pid": 42,
                "uid": 501,
                "birth_seconds": 123,
                "birth_microseconds": 456,
                "executable": "/fixture/helper"
            },
            "game": null,
            "starting": null,
            "socket": "/fixture/control.sock"
        })
    }

    fn bridge_snapshot(value: Value) -> crate::GameSnapshot {
        let snapshot: epic::Snapshot = serde_json::from_value(value).unwrap();
        snapshot.into()
    }

    #[test]
    fn epic_publishes_the_shared_rosetta_decision() {
        let mut value = failed_snapshot("fixture");
        value["phase"] = json!("Preparing");
        value["rosetta"] = json!({"decision_id": 17});
        let snapshot = bridge_snapshot(value);
        assert_eq!(snapshot.rosetta.unwrap().decision_id, 17);
    }

    #[test]
    fn old_epic_snapshots_have_no_rosetta_requirement() {
        assert!(
            bridge_snapshot(failed_snapshot("fixture"))
                .rosetta
                .is_none()
        );
    }

    #[test]
    fn typed_cancellation_reaches_the_bridge_independently_of_its_message() {
        let message = "A renamed cancellation message";
        let mut value = failed_snapshot(message);
        value["failure_reason"] = json!("Cancelled");

        let snapshot = bridge_snapshot(value);
        assert!(matches!(snapshot.phase, crate::GamePhase::Failed));
        assert!(matches!(
            snapshot.failure_reason,
            Some(crate::GameFailureReason::Cancelled)
        ));
        assert_eq!(snapshot.error.as_deref(), Some(message));
    }

    #[test]
    fn typed_failure_is_not_reclassified_by_a_legacy_cancellation_message() {
        for message in ["backend.epic.cloud.cancelled", "backend.epic.cancelled"] {
            let mut value = failed_snapshot(message);
            value["failure_reason"] = json!("Failed");

            let snapshot = bridge_snapshot(value);
            assert!(matches!(
                snapshot.failure_reason,
                Some(crate::GameFailureReason::Failed)
            ));
            assert_eq!(snapshot.error.as_deref(), Some(message));
        }
    }

    #[test]
    fn legacy_terminal_snapshots_reach_the_bridge_with_classified_failures() {
        for message in ["backend.epic.cloud.cancelled", "backend.epic.cancelled"] {
            let value = failed_snapshot(message);
            assert!(value.get("failure_reason").is_none());
            let snapshot = bridge_snapshot(value);
            assert!(matches!(
                snapshot.failure_reason,
                Some(crate::GameFailureReason::Cancelled)
            ));
            assert_eq!(snapshot.error.as_deref(), Some(message));
        }

        let snapshot = bridge_snapshot(failed_snapshot("backend.epic.unauthorized"));
        assert!(matches!(
            snapshot.failure_reason,
            Some(crate::GameFailureReason::Failed)
        ));
        assert_eq!(snapshot.error.as_deref(), Some("backend.epic.unauthorized"));
    }

    #[test]
    fn cancellation_outside_a_failed_phase_does_not_export_a_failure_reason() {
        for phase in ["Preparing", "Running", "Finalizing", "Finished"] {
            for reason in [None, Some("Cancelled")] {
                let mut value = failed_snapshot("backend.epic.cloud.cancelled");
                value["phase"] = json!(phase);
                if let Some(reason) = reason {
                    value["failure_reason"] = json!(reason);
                }
                assert!(bridge_snapshot(value).failure_reason.is_none());
            }
        }
    }
}
