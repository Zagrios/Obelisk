//! The Cloud sync through its interface, once for every storefront. Each
//! harness puts an in-memory storefront Cloud behind its real adapter.
use super::*;
use crate::cloud::Status;
use crate::cloud_status::{Indicator, Record, SavedState};
use crate::supervisor::client::no_other_session;
use std::{fs, path::PathBuf, time::Duration};

type Of<H> = <<H as Harness>::Store as Storefront>::Failure;

/// A storefront under test: one game with Cloud rules for its save file.
#[allow(async_fn_in_trait)]
pub(crate) trait Harness: Sized + 'static {
    type Store: Storefront;
    /// What the storefront reports when the game has no Cloud rules.
    const NO_RULES: UnavailableReason;
    fn game() -> Self;
    fn control(&self) -> &Arc<Control>;
    /// A new sync of the game, for a new Game session.
    fn open(&self) -> CloudSync<Self::Store>;
    /// The save file, and its name in the Cloud.
    fn save(&self) -> PathBuf;
    fn slot(&self) -> &'static str;
    fn install(&self) -> PathBuf;
    /// Another device publishes this save.
    fn publish(&self, bytes: &[u8]);
    fn cloud(&self) -> Option<Vec<u8>>;
    fn uploads(&self) -> usize;
    /// The next remote step fails. A transient failure is retried; the
    /// other one ends the check at once and still lets a launch go on.
    fn fail_next(&self, transient: bool);
    fn failure_key(&self) -> String;
    fn set_rules(&self, available: bool);
    /// Archives the saves, removes the installation and records the removal.
    async fn uninstall(&self);
    async fn maintain(&self, alive: &SessionAlive<'_>) -> Result<(), Of<Self>>;
    /// The storefront's durable work, as a new sync reads it.
    fn work(&self) -> Option<Work>;
    /// Pending discovery lists this game.
    fn listed(&self) -> bool;
    /// This game's Cloud indicator, whether or not it was requested.
    fn indicator(&self, requested: bool) -> Option<Indicator>;
    fn root(&self) -> PathBuf;
    fn scope(&self) -> StoreScope;
    fn account(&self) -> AccountIdentity;
}

fn status<H: Harness>(game: &H) -> Status {
    game.control().status()
}
fn record<H: Harness>(game: &H) -> Record {
    cloud_status::read(&game.root(), &game.scope()).unwrap()
}
fn vault<H: Harness>(game: &H) -> Vault {
    Vault::open(&game.root(), game.scope()).unwrap()
}
fn pending<H: Harness>(game: &H) -> Option<Pending> {
    game.work().and_then(|work| work.pending)
}
fn uploading(pending: Option<Pending>) -> Option<Manifest> {
    match pending {
        Some(Pending::Upload { desired }) => Some(desired),
        _ => None,
    }
}
fn conflict_key<H: Harness>() -> String {
    Of::<H>::conflict().key()
}

/// A first launch that finds both sides equal and records the baseline.
async fn synced<H: Harness>(game: &H, bytes: &[u8]) {
    fs::write(game.save(), bytes).unwrap();
    game.publish(bytes);
    game.open().prepare(&no_other_session).await.unwrap();
}

/// The game exits while the Cloud is unreachable: its exit save stays pending.
async fn played_offline<H: Harness>(game: &H, bytes: &[u8]) {
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    fs::write(game.save(), bytes).unwrap();
    game.fail_next(false);
    sync.finished(true).await;
    assert_eq!(status(game).phase, Phase::Pending);
}

/// The installation comes back without the saves the uninstall removed.
fn reinstall<H: Harness>(game: &H) {
    let _ = fs::remove_file(game.save());
    fs::create_dir_all(game.install()).unwrap();
    fs::create_dir_all(game.save().parent().unwrap()).unwrap();
}

