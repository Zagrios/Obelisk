use crate::{LocalIntent, Manifest, RemoteView};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Decision {
    Converged,
    KeepLocal,
    Download,
    Conflict,
}

/// Pure three-way comparison of complete content, including deletions. The
/// caller cannot represent an unavailable remote as a successful empty view.
pub fn reconcile(
    baseline: Option<&RemoteView>,
    local: &Manifest,
    remote: &RemoteView,
    intent: Option<&LocalIntent>,
) -> Decision {
    if local == &remote.files {
        return Decision::Converged;
    }
    if let Some(intent) = intent {
        return if &intent.against == remote {
            Decision::KeepLocal
        } else {
            // A stale local choice never authorizes replacing a newer remote.
            Decision::Conflict
        };
    }
    match baseline {
        Some(base) => match (local != &base.files, remote.files != base.files) {
            (false, true) => Decision::Download,
            (true, false) => Decision::KeepLocal,
            _ => Decision::Conflict,
        },
        None if local.0.is_empty() => Decision::Download,
        None if remote.files.0.is_empty() => Decision::KeepLocal,
        None => Decision::Conflict,
    }
}
