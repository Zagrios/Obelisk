//! Steam Cloud sync through the session interface. An in-memory Steam Cloud
//! stands in for the CM connection.
use super::{
    fake::{FakeSteamCloud, SLOT},
    *,
};
use std::fs;

pub(crate) const ACCOUNT: u64 = 0x0110_0001_0000_002a;

pub(crate) struct Game {
    _temporary: tempfile::TempDir,
    pub config: Config,
    pub cloud: FakeSteamCloud,
    pub control: Arc<Control>,
}

pub(crate) fn credentials() -> Credentials {
    Credentials {
        steam_id: ACCOUNT,
        account_name: "fixture".into(),
        refresh_token: "unused".into(),
    }
}

pub(crate) fn game() -> Game {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let config = Config {
        root: base.join("cloud"),
        home: base.join("home"),
        install: base.join("game"),
        app_id: 42,
    };
    for path in [&config.root, &config.home, &config.install] {
        crate::files::private_directory(path).unwrap();
    }
    Game {
        cloud: FakeSteamCloud::new(ACCOUNT, config.app_id),
        control: Arc::new(Control::new(ACCOUNT, |_| {})),
        config,
        _temporary: temporary,
    }
}

impl Game {
    pub fn save(&self) -> PathBuf {
        self.config.install.join("slot.sav")
    }

    pub fn session(&self) -> Session<FakeSteamCloud> {
        Session::open_with(
            self.cloud.handle(),
            self.config.clone(),
            credentials(),
            uuid::Uuid::new_v4().to_string(),
            Arc::clone(&self.control),
        )
        .unwrap()
    }

    pub fn status(&self) -> Status {
        self.control.state.lock().unwrap().status.clone()
    }

    /// A first launch that finds both sides equal and records the baseline.
    pub async fn synced(&self, bytes: &[u8]) {
        fs::write(self.save(), bytes).unwrap();
        self.cloud.publish(SLOT, bytes);
        self.session().prepare(&no_other_session).await.unwrap();
    }
}

/// Answers the next Cloud question offering `choice`, as the UI would. A
/// question already shown when this starts is an older one.
pub(crate) fn answer(control: &Arc<Control>, choice: Choice) -> std::thread::JoinHandle<()> {
    let control = Arc::clone(control);
    let shown = control.state.lock().unwrap().status.id;
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let status = control.state.lock().unwrap().status.clone();
            if status.id > shown && status.allowed_choices.contains(&choice) {
                control.respond(status.id, choice).unwrap();
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no Cloud question offered {choice:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}

#[tokio::test]
async fn a_launch_with_equal_saves_is_ready_and_records_a_cloud_sync() {
    let game = game();
    fs::write(game.save(), b"progress").unwrap();
    game.cloud.publish(SLOT, b"progress");
    let mut session = game.session();

    session.prepare(&no_other_session).await.unwrap();

    assert_eq!(game.status().phase, Phase::Ready);
    assert!(session.vault.synced_at().is_some());
    assert_eq!(game.cloud.uploads(), 0);
}

#[tokio::test]
async fn a_launch_downloads_a_newer_cloud_save() {
    let game = game();
    game.synced(b"old").await;
    game.cloud.publish(SLOT, b"from another device");

    game.session().prepare(&no_other_session).await.unwrap();

    assert_eq!(game.status().phase, Phase::Ready);
    assert_eq!(fs::read(game.save()).unwrap(), b"from another device");
}

#[tokio::test]
async fn an_unconfirmed_game_exit_only_captures_its_save() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    session.started().unwrap();
    fs::write(game.save(), b"maybe played").unwrap();

    // The helper exited without confirming that the game ever ran.
    session.finished(false).await;

    assert_eq!(game.cloud.file(SLOT).unwrap(), b"old");
    assert_eq!(game.cloud.uploads(), 0);
    assert_eq!(game.status().phase, Phase::Pending);
    assert_eq!(store::pending(&game.config.root, ACCOUNT).unwrap(), [42]);
}

#[tokio::test]
async fn a_launch_revoked_before_spawn_withdraws_its_cloud_authorization() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    let before = format!("{:?}", session.vault.authorize(ACCOUNT).unwrap());
    game.control.revoke(ACCOUNT).unwrap();

    session.aborted();

    let after = format!("{:?}", session.vault.authorize(ACCOUNT).unwrap());
    assert_ne!(
        before, after,
        "the account's authorization generation moved on"
    );
}

