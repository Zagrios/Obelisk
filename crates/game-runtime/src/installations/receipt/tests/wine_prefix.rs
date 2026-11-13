//! The Wine prefix a receipt names, where its saves live, and the engine that
//! picks it: the player's choice, else the game's profile, else the Linux
//! engine for an installation that holds Easy Anti-Cheat. The answer is the
//! prefix its launch runs in, whatever reads it.
use super::*;
use crate::{
    Locations,
    easy_anti_cheat::tests::protect,
    launch_settings::Choices,
    wine::{
        Engine,
        tests::{installation, locations},
    },
};
use std::{
    process::Command,
    sync::mpsc::{self, RecvTimeoutError},
    task::Poll,
    time::Duration,
};

/// A game no profile names.
const UNKNOWN: u32 = 42;
/// A game whose profile names the Linux engine.
const ELDEN_RING: u32 = 1245620;
/// A game whose profile names no engine.
const GROUNDED: u32 = 962130;

/// The receipt of Steam's `app_id`, installed in `install`.
fn steam_in(app_id: u32, install: &Path) -> Receipt<Steam> {
    Receipt {
        directory: install.to_string_lossy().into_owned(),
        ..steam_app(app_id)
    }
}

/// The prefix the engine keeps of the installation in `install`.
fn prefix_on(engine: Engine, locations: &Locations, install: &Path) -> PathBuf {
    let name = install.file_name().unwrap().to_string_lossy();
    match engine {
        Engine::Rosetta => locations.wine_prefixes.join(name.as_ref()),
        Engine::Linux => locations
            .wine_prefixes
            .join(format!("{name}-linux"))
            .join("prefix"),
    }
}

/// An installation `name` under `games`, holding Easy Anti-Cheat or not.
fn installed(games: &Path, name: &str, protected: bool) -> PathBuf {
    let install = games.join(name);
    std::fs::create_dir_all(&install).unwrap();
    if protected {
        protect(&install, "Game", "start_protected_game.exe", "Game.exe");
    }
    install
}

/// The prefix of `receipt`'s Wine, which its launch runs in too.
async fn prefix_of<S: Storefront>(receipt: &Receipt<S>, locations: &Locations) -> Option<PathBuf> {
    let prefix = receipt.wine_prefix(locations).await.unwrap();
    let launched = receipt
        .windows(locations)
        .ok()
        .flatten()
        .map(|windows| windows.prefix().to_owned());
    assert_eq!(prefix, launched, "{}", receipt.directory);
    prefix
}

fn choose_engine<S: Storefront>(receipt: &Receipt<S>, locations: &Locations, engine: Engine) {
    let choices = Choices {
        engine: Some(engine),
        ..Choices::default()
    };
    receipt
        .choose_launch_settings(&locations.library, choices)
        .unwrap();
}