async fn maintain<H: Harness>(game: &H) -> Result<(), Of<H>> {
    maintain_beside(game, &no_other_session).await
}
async fn maintain_beside<H: Harness>(game: &H, alive: &SessionAlive<'_>) -> Result<(), Of<H>> {
    // A maintenance never waits on a question it should not ask.
    tokio::time::timeout(Duration::from_secs(20), game.maintain(alive))
        .await
        .expect("maintenance finished")
}

/// Waits for a Cloud question newer than decision `after`.
fn next_question(control: &Control, after: u64) -> Status {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = control.status();
        if status.decision_id > after && !status.allowed_choices.is_empty() {
            return status;
        }
        assert!(Instant::now() < deadline, "no Cloud question after {after}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Answers the next Cloud question with `choice`, as the UI would. A
/// question already shown when this starts is an older one.
pub(crate) fn answer(control: &Arc<Control>, choice: Choice) -> std::thread::JoinHandle<()> {
    let control = Arc::clone(control);
    let shown = control.status().decision_id;
    std::thread::spawn(move || {
        let question = next_question(&control, shown);
        assert!(
            question.allowed_choices.contains(&choice),
            "{choice:?} is not offered"
        );
        control.answer(question.decision_id, choice).unwrap();
    })
}

pub(crate) async fn a_launch_with_equal_saves_is_ready_and_records_a_cloud_sync<H: Harness>() {
    let game = H::game();
    fs::write(game.save(), b"progress").unwrap();
    game.publish(b"progress");
    let mut sync = game.open();

    sync.prepare(&no_other_session).await.unwrap();

    assert_eq!(status(&game).phase, Phase::Ready);
    assert!(sync.vault.synced_at().is_some());
    assert_eq!(game.uploads(), 0);
}

pub(crate) async fn a_launch_downloads_a_newer_cloud_save<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    game.publish(b"from another device");

    game.open().prepare(&no_other_session).await.unwrap();

    assert_eq!(status(&game).phase, Phase::Ready);
    assert_eq!(fs::read(game.save()).unwrap(), b"from another device");
}

pub(crate) async fn an_unconfirmed_game_exit_only_captures_its_save<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    fs::write(game.save(), b"maybe played").unwrap();

    // The helper exited without confirming that the game ever ran.
    sync.finished(false).await;

    assert_eq!(game.cloud().unwrap(), b"old");
    assert_eq!(game.uploads(), 0);
    assert_eq!(status(&game).phase, Phase::Pending);
    assert!(game.listed());
}

pub(crate) async fn a_launch_revoked_before_spawn_withdraws_its_cloud_authorization<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    let before = format!("{:?}", sync.vault.authorize(game.account()).unwrap());
    game.control().revoke();

    sync.aborted();

    let after = format!("{:?}", sync.vault.authorize(game.account()).unwrap());
    assert_ne!(before, after, "the account's authorization moved on");
}

pub(crate) async fn a_game_exit_uploads_local_progress<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    fs::write(game.save(), b"new progress").unwrap();

    sync.finished(true).await;

    assert_eq!(status(&game).phase, Phase::Synced);
    assert_eq!(game.cloud().unwrap(), b"new progress");
    assert_eq!(game.uploads(), 1);
    assert_eq!(pending(&game), None);
}

pub(crate) async fn a_launch_conflict_asks_and_applies_the_cloud_choice<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    fs::write(game.save(), b"local progress").unwrap();
    game.publish(b"cloud progress");
    let answering = answer(game.control(), Choice::Cloud);

    game.open().prepare(&no_other_session).await.unwrap();

    answering.join().unwrap();
    assert_eq!(status(&game).phase, Phase::Ready);
    assert_eq!(fs::read(game.save()).unwrap(), b"cloud progress");
}

pub(crate) async fn a_failed_exit_upload_keeps_the_exit_save_pending<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;

    played_offline(&game, b"new progress").await;

    assert_eq!(status(&game).error, Some(game.failure_key()));
    assert_eq!(game.cloud().unwrap(), b"old");
    assert!(uploading(pending(&game)).is_some());
    assert!(game.listed());
}

