use super::{Game::*, *};
use obelisk_core::Platform::*;
use std::{io::Write, os::unix::fs::PermissionsExt};
use uuid::Uuid;

/// Writes `path`, with its folder, as last changed `age` before `now`.
fn write(path: &Path, now: SystemTime, age: Duration) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "log\n").unwrap();
    age_of(path, now, age);
    path.to_owned()
}

fn age_of(path: &Path, now: SystemTime, age: Duration) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(now - age))
        .unwrap();
}

fn seconds(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

#[test]
fn a_game_report_finds_what_its_launches_wrote() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let now = SystemTime::now();
    let steam = folder(home, Storefront::Steam);
    let log = game_log(home, Steam(1245620), Windows);
    fs::create_dir_all(&steam).unwrap();
    open_log(&log).unwrap().write_all(b"first launch").unwrap();
    open_log(&log).unwrap().write_all(b"second").unwrap();
    let previous = previous_log(&log);
    age_of(&previous, now, seconds(600));
    age_of(&log, now, seconds(60));
    let input = write(&input_log(&log), now, seconds(60));
    write(
        &game_log(home, Steam(1245620), Mac),
        now,
        seconds(3600 * 24 * 3),
    );
    write(
        &launch_report(&steam, 1245620, Uuid::new_v4()),
        now,
        seconds(7200),
    );
    let launch = write(
        &launch_report(&steam, 1245620, Uuid::new_v4()),
        now,
        seconds(60),
    );
    let failure = write(
        &failure_report(&steam, 1245620, Uuid::new_v4()),
        now,
        seconds(300),
    );
    write(
        &launch_report(&steam, 12456201, Uuid::new_v4()),
        now,
        seconds(10),
    );
    write(&game_log(home, Steam(774801), Windows), now, seconds(10));

    let files = files(home, Some(Steam(1245620)), now);

    assert_eq!(files, [log, previous, input, launch, failure]);
    assert_eq!(
        files[0],
        home.join("Library/Logs/Obelisk/Steam/wine-1245620.log")
    );
}

#[test]
fn the_build_launched_last_names_the_logs() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let now = SystemTime::now();
    let unrailed = Gog("1207658924");
    write(&game_log(home, unrailed, Windows), now, seconds(3600));
    let log = write(&game_log(home, unrailed, Mac), now, seconds(60));
    let previous = write(&previous_log(&log), now, seconds(600));
    assert_eq!(files(home, Some(unrailed), now), [log, previous]);

    let tied = tempfile::tempdir().unwrap();
    let tied = tied.path();
    let windows = write(&game_log(tied, unrailed, Windows), now, seconds(60));
    write(&game_log(tied, unrailed, Mac), now, seconds(60));
    assert_eq!(files(tied, Some(unrailed), now), [windows]);

    let fortnite = write(&game_log(home, Epic("Fortnite"), Windows), now, seconds(60));
    assert_eq!(
        fortnite,
        home.join("Library/Logs/Obelisk/Epic/wine-Fortnite.log")
    );
    assert_eq!(
        files(home, Some(Epic("Fortnite")), now),
        std::slice::from_ref(&fortnite)
    );
    let cloud = write(&cloud_note(home, Epic("Fortnite")), now, seconds(30));
    assert_eq!(files(home, Some(Epic("Fortnite")), now), [fortnite, cloud]);
}

#[test]
fn an_obelisk_report_finds_every_store_s_reports_of_the_last_day_newest_first() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let now = SystemTime::now();
    let steam = folder(home, Storefront::Steam);
    let launch = write(
        &launch_report(&steam, 1245620, Uuid::new_v4()),
        now,
        seconds(60),
    );
    let failure = write(
        &failure_report(&steam, 3527290, Uuid::new_v4()),
        now,
        seconds(300),
    );
    let epic = folder(home, Storefront::Epic);
    let elsewhere = write(
        &report(&epic, REPORTS[0], "Fortnite", Uuid::new_v4()),
        now,
        seconds(120),
    );
    write(
        &launch_report(&steam, 1245620, Uuid::new_v4()),
        now,
        seconds(3600 * 30),
    );
    let boundary = write(&launch_report(&steam, 1245620, Uuid::new_v4()), now, RECENT);
    write(&game_log(home, Steam(1245620), Windows), now, seconds(60));

    assert_eq!(
        files(home, None, now),
        [launch, elsewhere, failure, boundary]
    );
}

#[test]
fn hidden_files_and_missing_folders_give_nothing() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let now = SystemTime::now();
    assert_eq!(files(home, None, now), Vec::<PathBuf>::new());
    assert_eq!(files(home, Some(Gog("1")), now), Vec::<PathBuf>::new());

    let steam = folder(home, Storefront::Steam);
    let report = launch_report(&steam, 42, Uuid::new_v4());
    let hidden = steam.join(format!(
        ".tmp{}",
        report.file_name().unwrap().to_string_lossy()
    ));
    write(&hidden, now, seconds(60));
    assert_eq!(files(home, None, now), Vec::<PathBuf>::new());
    assert_eq!(files(home, Some(Steam(42)), now), Vec::<PathBuf>::new());
}

#[test]
fn names_on_disk_stay_those_older_versions_wrote() {
    // Older Obelisk versions and running supervisors wrote these names, so
    // they are spelled out here rather than taken from the naming functions.
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let now = SystemTime::now();
    let logs = home.join("Library/Logs/Obelisk");
    let run = Uuid::new_v4();
    let steam = [
        "Steam/wine-42.log".to_owned(),
        "Steam/wine-42.previous.log".to_owned(),
        "Steam/wine-42.input.log".to_owned(),
        format!("Steam/launch-42-{run}.json"),
        format!("Steam/failed-42-{run}.json"),
    ]
    .map(|name| write(&logs.join(name), now, seconds(60)));
    let epic = ["Epic/game-app.log", "Epic/cloud-app.log"]
        .map(|name| write(&logs.join(name), now, seconds(60)));
    let gog = write(&logs.join("GOG/game-7.log"), now, seconds(60));

    assert_eq!(files(home, Some(Steam(42)), now), steam);
    assert_eq!(files(home, Some(Epic("app")), now), epic);
    assert_eq!(files(home, Some(Gog("7")), now), [gog]);
}

#[test]
fn a_new_launch_keeps_the_last_launchs_log_beside_its_own() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("wine-1245620.log");
    open_log(&log).unwrap().write_all(b"first launch").unwrap();
    open_log(&log).unwrap().write_all(b"second").unwrap();
    assert_eq!(fs::read_to_string(&log).unwrap(), "second");
    let previous = directory.path().join("wine-1245620.previous.log");
    assert_eq!(fs::read_to_string(&previous).unwrap(), "first launch");
    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o777,
        0o600
    );
    open_log(&log).unwrap();
    assert_eq!(fs::read_to_string(&previous).unwrap(), "second");
}
