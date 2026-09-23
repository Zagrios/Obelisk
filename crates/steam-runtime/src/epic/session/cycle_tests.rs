//! Epic Cloud sync through the session interface. An in-memory Epic Cloud
//! stands in for the network.
use super::{
    fake::{Call, FakeCloud},
    *,
};
use crate::epic::MaintenanceChannel;
use std::fs;

pub(crate) const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";

pub(crate) struct Game {
    _temporary: tempfile::TempDir,
    pub config: Configuration,
    pub cloud: Arc<FakeCloud>,
    pub control: Arc<Control>,
}

pub(crate) fn rules() -> CloudRules {
    CloudRules {
        cloud_save_folder: None,
        cloud_save_folder_mac: "{Home}/Saves".into(),
        include: vec!["*.sav".into()],
        exclude: Vec::new(),
    }
}

pub(crate) fn game() -> Game {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().to_path_buf();
    let home = root.join("home");
    let config = Configuration {
        root: root.join("archive"),
        install: home.join("Game"),
        session_directory: root.join("sessions"),
        helper: root.join("helper"),
        home,
        namespace: "namespace".into(),
        catalog_item_id: "catalog".into(),
        app_name: "game".into(),
        platform: "Mac".into(),
        language: "en".into(),
    };
    for directory in [
        config.home.join("Saves"),
        config.install.clone(),
        config.session_directory.clone(),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    Game {
        control: crate::epic::tests::control(&config.session_directory),
        cloud: FakeCloud::new(ACCOUNT, Some(rules())),
        config,
        _temporary: temporary,
    }
}

impl Game {
    pub fn save(&self) -> PathBuf {
        self.config.home.join("Saves").join("slot.sav")
    }

    pub async fn session(&self) -> Session<FakeCloud> {
        Session::new(
            self.config.clone(),
            ACCOUNT.into(),
            Arc::clone(&self.cloud),
            self.control.clone(),
        )
        .await
        .unwrap()
        .expect("supported Cloud rules")
    }

    pub fn cloud_phase(&self) -> CloudPhase {
        self.control.snapshot().cloud_phase
    }

    /// A first launch that finds both sides equal and records the baseline.
    pub async fn synced(&self, bytes: &[u8]) {
        fs::write(self.save(), bytes).unwrap();
        self.cloud.publish(&[("slot.sav", bytes)]);
        self.session().await.prepare().await.unwrap();
    }
}

/// Answers the next Cloud question offering `choice`, as the UI would. A
/// question already shown when this starts is an older one.
pub(crate) fn answer(control: &Arc<Control>, choice: Choice) -> std::thread::JoinHandle<()> {
    let control = Arc::clone(control);
    let shown = control.snapshot().decision_id;
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let mut state = control.state.lock().unwrap();
                if state.snapshot.decision_id > shown
                    && state.snapshot.allowed_choices.contains(&choice)
                    && state.choice.is_none()
                {
                    state.choice = Some(choice);
                    control.notify_changed();
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
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
    game.cloud.publish(&[("slot.sav", b"progress")]);
    let mut session = game.session().await;

    session.prepare().await.unwrap();

    assert_eq!(game.cloud_phase(), CloudPhase::Ready);
    assert!(session.vault.synced_at().is_some());
    assert_eq!(game.cloud.uploads(), 0);
}

#[tokio::test]
async fn a_launch_downloads_a_newer_cloud_save() {
    let game = game();
    game.synced(b"old").await;
    game.cloud.publish(&[("slot.sav", b"from another device")]);

    game.session().await.prepare().await.unwrap();

    assert_eq!(game.cloud_phase(), CloudPhase::Ready);
    assert_eq!(fs::read(game.save()).unwrap(), b"from another device");
}

#[tokio::test]
async fn game_exit_uploads_local_progress() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session().await;
    session.prepare().await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    session.started(&id).unwrap();
    fs::write(game.save(), b"new progress").unwrap();

    session.finished(&id).await.unwrap();

    assert_eq!(game.cloud_phase(), CloudPhase::Synced);
    assert_eq!(game.cloud.files()["slot.sav"], b"new progress");
    assert_eq!(game.cloud.uploads(), 1);
}

#[tokio::test]
async fn a_launch_conflict_asks_and_applies_the_cloud_choice() {
    let game = game();
    game.synced(b"old").await;
    fs::write(game.save(), b"local progress").unwrap();
    game.cloud.publish(&[("slot.sav", b"cloud progress")]);
    let answering = answer(&game.control, Choice::Cloud);

    game.session().await.prepare().await.unwrap();

    answering.join().unwrap();
    assert_eq!(game.cloud_phase(), CloudPhase::Ready);
    assert_eq!(fs::read(game.save()).unwrap(), b"cloud progress");
}

#[tokio::test]
async fn a_failed_exit_upload_keeps_the_exit_save_pending() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session().await;
    session.prepare().await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    session.started(&id).unwrap();
    fs::write(game.save(), b"new progress").unwrap();
    game.cloud.fail_next(Call::Download, EpicError::Http(403));

    assert!(session.finished(&id).await.is_err());

    assert_eq!(game.cloud_phase(), CloudPhase::Pending);
    assert_eq!(game.cloud.files()["slot.sav"], b"old");
    assert_eq!(
        pending(&game.config.root, ACCOUNT).unwrap(),
        vec![game.config.clone()]
    );
}

