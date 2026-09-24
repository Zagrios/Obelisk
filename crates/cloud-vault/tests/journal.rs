use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
};

use cloud_vault::{Error, journal};

#[test]
fn journal_reads_reject_every_group_or_other_permission() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("journal.json");
    for mode in [0o640, 0o620, 0o610, 0o604, 0o602, 0o601, 0o644, 0o666] {
        journal::write(&path, &42_u64).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            matches!(journal::read::<u64>(&path), Err(Error::Invalid)),
            "journal with mode {mode:o} must be refused"
        );
    }
}

#[test]
fn journal_writes_remain_private_and_private_reads_preserve_json() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("journal.json");
    let value = serde_json::json!({"generation": 42, "files": ["slot-a", "slot-b"]});
    for mode in [0o600, 0o400, 0o700] {
        journal::write(&path, &value).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            journal::read::<serde_json::Value>(&path).unwrap(),
            Some(value.clone())
        );
    }
}

#[test]
fn journal_reads_preserve_missing_paths_and_reject_links_and_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let missing = temporary.path().join("missing.json");
    assert_eq!(journal::read::<u64>(&missing).unwrap(), None);
    let target = temporary.path().join("target.json");
    journal::write(&target, &42_u64).unwrap();
    let link = temporary.path().join("link.json");
    symlink(&target, &link).unwrap();
    assert!(matches!(journal::read::<u64>(&link), Err(Error::Invalid)));
    fs::remove_file(&target).unwrap();
    assert!(matches!(journal::read::<u64>(&link), Err(Error::Invalid)));
    assert!(matches!(
        journal::read::<u64>(temporary.path()),
        Err(Error::Invalid)
    ));
}

#[test]
fn journal_reads_keep_the_64_mib_boundary_and_parse_the_complete_json_document() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("journal.json");
    journal::write(&path, &42_u64).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    let spaces = [b' '; 64 * 1024];
    let mut remaining = 64 * 1024 * 1024 - 2;
    while remaining > 0 {
        let count = remaining.min(spaces.len());
        file.write_all(&spaces[..count]).unwrap();
        remaining -= count;
    }
    assert_eq!(journal::read::<u64>(&path).unwrap(), Some(42));
    file.write_all(b" ").unwrap();
    assert!(matches!(journal::read::<u64>(&path), Err(Error::Invalid)));
    drop(file);
    fs::write(&path, b"42\n43\n").unwrap();
    assert!(matches!(
        journal::read::<u64>(&path),
        Err(Error::Journal(_))
    ));
    fs::write(&path, b"42\n").unwrap();
    assert_eq!(journal::read::<u64>(&path).unwrap(), Some(42));
}