#[tokio::test]
async fn game_exit_uploads_local_progress() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    session.started().unwrap();
    fs::write(game.save(), b"new progress").unwrap();

    session.finished(true).await;

    assert_eq!(game.status().phase, Phase::Synced);
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"new progress");
    assert_eq!(game.cloud.uploads(), 1);
}

#[tokio::test]
async fn a_launch_conflict_asks_and_applies_the_cloud_choice() {
    let game = game();
    game.synced(b"old").await;
    fs::write(game.save(), b"local progress").unwrap();
    game.cloud.publish(SLOT, b"cloud progress");
    let answering = answer(&game.control, Choice::Cloud);

    game.session().prepare(&no_other_session).await.unwrap();

    answering.join().unwrap();
    assert_eq!(game.status().phase, Phase::Ready);
    assert_eq!(fs::read(game.save()).unwrap(), b"cloud progress");
}

#[tokio::test]
async fn a_failed_exit_upload_keeps_the_exit_save_pending() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    session.started().unwrap();
    fs::write(game.save(), b"new progress").unwrap();
    game.cloud.fail_next_connection(Failure::Quota);

    session.finished(true).await;

    assert_eq!(game.status().phase, Phase::Pending);
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"old");
    let saved = Context::load(&game.config, ACCOUNT).unwrap().unwrap();
    assert!(saved.needs_work);
    assert!(saved.desired.is_some());
}

fn record(game: &Game) -> crate::cloud_status::Record {
    let scope = cloud_vault::StoreScope::from(scope(&game.config, ACCOUNT));
    crate::cloud_status::read(&game.config.root, &scope).unwrap()
}

fn unavailable(game: &Game) -> Option<crate::cloud_status::UnavailableReason> {
    record(game).unavailable.map(|marker| marker.reason)
}

#[tokio::test]
async fn a_game_without_steam_cloud_is_unavailable_until_its_rules_return() {
    use crate::cloud_status::UnavailableReason;
    let game = game();
    game.cloud.set_metadata(None);

    game.session().prepare(&no_other_session).await.unwrap();

    assert_eq!(game.status().phase, Phase::Disabled);
    assert_eq!(unavailable(&game), Some(UnavailableReason::NoCloud));
    game.cloud.set_metadata(Some(super::fake::save_rule()));
    game.synced(b"progress").await;
    assert_eq!(unavailable(&game), None);
}

#[tokio::test]
async fn cloud_turned_off_in_steam_is_recorded_as_unavailable() {
    use crate::cloud_status::UnavailableReason;
    let game = game();
    fs::write(game.save(), b"progress").unwrap();
    game.cloud.set_enabled(Some(false));

    game.session().prepare(&no_other_session).await.unwrap();

    assert_eq!(game.status().phase, Phase::Disabled);
    assert_eq!(unavailable(&game), Some(UnavailableReason::Disabled));
}

#[tokio::test]
async fn playing_with_cloud_turned_off_leaves_no_pending_work() {
    use crate::cloud_status::UnavailableReason;
    let game = game();
    game.synced(b"old").await;
    game.cloud.set_enabled(Some(false));
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    session.started().unwrap();
    fs::write(game.save(), b"played with Cloud off").unwrap();

    session.finished(true).await;

    assert_eq!(game.status().phase, Phase::Disabled);
    let saved = Context::load(&game.config, ACCOUNT).unwrap().unwrap();
    assert!(!saved.needs_work);
    assert!(saved.desired.is_none());
    assert_eq!(unavailable(&game), Some(UnavailableReason::Disabled));
    assert!(
        store::pending(&game.config.root, ACCOUNT)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_cancelled_launch_check_is_a_failed_sync_until_the_next_sync() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    game.cloud.fail_next_connection(Failure::Quota);
    let answering = answer(&game.control, Choice::Cancel);

    assert!(game.session().prepare(&no_other_session).await.is_err());

    answering.join().unwrap();
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Failed);
    assert_eq!(problem.key, Failure::Quota.key());
    game.session().prepare(&no_other_session).await.unwrap();
    assert_eq!(record(&game).problem, None);
}

