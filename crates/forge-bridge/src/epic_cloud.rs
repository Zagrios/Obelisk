//! Presentation of the Rust-owned Epic supervisor and Cloud decisions.
use crate::{EpicError, EpicSession};
use std::{path::Path, sync::Arc};
use steam_runtime::epic;

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
        }
    }
}
macro_rules! bridge_enum {
    ($name:ident, $source:ident, $($variant:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, uniffi::Enum)]
        pub enum $name { $($variant),+ }
        impl From<epic::$source> for $name {
            fn from(value: epic::$source) -> Self { match value { $(epic::$source::$variant => Self::$variant),+ } }
        }
        impl From<$name> for epic::$source {
            fn from(value: $name) -> Self { match value { $($name::$variant => Self::$variant),+ } }
        }
    };
}
bridge_enum!(
    EpicGamePhase,
    Phase,
    Preparing,
    Running,
    Finalizing,
    Finished,
    Failed
);
bridge_enum!(EpicGameFailureReason, FailureReason, Cancelled, Failed);
bridge_enum!(
    EpicCloudPhase,
    CloudPhase,
    Checking,
    Ready,
    Syncing,
    Synced,
    Conflict,
    Pending,
    Disabled,
    Unsupported,
    Failed
);
bridge_enum!(
    EpicCloudChoice,
    Choice,
    Local,
    Cloud,
    Retry,
    Cancel,
    LaunchAnyway
);

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicGameSnapshot {
    pub id: String,
    pub account_id: String,
    pub namespace: String,
    pub app_name: String,
    pub phase: EpicGamePhase,
    pub cloud_phase: EpicCloudPhase,
    pub decision_id: u64,
    pub allowed_choices: Vec<EpicCloudChoice>,
    pub error: Option<String>,
    pub failure_reason: Option<EpicGameFailureReason>,
    pub exit_code: Option<i32>,
    pub revision: u64,
    pub reconnecting: bool,
}
impl From<epic::Snapshot> for EpicGameSnapshot {
    fn from(s: epic::Snapshot) -> Self {
        let failure_reason = s.failure_reason().map(Into::into);
        Self {
            id: s.id,
            account_id: s.account_id,
            namespace: s.namespace,
            app_name: s.app_name,
            phase: s.phase.into(),
            cloud_phase: s.cloud_phase.into(),
            decision_id: s.decision_id,
            allowed_choices: s.allowed_choices.into_iter().map(Into::into).collect(),
            error: s.error,
            failure_reason,
            exit_code: s.exit_code,
            revision: s.revision,
            reconnecting: s.reconnecting,
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

#[derive(uniffi::Object)]
pub struct EpicGameSession {
    client: Arc<epic::Client>,
}
#[uniffi::export(async_runtime = "tokio")]
impl EpicGameSession {
    pub async fn begin(self: Arc<Self>) -> Result<(), EpicError> {
        blocking(move || self.client.begin()).await
    }
    pub async fn snapshot(
        self: Arc<Self>,
        after_revision: Option<u64>,
    ) -> Result<EpicGameSnapshot, EpicError> {
        blocking(move || self.client.snapshot(after_revision).map(Into::into)).await
    }
    pub fn cancel_preparation(&self) {
        self.client.cancel_preparation();
    }
    pub async fn choose(
        self: Arc<Self>,
        account_id: String,
        decision_id: u64,
        choice: EpicCloudChoice,
    ) -> Result<(), EpicError> {
        blocking(move || self.client.choose(&account_id, decision_id, choice.into())).await
    }
    pub async fn revoke_cloud(self: Arc<Self>, account_id: String) -> Result<(), EpicError> {
        blocking(move || self.client.revoke_cloud(&account_id)).await
    }
}

struct Admission {
    cancel: forge_core::transfer::CancelToken,
    client: Arc<std::sync::Mutex<Option<Arc<epic::Client>>>>,
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
impl EpicSession {
    async fn supervised(
        &self,
        configuration: EpicCloudConfiguration,
        launch: bool,
    ) -> Result<Arc<EpicGameSession>, EpicError> {
        let cancel = forge_core::transfer::CancelToken::new();
        let client_slot = Arc::new(std::sync::Mutex::new(None));
        let mut admission = Admission {
            cancel: cancel.clone(),
            client: Arc::clone(&client_slot),
            armed: true,
        };
        let account = self.client.credentials().await?.account_id;
        let exchange = self.client.exchange_code().await?;
        if self.client.credentials().await?.account_id != account {
            return Err(EpicError::Unauthorized);
        }
        let client = tokio::task::spawn_blocking(move || {
            let client = Arc::new(epic::start(
                configuration.into(),
                account,
                exchange,
                launch,
                cancel.clone(),
            )?);
            *client_slot.lock().unwrap() = Some(Arc::clone(&client));
            if cancel.is_cancelled() {
                client.cancel_preparation();
            }
            Ok::<_, epic::Error>(client)
        })
        .await
        .map_err(|_| epic::Error::Session)??;
        // Completion transfers cancellation to the observer's explicit controls.
        admission.armed = false;
        Ok(Arc::new(EpicGameSession { client }))
    }
}
#[uniffi::export(async_runtime = "tokio")]
impl EpicSession {
    pub async fn launch_game(
        &self,
        configuration: EpicCloudConfiguration,
    ) -> Result<Arc<EpicGameSession>, EpicError> {
        self.supervised(configuration, true).await
    }
    pub async fn resume_cloud(
        &self,
        configuration: EpicCloudConfiguration,
    ) -> Result<Arc<EpicGameSession>, EpicError> {
        self.supervised(configuration, false).await
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
) -> Result<Option<Arc<EpicGameSession>>, EpicError> {
    blocking(move || {
        Ok(
            epic::Client::recover(Path::new(&session_directory), Path::new(&helper))?.map(
                |client| {
                    Arc::new(EpicGameSession {
                        client: Arc::new(client),
                    })
                },
            ),
        )
    })
    .await
}
#[uniffi::export]
pub fn epic_cloud_pending(
    root: String,
    account_id: String,
) -> Result<Vec<EpicCloudConfiguration>, EpicError> {
    Ok(epic::pending(Path::new(&root), &account_id)?
        .into_iter()
        .map(Into::into)
        .collect())
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
            "id": "fixture-session",
            "account_id": "fixture-account",
            "namespace": "fixture-namespace",
            "app_name": "fixture-game",
            "phase": "Failed",
            "cloud_phase": "Failed",
            "decision_id": 1,
            "allowed_choices": [],
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

    fn bridge_snapshot(value: Value) -> EpicGameSnapshot {
        let snapshot: epic::Snapshot = serde_json::from_value(value).unwrap();
        snapshot.into()
    }

    #[test]
    fn typed_cancellation_reaches_the_bridge_independently_of_its_message() {
        let message = "A renamed cancellation message";
        let mut value = failed_snapshot(message);
        value["failure_reason"] = json!("Cancelled");

        let snapshot = bridge_snapshot(value);
        assert!(matches!(snapshot.phase, EpicGamePhase::Failed));
        assert!(matches!(
            snapshot.failure_reason,
            Some(EpicGameFailureReason::Cancelled)
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
                Some(EpicGameFailureReason::Failed)
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
                Some(EpicGameFailureReason::Cancelled)
            ));
            assert_eq!(snapshot.error.as_deref(), Some(message));
        }

        let snapshot = bridge_snapshot(failed_snapshot("backend.epic.unauthorized"));
        assert!(matches!(
            snapshot.failure_reason,
            Some(EpicGameFailureReason::Failed)
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