pub(crate) async fn a_game_without_cloud_rules_is_unavailable_until_they_return<H: Harness>() {
    let game = H::game();
    game.set_rules(false);

    game.open().prepare(&no_other_session).await.unwrap();

    assert_eq!(status(&game).phase, Phase::Unsupported);
    assert_eq!(
        record(&game).unavailable.map(|marker| marker.reason),
        Some(H::NO_RULES)
    );
    assert_eq!(game.indicator(true).unwrap().state, SavedState::Unavailable);
    game.set_rules(true);
    synced(&game, b"progress").await;
    assert_eq!(record(&game).unavailable, None);
}

pub(crate) async fn saves_sailover_holds_never_pass_for_a_game_without_cloud<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    game.set_rules(false);
    let answering = answer(game.control(), Choice::LaunchAnyway);

    game.open().prepare(&no_other_session).await.unwrap();

    answering.join().unwrap();
    assert_eq!(status(&game).phase, Phase::Pending);
    assert_eq!(
        record(&game).problem.unwrap().key,
        Of::<H>::unsupported().key()
    );
}

pub(crate) async fn a_cancelled_launch_check_is_a_failed_sync_until_the_next_sync<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    game.fail_next(false);
    let answering = answer(game.control(), Choice::Cancel);

    assert!(game.open().prepare(&no_other_session).await.is_err());

    answering.join().unwrap();
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Failed);
    assert_eq!(problem.key, game.failure_key());
    game.open().prepare(&no_other_session).await.unwrap();
    assert_eq!(record(&game).problem, None);
}

pub(crate) async fn launching_anyway_without_pending_work_is_a_failed_sync<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    game.fail_next(false);
    let answering = answer(game.control(), Choice::LaunchAnyway);

    game.open().prepare(&no_other_session).await.unwrap();

    answering.join().unwrap();
    assert_eq!(status(&game).phase, Phase::Pending);
    assert_eq!(record(&game).problem.unwrap().kind, ProblemKind::Failed);
    assert_eq!(pending(&game), None, "unchanged saves are not pending");
}

pub(crate) async fn an_unanswered_launch_conflict_is_recorded_until_it_is_answered<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    fs::write(game.save(), b"local progress").unwrap();
    game.publish(b"cloud progress");
    let answering = answer(game.control(), Choice::Cancel);

    assert!(game.open().prepare(&no_other_session).await.is_err());

    answering.join().unwrap();
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Conflict);
    assert_eq!(problem.key, conflict_key::<H>());
    let answering = answer(game.control(), Choice::Local);
    game.open().prepare(&no_other_session).await.unwrap();
    answering.join().unwrap();
    assert_eq!(record(&game).problem, None);
    assert_eq!(game.cloud().unwrap(), b"local progress");
}

pub(crate) async fn a_first_conflict_is_durable_before_it_is_shown<H: Harness>() {
    let game = H::game();
    fs::write(game.save(), b"first local version").unwrap();
    game.publish(b"first cloud version");
    let control = Arc::clone(game.control());
    let shown = control.status().decision_id;
    let mut sync = game.open();

    // An observer may close or cancel as soon as the question shows.
    let (result, (durable, listed)) = tokio::join!(sync.prepare(&no_other_session), async {
        let question = loop {
            let status = control.status();
            if status.decision_id > shown && !status.allowed_choices.is_empty() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let seen = (uploading(pending(&game)), game.listed());
        control
            .answer(question.decision_id, Choice::Cancel)
            .unwrap();
        seen
    });

    assert!(result.is_err());
    assert!(listed);
    let desired = durable.expect("the local version waits");
    assert_eq!(
        sync.vault.read_content(&desired.0[game.slot()]).unwrap(),
        b"first local version"
    );
}

pub(crate) async fn a_choice_for_saves_changed_since_the_question_asks_again<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    fs::write(game.save(), b"local").unwrap();
    game.publish(b"cloud");
    let (control, save) = (Arc::clone(game.control()), game.save());
    let shown = control.status().decision_id;
    let answering = std::thread::spawn(move || {
        let first = next_question(&control, shown);
        fs::write(save, b"later local bytes").unwrap();
        control.answer(first.decision_id, Choice::Cloud).unwrap();
        // The Cloud choice was for saves that no longer exist.
        let again = next_question(&control, first.decision_id);
        control.answer(again.decision_id, Choice::Cancel).unwrap();
    });

    let result = game.open().prepare(&no_other_session).await;

    answering.join().unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read(game.save()).unwrap(), b"later local bytes");
    assert_eq!(game.cloud().unwrap(), b"cloud");
}