fn record(game: &Game) -> crate::cloud_status::Record {
    let scope = steam_cloud::StoreScope::from(game.config.scope(ACCOUNT));
    crate::cloud_status::read(&game.config.root, &scope).unwrap()
}

#[tokio::test]
async fn unsupported_epic_cloud_rules_are_unavailable_until_supported_rules_return() {
    use crate::cloud_status::UnavailableReason;
    let game = game();
    game.cloud.set_rules(None);

    let session = Session::new(
        game.config.clone(),
        ACCOUNT.into(),
        Arc::clone(&game.cloud),
        game.control.clone(),
    )
    .await
    .unwrap();

    assert!(session.is_none());
    assert_eq!(game.cloud_phase(), CloudPhase::Unsupported);
    assert_eq!(
        record(&game).unavailable.map(|marker| marker.reason),
        Some(UnavailableReason::Unsupported)
    );
    game.cloud.set_rules(Some(rules()));
    game.synced(b"progress").await;
    assert_eq!(record(&game).unavailable, None);
}

#[tokio::test]
async fn a_cancelled_launch_check_is_a_failed_sync_until_the_next_sync() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    game.cloud.fail_next(Call::Download, EpicError::Http(403));
    let answering = answer(&game.control, Choice::Cancel);

    assert!(game.session().await.prepare().await.is_err());

    answering.join().unwrap();
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Failed);
    assert_eq!(problem.key, "backend.epic.content_http");
    game.session().await.prepare().await.unwrap();
    assert_eq!(record(&game).problem, None);
}

#[tokio::test]
async fn an_unanswered_launch_conflict_is_recorded_until_it_is_answered() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    fs::write(game.save(), b"local progress").unwrap();
    game.cloud.publish(&[("slot.sav", b"cloud progress")]);
    let answering = answer(&game.control, Choice::Cancel);

    assert!(game.session().await.prepare().await.is_err());

    answering.join().unwrap();
    assert_eq!(record(&game).problem.unwrap().kind, ProblemKind::Conflict);
    let answering = answer(&game.control, Choice::Local);
    game.session().await.prepare().await.unwrap();
    answering.join().unwrap();
    assert_eq!(record(&game).problem, None);
}

#[tokio::test]
async fn pending_discovery_includes_uploads_the_vault_still_holds() {
    let game = game();
    game.synced(b"old").await;
    let mut vault = Vault::open(&game.config.root, game.config.scope(ACCOUNT)).unwrap();
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let files = Manifest([("slot.sav".into(), vault.stage(b"exit", 2).unwrap())].into());
    let authorization = vault.authorize(ACCOUNT).unwrap();
    let before = vault.baseline().unwrap().clone();
    vault
        .queue_upload(&files, &before, "fixture", authorization)
        .unwrap();
    assert!(
        Context::load(&game.config, ACCOUNT)
            .unwrap()
            .unwrap()
            .pending
            .is_none()
    );

    assert_eq!(
        pending(&game.config.root, ACCOUNT).unwrap(),
        vec![game.config.clone()]
    );
}

