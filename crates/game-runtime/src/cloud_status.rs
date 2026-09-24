//! Per-scope Cloud markers that the indicator reads without any lock. Both
//! storefront adapters write them beside the vault state they describe, while
//! they hold that storefront's vault. A marker is display state only.
use cloud_vault::StoreScope;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE: &str = "cloud-status.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The game this record describes. An Epic directory name is a hash, so
    /// a reader needs this to map the directory back to its game.
    pub scope: Option<StoreScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<Unavailable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<Problem>,
}

/// The last check found that Cloud cannot sync this game.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unavailable {
    pub reason: UnavailableReason,
    pub at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnavailableReason {
    /// The game has no Cloud saves on this storefront.
    NoCloud,
    /// The game's Cloud rules are not supported on macOS.
    Unsupported,
    /// Cloud is turned off in the storefront's preferences.
    Disabled,
}

/// The last sync ended without a confirmed sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    pub kind: ProblemKind,
    /// Localization key of the failure that ended it.
    pub key: String,
    pub at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProblemKind {
    /// It failed, was cancelled, or the game launched without it, and no
    /// pending work remains to show that.
    Failed,
    /// A conflict question ended without an answer.
    Conflict,
}

fn path(root: &Path, scope: &StoreScope) -> cloud_vault::Result<PathBuf> {
    Ok(scope.directory(root)?.join(FILE))
}

/// A missing file reads as an empty record.
pub fn read(root: &Path, scope: &StoreScope) -> cloud_vault::Result<Record> {
    Ok(cloud_vault::journal::read(&path(root, scope)?)?.unwrap_or_default())
}

/// The caller holds this scope's vault, which serializes writers.
pub(crate) fn update(
    root: &Path,
    scope: &StoreScope,
    change: impl FnOnce(&mut Record),
) -> cloud_vault::Result<()> {
    let mut record = read(root, scope)?;
    let before = record.clone();
    record.scope = Some(scope.clone());
    change(&mut record);
    if record == before {
        return Ok(());
    }
    cloud_vault::journal::write(&path(root, scope)?, &record)
}

pub(crate) fn unavailable(reason: UnavailableReason) -> impl FnOnce(&mut Record) {
    move |record| {
        if record.unavailable.as_ref().map(|u| u.reason) != Some(reason) {
            record.unavailable = Some(Unavailable { reason, at: now() });
        }
    }
}

pub(crate) fn available(record: &mut Record) {
    record.unavailable = None;
}

pub(crate) fn problem(kind: ProblemKind, key: &str) -> impl FnOnce(&mut Record) + '_ {
    move |record| {
        record.problem = Some(Problem {
            kind,
            key: key.into(),
            at: now(),
        });
    }
}

pub(crate) fn synced(record: &mut Record) {
    record.problem = None;
}

/// A conflict that got its answer is no longer unanswered.
pub(crate) fn answered(record: &mut Record) {
    if record
        .problem
        .as_ref()
        .is_some_and(|problem| problem.kind == ProblemKind::Conflict)
    {
        record.problem = None;
    }
}

/// What one game's indicator shows from persisted state. Forge adds the live
/// states on top: a sync in progress and a running game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavedState {
    RecoveryNeeded,
    UploadUncertain,
    Conflict,
    Pending,
    Unavailable,
    LocalChanges,
    LastFailed,
    /// Without `synced_at`, the sync predates the Last Cloud sync record.
    Synced,
    NeverSynced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indicator {
    pub state: SavedState,
    /// A game session's guard is set. Only Forge's live sessions tell
    /// whether that game still runs or its supervisor died.
    pub game_session: bool,
    pub synced_at: Option<u64>,
    /// Saves wait to be sent. An uninstalled game shows its indicator only
    /// while this holds.
    pub pending_upload: bool,
    pub installation_removed: bool,
}

/// A storefront context's facts that the vault summary cannot know.
#[derive(Clone, Debug, Default)]
pub(crate) struct Journal {
    /// An exit save waits to be sent.
    pub pending: bool,
    /// An upload transaction may have committed.
    pub uncertain: bool,
    /// A chosen Cloud version is being applied.
    pub applying: bool,
    pub installation_removed: bool,
}

