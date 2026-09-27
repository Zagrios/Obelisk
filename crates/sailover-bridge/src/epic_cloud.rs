//! Presentation of the Rust-owned Epic supervisor and Cloud decisions.
use crate::EpicError;
use game_runtime::epic;

impl From<epic::Error> for EpicError {
    fn from(error: epic::Error) -> Self {
        Self::Service {
            message: error.to_string(),
        }
    }
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
