//! Two accounts of the storefront signed in at once, account 43 beside the
//! fixture's account 42: they share its installations.
use super::*;

/// Account 43's installations, and the storefront session they reach.
struct Neighbour {
    fake: Arc<Fake>,
    installations: Installations<Steam>,
}

impl Neighbour {
    fn receipts(&self) -> Vec<Receipt<Steam>> {
        self.installations.state().receipts
    }
}

impl Fixture {
    /// Account 43 signs in beside account 42, with the receipts its own file
    /// holds: its library lists `games`.
    fn neighbour(&self, games: &[u32]) -> Neighbour {
        let fake = Fake::new();
        let installations = self.open(43, self.neighbour_file(), fake.clone());
        installations.lists(games.to_vec());
        Neighbour {
            fake,
            installations,
        }
    }

    fn neighbour_file(&self) -> PathBuf {
        self.root.join("steam-installations-43.json")
    }
}

#[tokio::test]
async fn an_account_holds_the_installations_its_neighbour_holds_of_the_games_its_library_lists() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[]);
    assert!(
        neighbour.receipts().is_empty(),
        "its library does not list the game"
    );

    neighbour.installations.lists(vec![440, 620]);
    assert_eq!(neighbour.receipts(), [fixture.receipt()]);
    assert_eq!(
        load::<Steam>(&fixture.neighbour_file()).unwrap(),
        [fixture.receipt()],
        "it keeps it once its neighbour signed out"
    );
    assert!(neighbour.fake.calls().is_empty(), "nothing is downloaded");
}

#[tokio::test]
async fn an_account_that_signs_in_gives_its_neighbour_the_installations_it_holds() {
    let fixture = Fixture::new(false).await;
    fixture.installations.lists(vec![620]);
    let installation = fixture.root.join("Games/Steam/steam-620-macos");
    std::fs::create_dir_all(&installation).unwrap();
    save(
        &fixture.neighbour_file(),
        &[receipt(&installation, Platform::Mac)],
    )
    .unwrap();

    let neighbour = fixture.neighbour(&[620]);
    assert_eq!(fixture.installations.state().receipts, neighbour.receipts());
    assert_eq!(ids(&fixture.saved()), ["620-macos"]);
}

#[tokio::test]
async fn an_account_takes_no_installation_whose_folder_went() {
    let fixture = Fixture::new(true).await;
    // Removed outside Obelisk: its receipt shows until a disk change.
    std::fs::remove_dir_all(&fixture.receipt().directory).unwrap();

    let neighbour = fixture.neighbour(&[620]);
    assert!(neighbour.receipts().is_empty());
    // Each drops it as its disk is read again, and neither gives it back.
    fixture.folder.volumes_changed();
    assert!(fixture.installations.state().receipts.is_empty());
    assert!(neighbour.receipts().is_empty());
}

#[tokio::test]
async fn a_game_one_account_installs_is_installed_for_its_neighbour() {
    let fixture = Fixture::new(false).await;
    let neighbour = fixture.neighbour(&[620]);

    fixture.installations.install(fixture.offer());
    idle(&fixture.installations).await;
    eventually(|| !neighbour.receipts().is_empty()).await;

    assert_eq!(neighbour.receipts(), fixture.installations.state().receipts);
    assert!(
        neighbour.fake.calls().is_empty(),
        "one download serves both"
    );
}

#[tokio::test]
async fn a_transfer_one_account_starts_asks_both_for_verification_until_one_succeeds() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);

    stopped_update(&fixture).await;
    assert_eq!(
        unverified(&fixture.installations.state().receipts),
        ["620-macos"]
    );
    assert_eq!(unverified(&neighbour.receipts()), ["620-macos"]);

    // The neighbour's own Transfer repairs it for both.
    neighbour.installations.update("620-macos");
    idle(&neighbour.installations).await;
    eventually(|| unverified(&fixture.installations.state().receipts).is_empty()).await;
    assert!(unverified(&neighbour.receipts()).is_empty());
    assert_eq!(fixture.saved(), fixture.installations.state().receipts);
}