pub(crate) async fn pending_discovery_includes_uploads_the_vault_still_holds<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut vault = vault(&game);
    vault.set_preferences(cloud_vault::Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let files = Manifest([(game.slot().into(), vault.stage(b"exit", 2).unwrap())].into());
    let authorization = vault.authorize(game.account()).unwrap();
    let before = vault.baseline().unwrap().clone();
    vault
        .queue_upload(&files, &before, "fixture", authorization)
        .unwrap();
    drop(vault);

    assert_eq!(pending(&game), None);
    assert!(game.listed());
}

pub(crate) async fn a_game_exit_conflict_is_asked_live_and_answered<H: Harness>() {
    for (choice, kept) in [
        (Choice::Local, b"played here".as_slice()),
        (Choice::Cloud, b"played elsewhere".as_slice()),
    ] {
        let game = H::game();
        synced(&game, b"old").await;
        let mut sync = game.open();
        sync.prepare(&no_other_session).await.unwrap();
        sync.started().unwrap();
        fs::write(game.save(), b"played here").unwrap();
        game.publish(b"played elsewhere");
        let answering = answer(game.control(), choice);

        sync.finished(true).await;

        answering.join().unwrap();
        assert_eq!(status(&game).phase, Phase::Synced);
        assert_eq!(fs::read(game.save()).unwrap(), kept);
        assert_eq!(game.cloud().unwrap(), kept);
        assert_eq!(pending(&game), None);
        assert_eq!(record(&game).problem, None);
    }
}

pub(crate) async fn an_unanswered_exit_conflict_stays_pending_with_a_conflict_marker<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    fs::write(game.save(), b"played here").unwrap();
    game.publish(b"played elsewhere");
    let answering = answer(game.control(), Choice::Cancel);

    sync.finished(true).await;

    answering.join().unwrap();
    let shown = status(&game);
    assert_eq!(shown.phase, Phase::Pending);
    assert_eq!(shown.error, Some(conflict_key::<H>()));
    assert!(shown.allowed_choices.is_empty());
    assert!(uploading(pending(&game)).is_some());
    assert_eq!(fs::read(game.save()).unwrap(), b"played here");
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Conflict);
    assert_eq!(problem.key, conflict_key::<H>());
}

pub(crate) async fn a_stopped_exit_upload_waits_without_claiming_a_conflict<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    fs::write(game.save(), b"new progress").unwrap();
    game.fail_next(true);
    let control = Arc::clone(game.control());
    // Stops the exit sync during its wait before the second attempt.
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        control.cancel();
    });

    sync.finished(true).await;

    stopper.join().unwrap();
    let shown = status(&game);
    assert_eq!(shown.phase, Phase::Pending);
    assert_eq!(shown.error, Some(Of::<H>::from(Stop::Cancelled).key()));
    assert_eq!(record(&game).problem, None);
    assert!(uploading(pending(&game)).is_some());
}

pub(crate) async fn maintenance_of_a_synced_game_confirms_it_without_launching<H: Harness>() {
    let game = H::game();
    synced(&game, b"progress").await;

    maintain(&game).await.unwrap();

    assert_eq!(status(&game).phase, Phase::Synced);
    assert_eq!(game.uploads(), 0);
    assert_eq!(record(&game).problem, None);
}

