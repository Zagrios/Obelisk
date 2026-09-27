//! Cloud indicators across UniFFI. An account's installations start
//! maintenance and answer its Cloud question.
use game_runtime::cloud_status;

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
