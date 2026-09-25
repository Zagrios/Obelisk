//! Steam Cloud behind the Cloud sync: the shared suite over an in-memory Steam
//! Cloud, and what only Steam has: its preferences, AutoCloud rules, SDK
//! storage and upload transactions.
use super::fake::{FakeSteamCloud, SLOT, save_rule};
use super::*;
use crate::cloud::sync::tests::{Harness, answer, cloud_suite};
use crate::cloud::{Choice, Phase};
use crate::cloud_status::{self, Indicator};
use crate::supervisor::client::no_other_session;
use std::path::PathBuf;
use steam_kit::cloud::CloudMetadata;

pub(crate) const ACCOUNT: u64 = 0x0110_0001_0000_002a;

pub(crate) fn credentials() -> Credentials {
    Credentials {
        steam_id: ACCOUNT,
        account_name: "fixture".into(),
        refresh_token: "unused".into(),
    }
}

pub(crate) struct SteamGame {
    _temporary: tempfile::TempDir,
    pub config: Config,
    pub cloud: FakeSteamCloud,
    pub control: Arc<Control>,
}

impl SteamGame {
    fn uninstalled(&self) -> MaintenanceTarget {
        MaintenanceTarget::Uninstalled {
            root: self.config.root.clone(),
            app_id: self.config.app_id,
        }
    }
    /// The adapter and its vault without a sync around them.
    fn adapter(&self) -> (Adapter<FakeSteamCloud>, Vault) {
        let vault = Vault::open(&self.config.root, scope(&self.config, ACCOUNT)).unwrap();
        let mut adapter = Adapter {
            config: self.config.clone(),
            credentials: credentials(),
            remote: self.cloud.handle(),
            preferences: CloudPreferencesCache::default(),
            downloaded: Manifest::default(),
            context: None,
        };
        adapter.load().unwrap();
        (adapter, vault)
    }
    fn unavailable(&self) -> Option<UnavailableReason> {
        cloud_status::read(&self.config.root, &Harness::scope(self))
            .unwrap()
            .unavailable
            .map(|marker| marker.reason)
    }
}

impl Harness for SteamGame {
    type Store = Adapter<FakeSteamCloud>;
    const NO_RULES: UnavailableReason = UnavailableReason::NoCloud;

    fn game() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let config = Config {
            root: base.join("cloud"),
            home: base.join("home"),
            install: base.join("game"),
            app_id: 42,
            prefix: None,
        };
        for path in [&config.root, &config.home, &config.install] {
            crate::files::private_directory(path).unwrap();
        }
        Self {
            cloud: FakeSteamCloud::new(ACCOUNT, config.app_id),
            control: Arc::new(Control::new(|_| {})),
            config,
            _temporary: temporary,
        }
    }
    fn control(&self) -> &Arc<Control> {
        &self.control
    }
    fn open(&self) -> Session<FakeSteamCloud> {
        open_with(
            self.cloud.handle(),
            self.config.clone(),
            credentials(),
            uuid::Uuid::new_v4().to_string(),
            Arc::clone(&self.control),
        )
        .unwrap()
    }
    fn save(&self) -> PathBuf {
        self.config.install.join("slot.sav")
    }
    fn slot(&self) -> &'static str {
        SLOT
    }
    fn install(&self) -> PathBuf {
        self.config.install.clone()
    }
    fn publish(&self, bytes: &[u8]) {
        self.cloud.publish(SLOT, bytes);
    }
    fn cloud(&self) -> Option<Vec<u8>> {
        self.cloud.file(SLOT)
    }
    fn uploads(&self) -> usize {
        self.cloud.uploads()
    }
    fn fail_next(&self, transient: bool) {
        self.cloud.fail_next_connection(if transient {
            Failure::Network
        } else {
            Failure::Quota
        });
    }
    fn failure_key(&self) -> String {
        Failure::Quota.key().into()
    }
    fn set_rules(&self, available: bool) {
        self.cloud.set_metadata(available.then(save_rule));
    }
    async fn uninstall(&self) {
        store::archive(&self.config, ACCOUNT).unwrap();
        fs::remove_dir_all(&self.config.install).unwrap();
        store::removed(&self.config, ACCOUNT).unwrap();
    }
    async fn maintain(&self, alive: &SessionAlive<'_>) -> Result<(), Failure> {
        let target = if self.config.install.exists() {
            MaintenanceTarget::Installed(self.config.clone())
        } else {
            self.uninstalled()
        };
        let control = Arc::clone(&self.control);
        maintain_with(self.cloud.handle(), target, &credentials(), control, alive).await
    }
    fn work(&self) -> Option<Work> {
        Context::load(&self.config, ACCOUNT)
            .unwrap()
            .map(|context| context.work())
    }
    fn listed(&self) -> bool {
        store::pending(&self.config.root, ACCOUNT)
            .unwrap()
            .contains(&self.config.app_id)
    }
    fn indicator(&self, requested: bool) -> Option<Indicator> {
        let requested: &[u32] = if requested { &[42] } else { &[] };
        store::indicators(&self.config.root, ACCOUNT, requested)
            .unwrap()
            .into_iter()
            .find(|(app_id, _)| *app_id == self.config.app_id)
            .map(|(_, indicator)| indicator)
    }
    fn root(&self) -> PathBuf {
        self.config.root.clone()
    }
    fn scope(&self) -> StoreScope {
        scope(&self.config, ACCOUNT).into()
    }
    fn account(&self) -> AccountIdentity {
        ACCOUNT.into()
    }
}

