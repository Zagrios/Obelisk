//! Steam Cloud behind the Cloud sync: the shared suite over an in-memory Steam
//! Cloud, and what only Steam has: its preferences, AutoCloud rules, SDK
//! storage and upload transactions.
use super::fake::{FakeSteamCloud, SLOT, save_rule};
use super::*;
use crate::cloud::record::tests::record_suite;
use crate::cloud::sync::{
    Work,
    tests::{Harness, answer, cloud_suite, listed, uninstall, work},
};
use crate::cloud::{Choice, Phase};
use crate::cloud_status;
use crate::supervisor::client::no_other_session;
use cloud_vault::{AccountIdentity, RemoteView, StoreScope};
use std::{
    fs,
    path::{Path, PathBuf},
};
use steam_kit::cloud::{CloudMetadata, CloudUpload};

pub(crate) const ACCOUNT: u64 = 0x0110_0001_0000_002a;

/// The remote name of `slot.sav` under [`roaming_rule`].
const ROAMING: &str = "%WinAppDataRoaming%Game/slot.sav";

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
    pub(crate) fn adapter(&self) -> (Adapter<FakeSteamCloud>, Vault) {
        let (vault, context) = record::locked(&self.config, &ACCOUNT).unwrap();
        let adapter = Adapter {
            config: self.config.clone(),
            credentials: credentials(),
            remote: self.cloud.handle(),
            preferences: CloudPreferencesCache::default(),
            context,
        };
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
        match &self.config.prefix {
            Some(prefix) => crate::wine::profile(prefix).join("AppData/Roaming/Game/slot.sav"),
            None => self.config.install.join("slot.sav"),
        }
    }
    fn slot(&self) -> &'static str {
        if self.config.prefix.is_some() {
            ROAMING
        } else {
            SLOT
        }
    }
    fn install(&self) -> PathBuf {
        self.config.install.clone()
    }
    fn publish(&self, bytes: &[u8]) {
        // A Windows game's save is published for Windows only.
        let platforms = if self.config.prefix.is_some() { 1 } else { 2 };
        self.cloud.publish_for(self.slot(), bytes, platforms);
    }
    fn cloud(&self) -> Option<Vec<u8>> {
        self.cloud.file(self.slot())
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
        let rule = if self.config.prefix.is_some() {
            roaming_rule
        } else {
            save_rule
        };
        self.cloud.set_metadata(available.then(rule));
    }
    fn run_in(&mut self, prefix: &Path) {
        self.config.prefix = Some(prefix.to_owned());
        self.cloud.set_metadata(Some(roaming_rule()));
        fs::create_dir_all(Harness::save(self).parent().unwrap()).unwrap();
    }
    async fn archive(&self) -> Result<(), Failure> {
        match self.cloud.archive_rules() {
            Some((metadata, listing)) => store::archive_with_rules(&self.config, metadata, listing),
            None => Ok(()),
        }
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
    fn configuration(&self) -> Config {
        self.config.clone()
    }
    fn account_id(&self) -> &u64 {
        &ACCOUNT
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
    super::record_suite!(super::SteamGame);
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
    assert_eq!(work(&game).unwrap().pending, None);
    assert_eq!(game.unavailable(), Some(UnavailableReason::Disabled));
    assert!(!listed(&game));
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
    uninstall(&game).await;
    let mut changed = save_rule();
    if let CloudMetadata::Object(fields) = &mut changed {
        fields.push(("quota".into(), CloudMetadata::Value("1000".into())));
    }
    game.cloud.set_metadata(Some(changed));

    let refused = game.maintain(&no_other_session).await;

    assert!(matches!(refused, Err(Failure::RulesChanged)));
    assert_eq!(game.cloud.file(SLOT).unwrap(), b"old");
    assert!(listed(&game));
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
async fn a_windows_game_reads_its_sdk_saves_from_the_same_native_storage() {
    let mut game = SteamGame::game();
    game.config.prefix = Some(game.config.root.with_file_name("prefix"));
    game.cloud.set_metadata(Some(CloudMetadata::Object(vec![
        ("quota".into(), CloudMetadata::Value("1000000".into())),
        ("maxnumfiles".into(), CloudMetadata::Value("1000".into())),
    ])));
    game.cloud.publish_for("slot.sav", b"windows progress", 1);
    game.cloud.publish_for("mac.sav", b"mac progress", 2);
    let mut sync = game.open();

    sync.prepare(&no_other_session).await.unwrap();

    let storage = sync.store().native_storage().unwrap().unwrap();
    assert_eq!(storage.platforms, 1);
    let context = sync.store().context.as_ref().unwrap();
    assert!(!context.bindings.contains_key("mac.sav"));
    let saved = &context.bindings["slot.sav"].path;
    assert!(saved.starts_with(storage.cache_directory()));
    assert_eq!(fs::read(saved).unwrap(), b"windows progress");
    let index = storage.cache_directory();
    assert_eq!(
        crate::steam::host::native_cache_files(index, 42, storage.platforms)
            .unwrap()
            .len(),
        1
    );
}

/// Saves in the Windows profile's roaming AppData, as ELDEN RING's.
fn roaming_rule() -> CloudMetadata {
    let rule = CloudMetadata::Object(
        [
            ("root", "WinAppDataRoaming"),
            ("path", "Game"),
            ("pattern", "*.sav"),
            ("recursive", "0"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), CloudMetadata::Value(value.into())))
        .collect(),
    );
    CloudMetadata::Object(vec![(
        "savefiles".into(),
        CloudMetadata::Object(vec![("0".into(), rule)]),
    )])
}

/// Rules for every platform in the game's install directory, as Quake's: the
/// rules spell the directory `Id1`, the server's older files `id1`.
fn all_platforms_install_rules() -> CloudMetadata {
    let value = |text: &str| CloudMetadata::Value(text.into());
    let rule = |pattern: &str| {
        CloudMetadata::Object(vec![
            ("root".into(), value("gameinstall")),
            ("path".into(), value("Id1")),
            ("pattern".into(), value(pattern)),
            (
                "platforms".into(),
                CloudMetadata::Object(vec![("1".into(), value("all"))]),
            ),
        ])
    };
    CloudMetadata::Object(vec![(
        "savefiles".into(),
        CloudMetadata::Object(vec![
            ("0".into(), rule("*.sav")),
            ("1".into(), rule("config.cfg")),
        ]),
    )])
}

#[tokio::test]
async fn rules_for_all_platforms_sync_files_the_server_spells_in_another_case() {
    let mut game = SteamGame::game();
    let prefix = game.config.root.join("fixture-prefix");
    game.run_in(&prefix);
    game.cloud.set_metadata(Some(all_platforms_install_rules()));
    let local = game.config.install.join("Id1/test.sav");
    fs::create_dir_all(local.parent().unwrap()).unwrap();
    // Only a case-insensitive volume, macOS's default, reaches `Id1` as `id1`.
    if !game.config.install.join("id1").is_dir() {
        return;
    }
    fs::write(&local, b"synthetic initial progress").unwrap();
    let remote = "%GameInstall%Id1/test.sav";
    game.cloud
        .publish_for(remote, b"synthetic initial progress", u32::MAX);
    let config = game.config.install.join("Id1/config.cfg");
    fs::write(&config, b"synthetic initial config").unwrap();
    let historical_config = "%GameInstall%id1/config.cfg";
    game.cloud
        .publish_for(historical_config, b"synthetic initial config", u32::MAX);
    let mut sync = game.open();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        sync.prepare(&no_other_session),
    )
    .await
    .expect("rules for all platforms should not block on an unsupported-rule question")
    .unwrap();
    assert_eq!(game.control.status().phase, Phase::Ready);
    assert_eq!(game.cloud.uploads(), 0);
    sync.started().unwrap();
    fs::write(&local, b"synthetic new progress").unwrap();
    fs::write(&config, b"synthetic new config").unwrap();
    fs::write(
        game.config.install.join("Id1/new.sav"),
        b"synthetic new file",
    )
    .unwrap();
    sync.finished(true).await;
    assert_eq!(game.control.status().phase, Phase::Synced);
    assert_eq!(game.cloud.file(remote).unwrap(), b"synthetic new progress");
    assert_eq!(
        game.cloud.file(historical_config).unwrap(),
        b"synthetic new config"
    );
    assert!(game.cloud.file("%GameInstall%Id1/config.cfg").is_none());
    let new_remote = "%GameInstall%Id1/new.sav";
    assert_eq!(game.cloud.file(new_remote).unwrap(), b"synthetic new file");
    assert_eq!(game.cloud.file_platforms(new_remote), Some(u32::MAX));
}

#[tokio::test]
async fn a_mac_build_whose_rules_are_all_for_windows_starts_without_cloud() {
    let game = SteamGame::game();
    game.cloud.set_metadata(Some(roaming_rule()));

    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        game.open().prepare(&no_other_session),
    )
    .await
    .expect("no question before the launch")
    .unwrap();

    let status = game.control.status();
    assert_eq!(status.phase, Phase::Unsupported);
    assert_eq!(status.error, None);
    assert_eq!(game.unavailable(), Some(UnavailableReason::Unsupported));
}

#[tokio::test]
async fn launching_anyway_past_rules_obelisk_cannot_follow_starts_the_game() {
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
    let bindings = adapter
        .context
        .as_mut()
        .unwrap()
        .bindings(&vault, None, true)
        .unwrap();
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
        let saved = Context::load(&game.config, &ACCOUNT).unwrap().unwrap();
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
