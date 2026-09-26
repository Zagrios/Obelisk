//! Presentation of the Rust-owned Epic supervisor and Cloud decisions.
use crate::{
    BeforeGameStart, EpicError, EpicSession, GameAdmission, GameSessionError, SailOverLocations,
    game_session::Admitted,
};
use game_runtime::epic;
use std::sync::Arc;

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

#[uniffi::export(async_runtime = "tokio")]
impl EpicSession {
    /// Launches the build `configuration` names for its admission, readying
    /// Voile and the prefix of a Windows build before any exchange code
    /// exists, and runs `before_start`, which saves the credentials the
    /// exchange rotated, before the game starts. Then follows its session
    /// until it is finished.
    pub async fn launch_game(
        &self,
        admission: Arc<GameAdmission>,
        configuration: EpicCloudConfiguration,
        locations: SailOverLocations,
        before_start: Arc<dyn BeforeGameStart>,
    ) -> Result<(), GameSessionError> {
        use game_runtime::sessions::{Failure, Handoff};
        let Admitted::Epic(slot) = &admission.admission else {
            return Err(GameSessionError::new("backend.epic.cloud.invalidSession"));
        };
        let admission = slot.take()?;
        let canceller = admission.canceller();
        let client = self.client.clone();
        let configuration: epic::Configuration = configuration.into();
        let refused = |error: EpicError| Handoff::Refused(Failure::Failed(error.to_string()));
        let handoff = async move {
            // A Windows build runs in Wine, readied before any exchange code exists.
            let wine = match configuration.prefix.is_some() {
                false => None,
                true => {
                    let locations: game_runtime::Locations = (&locations).into();
                    canceller
                        .install_runtime(&locations.voile)
                        .await
                        .map_err(Handoff::Refused)?;
                    let configuration = configuration.clone();
                    Some(
                        blocking(move || epic::prepare_windows(&configuration, &locations))
                            .await
                            .map_err(refused)?,
                    )
                }
            };
            let account = client
                .credentials()
                .await
                .map_err(|e| refused(e.into()))?
                .account_id;
            let exchange = client
                .exchange_code()
                .await
                .map_err(|e| refused(e.into()))?;
            if client
                .credentials()
                .await
                .map_err(|e| refused(e.into()))?
                .account_id
                != account
            {
                return Err(refused(EpicError::Unauthorized));
            }
            if canceller.cancelled() {
                return Err(Handoff::Refused(Failure::Cancelled));
            }
            let cancel = sailover_core::transfer::CancelToken::new();
            let token = cancel.clone();
            canceller.on_cancel(move || token.cancel());
            let started = tokio::task::spawn_blocking(move || {
                epic::start(configuration, account, exchange, wine, cancel)
            })
            .await
            .map_err(|_| Handoff::Lost(epic::Error::Session.to_string()))?
            .map_err(|error| Handoff::Lost(error.to_string()))?;
            Ok(Arc::new(started) as Arc<dyn game_runtime::sessions::Supervised<epic::Epic>>)
        };
        // The exchange code rotated the account's credentials: they are saved
        // before the game starts.
        Ok(admission
            .launch_before(handoff, Box::new(move || before_start.run()))
            .await?)
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

    fn bridge_snapshot(value: Value) -> game_runtime::sessions::Observation<epic::Epic> {
        let snapshot: epic::Snapshot = serde_json::from_value(value).unwrap();
        snapshot.into()
    }

    #[test]
    fn epic_publishes_the_shared_rosetta_decision() {
        let mut value = failed_snapshot("fixture");
        value["phase"] = json!("Preparing");
        value["rosetta"] = json!({"decision_id": 17});
        let snapshot = bridge_snapshot(value);
        assert_eq!(snapshot.rosetta.unwrap(), 17);
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
        assert!(matches!(snapshot.phase, epic::Phase::Failed));
        assert!(matches!(
            snapshot.failure,
            Some(epic::FailureReason::Cancelled)
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
                snapshot.failure,
                Some(epic::FailureReason::Failed)
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
                snapshot.failure,
                Some(epic::FailureReason::Cancelled)
            ));
            assert_eq!(snapshot.error.as_deref(), Some(message));
        }

        let snapshot = bridge_snapshot(failed_snapshot("backend.epic.unauthorized"));
        assert!(matches!(
            snapshot.failure,
            Some(epic::FailureReason::Failed)
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
                assert!(bridge_snapshot(value).failure.is_none());
            }
        }
    }
}