mod suite {
    super::cloud_suite!(super::SteamGame);
}

async fn synced(game: &SteamGame, bytes: &[u8]) {
    fs::write(Harness::save(game), bytes).unwrap();
    game.cloud.publish(SLOT, bytes);
    game.open().prepare(&no_other_session).await.unwrap();
}

#[tokio::test]
async fn cloud_turned_off_in_steam_is_recorded_as_unavailable() {
    let game = SteamGame::game();
    fs::write(Harness::save(&game), b"progress").unwrap();
    game.cloud.set_enabled(Some(false));

    game.open().prepare(&no_other_session).await.unwrap();

    assert_eq!(game.control.status().phase, Phase::Disabled);
    assert_eq!(game.unavailable(), Some(UnavailableReason::Disabled));
}

#[tokio::test]
async fn playing_with_cloud_turned_off_leaves_no_pending_work() {
    let game = SteamGame::game();
    synced(&game, b"old").await;
    game.cloud.set_enabled(Some(false));
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    fs::write(Harness::save(&game), b"played with Cloud off").unwrap();

    sync.finished(true).await;

    assert_eq!(game.control.status().phase, Phase::Disabled);
    assert_eq!(game.work().unwrap().pending, None);
    assert_eq!(game.unavailable(), Some(UnavailableReason::Disabled));
    assert!(!game.listed());
}

#[tokio::test]
async fn unknown_steam_preferences_never_authorize_a_transfer() {
    let game = SteamGame::game();
    synced(&game, b"old").await;
    fs::write(Harness::save(&game), b"new progress").unwrap();
    game.cloud.set_enabled(None);
    let control = Arc::clone(&game.control);
    let answering = std::thread::spawn(move || {
        let shown = control.status().decision_id;
        loop {
            let status = control.status();
            if status.decision_id > shown && !status.allowed_choices.is_empty() {
                assert!(status.allowed_choices.contains(&Choice::LaunchAnyway));
                control
                    .answer(status.decision_id, Choice::LaunchAnyway)
                    .unwrap();
                return status.error;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });

    game.open().prepare(&no_other_session).await.unwrap();

    assert_eq!(
        answering.join().unwrap().as_deref(),
        Some(Failure::Preferences.key())
    );
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"old");
    assert_eq!(game.cloud.uploads(), 0);
}

#[tokio::test]
async fn maintenance_refuses_an_uninstalled_exit_save_after_its_cloud_rules_changed() {
    let game = SteamGame::game();
    synced(&game, b"old").await;
    let mut sync = game.open();
    sync.prepare(&no_other_session).await.unwrap();
    sync.started().unwrap();
    fs::write(Harness::save(&game), b"last session").unwrap();
    game.cloud.fail_next_connection(Failure::Quota);
    sync.finished(true).await;
    drop(sync);
    game.uninstall().await;
    let mut changed = save_rule();
    if let CloudMetadata::Object(fields) = &mut changed {
        fields.push(("quota".into(), CloudMetadata::Value("1000".into())));
    }
    game.cloud.set_metadata(Some(changed));

    let refused = game.maintain(&no_other_session).await;

    assert!(matches!(refused, Err(Failure::RulesChanged)));
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"old");
    assert!(game.listed());
    assert_eq!(
        Failure::RulesChanged.key(),
        "backend.steam.cloud.rulesChanged"
    );
}