#[tokio::test]
async fn accounts_that_meet_both_verify_an_installation_either_left_unverified() {
    let fixture = Fixture::new(true).await;
    save(&fixture.neighbour_file(), &[half(fixture.receipt())]).unwrap();

    let neighbour = fixture.neighbour(&[620]);
    assert!(fixture.receipt().needs_verification);
    assert!(neighbour.receipts()[0].needs_verification);
    assert_eq!(
        (
            fixture.receipt().total_bytes,
            neighbour.receipts()[0].total_bytes
        ),
        (100, 0),
        "each keeps the rest of what it recorded"
    );
}

#[tokio::test]
async fn an_account_keeps_its_own_folder_of_a_game_its_neighbour_installed_elsewhere() {
    let fixture = Fixture::new(true).await;
    let own = fixture.outside.path().join("Steam/steam-620-macos");
    std::fs::create_dir_all(&own).unwrap();
    save(&fixture.neighbour_file(), &[receipt(&own, Platform::Mac)]).unwrap();
    let neighbour = fixture.neighbour(&[620]);

    stopped_update(&fixture).await;
    fixture.installations.update("620-macos");
    idle(&fixture.installations).await;

    assert_eq!(
        neighbour.receipts(),
        [receipt(&own, Platform::Mac)],
        "another folder is another installation"
    );
}

#[tokio::test]
async fn a_transfer_holds_its_game_for_every_account_of_the_storefront() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    assert_eq!(neighbour.installations.state().gate, open_gate());

    fixture.fake.hold();
    fixture.installations.update("620-macos");
    eventually(|| fixture.fake.calls().len() == 1).await;
    eventually(|| neighbour.installations.state().gate != open_gate()).await;

    let state = neighbour.installations.state();
    assert_eq!(
        state.gate,
        Gate {
            modify: Opening::Open { except: vec![620] },
            uninstall: Opening::Closed,
            launch: Opening::Open { except: vec![620] },
            maintain: Opening::Open { except: vec![620] },
        }
    );
    assert_eq!(queued(&state), [620], "its page shows the game as queued");
    assert!(
        !state.downloading,
        "the Transfer is its neighbour's to show"
    );
    neighbour.installations.launch("620-macos");
    neighbour.installations.update("620-macos");
    neighbour.installations.uninstall("620-macos");
    let state = neighbour.installations.state();
    assert!(!state.launching && state.uninstall_game.is_none());
    assert!(neighbour.fake.calls().is_empty());

    fixture.fake.release();
    idle(&fixture.installations).await;
    eventually(|| neighbour.installations.state().gate == open_gate()).await;
    assert!(queued(&neighbour.installations.state()).is_empty());
}

#[tokio::test]
async fn a_game_queued_by_a_neighbour_leaves_the_queue_from_either_account() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    fixture.fake.hold();
    fixture.installations.update("620-macos");
    eventually(|| queued(&neighbour.installations.state()) == [620]).await;

    neighbour.installations.cancel(&620);
    idle(&fixture.installations).await;
    fixture.fake.release();

    eventually(|| queued(&neighbour.installations.state()).is_empty()).await;
    assert_eq!(
        fixture.installations.state().transfer.unwrap().state,
        TransferState::Cancelled
    );
}

#[tokio::test]
async fn a_newer_build_installs_once_for_the_accounts_that_hold_its_game() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    for fake in [&fixture.fake, &neighbour.fake] {
        fake.with(|script| script.newer.push(620));
    }
    fixture.installations.set_automatic_updates(true);
    neighbour.installations.set_automatic_updates(true);

    fixture.fake.hold();
    fixture.installations.check_updates();
    eventually(|| fixture.fake.calls() == ["download:620"]).await;
    neighbour.installations.check_updates();
    fixture.fake.release();
    idle(&fixture.installations).await;
    eventually(|| unverified(&neighbour.receipts()).is_empty()).await;
    // As Steam answers once the build is installed.
    neighbour.fake.with(|script| script.newer.clear());

    idle(&neighbour.installations).await;
    assert!(neighbour.installations.state().updates.is_empty());
    assert!(neighbour.fake.calls().is_empty());
}

