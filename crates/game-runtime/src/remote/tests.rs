use super::*;
use serde_json::{Value, json};
use std::{future::ready, os::unix::fs::MetadataExt};

const BUILT_IN: &str = r#"{"format": 1, "games": [{"name": "built"}]}"#;

/// A game of the toy table: its name, unless it is `refused`.
fn entry(game: Value) -> Option<String> {
    let name = game["name"].as_str()?;
    (name != "refused").then(|| name.to_owned())
}

fn table() -> Table<String> {
    Table::new("table.json", BUILT_IN, entry)
}

/// A table of the format Obelisk reads, of the games `names`.
fn of(names: &[&str]) -> Vec<u8> {
    let games: Vec<Value> = names.iter().map(|name| json!({"name": name})).collect();
    json!({"format": FORMAT, "games": games})
        .to_string()
        .into_bytes()
}

fn names(table: &Table<String>) -> Vec<&'static str> {
    table.entries().iter().map(String::as_str).collect()
}

#[tokio::test]
async fn a_table_read_from_main_is_read_and_kept() {
    let library = tempfile::tempdir().unwrap();
    let table = table();
    table
        .read_again(library.path(), ready(Some(of(&["main"]))))
        .await;
    assert_eq!(names(&table), ["main"]);
    assert_eq!(
        fs::read(library.path().join("table.json")).unwrap(),
        of(&["main"])
    );
}

#[tokio::test]
async fn without_main_the_kept_table_is_read() {
    let library = tempfile::tempdir().unwrap();
    let kept = library.path().join("table.json");
    fs::write(&kept, of(&["kept"])).unwrap();
    let table = table();
    table.read_again(library.path(), ready(None)).await;
    assert_eq!(names(&table), ["kept"]);
    assert_eq!(fs::read(&kept).unwrap(), of(&["kept"]));
}

#[tokio::test]
async fn without_main_or_a_kept_table_the_built_in_one_is_read() {
    let library = tempfile::tempdir().unwrap();
    let table = table();
    table.read_again(library.path(), ready(None)).await;
    assert_eq!(names(&table), ["built"]);
    assert!(!library.path().join("table.json").exists());
}

#[tokio::test]
async fn main_s_table_wins_over_the_kept_one() {
    let library = tempfile::tempdir().unwrap();
    let kept = library.path().join("table.json");
    fs::write(&kept, of(&["kept"])).unwrap();
    let table = table();
    table
        .read_again(library.path(), ready(Some(of(&["main"]))))
        .await;
    assert_eq!(names(&table), ["main"]);
    assert_eq!(fs::read(&kept).unwrap(), of(&["main"]));
}

#[tokio::test]
async fn the_kept_table_is_read_before_main_answers() {
    let library = tempfile::tempdir().unwrap();
    fs::write(library.path().join("table.json"), of(&["kept"])).unwrap();
    let table = table();
    let main = async {
        assert_eq!(names(&table), ["kept"]);
        None
    };
    table.read_again(library.path(), main).await;
}

#[tokio::test]
async fn a_table_obelisk_does_not_read_is_neither_read_nor_kept() {
    let library = tempfile::tempdir().unwrap();
    let kept = library.path().join("table.json");
    fs::write(&kept, of(&["kept"])).unwrap();
    let table = table();
    table.read_again(library.path(), ready(None)).await;
    let read = table.entries();
    let mut other_format: Value = serde_json::from_slice(&of(&["main"])).unwrap();
    other_format["format"] = 2.into();
    let mut no_format = other_format.clone();
    no_format.as_object_mut().unwrap().remove("format");
    for unread in [
        b"404: Not Found".to_vec(),
        br#"{"format": 1, "games": ["#.to_vec(),
        other_format.to_string().into_bytes(),
        no_format.to_string().into_bytes(),
        of(&[]),
        of(&["refused", "refused"]),
    ] {
        table.read_again(library.path(), ready(Some(unread))).await;
        assert!(std::ptr::eq(table.entries(), read));
        assert_eq!(fs::read(&kept).unwrap(), of(&["kept"]));
    }
}

#[tokio::test]
async fn a_kept_table_obelisk_does_not_read_leaves_the_built_in_one() {
    let library = tempfile::tempdir().unwrap();
    fs::write(library.path().join("table.json"), of(&[])).unwrap();
    let table = table();
    table.read_again(library.path(), ready(None)).await;
    assert_eq!(names(&table), ["built"]);
}

#[tokio::test]
async fn the_same_table_is_neither_read_nor_written_again() {
    let library = tempfile::tempdir().unwrap();
    let kept = library.path().join("table.json");
    let table = table();
    table
        .read_again(library.path(), ready(Some(of(&["main"]))))
        .await;
    let (read, written) = (table.entries(), fs::metadata(&kept).unwrap().ino());
    table
        .read_again(library.path(), ready(Some(of(&["main"]))))
        .await;
    assert!(std::ptr::eq(table.entries(), read));
    assert_eq!(fs::metadata(&kept).unwrap().ino(), written);

    // `main` answering with the table Obelisk was built with keeps it.
    let library = tempfile::tempdir().unwrap();
    let table = self::table();
    let built_in = table.entries();
    table
        .read_again(library.path(), ready(Some(BUILT_IN.as_bytes().to_vec())))
        .await;
    assert!(std::ptr::eq(table.entries(), built_in));
    assert_eq!(
        fs::read(library.path().join("table.json")).unwrap(),
        BUILT_IN.as_bytes()
    );
}

#[tokio::test]
async fn an_entry_obelisk_refuses_leaves_the_others() {
    let library = tempfile::tempdir().unwrap();
    let table = table();
    table
        .read_again(library.path(), ready(Some(of(&["a", "refused", "b"]))))
        .await;
    assert_eq!(names(&table), ["a", "b"]);
}

#[tokio::test]
async fn only_the_first_refresh_reads_the_kept_table() {
    let library = tempfile::tempdir().unwrap();
    let kept = library.path().join("table.json");
    fs::write(&kept, of(&["kept"])).unwrap();
    let table = table();
    table.read_again(library.path(), ready(None)).await;
    assert_eq!(names(&table), ["kept"]);
    fs::write(&kept, of(&["later"])).unwrap();
    table.read_again(library.path(), ready(None)).await;
    assert_eq!(names(&table), ["kept"]);

    table
        .read_again(library.path(), ready(Some(of(&["main"]))))
        .await;
    fs::write(&kept, of(&["later"])).unwrap();
    table.read_again(library.path(), ready(None)).await;
    assert_eq!(names(&table), ["main"]);
}

/// Each name is also the kept file's, in the library.
#[test]
fn the_tables_are_where_they_were_on_main_and_in_the_library() {
    assert_eq!(
        url(crate::profile::NAME),
        "https://raw.githubusercontent.com/Zagrios/Obelisk/main/crates/game-runtime/profiles.json"
    );
    assert_eq!(
        url(crate::notes::NAME),
        "https://raw.githubusercontent.com/Zagrios/Obelisk/main/crates/game-runtime/notes.json"
    );
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn a_development_build_keeps_the_table_it_was_built_with() {
    let library = tempfile::tempdir().unwrap();
    let kept = library.path().join("table.json");
    fs::write(&kept, of(&["kept"])).unwrap();
    let table = table();
    table.refresh(library.path()).await;
    assert_eq!(names(&table), ["built"]);
    assert_eq!(fs::read(&kept).unwrap(), of(&["kept"]));
}
