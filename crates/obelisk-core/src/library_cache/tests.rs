use super::*;
use serde_json::json;
use tempfile::TempDir;

fn entry(value: Value) -> Entry {
    match value {
        Value::Object(entry) => entry,
        _ => panic!("an entry is an object"),
    }
}

fn names(entries: &[Entry]) -> Vec<&str> {
    entries
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect()
}

fn library(cache: &LibraryCache, storefront: &str, account: &str) -> Vec<Entry> {
    cache.library(storefront, account, |entry| Some(entry.clone()))
}

/// The file as a later Obelisk would read it, past every shared document.
fn on_disk(directory: &TempDir) -> Document {
    Document::read(&directory.path().join(FILE_NAME))
}

#[test]
fn every_storefront_and_account_keeps_its_own_library_in_one_compact_file() {
    let directory = TempDir::new().unwrap();
    let cache = LibraryCache::open(directory.path());
    cache.account("steam", "7").replace(
        vec![entry(
            json!({"id": "620", "name": "Portal 2", "record": {"has_dlc": true}}),
        )],
        &[],
    );
    cache
        .account("steam", "8")
        .replace(vec![entry(json!({"id": "570", "name": "Dota 2"}))], &[]);
    cache.account("epic", "epic-a").replace(
        vec![entry(
            json!({"id": "space:rocket", "name": "Rocket League"}),
        )],
        &[],
    );
    cache.flush();

    let bytes = fs::read(directory.path().join(FILE_NAME)).unwrap();
    assert!(!bytes.contains(&b'\n'), "written compact");
    let file: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(file["version"], 1);
    assert_eq!(file["steam"]["7"][0]["record"]["has_dlc"], true);
    let reopened = on_disk(&directory);
    assert_eq!(
        names(&reopened.libraries["steam"]["8"]),
        ["Dota 2"],
        "saving one library keeps the others"
    );
    assert_eq!(
        names(&reopened.libraries["epic"]["epic-a"]),
        ["Rocket League"]
    );
    assert!(library(&cache, "steam", "9").is_empty());
}

#[test]
fn every_cache_of_a_directory_shares_one_document() {
    let directory = TempDir::new().unwrap();
    let first = LibraryCache::open(directory.path());
    let second = LibraryCache::open(directory.path());
    first
        .account("steam", "7")
        .replace(vec![entry(json!({"id": "620", "name": "Portal 2"}))], &[]);
    second.record_launch("steam", "7", "620", 1_790_000_000);
    second.account("epic", "epic-a").replace(
        vec![entry(
            json!({"id": "space:rocket", "name": "Rocket League"}),
        )],
        &[],
    );
    first.flush();

    let reopened = on_disk(&directory);
    assert_eq!(
        reopened.libraries["steam"]["7"][0][LAUNCH], "2026-09-21T14:13:20Z",
        "neither cache overwrote the other's change"
    );
    assert_eq!(
        names(&reopened.libraries["epic"]["epic-a"]),
        ["Rocket League"]
    );
}

#[test]
fn a_reloaded_library_keeps_each_games_launch_and_the_details_another_request_loaded() {
    let directory = TempDir::new().unwrap();
    let cache = LibraryCache::open(directory.path());
    let account = cache.account("steam", "7");
    account.replace(
        vec![
            entry(json!({"id": "620", "name": "Portal 2"})),
            entry(json!({"id": "570", "name": "Dota 2"})),
        ],
        &["platforms"],
    );
    account.update(|entry| entry.insert("platforms".into(), json!(["macos"])).is_none());
    cache.record_launch("steam", "7", "620", 1_790_000_000);

    account.replace(
        vec![
            entry(json!({"id": "620", "name": "Portal 2 renamed"})),
            entry(json!({"id": "400", "name": "Portal", "platforms": ["windows"]})),
        ],
        &["platforms"],
    );
    let library = library(&cache, "steam", "7");
    assert_eq!(
        names(&library),
        ["Portal 2 renamed", "Portal"],
        "Dota 2 left the library"
    );
    assert_eq!(library[0]["platforms"], json!(["macos"]));
    assert_eq!(library[0][LAUNCH], "2026-09-21T14:13:20Z");
    assert_eq!(
        library[1]["platforms"],
        json!(["windows"]),
        "a loaded field wins"
    );
}