#[tokio::test]
async fn a_revoked_launch_check_fails_for_the_account_rather_than_as_a_cancellation() {
    let game = SteamGame::game();
    synced(&game, b"old").await;
    game.control.revoke();

    let refused = game.open().prepare(&no_other_session).await;

    assert!(matches!(refused, Err(Failure::Authentication)));
    let status = game.control.status();
    assert_eq!(status.phase, Phase::Failed);
    assert_eq!(status.error.as_deref(), Some(Failure::Authentication.key()));
}

#[tokio::test]
async fn sdk_saves_download_into_the_cache_the_game_reads() {
    let game = SteamGame::game();
    game.cloud.set_metadata(Some(CloudMetadata::Object(vec![
        ("quota".into(), CloudMetadata::Value("1000000".into())),
        ("maxnumfiles".into(), CloudMetadata::Value("1000".into())),
    ])));
    game.cloud.publish("slot.sav", b"cloud progress");
    let mut sync = game.open();

    sync.prepare(&no_other_session).await.unwrap();

    let storage = sync.store().native_storage().unwrap().unwrap();
    assert!(!storage.home.starts_with(&game.config.root));
    let context = sync.store().context.as_ref().unwrap();
    let saved = &context.bindings["slot.sav"].path;
    assert!(saved.starts_with(storage.cache_directory()));
    assert_eq!(fs::read(saved).unwrap(), b"cloud progress");
}

#[tokio::test]
async fn launching_anyway_past_rules_forge_cannot_follow_starts_the_game() {
    let game = SteamGame::game();
    game.cloud.set_metadata(Some(CloudMetadata::Object(vec![(
        "quota".into(),
        CloudMetadata::Value("1000000".into()),
    )])));
    let answering = answer(&game.control, Choice::LaunchAnyway);

    game.open().prepare(&no_other_session).await.unwrap();

    answering.join().unwrap();
    let status = game.control.status();
    assert_eq!(status.phase, Phase::Pending);
    assert_eq!(status.error.as_deref(), Some(Failure::Unsupported.key()));
}

#[test]
fn loading_by_app_rejects_a_context_copied_into_another_account_or_app() {
    let game = SteamGame::game();
    let context = Context::new(&game.config, ACCOUNT, save_rule());
    context.save().unwrap();
    let root = game.config.root.clone();

    assert!(Context::load_app(&root, ACCOUNT, 42).is_ok());
    for (account, app_id) in [(ACCOUNT + 1, 42), (ACCOUNT, 43)] {
        let copied = root
            .join(account.to_string())
            .join(app_id.to_string())
            .join("context.json");
        store::write(&copied, &context).unwrap();
        assert!(matches!(
            Context::load_app(&root, account, app_id),
            Err(Failure::Storage)
        ));
    }
}

#[test]
fn cleanup_keeps_the_last_durable_context_when_memory_has_advanced() {
    let game = SteamGame::game();
    Context::new(&game.config, ACCOUNT, save_rule())
        .save()
        .unwrap();
    let (mut adapter, mut vault) = game.adapter();
    let stage = |bytes: &[u8]| Manifest([(SLOT.into(), vault.stage(bytes, 2).unwrap())].into());
    let desired = stage(b"durable exit");
    let remote = RemoteView {
        revision: "2".into(),
        files: stage(b"durable conflict"),
    };
    let context = adapter.context.as_mut().unwrap();
    context.desired = Some(desired.clone());
    context.needs_work = true;
    context.remote = remote.clone();
    context.save().unwrap();
    // As after an unsuccessful context write, disk still owns these bytes.
    context.desired = None;
    context.remote = RemoteView {
        revision: "3".into(),
        files: Manifest::default(),
    };

    let retained = adapter.retained().unwrap();
    vault
        .collect_garbage(&retained.iter().collect::<Vec<_>>())
        .unwrap();

    assert!(vault.read_content(&desired.0[SLOT]).is_ok());
    assert!(vault.read_content(&remote.files.0[SLOT]).is_ok());
}