/// The first matching row wins. A switch plan that belongs to another game is
/// transient and does not concern this one.
pub(crate) fn indicator(
    scope: &StoreScope,
    vault: &cloud_vault::Summary,
    journal: &Journal,
    record: &Record,
) -> Indicator {
    let problem = record.problem.as_ref().map(|problem| problem.kind);
    let uncertain =
        vault.pending == Some(cloud_vault::UploadState::MayHaveCommitted) || journal.uncertain;
    let pending = journal.pending || vault.pending.is_some();
    let state = if vault.applying || journal.applying || vault.switching.as_ref() == Some(scope) {
        SavedState::RecoveryNeeded
    } else if uncertain {
        SavedState::UploadUncertain
    } else if problem == Some(ProblemKind::Conflict) {
        SavedState::Conflict
    } else if pending {
        SavedState::Pending
    } else if record.unavailable.is_some() {
        SavedState::Unavailable
    } else if vault.local_changes && !journal.installation_removed {
        SavedState::LocalChanges
    } else if problem == Some(ProblemKind::Failed) {
        SavedState::LastFailed
    } else if vault.baseline {
        SavedState::Synced
    } else {
        SavedState::NeverSynced
    };
    Indicator {
        state,
        game_session: vault.game_session,
        synced_at: vault.synced_at,
        pending_upload: pending || uncertain,
        installation_removed: journal.installation_removed,
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud_vault::{Summary, UploadState};

    fn scope(app_id: u32) -> StoreScope {
        cloud_vault::Scope {
            steam_id: 0x0110_0001_0000_0001,
            app_id,
            cloud_app_id: app_id,
        }
        .into()
    }

    fn synced() -> Summary {
        Summary {
            baseline: true,
            synced_at: Some(1_000),
            ..Summary::default()
        }
    }

    fn problem(kind: ProblemKind) -> Record {
        Record {
            problem: Some(Problem {
                kind,
                key: "fixture".into(),
                at: 1,
            }),
            ..Record::default()
        }
    }

    fn unavailable() -> Record {
        Record {
            unavailable: Some(Unavailable {
                reason: UnavailableReason::Disabled,
                at: 1,
            }),
            ..Record::default()
        }
    }

    #[test]
    fn the_first_matching_saved_state_wins() {
        let pending = Journal {
            pending: true,
            ..Journal::default()
        };
        let removed = Journal {
            installation_removed: true,
            ..Journal::default()
        };
        let changed = Summary {
            local_changes: true,
            ..synced()
        };
        let cases = [
            (
                Summary {
                    applying: true,
                    ..synced()
                },
                pending.clone(),
                problem(ProblemKind::Conflict),
                SavedState::RecoveryNeeded,
            ),
            (
                Summary {
                    switching: Some(scope(42)),
                    ..synced()
                },
                Journal::default(),
                Record::default(),
                SavedState::RecoveryNeeded,
            ),
            (
                Summary {
                    switching: Some(scope(7)),
                    ..synced()
                },
                Journal::default(),
                Record::default(),
                SavedState::Synced,
            ),
            (
                synced(),
                Journal {
                    applying: true,
                    ..Journal::default()
                },
                Record::default(),
                SavedState::RecoveryNeeded,
            ),
            (
                Summary {
                    pending: Some(UploadState::MayHaveCommitted),
                    ..synced()
                },
                Journal::default(),
                problem(ProblemKind::Conflict),
                SavedState::UploadUncertain,
            ),
            (
                synced(),
                Journal {
                    uncertain: true,
                    ..Journal::default()
                },
                Record::default(),
                SavedState::UploadUncertain,
            ),
            (
                synced(),
                pending.clone(),
                problem(ProblemKind::Conflict),
                SavedState::Conflict,
            ),
            (
                Summary {
                    pending: Some(UploadState::Queued),
                    ..synced()
                },
                Journal::default(),
                unavailable(),
                SavedState::Pending,
            ),
            (synced(), pending, Record::default(), SavedState::Pending),
            (
                changed.clone(),
                Journal::default(),
                unavailable(),
                SavedState::Unavailable,
            ),
            (
                changed.clone(),
                Journal::default(),
                problem(ProblemKind::Failed),
                SavedState::LocalChanges,
            ),
            (
                changed,
                removed,
                problem(ProblemKind::Failed),
                SavedState::LastFailed,
            ),
            (
                synced(),
                Journal::default(),
                Record::default(),
                SavedState::Synced,
            ),
            (
                Summary {
                    synced_at: None,
                    ..synced()
                },
                Journal::default(),
                Record::default(),
                SavedState::Synced,
            ),
            (
                Summary::default(),
                Journal::default(),
                Record::default(),
                SavedState::NeverSynced,
            ),
        ];
        for (index, (vault, journal, record, expected)) in cases.into_iter().enumerate() {
            assert_eq!(
                indicator(&scope(42), &vault, &journal, &record).state,
                expected,
                "case {index}"
            );
        }
    }

    #[test]
    fn an_indicator_carries_its_game_guard_date_and_pending_work() {
        let vault = Summary {
            game_session: true,
            pending: Some(UploadState::MayHaveCommitted),
            ..synced()
        };
        let journal = Journal {
            installation_removed: true,
            ..Journal::default()
        };

        let shown = indicator(&scope(42), &vault, &journal, &Record::default());

        assert!(shown.game_session);
        assert_eq!(shown.synced_at, Some(1_000));
        assert!(shown.pending_upload);
        assert!(shown.installation_removed);
        assert!(
            !indicator(
                &scope(42),
                &synced(),
                &Journal::default(),
                &Record::default()
            )
            .pending_upload
        );
    }
}