#[tokio::test]
async fn uninstall_archive_needs_only_the_epic_storefront_lock() {
    let game = game();
    game.synced(b"progress").await;
    let steam_game = Vault::open(
        &game.config.root,
        steam_cloud::Scope {
            steam_id: 0x0110_0001_0000_0001,
            app_id: 10,
            cloud_app_id: 10,
        },
    )
    .unwrap();

    archive_with(game.config.clone(), game.cloud.as_ref())
        .await
        .unwrap();

    assert!(
        Context::load(&game.config, ACCOUNT)
            .unwrap()
            .unwrap()
            .installation_removed
    );
    let other_epic_game = Vault::open(
        &game.config.root,
        steam_cloud::EpicScope {
            account_id: ACCOUNT.into(),
            namespace: "namespace".into(),
            app_name: "other-game".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        archive_with(game.config.clone(), game.cloud.as_ref()).await,
        Err(Error::Busy)
    ));
    drop((steam_game, other_epic_game));
}

fn channel() -> Arc<MaintenanceChannel> {
    Arc::new(MaintenanceChannel::new(|_| {}))
}

/// Answers the next maintenance question offering `choice`, as the UI would.
fn answer_maintenance(
    channel: &Arc<MaintenanceChannel>,
    choice: Choice,
) -> std::thread::JoinHandle<()> {
    let channel = Arc::clone(channel);
    let shown = channel.status().decision_id;
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = channel.status();
            if status.decision_id > shown && status.allowed_choices.contains(&choice) {
                channel.answer(status.decision_id, choice).unwrap();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no maintenance question offered {choice:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    })
}

async fn maintain(game: &Game, channel: &Arc<MaintenanceChannel>) -> Result<()> {
    // A maintenance never waits on a question it should not ask.
    tokio::time::timeout(
        Duration::from_secs(20),
        maintain_with(
            game.config.clone(),
            Arc::clone(&game.cloud),
            channel.clone() as Arc<dyn CloudChannel>,
        ),
    )
    .await
    .expect("maintenance finished")
}

/// The game exits while Epic Cloud is unreachable: its exit save stays pending.
async fn played_offline(game: &Game, bytes: &[u8]) {
    let mut session = game.session().await;
    session.prepare().await.unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    session.started(&id).unwrap();
    fs::write(game.save(), bytes).unwrap();
    game.cloud.fail_next(Call::Download, EpicError::Http(403));
    assert!(session.finished(&id).await.is_err());
}

#[tokio::test]
async fn maintenance_of_a_synced_epic_game_confirms_it_without_launching() {
    let game = game();
    game.synced(b"progress").await;
    let channel = channel();

    maintain(&game, &channel).await.unwrap();

    assert_eq!(channel.status().cloud_phase, CloudPhase::Synced);
    assert_eq!(game.cloud.uploads(), 0);
    assert_eq!(record(&game).problem, None);
}

#[tokio::test]
async fn maintenance_sends_an_epic_exit_save_left_pending() {
    let game = game();
    game.synced(b"old").await;
    played_offline(&game, b"played offline").await;

    maintain(&game, &channel()).await.unwrap();

    assert_eq!(game.cloud.files()["slot.sav"], b"played offline");
    assert!(pending(&game.config.root, ACCOUNT).unwrap().is_empty());
}

#[tokio::test]
async fn maintenance_downloads_a_newer_epic_cloud_save() {
    let game = game();
    game.synced(b"old").await;
    game.cloud.publish(&[("slot.sav", b"from another device")]);

    maintain(&game, &channel()).await.unwrap();

    assert_eq!(fs::read(game.save()).unwrap(), b"from another device");
}

#[tokio::test]
async fn maintenance_asks_about_an_epic_conflict_and_cancel_keeps_the_work_pending() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    fs::write(game.save(), b"played here").unwrap();
    game.cloud.publish(&[("slot.sav", b"played elsewhere")]);
    let channel = channel();
    let answering = answer_maintenance(&channel, Choice::Cancel);

    assert!(matches!(
        maintain(&game, &channel).await,
        Err(Error::Cancelled)
    ));

    answering.join().unwrap();
    assert_eq!(record(&game).problem.unwrap().kind, ProblemKind::Conflict);
    assert_eq!(pending(&game.config.root, ACCOUNT).unwrap().len(), 1);
    let answering = answer_maintenance(&channel, Choice::Local);
    maintain(&game, &channel).await.unwrap();
    answering.join().unwrap();
    assert_eq!(game.cloud.files()["slot.sav"], b"played here");
    assert_eq!(record(&game).problem, None);
}