/// An upload whose receipt was lost: the vault may have committed it, and
/// its transaction stopped in `phase`.
fn lost_receipt(game: &SteamGame, phase: steam_kit::cloud::UploadPhase) -> (PathBuf, Vec<u8>) {
    let (mut adapter, mut vault) = game.adapter();
    let before = vault.baseline().unwrap().clone();
    vault.set_preferences(Preferences {
        account_enabled: Some(true),
        game_enabled: Some(true),
    });
    let bindings = adapter.bindings(&vault, None, true).unwrap();
    let local = vault.capture(&bindings).unwrap();
    let authorization = vault.authorize(ACCOUNT).unwrap();
    let id = vault
        .queue_upload(&local, &before, "steam-cm-autocloud", authorization)
        .unwrap();
    let transaction = UploadTransaction::create(
        &store::directory(&game.config, ACCOUNT).unwrap(),
        adapter.context().unwrap().listing.clone(),
        vec![CloudUpload {
            name: SLOT.into(),
            bytes: fs::read(Harness::save(game)).unwrap(),
            timestamp: 1,
            platforms: 2,
        }],
        vec![],
    )
    .unwrap();
    let directory = transaction.directory().to_path_buf();
    drop(transaction);
    let journal = directory.join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    state["phase"] = serde_json::to_value(phase).unwrap();
    state["unanswered_mutation"] = true.into();
    let old_bytes = serde_json::to_vec(&state).unwrap();
    fs::write(&journal, &old_bytes).unwrap();
    let context = adapter.context.as_mut().unwrap();
    context.set_work(Work {
        pending: Some(sync::Pending::Upload { desired: local }),
        installation_removed: false,
    });
    context.transaction = Some(directory);
    context.save().unwrap();
    vault.begin_upload(&id, authorization, &before).unwrap();
    drop((adapter, vault));
    crate::tests::wait_for_lock_release(&game.config.root.join("owner.lock"));
    (journal, old_bytes)
}

#[tokio::test]
async fn a_lost_receipt_reopens_a_fresh_upload_without_rewriting_the_old_transaction() {
    use steam_kit::cloud::UploadPhase;
    for phase in [
        UploadPhase::Beginning,
        UploadPhase::Open { batch_id: 7 },
        UploadPhase::Interrupted { batch_id: Some(7) },
        UploadPhase::Closed { batch_id: 7 },
    ] {
        let game = SteamGame::game();
        synced(&game, b"old").await;
        fs::write(Harness::save(&game), b"new progress").unwrap();
        let (journal, old_bytes) = lost_receipt(&game, phase);

        game.open().prepare(&no_other_session).await.unwrap();

        assert_eq!(game.cloud.file(SLOT).unwrap(), b"new progress");
        assert_eq!(game.cloud.uploads(), 1, "one fresh upload");
        assert_eq!(fs::read(&journal).unwrap(), old_bytes);
        let saved = Context::load(&game.config, ACCOUNT).unwrap().unwrap();
        assert!(saved.transaction.is_none());
        assert_eq!(saved.work().pending, None);
    }
}

#[tokio::test]
async fn an_upload_that_landed_before_its_receipt_was_lost_is_not_sent_again() {
    let game = SteamGame::game();
    synced(&game, b"old").await;
    fs::write(Harness::save(&game), b"new progress").unwrap();
    lost_receipt(&game, steam_kit::cloud::UploadPhase::Closed { batch_id: 7 });
    // The server committed it before the reply was lost.
    game.cloud.publish(SLOT, b"new progress");

    game.open().prepare(&no_other_session).await.unwrap();

    assert_eq!(game.cloud.uploads(), 0);
    let vault = Vault::open(&game.config.root, scope(&game.config, ACCOUNT)).unwrap();
    assert!(vault.pending().is_none());
    assert_eq!(
        vault
            .read_content(&vault.baseline().unwrap().files.0[SLOT])
            .unwrap(),
        b"new progress"
    );
}

#[test]
fn receipt_recovery_keeps_hard_errors_and_allows_a_fresh_read_after_timeout() {
    use steam_kit::cloud::CloudError;
    for error in [
        CloudError::Uncertain,
        CloudError::Network,
        CloudError::Timeout,
        CloudError::Changed,
    ] {
        assert!(!recovery_is_settled(Err(error), false).unwrap());
    }
    for error in [
        CloudError::Revoked,
        CloudError::Storage,
        CloudError::Integrity,
        CloudError::Invalid("fixture"),
    ] {
        assert!(recovery_is_settled(Err(error), false).is_err());
    }
    assert!(recovery_is_settled(Ok(()), true).unwrap());
}