#[tokio::test]
async fn a_receipt_without_a_choice_follows_its_game_s_profile_then_its_installation_s_easy_anti_cheat()
 {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let games = root.join("Games");

    for (name, app_id, protected, engine) in [
        // Easy Anti-Cheat alone puts a game no profile names on the Linux
        // engine.
        ("unknown-plain", UNKNOWN, false, Engine::Rosetta),
        ("unknown-protected", UNKNOWN, true, Engine::Linux),
        // A profile that names the game decides, whatever the installation
        // holds.
        ("grounded-plain", GROUNDED, false, Engine::Rosetta),
        ("grounded-protected", GROUNDED, true, Engine::Rosetta),
        ("elden-ring-plain", ELDEN_RING, false, Engine::Linux),
        ("elden-ring-protected", ELDEN_RING, true, Engine::Linux),
    ] {
        let install = installed(&games, name, protected);
        let receipt = steam_in(app_id, &install);

        assert_eq!(
            prefix_of(&receipt, &locations).await,
            Some(prefix_on(engine, &locations, &install)),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_chosen_engine_decides_the_prefix_whatever_the_profile_and_the_installation_say() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let games = root.join("Games");

    for (name, app_id, protected, engine) in [
        ("unknown-plain", UNKNOWN, false, Engine::Linux),
        ("unknown-protected", UNKNOWN, true, Engine::Rosetta),
        ("grounded-protected", GROUNDED, true, Engine::Linux),
        ("elden-ring-plain", ELDEN_RING, false, Engine::Rosetta),
    ] {
        let install = installed(&games, name, protected);
        let receipt = steam_in(app_id, &install);
        choose_engine(&receipt, &locations, engine);

        assert_eq!(
            prefix_of(&receipt, &locations).await,
            Some(prefix_on(engine, &locations, &install)),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_missing_installation_folder_still_names_its_prefix_by_its_game_and_its_choice() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let missing = |name: &str| root.join("Games").join(name);

    for (name, app_id, engine) in [
        ("missing-unknown", UNKNOWN, Engine::Rosetta),
        ("missing-grounded", GROUNDED, Engine::Rosetta),
        ("missing-elden-ring", ELDEN_RING, Engine::Linux),
    ] {
        let receipt = steam_in(app_id, &missing(name));

        assert_eq!(
            prefix_of(&receipt, &locations).await,
            Some(prefix_on(engine, &locations, &missing(name))),
            "{name}"
        );
        assert!(!missing(name).exists());
    }
    let chosen = steam_in(UNKNOWN, &missing("missing-chosen"));
    choose_engine(&chosen, &locations, Engine::Linux);
    assert_eq!(
        prefix_of(&chosen, &locations).await,
        Some(prefix_on(
            Engine::Linux,
            &locations,
            &missing("missing-chosen")
        ))
    );
}

#[tokio::test]
async fn a_mac_build_names_no_prefix_whatever_engine_its_game_would_run_on() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let install = installed(&root.join("Games"), "native", false);

    for app_id in [UNKNOWN, ELDEN_RING] {
        let mac = Receipt {
            platform: Platform::Mac,
            ..steam_in(app_id, &install)
        };

        // A Mac build runs outside Wine: its launch has no prefix either.
        assert_eq!(prefix_of(&mac, &locations).await, None);
    }
}

#[tokio::test]
async fn a_folder_that_names_no_prefix_has_none_and_a_launch_that_cannot_start() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let games = root.join("Games");
    std::fs::create_dir_all(games.join("anywhere")).unwrap();
    // A folder with no name of its own, such as `..`.
    let nameless = games.join("anywhere/..");

    for app_id in [UNKNOWN, ELDEN_RING] {
        let receipt = steam_in(app_id, &nameless);

        assert_eq!(prefix_of(&receipt, &locations).await, None);
        assert!(matches!(
            receipt.windows(&locations),
            Err(crate::launch::Error::NotExecutable)
        ));
    }
}

#[tokio::test]
async fn a_receipt_reads_its_installation_each_time_it_names_its_prefix() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let install = installed(&root.join("Games"), "changing", false);
    let receipt = steam_in(UNKNOWN, &install);
    let on = |engine| Some(prefix_on(engine, &locations, &install));

    assert_eq!(prefix_of(&receipt, &locations).await, on(Engine::Rosetta));
    protect(&install, "Game", "start_protected_game.exe", "Game.exe");
    assert_eq!(prefix_of(&receipt, &locations).await, on(Engine::Linux));
    std::fs::remove_dir_all(install.join("Game")).unwrap();
    assert_eq!(prefix_of(&receipt, &locations).await, on(Engine::Rosetta));
}

/// The receipt of each storefront's game in `install`.
fn every_storefront_in(install: &Path) -> (Receipt<Steam>, Receipt<Epic>, Receipt<Gog>) {
    let directory = install.to_string_lossy().into_owned();
    (
        steam_in(UNKNOWN, install),
        Receipt {
            directory: directory.clone(),
            ..sandbox_game(Platform::Windows)
        },
        Receipt {
            directory,
            ..witcher(Platform::Windows)
        },
    )
}

#[tokio::test]
async fn every_storefront_s_receipt_names_its_prefix_the_same_way() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let games = root.join("Games");

    for (name, protected, engine) in [
        ("shared-plain", false, Engine::Rosetta),
        ("shared-protected", true, Engine::Linux),
    ] {
        let install = installed(&games, name, protected);
        let (steam, epic, gog) = every_storefront_in(&install);
        let expected = Some(prefix_on(engine, &locations, &install));

        assert_eq!(
            prefix_of(&steam, &locations).await,
            expected,
            "Steam {name}"
        );
        assert_eq!(prefix_of(&epic, &locations).await, expected, "Epic {name}");
        assert_eq!(prefix_of(&gog, &locations).await, expected, "GOG {name}");
    }
    let install = installed(&games, "shared-chosen", false);
    let (steam, epic, gog) = every_storefront_in(&install);
    choose_engine(&steam, &locations, Engine::Linux);
    let expected = Some(prefix_on(Engine::Linux, &locations, &install));
    // One file of choices per installation folder: a receipt of another
    // storefront in the same folder reads it.
    assert_eq!(prefix_of(&steam, &locations).await, expected);
    assert_eq!(prefix_of(&epic, &locations).await, expected);
    assert_eq!(prefix_of(&gog, &locations).await, expected);
}

/// Opens the write end of the named pipe `pipe` and closes it: it waits for a
/// reader, which then reads to its end.
fn release(pipe: &Path) {
    drop(std::fs::OpenOptions::new().write(true).open(pipe));
}

#[tokio::test]
async fn a_wine_prefix_is_worked_out_off_the_async_runtime_while_the_disk_waits() {
    let (_directory, root) = installation();
    let locations = locations(&root);
    let install = installed(&root.join("Games"), "sleeping", false);
    // A named pipe where the anti-cheat's settings should be: reading it waits
    // for a writer, as a disk that is still spinning up makes a read wait.
    let pipe = install.join("EasyAntiCheat/Settings.json");
    std::fs::create_dir_all(pipe.parent().unwrap()).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success()
    );
    // A walk that held this thread would never come back: a watchdog frees it
    // after a while, so that the test fails instead of hanging.
    let (disarm, armed) = mpsc::channel::<()>();
    let watched = pipe.clone();
    std::thread::spawn(move || {
        if armed.recv_timeout(Duration::from_secs(5)) == Err(RecvTimeoutError::Timeout) {
            release(&watched);
        }
    });
    let receipt = steam_in(UNKNOWN, &install);

    let mut asked = std::pin::pin!(receipt.wine_prefix(&locations));
    let first = std::future::poll_fn(|cx| Poll::Ready(asked.as_mut().poll(cx))).await;
    drop(disarm);
    assert!(
        first.is_pending(),
        "the walk held the caller's thread: {first:?}"
    );

    let released = std::thread::spawn(move || release(&pipe));
    assert_eq!(
        asked.await.unwrap(),
        Some(prefix_on(Engine::Rosetta, &locations, &install)),
        "the pipe is no settings file, so the walk finds no anti-cheat"
    );
    released.join().unwrap();
}