#[tokio::test]
async fn maintenance_clears_the_guard_of_a_dead_epic_game_session() {
    let game = game();
    game.synced(b"old").await;
    let mut session = game.session().await;
    let id = uuid::Uuid::new_v4().to_string();
    session.started(&id).unwrap();
    fs::write(game.save(), b"played before a crash").unwrap();
    drop(session);
    let mut dead = game.control.snapshot();
    dead.id = id;
    dead.account_id = ACCOUNT.into();
    dead.app_name = game.config.app_name.clone();
    crate::epic::write_snapshot(&game.config.session_directory, &dead).unwrap();
    let channel = channel();
    let mut session = Session::new(
        game.config.clone(),
        ACCOUNT.into(),
        Arc::clone(&game.cloud),
        channel.clone() as Arc<dyn CloudChannel>,
    )
    .await
    .unwrap()
    .unwrap();
    session.inspect = |_, _| Ok(None);

    session.maintain().await.unwrap();

    assert_eq!(game.cloud.files()["slot.sav"], b"played before a crash");
    assert!(session.vault.game_session().is_none());
}

#[tokio::test]
async fn maintenance_sends_the_exit_save_of_an_uninstalled_epic_game() {
    let game = game();
    game.synced(b"old").await;
    played_offline(&game, b"last session").await;
    archive_with(game.config.clone(), game.cloud.as_ref())
        .await
        .unwrap();
    fs::remove_dir_all(&game.config.install).unwrap();
    removed(&game.config, ACCOUNT).unwrap();
    let channel = channel();
    // A removed installation cannot prove its saves complete: the player
    // confirms that this exit save should replace the Cloud one.
    let answering = answer_maintenance(&channel, Choice::Local);

    maintain(&game, &channel).await.unwrap();

    answering.join().unwrap();
    assert_eq!(game.cloud.files()["slot.sav"], b"last session");
    assert!(!game.config.install.exists());
    assert!(pending(&game.config.root, ACCOUNT).unwrap().is_empty());
}

#[tokio::test]
async fn an_epic_maintenance_failure_ends_the_attempt_without_asking() {
    use crate::cloud_status::ProblemKind;
    let game = game();
    game.synced(b"old").await;
    game.cloud.fail_next(Call::Download, EpicError::Http(403));
    let channel = channel();

    assert!(maintain(&game, &channel).await.is_err());

    assert!(channel.status().allowed_choices.is_empty());
    let problem = record(&game).problem.unwrap();
    assert_eq!(problem.kind, ProblemKind::Failed);
    assert_eq!(problem.key, "backend.epic.content_http");
}

#[tokio::test]
async fn maintenance_reports_unsupported_epic_rules_as_unavailable() {
    let game = game();
    game.cloud.set_rules(None);
    let channel = channel();

    maintain(&game, &channel).await.unwrap();

    assert_eq!(channel.status().cloud_phase, CloudPhase::Unsupported);
}

#[tokio::test]
async fn epic_indicators_follow_installed_and_uninstalled_games() {
    use crate::cloud_status::SavedState;
    let game = game();
    game.synced(b"old").await;
    let requested = [("namespace".to_owned(), "game".to_owned())];
    let shown = indicators(&game.config.root, ACCOUNT, &requested).unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].indicator.state, SavedState::Synced);

    played_offline(&game, b"played offline").await;
    archive_with(game.config.clone(), game.cloud.as_ref())
        .await
        .unwrap();
    fs::remove_dir_all(&game.config.install).unwrap();
    removed(&game.config, ACCOUNT).unwrap();

    let shown = indicators(&game.config.root, ACCOUNT, &[]).unwrap();
    assert_eq!(
        shown.len(),
        1,
        "uninstalled games with pending work are listed"
    );
    assert_eq!(shown[0].app_name, "game");
    assert_eq!(shown[0].indicator.state, SavedState::Pending);
    assert!(shown[0].indicator.installation_removed);
    assert_eq!(shown[0].configuration.as_ref(), Some(&game.config));
}