pub(crate) async fn maintenance_sends_an_exit_save_left_pending<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"played offline").await;

    maintain(&game).await.unwrap();

    assert_eq!(status(&game).phase, Phase::Synced);
    assert_eq!(game.cloud().unwrap(), b"played offline");
    assert_eq!(pending(&game), None);
    assert!(!game.listed());
}

pub(crate) async fn maintenance_downloads_a_newer_cloud_save<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    game.publish(b"from another device");

    maintain(&game).await.unwrap();

    assert_eq!(status(&game).phase, Phase::Synced);
    assert_eq!(fs::read(game.save()).unwrap(), b"from another device");
}

pub(crate) async fn maintenance_asks_about_a_conflict_and_cancel_keeps_the_work_pending<
    H: Harness,
>() {
    let game = H::game();
    synced(&game, b"old").await;
    fs::write(game.save(), b"played here").unwrap();
    game.publish(b"played elsewhere");
    let answering = answer(game.control(), Choice::Cancel);

    let cancelled = maintain(&game).await.unwrap_err();

    answering.join().unwrap();
    assert_eq!(cancelled.kind(), Kind::Cancelled);
    assert_eq!(record(&game).problem.unwrap().kind, ProblemKind::Conflict);
    assert!(game.listed());
    let answering = answer(game.control(), Choice::Local);
    maintain(&game).await.unwrap();
    answering.join().unwrap();
    assert_eq!(game.cloud().unwrap(), b"played here");
    assert_eq!(record(&game).problem, None);
}

pub(crate) async fn maintenance_leaves_unchanged_saves_unqueued_when_it_fails<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    game.fail_next(false);

    let failed = maintain(&game).await.unwrap_err();

    assert_eq!(failed.key(), game.failure_key());
    assert!(status(&game).allowed_choices.is_empty());
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Failed);
    assert_eq!(problem.key, game.failure_key());
    assert_eq!(pending(&game), None);
    assert!(!game.listed());
}

pub(crate) async fn maintenance_clears_the_guard_of_a_dead_game_session<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.started().unwrap();
    fs::write(game.save(), b"played before a crash").unwrap();
    drop(sync);
    assert!(game.listed(), "the admitted game's exit save is scheduled");

    maintain(&game).await.unwrap();

    assert_eq!(game.cloud().unwrap(), b"played before a crash");
    assert!(vault(&game).game_session().is_none());
}

pub(crate) async fn maintenance_keeps_the_guard_of_a_game_session_that_still_runs<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.started().unwrap();
    fs::write(game.save(), b"still playing").unwrap();
    drop(sync);

    let refused = maintain_beside(&game, &|_| Ok(true)).await.unwrap_err();

    assert_eq!(refused.key(), Of::<H>::from(cloud_vault::Error::Busy).key());
    assert_eq!(game.cloud().unwrap(), b"old");
    assert!(vault(&game).game_session().is_some());
}

pub(crate) async fn maintenance_sends_the_exit_save_of_an_uninstalled_game<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"last session").await;
    game.uninstall().await;
    // A removed installation cannot prove its saves complete: the player
    // confirms that this exit save should replace the Cloud one.
    let answering = answer(game.control(), Choice::Local);

    maintain(&game).await.unwrap();

    answering.join().unwrap();
    assert_eq!(status(&game).phase, Phase::Synced);
    assert_eq!(game.cloud().unwrap(), b"last session");
    assert!(!game.install().exists());
    assert!(!game.listed());
}