#[tokio::test]
async fn launching_anyway_without_pending_work_is_a_failed_sync() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    game.cloud.fail_next_connection(Failure::Quota);
    let answering = answer(&game.control, Choice::LaunchAnyway);

    game.session().prepare(&no_other_session).await.unwrap();

    answering.join().unwrap();
    assert_eq!(game.status().phase, Phase::Pending);
    assert_eq!(record(&game).problem.unwrap().kind, ProblemKind::Failed);
}

#[tokio::test]
async fn an_unanswered_launch_conflict_is_recorded_until_it_is_answered() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    fs::write(game.save(), b"local progress").unwrap();
    game.cloud.publish(SLOT, b"cloud progress");
    let answering = answer(&game.control, Choice::Cancel);

    assert!(game.session().prepare(&no_other_session).await.is_err());

    answering.join().unwrap();
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Conflict);
    assert_eq!(problem.key, Failure::Conflict.key());
    let answering = answer(&game.control, Choice::Local);
    game.session().prepare(&no_other_session).await.unwrap();
    answering.join().unwrap();
    assert_eq!(record(&game).problem, None);
}

#[test]
fn pending_discovery_includes_uploads_the_vault_still_holds() {
    let game = game();
    let mut vault = Vault::open(&game.config.root, scope(&game.config, ACCOUNT)).unwrap();
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let files = Manifest([(SLOT.into(), vault.stage(b"exit", 2).unwrap())].into());
    let authorization = vault.authorize(ACCOUNT).unwrap();
    vault
        .queue_upload(
            &files,
            &RemoteView {
                revision: "1".into(),
                files: Manifest::default(),
            },
            "fixture",
            authorization,
        )
        .unwrap();
    Context::new(&game.config, ACCOUNT, super::fake::save_rule())
        .save()
        .unwrap();

    let pending = store::pending(&game.config.root, ACCOUNT).unwrap();

    assert_eq!(pending, vec![game.config.app_id]);
}

#[tokio::test]
async fn a_game_exit_conflict_is_asked_live_and_answered() {
    for (choice, kept) in [
        (Choice::Local, b"played here".as_slice()),
        (Choice::Cloud, b"played elsewhere".as_slice()),
    ] {
        let game = game();
        game.synced(b"old").await;
        let mut session = game.session();
        session.prepare(&no_other_session).await.unwrap();
        session.started().unwrap();
        fs::write(game.save(), b"played here").unwrap();
        game.cloud.publish(SLOT, b"played elsewhere");
        let answering = answer(&game.control, choice);

        session.finished(true).await;

        answering.join().unwrap();
        assert_eq!(game.status().phase, Phase::Synced);
        assert_eq!(fs::read(game.save()).unwrap(), kept);
        assert_eq!(game.cloud.file(SLOT).unwrap(), kept);
        assert!(
            !Context::load(&game.config, ACCOUNT)
                .unwrap()
                .unwrap()
                .needs_work
        );
        assert_eq!(record(&game).problem, None);
    }
}

#[tokio::test]
async fn an_unanswered_exit_conflict_stays_pending_with_a_conflict_marker() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    session.started().unwrap();
    fs::write(game.save(), b"played here").unwrap();
    game.cloud.publish(SLOT, b"played elsewhere");
    let answering = answer(&game.control, Choice::Cancel);

    session.finished(true).await;

    answering.join().unwrap();
    assert_eq!(game.status().phase, Phase::Pending);
    assert!(game.status().allowed_choices.is_empty());
    let saved = Context::load(&game.config, ACCOUNT).unwrap().unwrap();
    assert!(saved.needs_work);
    assert!(saved.desired.is_some());
    assert_eq!(fs::read(game.save()).unwrap(), b"played here");
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Conflict);
    assert_eq!(problem.key, Failure::Conflict.key());
}

async fn maintain(game: &Game, target: MaintenanceTarget) -> Result<(), Failure> {
    maintain_beside(game, target, &no_other_session).await
}

async fn maintain_beside(
    game: &Game,
    target: MaintenanceTarget,
    alive: &SessionAlive<'_>,
) -> Result<(), Failure> {
    // A maintenance never waits on a question it should not ask.
    tokio::time::timeout(
        Duration::from_secs(20),
        maintain_with(
            game.cloud.handle(),
            target,
            &credentials(),
            Arc::clone(&game.control),
            alive,
        ),
    )
    .await
    .expect("maintenance finished")
}

