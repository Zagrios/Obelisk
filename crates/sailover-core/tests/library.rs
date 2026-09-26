use sailover_core::{CoreError, GameKind, Library};
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

fn game_file(root: &TempDir, name: &str) -> std::path::PathBuf {
    let path = root.path().join(name);
    fs::write(&path, b"fixture, not a runnable executable").unwrap();
    path
}

#[test]
fn imports_reload_and_remove_preserve_original_game() {
    let root = TempDir::new().unwrap();
    let storage = root.path().join("catalog");
    let path = game_file(&root, "Example.exe");
    let library = Library::open(&storage).unwrap();
    let game = library.add_local_game(&path).unwrap();
    assert_eq!(game.title, "Example");
    assert_eq!(game.kind, GameKind::WindowsExecutable);
    assert_eq!(game.last_played_at, None);
    library.record_launch(&game.id).unwrap();
    drop(library);

    let reloaded = Library::open(&storage).unwrap();
    let games = reloaded.games().unwrap();
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].id, game.id);
    assert!(games[0].last_played_at.is_some());
    reloaded.remove_game(&game.id).unwrap();
    assert!(reloaded.games().unwrap().is_empty());
    assert_eq!(
        fs::read(path).unwrap(),
        b"fixture, not a runnable executable"
    );
}

#[test]
fn accepts_app_directory_and_rejects_wrong_types_and_missing_paths() {
    let root = TempDir::new().unwrap();
    let library = Library::open(root.path().join("catalog")).unwrap();
    let app = root.path().join("Example.app");
    fs::create_dir(&app).unwrap();
    assert_eq!(
        library.add_local_game(app).unwrap().kind,
        GameKind::NativeMac
    );

    let exe_directory = root.path().join("Folder.exe");
    fs::create_dir(&exe_directory).unwrap();
    for path in [
        game_file(&root, "NotAnApp.app"),
        game_file(&root, "Notes.txt"),
        exe_directory,
        root.path().join("Missing.exe"),
    ] {
        assert!(matches!(
            library.add_local_game(path),
            Err(CoreError::InvalidGame { .. })
        ));
    }
    assert_eq!(library.games().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn canonical_paths_deduplicate_symlinks() {
    let root = TempDir::new().unwrap();
    let path = game_file(&root, "Example.exe");
    let alias = root.path().join("Alias.exe");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let library = Library::open(root.path().join("catalog")).unwrap();
    let first = library.add_local_game(&path).unwrap();
    let second = library.add_local_game(&alias).unwrap();
    assert_eq!(first, second);
    assert_eq!(library.games().unwrap().len(), 1);
}

#[test]
fn corrupt_and_unsupported_catalogs_are_preserved() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("library.json");
    for content in [
        "broken json",
        r#"{"schema_version":999,"games":[]}"#,
        r#"{"schema_version":1,"games":[{"id":"bad"}]}"#,
    ] {
        fs::write(&path, content).unwrap();
        assert!(matches!(
            Library::open(root.path()),
            Err(CoreError::CorruptLibrary { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
    }
}

#[test]
fn corruption_after_open_blocks_mutations_instead_of_overwriting() {
    let root = TempDir::new().unwrap();
    let library = Library::open(root.path().join("catalog")).unwrap();
    let catalog = root.path().join("catalog/library.json");
    fs::write(&catalog, "broken json").unwrap();
    assert!(matches!(
        library.add_local_game(game_file(&root, "Example.exe")),
        Err(CoreError::CorruptLibrary { .. })
    ));
    assert_eq!(fs::read_to_string(catalog).unwrap(), "broken json");
}

#[test]
fn multiple_instances_merge_changes_and_threads_deduplicate() {
    let root = TempDir::new().unwrap();
    let storage = root.path().join("catalog");
    let first = Arc::new(Library::open(&storage).unwrap());
    let second = Library::open(&storage).unwrap();
    let path = game_file(&root, "First.exe");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| first.add_local_game(&path).unwrap());
        }
    });
    second
        .add_local_game(game_file(&root, "Second.exe"))
        .unwrap();
    assert_eq!(first.games().unwrap().len(), 2);
    assert_eq!(second.games().unwrap().len(), 2);
}

#[test]
fn missing_ids_do_not_modify_the_catalog_or_original() {
    let root = TempDir::new().unwrap();
    let storage = root.path().join("catalog");
    let original = game_file(&root, "Example.exe");
    let library = Library::open(&storage).unwrap();
    library.add_local_game(&original).unwrap();
    let before = fs::read(storage.join("library.json")).unwrap();
    assert!(matches!(
        library.remove_game("missing"),
        Err(CoreError::NotFound { .. })
    ));
    assert!(matches!(
        library.record_launch("missing"),
        Err(CoreError::NotFound { .. })
    ));
    assert_eq!(fs::read(storage.join("library.json")).unwrap(), before);
    assert!(original.exists());
}

#[test]
fn orders_by_activity_then_title_and_keeps_missing_games() {
    let root = TempDir::new().unwrap();
    let storage = root.path().join("catalog");
    let library = Library::open(&storage).unwrap();
    let alpha = library
        .add_local_game(game_file(&root, "Alpha.exe"))
        .unwrap();
    let beta = library
        .add_local_game(game_file(&root, "Beta.exe"))
        .unwrap();
    let catalog_path = storage.join("library.json");
    let mut catalog: serde_json::Value =
        serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    for game in catalog["games"].as_array_mut().unwrap() {
        game["added_at"] = 1.into();
        game["last_played_at"] = if game["id"] == beta.id {
            2.into()
        } else {
            serde_json::Value::Null
        };
    }
    fs::write(&catalog_path, serde_json::to_vec(&catalog).unwrap()).unwrap();
    fs::remove_file(&alpha.path).unwrap();
    assert_eq!(
        library
            .games()
            .unwrap()
            .iter()
            .map(|g| g.id.clone())
            .collect::<Vec<_>>(),
        vec![beta.id, alpha.id]
    );
}

#[test]
fn failed_storage_access_does_not_delete_game() {
    let root = TempDir::new().unwrap();
    let storage = root.path().join("catalog");
    let library = Library::open(&storage).unwrap();
    let game_path = game_file(&root, "Example.exe");
    let game = library.add_local_game(&game_path).unwrap();
    let preserved = root.path().join("preserved-catalog");
    fs::rename(&storage, &preserved).unwrap();
    fs::write(&storage, "storage path is now a file").unwrap();
    assert!(matches!(
        library.remove_game(&game.id),
        Err(CoreError::Storage { .. })
    ));
    assert!(game_path.exists());
    assert_eq!(Library::open(preserved).unwrap().games().unwrap().len(), 1);
}