pub(crate) async fn an_uninstalled_game_keeps_its_archive_when_the_cloud_is_chosen<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"last session").await;
    game.uninstall().await;
    game.publish(b"played elsewhere");
    let answering = answer(game.control(), Choice::Cloud);

    maintain(&game).await.unwrap();

    answering.join().unwrap();
    let vault = vault(&game);
    let baseline = vault.baseline().unwrap();
    assert_eq!(
        vault.read_content(&baseline.files.0[game.slot()]).unwrap(),
        b"played elsewhere"
    );
    let archived = vault.local().unwrap();
    assert_eq!(
        vault.read_content(&archived.0[game.slot()]).unwrap(),
        b"last session"
    );
    assert!(!game.install().exists());
    let work = game.work().unwrap();
    assert!(work.installation_removed, "a reinstall still asks");
    assert_eq!(work.pending, None);
}

pub(crate) async fn an_exit_capture_left_for_after_an_uninstall_keeps_the_archived_save<
    H: Harness,
>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.started().unwrap();
    fs::write(game.save(), b"actual game exit").unwrap();
    // The game exited, then the supervisor disappeared before its capture.
    sync.vault.game_exited(&sync.session_id).unwrap();
    drop(sync);
    assert_eq!(pending(&game), Some(Pending::Capture));
    game.uninstall().await;
    let answering = answer(game.control(), Choice::Local);

    maintain(&game).await.unwrap();

    answering.join().unwrap();
    assert_eq!(game.cloud().unwrap(), b"actual game exit");
    assert!(!game.install().exists());
}

pub(crate) async fn a_missing_installation_without_an_uninstall_keeps_the_saved_work<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"exit").await;
    fs::remove_dir_all(game.install()).unwrap();

    let refused = maintain(&game).await.unwrap_err();

    assert_eq!(refused.key(), Of::<H>::recovery().key());
    assert!(uploading(pending(&game)).is_some());
    assert!(game.listed());
}

pub(crate) async fn a_reinstall_restores_saves_the_uninstall_removed<H: Harness>() {
    let game = H::game();
    synced(&game, b"progress").await;
    game.uninstall().await;
    reinstall(&game);

    game.open().prepare(&no_other_session).await.unwrap();

    assert_eq!(fs::read(game.save()).unwrap(), b"progress");
    assert!(!game.work().unwrap().installation_removed);
    assert_eq!(game.uploads(), 0);
}

pub(crate) async fn keeping_local_saves_after_a_reinstall_never_deletes_cloud_saves<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"last session").await;
    game.uninstall().await;
    reinstall(&game);
    // A reinstall lacks saves nobody deleted: the player confirms first.
    let answering = answer(game.control(), Choice::Local);

    game.open().prepare(&no_other_session).await.unwrap();

    answering.join().unwrap();
    assert_eq!(game.cloud().unwrap(), b"last session");
    assert_eq!(fs::read(game.save()).unwrap(), b"last session");
    let work = game.work().unwrap();
    assert!(!work.installation_removed);
    assert_eq!(work.pending, None);
}

pub(crate) async fn a_reinstall_clears_its_removal_fence_once_the_saves_agree<H: Harness>() {
    // Equal saves, then an explicit Cloud choice for different ones.
    for (local, choice) in [
        (b"progress".as_slice(), None),
        (b"other", Some(Choice::Cloud)),
    ] {
        let game = H::game();
        synced(&game, b"progress").await;
        game.uninstall().await;
        reinstall(&game);
        fs::write(game.save(), local).unwrap();
        let answering = choice.map(|choice| answer(game.control(), choice));

        game.open().prepare(&no_other_session).await.unwrap();

        if let Some(answering) = answering {
            answering.join().unwrap();
        }
        assert_eq!(fs::read(game.save()).unwrap(), b"progress");
        assert!(!game.work().unwrap().installation_removed);
    }
}

pub(crate) async fn launching_anyway_after_a_reinstall_keeps_the_removal_fence<H: Harness>() {
    let game = H::game();
    synced(&game, b"archived").await;
    game.uninstall().await;
    reinstall(&game);
    game.fail_next(false);
    let answering = answer(game.control(), Choice::LaunchAnyway);
    let mut sync = game.open();

    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();

    answering.join().unwrap();
    assert!(sync.guard_held());
    assert!(game.work().unwrap().installation_removed);
    let archived = sync.vault.local().unwrap();
    assert_eq!(
        sync.vault.read_content(&archived.0[game.slot()]).unwrap(),
        b"archived"
    );
}