fn installed(game: &Game) -> MaintenanceTarget {
    MaintenanceTarget::Installed(game.config.clone())
}

fn uninstalled(game: &Game) -> MaintenanceTarget {
    MaintenanceTarget::Uninstalled {
        root: game.config.root.clone(),
        app_id: game.config.app_id,
    }
}

/// The game exits while Steam Cloud is unreachable: its exit save stays pending.
async fn played_offline(game: &Game, bytes: &[u8]) {
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    session.started().unwrap();
    fs::write(game.save(), bytes).unwrap();
    game.cloud.fail_next_connection(Failure::Quota);
    session.finished(true).await;
    assert_eq!(game.status().phase, Phase::Pending);
}

fn uninstall(game: &Game) {
    store::archive(&game.config, ACCOUNT).unwrap();
    fs::remove_dir_all(&game.config.install).unwrap();
    store::removed(&game.config, ACCOUNT).unwrap();
}

#[tokio::test]
async fn maintenance_of_a_synced_game_confirms_it_without_launching() {
    let game = game();
    game.synced(b"progress").await;

    maintain(&game, installed(&game)).await.unwrap();

    assert_eq!(game.status().phase, Phase::Synced);
    assert_eq!(game.cloud.uploads(), 0);
    assert_eq!(record(&game).problem, None);
}

#[tokio::test]
async fn maintenance_sends_an_exit_save_left_pending() {
    let game = game();
    game.synced(b"old").await;
    played_offline(&game, b"played offline").await;

    maintain(&game, installed(&game)).await.unwrap();

    assert_eq!(game.status().phase, Phase::Synced);
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"played offline");
    let saved = Context::load(&game.config, ACCOUNT).unwrap().unwrap();
    assert!(!saved.needs_work);
    assert!(
        store::pending(&game.config.root, ACCOUNT)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn maintenance_downloads_a_newer_cloud_save() {
    let game = game();
    game.synced(b"old").await;
    game.cloud.publish(SLOT, b"from another device");

    maintain(&game, installed(&game)).await.unwrap();

    assert_eq!(game.status().phase, Phase::Synced);
    assert_eq!(fs::read(game.save()).unwrap(), b"from another device");
}

#[tokio::test]
async fn maintenance_asks_about_a_conflict_and_cancel_keeps_the_work_pending() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    fs::write(game.save(), b"played here").unwrap();
    game.cloud.publish(SLOT, b"played elsewhere");
    let answering = answer(&game.control, Choice::Cancel);

    assert!(matches!(
        maintain(&game, installed(&game)).await,
        Err(Failure::Cancelled)
    ));

    answering.join().unwrap();
    assert_eq!(record(&game).problem.unwrap().kind, ProblemKind::Conflict);
    assert!(
        Context::load(&game.config, ACCOUNT)
            .unwrap()
            .unwrap()
            .needs_work
    );
    let answering = answer(&game.control, Choice::Local);
    maintain(&game, installed(&game)).await.unwrap();
    answering.join().unwrap();
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"played here");
    assert_eq!(record(&game).problem, None);
}

#[tokio::test]
async fn maintenance_clears_the_guard_of_a_dead_game_session() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.started().unwrap();
    fs::write(game.save(), b"played before a crash").unwrap();
    drop(session);

    maintain(&game, installed(&game)).await.unwrap();

    assert_eq!(game.cloud.file(SLOT).unwrap(), b"played before a crash");
    let vault = Vault::open(&game.config.root, scope(&game.config, ACCOUNT)).unwrap();
    assert!(vault.game_session().is_none());
}

#[tokio::test]
async fn maintenance_keeps_the_guard_of_a_game_session_that_still_runs() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.started().unwrap();
    fs::write(game.save(), b"still playing").unwrap();
    drop(session);

    let refused = maintain_beside(&game, installed(&game), &|_| Ok(true)).await;

    assert!(matches!(refused, Err(Failure::Busy)));
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"old");
    let vault = Vault::open(&game.config.root, scope(&game.config, ACCOUNT)).unwrap();
    assert!(vault.game_session().is_some());
}