#[tokio::test]
async fn an_automatic_update_one_account_stopped_waits_for_the_next_check_on_both() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    for (fake, installations) in [
        (&fixture.fake, &fixture.installations),
        (&neighbour.fake, &neighbour.installations),
    ] {
        fake.with(|script| script.newer.push(620));
        installations.check_updates();
        eventually(|| installations.state().updates == ["620-macos"]).await;
    }

    fixture.fake.hold();
    fixture.installations.set_automatic_updates(true);
    eventually(|| fixture.fake.calls() == ["download:620"]).await;
    neighbour.installations.set_automatic_updates(true);
    // Its player stops it: the neighbour's gate opens again.
    fixture.installations.cancel(&620);
    idle(&fixture.installations).await;
    fixture.fake.release();
    eventually(|| neighbour.installations.state().gate == open_gate()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        neighbour.fake.calls().is_empty(),
        "the storefront tried it since the last check"
    );
    assert_eq!(fixture.fake.calls(), ["download:620"]);
    assert_eq!(neighbour.installations.state().updates, ["620-macos"]);
}

#[tokio::test]
async fn an_uninstall_removes_the_installation_for_every_account_that_holds_it() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    let installation = PathBuf::from(&fixture.receipt().directory);

    neighbour.installations.uninstall("620-macos");
    eventually(|| neighbour.installations.state().uninstall_game.is_none()).await;

    assert!(!installation.exists());
    assert!(neighbour.receipts().is_empty());
    assert!(fixture.installations.state().receipts.is_empty());
    assert!(fixture.saved().is_empty());
    assert_eq!(neighbour.installations.state().report, None);
    // Each account's Cloud archive took its saves before the files went.
    for fake in [&fixture.fake, &neighbour.fake] {
        assert_eq!(fake.calls(), ["archive:620", "removed:620"]);
    }
}

#[tokio::test]
async fn an_uninstall_keeps_a_game_whose_neighbour_s_saves_cannot_be_archived() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    let installation = PathBuf::from(&fixture.receipt().directory);
    fixture.fake.with(|script| {
        script.archive = Some(Failure::Failed("backend.steam.cloud.network".into()))
    });

    neighbour.installations.uninstall("620-macos");
    eventually(|| neighbour.installations.state().report.is_some()).await;

    assert_eq!(
        neighbour.installations.state().report,
        Some(Report::Uninstall("backend.steam.cloud.network".into()))
    );
    assert!(installation.exists());
    assert_eq!(ids(&fixture.installations.state().receipts), ["620-macos"]);
    assert_eq!(ids(&neighbour.receipts()), ["620-macos"]);
}

#[tokio::test]
async fn an_uninstall_holds_the_storefront_for_every_account() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    neighbour.fake.hold_archive();

    neighbour.installations.uninstall("620-macos");
    eventually(|| fixture.installations.state().gate != open_gate()).await;
    assert_eq!(
        fixture.installations.state().gate,
        Gate {
            modify: Opening::Open { except: vec![620] },
            uninstall: Opening::Closed,
            launch: Opening::Closed,
            maintain: Opening::Open { except: vec![620] },
        }
    );
    fixture.installations.launch("620-macos");
    assert!(!fixture.installations.state().launching);

    neighbour.fake.release_archive();
    eventually(|| fixture.installations.state().receipts.is_empty()).await;
    eventually(|| fixture.installations.state().gate == open_gate()).await;
}

#[tokio::test]
async fn an_installation_two_accounts_hold_moves_once() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    let (chosen, moved) = fixture.elsewhere();
    fixture.fake.hold_relocation();

    fixture.folder.choose(&chosen).unwrap();
    eventually(|| {
        fixture
            .fake
            .calls()
            .contains(&"relocated:620:steam-620-macos".to_owned())
    })
    .await;
    assert_eq!(fixture.moves().len(), 1);
    fixture.fake.release_relocation();
    eventually(|| fixture.moves().is_empty()).await;

    for installations in [&fixture.installations, &neighbour.installations] {
        eventually(|| installations.state().receipts[0].directory == moved.to_string_lossy()).await;
    }
    assert!(neighbour.fake.calls().is_empty());
}

#[tokio::test]
async fn an_account_that_signed_out_no_longer_holds_its_neighbours() {
    let fixture = Fixture::new(true).await;
    let neighbour = fixture.neighbour(&[620]);
    fixture.fake.hold();
    fixture.installations.update("620-macos");
    eventually(|| neighbour.installations.state().gate != open_gate()).await;

    fixture.installations.signed_out();
    eventually(|| neighbour.installations.state().gate == open_gate()).await;
    fixture.fake.release();
}