pub(crate) async fn a_completed_sync_deletes_orphaned_archive_blobs<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    let orphan = sync.vault.stage(b"orphaned bytes", 2).unwrap();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    // Cleanup never runs through an active game guard.
    let held = sync.vault.stage(b"staged while playing", 2).unwrap();
    sync.collect_garbage();
    assert!(sync.vault.read_content(&held).is_ok());
    fs::write(game.save(), b"new progress").unwrap();

    sync.finished(true).await;

    assert!(sync.vault.read_content(&orphan).is_err());
    assert!(sync.vault.read_content(&held).is_err());
    let local = sync.vault.local().unwrap();
    assert_eq!(
        sync.vault.read_content(&local.0[game.slot()]).unwrap(),
        b"new progress"
    );
}

pub(crate) async fn cleanup_keeps_what_pending_work_and_a_shown_conflict_reference<H: Harness>() {
    let game = H::game();
    synced(&game, b"old").await;
    played_offline(&game, b"exit save").await;
    fs::write(game.save(), b"exit save").unwrap();
    game.publish(b"played elsewhere");
    let answering = answer(game.control(), Choice::Cancel);
    let mut sync = game.open();

    assert!(sync.prepare(&no_other_session).await.is_err());

    answering.join().unwrap();
    sync.collect_garbage();
    let desired = uploading(pending(&game)).unwrap();
    assert_eq!(
        sync.vault.read_content(&desired.0[game.slot()]).unwrap(),
        b"exit save"
    );
    let (remote, _) = sync.shown.clone().unwrap();
    assert_eq!(
        sync.vault
            .read_content(&remote.files.0[game.slot()])
            .unwrap(),
        b"played elsewhere"
    );
}

pub(crate) async fn an_interrupted_cloud_choice_never_resends_the_discarded_save<H: Harness>() {
    for applied in [false, true] {
        let game = H::game();
        synced(&game, b"old").await;
        played_offline(&game, b"old local exit").await;
        let desired = uploading(pending(&game)).unwrap();
        game.publish(b"selected remote");
        let mut sync = game.open();
        let mut link = sync.store.connect(&sync.control).await.unwrap();
        sync.store
            .rules(&mut link, &mut sync.vault, true)
            .await
            .unwrap();
        let remote = sync
            .store
            .view(&mut link, &mut sync.vault, &sync.control)
            .await
            .unwrap();
        drop(link);
        // The choice is durable before the projection or the baseline change.
        let mut work = sync.store.work().unwrap();
        work.pending = Some(Pending::Applying {
            desired: desired.clone(),
            remote: remote.clone(),
            project: true,
        });
        sync.store.save(work).unwrap();
        if applied {
            let bindings = sync
                .store
                .bindings(&sync.vault, Some(&remote.files), true)
                .unwrap();
            sync.vault
                .apply_download(&bindings, &remote, &remote, true)
                .unwrap();
        }
        drop(sync);
        game.set_rules(false);

        // Recovery runs first; no rules then stops the sync before transfers.
        assert!(maintain(&game).await.is_err());

        if applied {
            assert_eq!(pending(&game), None);
            assert_eq!(fs::read(game.save()).unwrap(), b"selected remote");
        } else {
            assert_eq!(uploading(pending(&game)), Some(desired));
            assert_eq!(fs::read(game.save()).unwrap(), b"old local exit");
        }
    }
}

pub(crate) async fn maintenance_of_a_game_without_cloud_rules_reports_it_unavailable<H: Harness>() {
    let game = H::game();
    game.set_rules(false);

    maintain(&game).await.unwrap();

    assert_eq!(status(&game).phase, Phase::Unsupported);
}

pub(crate) async fn indicators_follow_installed_and_uninstalled_games_without_locking<
    H: Harness,
