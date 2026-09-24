use super::*;
use std::os::unix::fs::symlink;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Record {
    version: u32,
    id: String,
    value: u32,
}
impl Entry for Record {
    fn id(&self) -> &str {
        &self.id
    }
    fn valid(&self) -> bool {
        self.value != 0
    }
}

fn record(value: u32) -> Record {
    Record {
        version: VERSION,
        id: uuid::Uuid::new_v4().to_string(),
        value,
    }
}

fn directory() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn private(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn a_written_session_is_current_and_readable_by_id() {
    let root = directory();
    assert!(current::<Record>(root.path()).unwrap().is_none());
    let first = record(1);
    write(root.path(), &first).unwrap();
    let second = record(2);
    write(root.path(), &second).unwrap();

    assert_eq!(
        current::<Record>(root.path()).unwrap(),
        Some(second.clone())
    );
    assert_eq!(
        by_id::<Record>(root.path(), &first.id).unwrap(),
        Some(first.clone())
    );
    for name in ["session.v2.json", &format!("{}.v2.json", second.id)] {
        let mode = fs::metadata(root.path().join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{name} is private");
    }
}

#[test]
fn a_session_named_current_without_its_record_was_never_accepted() {
    let root = directory();
    let entry = record(1);
    private(
        &root.path().join("session.v2.json"),
        serde_json::to_vec(&Current {
            version: VERSION,
            id: entry.id.clone(),
        })
        .unwrap(),
    );

    assert!(current::<Record>(root.path()).unwrap().is_none());
    assert!(by_id::<Record>(root.path(), &entry.id).unwrap().is_none());
}

#[test]
fn links_shared_or_oversized_files_and_mismatched_records_fail_closed() {
    let root = directory();
    let entry = record(1);
    write(root.path(), &entry).unwrap();
    let path = root.path().join(format!("{}.v2.json", entry.id));
    let saved = fs::read(&path).unwrap();

    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(matches!(
        by_id::<Record>(root.path(), &entry.id),
        Err(Error::Storage)
    ));

    let elsewhere = root.path().join("elsewhere");
    private(&elsewhere, &saved);
    fs::remove_file(&path).unwrap();
    symlink(&elsewhere, &path).unwrap();
    assert!(matches!(
        by_id::<Record>(root.path(), &entry.id),
        Err(Error::Storage)
    ));
    fs::remove_file(&path).unwrap();

    private(&path, vec![b' '; MAX as usize + 1]);
    assert!(matches!(
        by_id::<Record>(root.path(), &entry.id),
        Err(Error::Storage)
    ));

    private(&path, "not json");
    assert!(matches!(
        current::<Record>(root.path()),
        Err(Error::Storage)
    ));

    let other = record(1);
    private(&path, serde_json::to_vec(&other).unwrap());
    assert!(matches!(
        by_id::<Record>(root.path(), &entry.id),
        Err(Error::InvalidSession)
    ));

    private(
        &path,
        serde_json::to_vec(&Record {
            value: 0,
            ..entry.clone()
        })
        .unwrap(),
    );
    assert!(matches!(
        by_id::<Record>(root.path(), &entry.id),
        Err(Error::Storage)
    ));
    assert!(matches!(
        by_id::<Record>(root.path(), "not-an-id"),
        Err(Error::InvalidSession)
    ));
}

#[test]
fn a_newer_record_is_an_unknown_live_session_and_unknown_fields_are_tolerated() {
    let root = directory();
    let entry = record(1);
    write(root.path(), &entry).unwrap();
    let path = root.path().join(format!("{}.v2.json", entry.id));

    let mut extended = serde_json::to_value(&entry).unwrap();
    extended["added_later"] = serde_json::json!({"any": ["shape"]});
    private(&path, serde_json::to_vec(&extended).unwrap());
    assert_eq!(current::<Record>(root.path()).unwrap(), Some(entry));

    extended["version"] = (VERSION + 1).into();
    extended["value"] = "changed type".into();
    private(&path, serde_json::to_vec(&extended).unwrap());
    assert!(matches!(
        current::<Record>(root.path()),
        Err(Error::NewerSession)
    ));

    extended["version"] = (VERSION - 1).into();
    private(&path, serde_json::to_vec(&extended).unwrap());
    assert!(matches!(
        current::<Record>(root.path()),
        Err(Error::Storage)
    ));
}

#[test]
fn a_refused_write_keeps_the_previous_record_and_leaves_no_temporary_file() {
    let root = directory();
    let mut entry = record(1);
    write(root.path(), &entry).unwrap();
    let names = || {
        let mut names: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = names();

    entry.value = 2;
    let path = root.path().join(format!("{}.v2.json", entry.id));
    let saved = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    symlink(root.path().join("elsewhere"), &path).unwrap();
    assert!(
        write(root.path(), &entry).is_err(),
        "a link is never replaced"
    );
    fs::remove_file(&path).unwrap();
    private(&path, &saved);

    struct Huge(Record);
    impl Serialize for Huge {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut value = serde_json::to_value(&self.0).unwrap();
            value["padding"] = "x".repeat(MAX as usize).into();
            value.serialize(serializer)
        }
    }
    assert!(replace(&path, &Huge(entry.clone())).is_err());

    assert_eq!(names(), before);
    assert_eq!(
        by_id::<Record>(root.path(), &entry.id)
            .unwrap()
            .unwrap()
            .value,
        1
    );
}