#[test]
fn a_library_loaded_again_unchanged_is_not_written_again() {
    let directory = TempDir::new().unwrap();
    let cache = LibraryCache::open(directory.path());
    let account = cache.account("steam", "7");
    let games = || vec![entry(json!({"id": "620", "name": "Portal 2"}))];
    account.replace(games(), &[]);
    account.update(|entry| entry.insert("platforms".into(), json!(["macos"])).is_none());
    cache.flush();
    fs::remove_file(directory.path().join(FILE_NAME)).unwrap();

    account.replace(games(), &["platforms"]);
    account.update(|_| false);
    cache.flush();
    assert!(!directory.path().join(FILE_NAME).exists());
}

#[test]
fn an_update_never_creates_a_library() {
    let directory = TempDir::new().unwrap();
    let cache = LibraryCache::open(directory.path());
    cache
        .account("epic", "epic-a")
        .update(|entry| entry.insert("playtimeSeconds".into(), json!(60)).is_none());
    cache.record_launch("epic", "epic-a", "space:rocket", 1_790_000_000);
    cache.flush();
    assert!(library(&cache, "epic", "epic-a").is_empty());
    assert!(!directory.path().join(FILE_NAME).exists());
}

#[test]
fn forgetting_an_account_drops_its_library_and_launches_and_its_earlier_session_can_no_longer_write_it()
 {
    let directory = TempDir::new().unwrap();
    let cache = LibraryCache::open(directory.path());
    let earlier = cache.account("steam", "7");
    earlier.replace(vec![entry(json!({"id": "620", "name": "Portal 2"}))], &[]);
    cache
        .account("steam", "8")
        .replace(vec![entry(json!({"id": "570", "name": "Dota 2"}))], &[]);
    cache.record_launch("steam", "7", "620", 1_790_000_000);

    cache.forget("steam", "7");
    earlier.replace(vec![entry(json!({"id": "620", "name": "Portal 2"}))], &[]);
    assert!(
        library(&cache, "steam", "7").is_empty(),
        "a signed-out session's late answer"
    );
    assert!(cache.launches().is_empty());
    assert_eq!(names(&library(&cache, "steam", "8")), ["Dota 2"]);

    cache
        .account("steam", "7")
        .replace(vec![entry(json!({"id": "620", "name": "Portal 2"}))], &[]);
    assert_eq!(
        names(&library(&cache, "steam", "7")),
        ["Portal 2"],
        "signing in again"
    );
}

#[test]
fn launches_read_back_as_swift_wrote_them() {
    let directory = TempDir::new().unwrap();
    fs::write(
        directory.path().join(FILE_NAME),
        json!({
            "version": 1,
            "steam": {"7": [
                {"id": "620", "lastLaunchedAt": "2026-09-14T23:53:20Z"},
                {"id": "570", "lastLaunchedAt": "yesterday"},
                {"id": "400"},
            ]},
            "epic": {"epic-a": [{"id": "space:rocket", "lastLaunchedAt": "2000-02-29T00:00:00Z"}]},
        })
        .to_string(),
    )
    .unwrap();
    let mut launches = LibraryCache::open(directory.path()).launches();
    launches.sort_by_key(|launch| launch.at);
    assert_eq!(
        launches,
        [
            Launch {
                storefront: "epic".into(),
                account: "epic-a".into(),
                game: "space:rocket".into(),
                at: 951_782_400,
            },
            Launch {
                storefront: "steam".into(),
                account: "7".into(),
                game: "620".into(),
                at: 1_789_430_000,
            },
        ]
    );
}

#[test]
fn dates_are_iso_8601_in_utc() {
    for (at, text) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (1_790_000_000, "2026-09-21T14:13:20Z"),
        (4_102_444_799, "2099-12-31T23:59:59Z"),
    ] {
        assert_eq!(iso8601(at), text);
        assert_eq!(parse_iso8601(text), Some(at));
    }
    for text in [
        "2026-09-21T14:13:20",
        "2026-13-01T00:00:00Z",
        "2026-09-21T24:00:00Z",
        "1969-12-31T23:59:59Z",
        "2026-09-21 14:13:20Z",
    ] {
        assert_eq!(parse_iso8601(text), None, "{text}");
    }
}

#[test]
fn an_unreadable_file_has_no_library_and_the_next_change_replaces_it() {
    for content in ["not json", r#"{"version": 2, "steam": {"7": []}}"#] {
        let directory = TempDir::new().unwrap();
        fs::write(directory.path().join(FILE_NAME), content).unwrap();
        let cache = LibraryCache::open(directory.path());
        assert!(library(&cache, "steam", "7").is_empty(), "{content}");
        cache
            .account("steam", "7")
            .replace(vec![entry(json!({"id": "620", "name": "Portal 2"}))], &[]);
        cache.flush();
        assert_eq!(
            names(&on_disk(&directory).libraries["steam"]["7"]),
            ["Portal 2"]
        );
    }
}
