//! What Steam hands the publish rule, as a depot download shows it against a
//! CDN in memory: the permission bits its flags give, a file already whole,
//! the error a failed hash becomes and what a redirected folder leaves for the
//! retry. The rule itself is tested in `obelisk_core::publish`.
use std::os::unix::fs::MetadataExt;

use obelisk_core::testing::disk::{
    mode, partial_files, place, record, swap_for_a_link_when_assembled,
};

use super::*;

/// A depot served by a CDN in memory.
struct Depot {
    cdn: CdnFixture,
}

/// A depot of `(name, flags, contents)` files, each in a chunk of its own; a
/// file with no contents has no chunks.
fn depot(files: &[(&str, u32, &[u8])]) -> Depot {
    depot_edited(files, |_| {})
}

/// The same, with `edit` changing the manifest's entries before it is served.
fn depot_edited(files: &[(&str, u32, &[u8])], edit: impl FnOnce(&mut [ManifestFile])) -> Depot {
    let mut items: Vec<ManifestFile> = files
        .iter()
        .map(|(name, flags, bytes)| {
            let pieces: &[&[u8]] = if bytes.is_empty() { &[] } else { &[bytes] };
            let mut item = file(name, pieces);
            item.flags = *flags;
            item
        })
        .collect();
    edit(&mut items);
    let mut routes = HashMap::from([(
        "/depot/42/manifest/99/5/123".into(),
        zip(&manifest(items, false)),
    )]);
    for (_, _, bytes) in files.iter().filter(|(_, _, bytes)| !bytes.is_empty()) {
        routes.insert(
            format!("/depot/42/chunk/{}", hex(&Sha1::digest(bytes))),
            encrypt(&zip(bytes)),
        );
    }
    Depot {
        cdn: CdnFixture::new(routes),
    }
}

impl Depot {
    async fn install(&self, root: &Path) -> Result<DepotInstallation, ContentError> {
        self.install_watched(root, |_| {}).await
    }

    async fn install_watched(
        &self,
        root: &Path,
        progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Result<DepotInstallation, ContentError> {
        self.cdn
            .client()
            .download_depot(&plan(root), &CancelToken::new(), progress)
            .await
    }
}

#[tokio::test]
async fn files_are_published_with_the_modes_their_flags_give() {
    let files: [(&str, u32, &[u8]); 6] = [
        ("bin/flagged", 32, b"flagged bytes"),
        // Epic's executable bit, and Steam's read-only and hidden flags,
        // mean nothing here.
        ("bin/other-store-bit", 4, b"other store bytes"),
        ("bin/read-only-hidden", 8 | 16, b"read-only bytes"),
        ("data/plain.bin", 0, b"plain bytes"),
        ("data/empty", 0, b""),
        ("bin/empty-flagged", 32, b""),
    ];
    let expected = [0o755, 0o644, 0o644, 0o644, 0o644, 0o755];
    let depot = depot(&files);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");

    depot.install(&root).await.unwrap();

    for ((name, _, bytes), expected) in files.iter().zip(expected) {
        assert_eq!(fs::read(root.join(name)).unwrap(), *bytes, "{name}");
        assert_eq!(mode(&root.join(name)), expected, "{name}");
    }
    assert!(partial_files(&root).is_empty(), "no partial file is left");
    assert!(record(&root).is_file(), "a finished Transfer records");
}

#[tokio::test]
async fn a_file_that_is_already_whole_is_neither_fetched_nor_rewritten_but_takes_the_mode() {
    let helper_bytes = b"\xcf\xfa\xed\xfe helper";
    let depot = depot(&[
        ("bin/tool", 32, b"tool bytes"),
        ("data/table", 0, b"table bytes"),
        ("bin/helper", 0, helper_bytes),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let tool = place(&root, "bin/tool", b"tool bytes", 0o600);
    let table = place(&root, "data/table", b"table bytes", 0o777);
    let helper = place(&root, "bin/helper", helper_bytes, 0o600);
    let inodes = [&tool, &table, &helper].map(|path| fs::metadata(path).unwrap().ino());

    depot.install(&root).await.unwrap();

    assert_eq!(depot.cdn.chunk_requests(), 0);
    assert_eq!(
        (mode(&tool), mode(&table), mode(&helper)),
        (0o755, 0o644, 0o755)
    );
    assert_eq!(
        [&tool, &table, &helper].map(|path| fs::metadata(path).unwrap().ino()),
        inodes
    );
}

#[tokio::test]
async fn a_file_that_fails_its_hash_is_never_published_and_replaces_nothing() {
    // The chunk is right; the manifest's hash of the whole file is not.
    let depot = depot_edited(&[("data/bad", 0, b"bad bytes")], |items| {
        items[0].sha[0] ^= 1;
    });
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let bad = place(&root, "data/bad", b"old bad bytes", 0o640);

    let result = depot.install(&root).await;

    assert!(
        matches!(&result, Err(ContentError::InvalidData(reason))
            if reason == "backend.steam.content.assembledHashMismatch"),
        "{result:?}"
    );
    assert_eq!(fs::read(&bad).unwrap(), b"old bad bytes");
    assert_eq!(mode(&bad), 0o640);
    assert!(
        !record(&root).exists(),
        "a Transfer that failed records nothing"
    );
    let kept = partial_files(&root);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        fs::read(&kept[0]).unwrap(),
        b"bad bytes",
        "the assembled file stays unpublished"
    );
}

#[tokio::test]
async fn a_folder_swapped_for_a_link_before_the_rename_is_refused_and_the_retry_resumes() {
    let depot = depot(&[("bin/tool", 32, b"tool bytes")]);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let outside = tempfile::tempdir().unwrap();
    // The download makes the folder as it publishes the file; this one is
    // there already, for the swap to replace.
    fs::create_dir_all(root.join("bin")).unwrap();

    let result = depot
        .install_watched(
            &root,
            swap_for_a_link_when_assembled(root.join("bin"), outside.path().into(), 10),
        )
        .await;

    assert!(
        matches!(result, Err(ContentError::UnsafePath(_))),
        "{result:?}"
    );
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing reached the folder the link leads to"
    );
    assert!(!record(&root).exists());
    let kept = partial_files(&root);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(fs::read(&kept[0]).unwrap(), b"tool bytes");
    assert_eq!(depot.cdn.chunk_requests(), 1);

    // The folder is put back: what the refusal left is enough to publish.
    fs::remove_file(root.join("bin")).unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    depot.install(&root).await.unwrap();

    assert_eq!(fs::read(root.join("bin/tool")).unwrap(), b"tool bytes");
    assert_eq!(mode(&root.join("bin/tool")), 0o755);
    assert_eq!(
        depot.cdn.chunk_requests(),
        1,
        "the retry fetches nothing again"
    );
    assert!(partial_files(&root).is_empty());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(record(&root).is_file());
}
