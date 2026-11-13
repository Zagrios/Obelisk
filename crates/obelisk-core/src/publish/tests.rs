use super::*;
use crate::{
    install::{FileDigest, PARTIAL_DIRECTORY},
    testing::disk::{self, mode},
};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{MetadataExt, symlink},
};

/// An installation under a canonical root, with the folder partial files wait
/// in, and a folder outside it that nothing may reach.
struct Site {
    _directory: tempfile::TempDir,
    root: PathBuf,
    outside: tempfile::TempDir,
    cancel: CancelToken,
    partials: usize,
}

impl Site {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        fs::create_dir_all(root.join(PARTIAL_DIRECTORY)).unwrap();
        Self {
            _directory: directory,
            root,
            outside: tempfile::tempdir().unwrap(),
            cancel: CancelToken::new(),
            partials: 0,
        }
    }

    fn publisher(&self) -> Publisher {
        Publisher::new(&self.root, &self.cancel)
    }

    /// A partial file holding `bytes`, open for writing.
    fn partial(&mut self, bytes: &[u8]) -> (File, PathBuf) {
        self.partials += 1;
        let path = self
            .root
            .join(PARTIAL_DIRECTORY)
            .join(format!("{}.part", self.partials));
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        disk::set_mode(&path, 0o600);
        (file, path)
    }

    /// Publishes `bytes` as `name`, checked part by part. The partial file is
    /// returned beside the outcome.
    fn publish(
        &mut self,
        publisher: &mut Publisher,
        name: &str,
        bytes: &[u8],
        program: bool,
    ) -> (PathBuf, Result<(), PublishError>) {
        self.publish_checked(publisher, name, bytes, program, Integrity::PartsChecked)
    }

    fn publish_checked(
        &mut self,
        publisher: &mut Publisher,
        name: &str,
        bytes: &[u8],
        program: bool,
        integrity: Integrity<'_>,
    ) -> (PathBuf, Result<(), PublishError>) {
        let (file, at) = self.partial(bytes);
        let result = publisher.publish(Assembled {
            file,
            at: &at,
            name,
            program,
            integrity,
        });
        (at, result)
    }

    /// Writes `bytes` at `name` with `mode`, making its folders.
    fn place(&self, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
        disk::place(&self.root, name, bytes, mode)
    }

    /// A file outside the installation, which nothing may touch.
    fn elsewhere(&self) -> PathBuf {
        let path = self.outside.path().join("elsewhere");
        fs::write(&path, b"unchanged").unwrap();
        disk::set_mode(&path, 0o600);
        path
    }

    fn outside_is_untouched(&self) {
        let found: Vec<_> = fs::read_dir(self.outside.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let elsewhere = self.outside.path().join("elsewhere");
        if elsewhere.exists() {
            assert_eq!(found.len(), 1, "{found:?}");
            assert_eq!(fs::read(&elsewhere).unwrap(), b"unchanged");
            assert_eq!(mode(&elsewhere), 0o600);
        } else {
            assert!(found.is_empty(), "{found:?}");
        }
    }

    fn record(&self) -> PathBuf {
        disk::record(&self.root)
    }
}

fn unsafe_path(result: &Result<(), PublishError>) -> bool {
    matches!(
        result,
        Err(PublishError::Install(InstallError::UnsafePath(_)))
    )
}

/// A manifest entry of a file whose digest does not matter to `complete`.
struct Entry(String, u64);

impl VerificationFile for Entry {
    fn name(&self) -> &str {
        &self.0
    }
    fn size(&self) -> u64 {
        self.1
    }
    fn digest(&self) -> FileDigest<'_> {
        FileDigest::Sha1(&[0; 20])
    }
}

#[test]
fn a_published_file_replaces_what_was_there_by_a_rename() {
    let mut site = Site::new();
    let mut publisher = site.publisher();
    let tool = site.place("bin/tool", b"old tool", 0o444);
    let table = site.place("data/table", b"old table", 0o777);
    let old = File::open(&tool).unwrap();
    let inodes = [&tool, &table].map(|path| fs::metadata(path).unwrap().ino());

    let (at, result) = site.publish(&mut publisher, "bin/tool", b"new tool", true);
    result.unwrap();
    let (_, result) = site.publish(&mut publisher, "data/table", b"new table", false);
    result.unwrap();
    let (nested, result) = site.publish(&mut publisher, "data/deep/er/table", b"table", false);
    result.unwrap();

    assert_eq!(fs::read(&tool).unwrap(), b"new tool");
    assert_eq!(fs::read(&table).unwrap(), b"new table");
    assert_eq!(
        (mode(&tool), mode(&table)),
        (0o755, 0o644),
        "an installed copy takes the published mode"
    );
    assert_eq!(
        io::read_to_string(&old).unwrap(),
        "old tool",
        "the old file was not written over"
    );
    assert_ne!(fs::metadata(&tool).unwrap().ino(), inodes[0]);
    assert_ne!(fs::metadata(&table).unwrap().ino(), inodes[1]);
    assert!(!at.exists() && !nested.exists());
    assert_eq!(
        fs::read(site.root.join("data/deep/er/table")).unwrap(),
        b"table",
        "the folders were made"
    );
}

