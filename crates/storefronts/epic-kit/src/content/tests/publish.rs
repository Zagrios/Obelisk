//! What Epic hands the publish rule, as its download shows it against a CDN in
//! memory: the permission bits its flags give, a file already whole, the error
//! a failed hash becomes and what a redirected folder leaves for the retry. The
//! rule itself is tested in `obelisk_core::publish`.
use std::os::unix::fs::MetadataExt;

use obelisk_core::testing::disk::{
    mode, partial_files, place, record, swap_for_a_link_when_assembled,
};

use super::*;

/// A game whose files are made of chunks served by a CDN in memory.
struct Game {
    manifest: Arc<Manifest>,
    server: Server,
}

/// A game of `(name, flags, contents)` files, each in a chunk of its own; a
/// file with no contents has no parts.
fn game(files: &[(&str, u8, &[u8])]) -> Game {
    let mut entries = Vec::new();
    let mut chunks = std::collections::BTreeMap::new();
    let mut responses = HashMap::new();
    for (index, (name, flags, bytes)) in files.iter().enumerate() {
        let mut parts = Vec::new();
        if !bytes.is_empty() {
            let (chunk, wire) = chunk(bytes, 100 + index as u32, false);
            responses.insert(format!("/{}", chunk.relative_path(18)), wire);
            parts.push(ChunkPart {
                guid: chunk.guid,
                offset: 0,
                size: bytes.len() as u32,
            });
            chunks.insert(chunk.guid, chunk);
        }
        entries.push(ManifestFile {
            filename: (*name).into(),
            symlink_target: None,
            hash: Sha1::digest(bytes).into(),
            flags: *flags,
            parts,
        });
    }
    Game {
        manifest: Arc::new(manifest("publish-game", entries, chunks)),
        server: Server::new(responses, false),
    }
}

impl Game {
    async fn install(&self, root: &Path) -> Result<DownloadSummary, EpicError> {
        self.install_watched(root, |_| {}).await
    }

    async fn install_watched(
        &self,
        root: &Path,
        progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Result<DownloadSummary, EpicError> {
        client(&[&self.server])
            .download(
                Arc::clone(&self.manifest),
                std::slice::from_ref(&self.server.base),
                root,
                &CancelToken::new(),
                progress,
            )
            .await
    }

    /// The chunk requests the CDN took.
    fn requests(&self) -> usize {
        self.server.paths().len()
    }
}

#[tokio::test]
async fn files_are_published_with_the_modes_their_flags_give() {
    let files: [(&str, u8, &[u8]); 6] = [
        ("bin/flagged", 4, b"flagged bytes"),
        // Epic's read-only flag and Steam's executable bit mean nothing here.
        ("bin/read-only", 1, b"read-only bytes"),
        ("bin/other-store-bit", 32, b"other store bytes"),
        ("data/plain.bin", 0, b"plain bytes"),
        ("data/empty", 0, b""),
        ("bin/empty-flagged", 4, b""),
    ];
    let expected = [0o755, 0o644, 0o644, 0o644, 0o644, 0o755];
    let game = game(&files);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");

    game.install(&root).await.unwrap();

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
    let game = game(&[
        ("bin/tool", 4, b"tool bytes"),
        ("data/table", 0, b"table bytes"),
        ("bin/helper", 0, helper_bytes),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let tool = place(&root, "bin/tool", b"tool bytes", 0o600);
    let table = place(&root, "data/table", b"table bytes", 0o777);
    let helper = place(&root, "bin/helper", helper_bytes, 0o600);
    let inodes = [&tool, &table, &helper].map(|path| fs::metadata(path).unwrap().ino());

    game.install(&root).await.unwrap();

    assert_eq!(game.requests(), 0);
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
async fn a_file_that_fails_its_hash_is_never_published_and_the_files_before_it_stay() {
    let mut game = game(&[
        ("bin/good", 4, b"good bytes"),
        ("data/bad", 0, b"bad bytes"),
    ]);
    // The chunk is right; the manifest's hash of the whole file is not.
    Arc::get_mut(&mut game.manifest).unwrap().files[1].hash[0] ^= 1;
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let bad = place(&root, "data/bad", b"old bad bytes", 0o640);

    let result = game.install(&root).await;

    assert!(matches!(result, Err(EpicError::Integrity)), "{result:?}");
    assert_eq!(fs::read(&bad).unwrap(), b"old bad bytes");
    assert_eq!(mode(&bad), 0o640);
    assert_eq!(fs::read(root.join("bin/good")).unwrap(), b"good bytes");
    assert_eq!(mode(&root.join("bin/good")), 0o755);
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
    let game = game(&[("bin/tool", 4, b"tool bytes")]);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let outside = tempfile::tempdir().unwrap();
    // The download makes the folder as it publishes the file; this one is
    // there already, for the swap to replace.
    fs::create_dir_all(root.join("bin")).unwrap();

    let result = game
        .install_watched(
            &root,
            swap_for_a_link_when_assembled(root.join("bin"), outside.path().into(), 10),
        )
        .await;

    assert!(
        matches!(result, Err(EpicError::UnsafePath(_))),
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
    assert_eq!(game.requests(), 1);

    // The folder is put back: what the refusal left is enough to publish.
    fs::remove_file(root.join("bin")).unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    game.install(&root).await.unwrap();

    assert_eq!(fs::read(root.join("bin/tool")).unwrap(), b"tool bytes");
    assert_eq!(mode(&root.join("bin/tool")), 0o755);
    assert_eq!(game.requests(), 1, "the retry fetches nothing again");
    assert!(partial_files(&root).is_empty());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(record(&root).is_file());
}
