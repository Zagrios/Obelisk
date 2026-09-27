//! Cloud indicators, and the sync a storefront stand-in runs for Cloud
//! maintenance, across UniFFI. An account's installations start maintenance
//! and answer its Cloud question.
use crate::{CloudChoice, CloudPhase, EpicGameId};
use game_runtime::{cloud, cloud_status};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CloudSavedState {
    RecoveryNeeded,
    UploadUncertain,
    Conflict,
    Pending,
    Unavailable,
    LocalChanges,
    LastFailed,
    Synced,
    NeverSynced,
}

impl From<cloud_status::SavedState> for CloudSavedState {
    fn from(state: cloud_status::SavedState) -> Self {
        use cloud_status::SavedState;
        match state {
            SavedState::RecoveryNeeded => Self::RecoveryNeeded,
            SavedState::UploadUncertain => Self::UploadUncertain,
            SavedState::Conflict => Self::Conflict,
            SavedState::Pending => Self::Pending,
            SavedState::Unavailable => Self::Unavailable,
            SavedState::LocalChanges => Self::LocalChanges,
            SavedState::LastFailed => Self::LastFailed,
            SavedState::Synced => Self::Synced,
            SavedState::NeverSynced => Self::NeverSynced,
        }
    }
}

/// A game's Cloud indicator from persisted state. Swift adds the live states:
/// a sync in progress, a running game, and a game guard with no live session.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CloudIndicator {
    pub state: CloudSavedState,
    pub game_session: bool,
    /// Last Cloud sync, in Unix seconds.
    pub synced_at: Option<u64>,
    pub pending_upload: bool,
    pub installation_removed: bool,
}

impl From<cloud_status::Indicator> for CloudIndicator {
    fn from(indicator: cloud_status::Indicator) -> Self {
        Self {
            state: indicator.state.into(),
            game_session: indicator.game_session,
            synced_at: indicator.synced_at,
            pending_upload: indicator.pending_upload,
            installation_removed: indicator.installation_removed,
        }
    }
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SteamCloudIndicator {
    pub app_id: u32,
    pub indicator: CloudIndicator,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct EpicCloudIndicator {
    pub game: EpicGameId,
    pub indicator: CloudIndicator,
}

/// A Cloud maintenance sync, as a storefront stand-in runs it: it shows the
/// sync's progress and asks the player.
#[derive(uniffi::Object)]
pub struct CloudSync(pub(crate) Arc<cloud::Control>);

#[uniffi::export(async_runtime = "tokio")]
impl CloudSync {
    /// Shows progress. Any question shown is closed.
    pub fn show(&self, phase: CloudPhase, error: Option<String>) {
        self.0.show(phase.into(), error);
    }

    /// Asks about a conflict and waits for the answer: `None` once the player
    /// cancels or the account signs out.
    pub async fn ask_conflict(&self, error: Option<String>) -> Option<CloudChoice> {
        self.0
            .ask(cloud::Question::Conflict, error)
            .await
            .ok()
            .map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indicators_keep_every_saved_state_and_their_facts() {
        use cloud_status::SavedState;
        let states = [
            (SavedState::RecoveryNeeded, CloudSavedState::RecoveryNeeded),
            (
                SavedState::UploadUncertain,
                CloudSavedState::UploadUncertain,
            ),
            (SavedState::Conflict, CloudSavedState::Conflict),
            (SavedState::Pending, CloudSavedState::Pending),
            (SavedState::Unavailable, CloudSavedState::Unavailable),
            (SavedState::LocalChanges, CloudSavedState::LocalChanges),
            (SavedState::LastFailed, CloudSavedState::LastFailed),
            (SavedState::Synced, CloudSavedState::Synced),
            (SavedState::NeverSynced, CloudSavedState::NeverSynced),
        ];
        for (saved, exported) in states {
            let indicator = CloudIndicator::from(cloud_status::Indicator {
                state: saved,
                game_session: true,
                synced_at: Some(1_789_000_000),
                pending_upload: true,
                installation_removed: true,
            });
            assert_eq!(
                indicator,
                CloudIndicator {
                    state: exported,
                    game_session: true,
                    synced_at: Some(1_789_000_000),
                    pending_upload: true,
                    installation_removed: true,
                }
            );
        }
    }
}