#[tokio::test]
async fn maintenance_sends_the_exit_save_of_an_uninstalled_game() {
    let game = game();
    game.synced(b"old").await;
    played_offline(&game, b"last session").await;
    uninstall(&game);
    // A removed installation cannot prove its saves complete: the player
    // confirms that this exit save should replace the Cloud one.
    let answering = answer(&game.control, Choice::Local);

    maintain(&game, uninstalled(&game)).await.unwrap();

    answering.join().unwrap();
    assert_eq!(game.status().phase, Phase::Synced);
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"last session");
    assert!(!game.config.install.exists());
    assert!(
        store::pending(&game.config.root, ACCOUNT)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn maintenance_refuses_an_uninstalled_exit_save_after_its_cloud_rules_changed() {
    let game = game();
    game.synced(b"old").await;
    played_offline(&game, b"last session").await;
    uninstall(&game);
    let mut changed = super::fake::save_rule();
    if let steam_kit::cloud::CloudMetadata::Object(fields) = &mut changed {
        fields.push((
            "quota".into(),
            steam_kit::cloud::CloudMetadata::Value("1000".into()),
        ));
    }
    game.cloud.set_metadata(Some(changed));

    assert!(matches!(
        maintain(&game, uninstalled(&game)).await,
        Err(Failure::RulesChanged)
    ));

    assert_eq!(game.cloud.file(SLOT).unwrap(), b"old");
    assert_eq!(store::pending(&game.config.root, ACCOUNT).unwrap().len(), 1);
    assert_eq!(
        Failure::RulesChanged.key(),
        "backend.steam.cloud.rulesChanged"
    );
}

#[tokio::test]
async fn a_maintenance_failure_ends_the_attempt_without_asking() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    game.cloud.fail_next_connection(Failure::Quota);

    assert!(matches!(
        maintain(&game, installed(&game)).await,
        Err(Failure::Quota)
    ));

    assert!(game.status().allowed_choices.is_empty());
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Failed);
    assert_eq!(problem.key, Failure::Quota.key());
}

#[tokio::test]
async fn indicators_follow_installed_and_uninstalled_games_without_locking() {
    use crate::cloud_status::SavedState;
    let game = game();
    game.synced(b"old").await;
    let shown = store::indicators(&game.config.root, ACCOUNT, &[42, 7]).unwrap();
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[0].0, 7);
    assert_eq!(shown[0].1.state, SavedState::NeverSynced);
    assert_eq!(shown[1].1.state, SavedState::Synced);
    assert!(shown[1].1.synced_at.is_some());

    played_offline(&game, b"played offline").await;
    let held = game.session();
    let shown = store::indicators(&game.config.root, ACCOUNT, &[42]).unwrap();
    assert_eq!(shown[0].1.state, SavedState::Pending);
    assert!(shown[0].1.pending_upload);
    drop(held);

    uninstall(&game);
    let shown = store::indicators(&game.config.root, ACCOUNT, &[]).unwrap();
    assert_eq!(
        shown.len(),
        1,
        "uninstalled games with pending work are listed"
    );
    assert_eq!(shown[0].0, 42);
    assert!(shown[0].1.installation_removed);
    assert!(shown[0].1.pending_upload);
}

#[tokio::test]
async fn a_game_without_steam_cloud_has_an_unavailable_indicator() {
    use crate::cloud_status::SavedState;
    let game = game();
    game.cloud.set_metadata(None);
    game.session().prepare(&no_other_session).await.unwrap();

    let shown = store::indicators(&game.config.root, ACCOUNT, &[42]).unwrap();

    assert_eq!(shown[0].1.state, SavedState::Unavailable);
}

#[tokio::test]
async fn a_stopped_exit_upload_waits_without_claiming_a_conflict() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session();
    session.prepare(&no_other_session).await.unwrap();
    session.started().unwrap();
    fs::write(game.save(), b"new progress").unwrap();
    game.cloud.fail_next_connection(Failure::Network);
    let control = Arc::clone(&game.control);
    // Stops the exit sync during its wait before the second attempt.
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        control.cancel();
    });

    session.finished(true).await;

    stopper.join().unwrap();
    let status = game.status();
    assert_eq!(status.phase, Phase::Pending);
    assert_eq!(status.error.as_deref(), Some(Failure::Cancelled.key()));
    assert_eq!(record(&game).problem, None);
}