>() {
    let game = H::game();
    assert_eq!(game.indicator(true).unwrap().state, SavedState::NeverSynced);
    synced(&game, b"old").await;
    let shown = game.indicator(true).unwrap();
    assert_eq!(shown.state, SavedState::Synced);
    assert!(shown.synced_at.is_some());

    played_offline(&game, b"played offline").await;
    let held = game.open();
    let shown = game.indicator(true).unwrap();
    assert_eq!(shown.state, SavedState::Pending);
    assert!(shown.pending_upload);
    drop(held);

    game.uninstall().await;
    let shown = game
        .indicator(false)
        .expect("uninstalled games with pending work are listed");
    assert!(shown.installation_removed);
    assert!(shown.pending_upload);
}

/// Instantiates every Cloud sync scenario for one storefront's harness.
macro_rules! cloud_suite {
    ($harness:ty) => {
        $crate::cloud::sync::tests::cloud_suite!(
            $harness;
            a_launch_with_equal_saves_is_ready_and_records_a_cloud_sync,
            a_launch_downloads_a_newer_cloud_save,
            an_unconfirmed_game_exit_only_captures_its_save,
            a_launch_revoked_before_spawn_withdraws_its_cloud_authorization,
            a_game_exit_uploads_local_progress,
            a_launch_conflict_asks_and_applies_the_cloud_choice,
            a_failed_exit_upload_keeps_the_exit_save_pending,
            a_game_without_cloud_rules_is_unavailable_until_they_return,
            saves_sailover_holds_never_pass_for_a_game_without_cloud,
            a_cancelled_launch_check_is_a_failed_sync_until_the_next_sync,
            launching_anyway_without_pending_work_is_a_failed_sync,
            an_unanswered_launch_conflict_is_recorded_until_it_is_answered,
            a_first_conflict_is_durable_before_it_is_shown,
            a_choice_for_saves_changed_since_the_question_asks_again,
            pending_discovery_includes_uploads_the_vault_still_holds,
            a_game_exit_conflict_is_asked_live_and_answered,
            an_unanswered_exit_conflict_stays_pending_with_a_conflict_marker,
            a_stopped_exit_upload_waits_without_claiming_a_conflict,
            maintenance_of_a_synced_game_confirms_it_without_launching,
            maintenance_sends_an_exit_save_left_pending,
            maintenance_downloads_a_newer_cloud_save,
            maintenance_asks_about_a_conflict_and_cancel_keeps_the_work_pending,
            maintenance_leaves_unchanged_saves_unqueued_when_it_fails,
            maintenance_clears_the_guard_of_a_dead_game_session,
            maintenance_keeps_the_guard_of_a_game_session_that_still_runs,
            maintenance_sends_the_exit_save_of_an_uninstalled_game,
            an_uninstalled_game_keeps_its_archive_when_the_cloud_is_chosen,
            an_exit_capture_left_for_after_an_uninstall_keeps_the_archived_save,
            a_missing_installation_without_an_uninstall_keeps_the_saved_work,
            a_reinstall_restores_saves_the_uninstall_removed,
            keeping_local_saves_after_a_reinstall_never_deletes_cloud_saves,
            a_reinstall_clears_its_removal_fence_once_the_saves_agree,
            launching_anyway_after_a_reinstall_keeps_the_removal_fence,
            a_completed_sync_deletes_orphaned_archive_blobs,
            cleanup_keeps_what_pending_work_and_a_shown_conflict_reference,
            an_interrupted_cloud_choice_never_resends_the_discarded_save,
            maintenance_of_a_game_without_cloud_rules_reports_it_unavailable,
            indicators_follow_installed_and_uninstalled_games_without_locking,
        );
    };
    ($harness:ty; $($scenario:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $scenario() {
                $crate::cloud::sync::tests::$scenario::<$harness>().await
            }
        )*
    };
}
pub(crate) use cloud_suite;