#[test]
fn programs_are_published_executable_and_the_rest_readable() {
    let rows: [(&[u8], bool, u32); 11] = [
        (b"", true, 0o755),
        (b"plain", true, 0o755),
        (b"\xcf\xfa\xed\xfe program", false, 0o755),
        (b"\xce\xfa\xed\xfe program", false, 0o755),
        (b"\xca\xfe\xba\xbe universal", false, 0o755),
        (b"\xca\xfe\xba\xbf universal", false, 0o755),
        (b"#!/bin/sh\n", false, 0o755),
        (b"{\"json\": true}", false, 0o644),
        (b"# not a script", false, 0o644),
        (b"#", false, 0o644),
        (b"", false, 0o644),
    ];
    let mut site = Site::new();
    let mut publisher = site.publisher();
    for (index, (bytes, program, expected)) in rows.into_iter().enumerate() {
        let name = format!("published/{index}");
        let (_, result) = site.publish(&mut publisher, &name, bytes, program);
        result.unwrap();
        assert_eq!(mode(&site.root.join(&name)), expected, "published {index}");

        let name = format!("kept/{index}");
        let path = site.place(&name, bytes, if expected == 0o755 { 0o600 } else { 0o777 });
        publisher.keep(&name, program).unwrap();
        assert_eq!(mode(&path), expected, "kept {index}");
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes,
            "kept {index} is not rewritten"
        );
    }
}

#[test]
fn a_name_that_escapes_the_installation_is_refused_before_the_partial_file_is_touched() {
    let mut site = Site::new();
    let mut publisher = site.publisher();
    for name in [
        "../x",
        "/abs",
        "a//b",
        "C:\\x",
        ".obelisk-content/x",
        ".obelisk-install.json",
    ] {
        let (at, result) = site.publish(&mut publisher, name, b"bytes", true);
        assert!(unsafe_path(&result), "{name}: {result:?}");
        assert_eq!(fs::read(&at).unwrap(), b"bytes", "{name}");
        assert_eq!(mode(&at), 0o600, "{name}");
    }
    assert!(publisher.keep("../x", false).is_err());
    site.outside_is_untouched();
}

#[test]
fn a_link_or_a_file_in_the_way_is_refused_and_nothing_outside_is_reached() {
    let mut site = Site::new();
    let mut publisher = site.publisher();
    let elsewhere = site.elsewhere();
    let outside = site.outside.path().to_path_buf();

    // A link where a folder belongs, there before the call.
    symlink(&outside, site.root.join("linked")).unwrap();
    let (at, result) = site.publish(&mut publisher, "linked/file", b"bytes", true);
    assert!(unsafe_path(&result), "{result:?}");
    assert_eq!(fs::read(&at).unwrap(), b"bytes");

    // A folder that becomes a link after an earlier file went into it: the
    // folders made already are checked again for every file.
    let (_, result) = site.publish(&mut publisher, "bin/first", b"first", false);
    result.unwrap();
    fs::rename(site.root.join("bin"), site.root.join("moved")).unwrap();
    symlink(&outside, site.root.join("bin")).unwrap();
    let (at, result) = site.publish(&mut publisher, "bin/second", b"second", false);
    assert!(unsafe_path(&result), "{result:?}");
    assert_eq!(fs::read(&at).unwrap(), b"second");

    // A file where a folder belongs.
    site.place("plain", b"file", 0o644);
    let (at, result) = site.publish(&mut publisher, "plain/inside", b"bytes", false);
    assert!(unsafe_path(&result), "{result:?}");
    assert_eq!(fs::read(&at).unwrap(), b"bytes");

    // A folder, a link and a second link to a file at the target.
    fs::create_dir_all(site.root.join("data/folder")).unwrap();
    let (at, result) = site.publish(&mut publisher, "data/folder", b"bytes", false);
    assert!(unsafe_path(&result), "{result:?}");
    assert_eq!(fs::read(&at).unwrap(), b"bytes");
    symlink(&elsewhere, site.root.join("data/link")).unwrap();
    let (at, result) = site.publish(&mut publisher, "data/link", b"bytes", true);
    assert!(unsafe_path(&result), "{result:?}");
    assert_eq!(fs::read(&at).unwrap(), b"bytes");
    fs::hard_link(&elsewhere, site.root.join("data/hard")).unwrap();
    let (at, result) = site.publish(&mut publisher, "data/hard", b"bytes", true);
    assert!(unsafe_path(&result), "{result:?}");
    assert_eq!(fs::read(&at).unwrap(), b"bytes");

    assert_eq!(fs::read(&elsewhere).unwrap(), b"unchanged");
    assert_eq!(mode(&elsewhere), 0o600);
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        1,
        "nothing reached the folder the link leads to"
    );

    // Files in place are not given a mode through a link either.
    for name in ["linked/elsewhere", "data/link", "data/hard", "plain/inside"] {
        let result = publisher.keep(name, true);
        assert!(
            matches!(result, Err(InstallError::UnsafePath(_))),
            "{name}: {result:?}"
        );
    }
    assert_eq!(mode(&elsewhere), 0o600);
}

