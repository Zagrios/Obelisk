use super::*;
use cloud_vault::{Bindings, Scope, SelectedFile, Vault};

/// A library with a Windows game's save in its Wine prefix, which the Cloud
/// archive claims, and a disk to move it to, both canonical.
struct Places {
    _root: tempfile::TempDir,
    library: PathBuf,
    disk: PathBuf,
    save: PathBuf,
}

fn scope() -> Scope {
    Scope {
        steam_id: 0x0110_0001_0000_0001,
        app_id: 10,
        cloud_app_id: 10,
    }
}

fn bindings(save: &Path) -> Bindings {
    Bindings::from([(
        "slot".to_owned(),
        SelectedFile {
            path: save.to_owned(),
            platforms: 7,
        },
    )])
}

fn places() -> Places {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap();
    let (library, disk) = (path.join("com.zagrios.obelisk"), path.join("Disk"));
    let saves = library.join("Prefixes/Game/drive_c/users/obelisk/Saved Games");
    fs::create_dir_all(&saves).unwrap();
    fs::create_dir_all(&disk).unwrap();
    let save = saves.join("slot.sav");
    fs::write(&save, b"level 3").unwrap();
    std::os::unix::fs::symlink("../drive_c", library.join("Prefixes/Game/c:")).unwrap();
    let mut vault = Vault::open(&library.join("SteamCloud"), scope()).unwrap();
    vault.capture(&bindings(&save)).unwrap();
    Places {
        _root: root,
        library,
        disk,
        save,
    }
}

/// The saves `owners.json` names, at `library`.
fn claimed(library: &Path) -> Vec<String> {
    let owners: serde_json::Value =
        cloud_vault::journal::read(&library.join("SteamCloud/owners.json"))
            .unwrap()
            .unwrap();
    owners["files"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

#[test]
fn a_chosen_folder_gets_obelisk_s_library_folder_inside_it() {
    let places = places();
    assert_eq!(chosen(&places.disk), places.disk.join(CHOSEN_DIRECTORY));
    let ours = places.disk.join(CHOSEN_DIRECTORY);
    assert_eq!(chosen(&ours), ours);
}

#[test]
fn the_library_moves_whole_on_its_own_disk_and_its_saves_follow() {
    let places = places();
    let destination = chosen(&places.disk);
    let candidate = inspect(&places.library, &destination).unwrap();
    assert_eq!(candidate.path, destination);
    assert_eq!(candidate.copied_bytes, 0, "nothing copies on one disk");
    let moved = move_library(
        &places.library,
        &destination,
        &CancelToken::new(),
        |_, _| {},
    )
    .unwrap();
    assert_eq!(moved, destination);
    assert!(!places.library.exists());
    let save = destination.join(places.save.strip_prefix(&places.library).unwrap());
    assert_eq!(fs::read(&save).unwrap(), b"level 3");
    assert_eq!(claimed(&destination), [save.to_string_lossy()]);
}

#[test]
fn a_library_copied_to_another_disk_keeps_its_links_and_the_original() {
    let places = places();
    let destination = chosen(&places.disk);
    let mut heard = Vec::new();
    let other_disk = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::CrossesDevices));
    let moved = move_with(
        &places.library,
        &destination,
        &CancelToken::new(),
        |copied, total| heard.push((copied, total)),
        other_disk,
    )
    .unwrap();
    assert_eq!(moved, destination);
    assert!(
        heard
            .last()
            .is_some_and(|(copied, total)| copied == total && *total > 0)
    );
    let link = destination.join("Prefixes/Game/c:");
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(fs::read_link(&link).unwrap(), Path::new("../drive_c"));
    let save = destination.join(places.save.strip_prefix(&places.library).unwrap());
    assert_eq!(claimed(&destination), [save.to_string_lossy()]);
    // The original stays whole until Obelisk starts in the new library.
    assert_eq!(fs::read(&places.save).unwrap(), b"level 3");
    assert_eq!(claimed(&places.library), [places.save.to_string_lossy()]);
    remove_left(&places.library, &destination).unwrap();
    assert!(!places.library.exists());
    assert!(destination.join("SteamCloud").is_dir());
}

#[test]
fn a_cancelled_copy_leaves_nothing_on_the_disk() {
    let places = places();
    let cancel = CancelToken::new();
    cancel.cancel();
    let failure = move_with(
        &places.library,
        &chosen(&places.disk),
        &cancel,
        |_, _| {},
        |_, _| Err(io::Error::from(io::ErrorKind::CrossesDevices)),
    )
    .unwrap_err();
    assert!(matches!(failure, MoveFailure::Cancelled));
    assert_eq!(fs::read_dir(&places.disk).unwrap().count(), 0);
    assert_eq!(fs::read(&places.save).unwrap(), b"level 3");
}

#[test]
fn a_folder_missing_inside_the_library_or_taken_is_refused() {
    let places = places();
    let refused = |destination: &Path| inspect(&places.library, destination).unwrap_err();
    assert_eq!(
        refused(&places.disk.join("Gone/Obelisk Library")),
        Refusal::Missing
    );
    assert_eq!(
        refused(Path::new("relative/Obelisk Library")),
        Refusal::Missing
    );
    assert_eq!(
        refused(&places.library.join("Prefixes/Obelisk Library")),
        Refusal::Inside
    );
    assert_eq!(refused(&places.library), Refusal::Inside);
    let taken = chosen(&places.disk);
    fs::create_dir_all(taken.join("Something")).unwrap();
    assert_eq!(refused(&taken), Refusal::Occupied);
    // An empty folder, as Finder leaves it, takes the library.
    fs::remove_dir(taken.join("Something")).unwrap();
    fs::write(taken.join(".DS_Store"), b"").unwrap();
    assert!(move_library(&places.library, &taken, &CancelToken::new(), |_, _| {}).is_ok());
}

#[test]
fn games_in_obelisk_s_own_games_folder_keep_the_library_in_place() {
    let places = places();
    let storefront = places.library.join("Games/steam");
    fs::create_dir_all(&storefront).unwrap();
    // An empty storefront folder holds no game.
    assert!(inspect(&places.library, &chosen(&places.disk)).is_ok());
    fs::create_dir(storefront.join("steam-620-macos")).unwrap();
    assert_eq!(
        inspect(&places.library, &chosen(&places.disk)).unwrap_err(),
        Refusal::HoldsGames
    );
}

#[test]
fn a_library_reached_through_a_link_goes_to_the_real_folder() {
    let places = places();
    let real = places.disk.join("Real");
    fs::create_dir(&real).unwrap();
    let link = places.disk.join("Link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let candidate = inspect(&places.library, &link.join(CHOSEN_DIRECTORY)).unwrap();
    assert_eq!(candidate.path, real.join(CHOSEN_DIRECTORY));
}

#[test]
fn only_a_library_apart_from_the_new_one_is_removed() {
    let places = places();
    assert!(remove_left(&places.library.join("SteamCloud"), &places.library).is_err());
    assert!(remove_left(places.library.parent().unwrap(), &places.library).is_err());
    assert!(places.library.join("SteamCloud").is_dir());
    remove_left(&places.disk.join("Gone"), &places.library).unwrap();
}