#[test]
fn a_file_that_does_not_hash_to_its_manifest_replaces_nothing() {
    let mut site = Site::new();
    let mut publisher = site.publisher();
    let bad = site.place("data/bad", b"old bytes", 0o640);
    let old_inode = fs::metadata(&bad).unwrap().ino();

    let (file, at) = site.partial(b"new bytes");
    let result = publisher.publish(Assembled {
        file,
        at: &at,
        name: "data/bad",
        program: true,
        integrity: Integrity::Whole {
            written: &[1; 20],
            expected: &[2; 20],
        },
    });
    assert!(matches!(result, Err(PublishError::Mismatch)), "{result:?}");
    assert_eq!(fs::read(&bad).unwrap(), b"old bytes");
    assert_eq!(mode(&bad), 0o640);
    assert_eq!(fs::metadata(&bad).unwrap().ino(), old_inode);
    assert_eq!(fs::read(&at).unwrap(), b"new bytes");
    assert_eq!(mode(&at), 0o600, "a rejected file is not given a mode");

    let (file, at) = site.partial(b"bytes");
    let result = publisher.publish(Assembled {
        file,
        at: &at,
        name: "fresh/deep/bad",
        program: false,
        integrity: Integrity::Whole {
            written: &[1; 20],
            expected: &[2; 20],
        },
    });
    assert!(matches!(result, Err(PublishError::Mismatch)), "{result:?}");
    assert!(!site.root.join("fresh/deep/bad").exists());
    assert!(
        site.root.join("fresh/deep").is_dir(),
        "the folders were made before the hash was compared"
    );

    let (_, result) = site.publish_checked(
        &mut publisher,
        "data/good",
        b"good",
        false,
        Integrity::Whole {
            written: &[3; 20],
            expected: &[3; 20],
        },
    );
    result.unwrap();
    let (_, result) = site.publish(&mut publisher, "data/parts", b"parts", false);
    result.unwrap();
    assert_eq!(fs::read(site.root.join("data/good")).unwrap(), b"good");
    assert_eq!(fs::read(site.root.join("data/parts")).unwrap(), b"parts");
}

#[test]
fn a_cancelled_transfer_stops_before_the_rename() {
    let mut site = Site::new();
    let mut publisher = site.publisher();
    site.cancel.cancel();

    let (at, result) = site.publish(&mut publisher, "bin/tool", b"bytes", false);

    assert!(
        matches!(result, Err(PublishError::Install(InstallError::Cancelled))),
        "{result:?}"
    );
    assert_eq!(fs::read(&at).unwrap(), b"bytes");
    assert!(!site.root.join("bin/tool").exists());
}

#[test]
fn a_folder_is_handed_to_the_drive_when_the_next_file_lands_elsewhere() {
    // A folder that cannot be opened any more shows when it was synced: not
    // after each of its files, but once the next file lands in another.
    let mut site = Site::new();
    let mut publisher = site.publisher();
    for name in ["a/1", "a/2"] {
        let (_, result) = site.publish(&mut publisher, name, b"bytes", false);
        result.unwrap();
    }
    fs::rename(site.root.join("a"), site.root.join("moved")).unwrap();

    let (_, result) = site.publish(&mut publisher, "b/1", b"bytes", false);

    assert!(
        matches!(&result, Err(PublishError::Install(InstallError::Io(error)))
            if error.kind() == io::ErrorKind::NotFound),
        "{result:?}"
    );
    assert_eq!(
        fs::read(site.root.join("b/1")).unwrap(),
        b"bytes",
        "the file is in place when the previous folder fails"
    );

    let mut site = Site::new();
    let mut publisher = site.publisher();
    for name in ["a/1", "a/2", "b/1", "a/3"] {
        let (_, result) = site.publish(&mut publisher, name, b"bytes", false);
        result.unwrap();
    }
}

#[test]
fn the_last_folder_is_handed_to_the_drive_before_the_files_are_recorded() {
    let mut site = Site::new();
    let mut publisher = site.publisher();
    let (_, result) = site.publish(&mut publisher, "a/1", b"bytes", false);
    result.unwrap();
    fs::rename(site.root.join("a"), site.root.join("moved")).unwrap();

    assert!(publisher.complete(&[Entry("a/1".into(), 5)]).is_err());
    assert!(!site.record().exists());

    let mut site = Site::new();
    let mut publisher = site.publisher();
    let (_, result) = site.publish(&mut publisher, "a/1", b"bytes", false);
    result.unwrap();

    publisher.complete(&[Entry("a/1".into(), 5)]).unwrap();
    assert!(site.record().is_file());
}
