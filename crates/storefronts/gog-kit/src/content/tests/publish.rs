//! What GOG hands the publish rule, as its download shows it against a CDN in
//! memory: the permission bits its flag gives, a file already whole, a link in
//! a file's place, what a chunk that fails its MD5 leaves behind and what a
//! redirected folder leaves for the retry. The rule itself is tested in
//! `obelisk_core::publish`.
use std::os::unix::fs::MetadataExt;

use obelisk_core::testing::disk::{
    mode, partial_files, place, record, swap_for_a_link_when_assembled,
};

use super::*;

const PRODUCT: &str = "1207658924";

/// A depot whose files are one chunk each, served by a CDN in memory.
struct Depot {
    cdn: Cdn,
    installed: Arc<Installed>,
}

/// A depot of `(path, flags, contents)` files; a file with no contents has no
/// chunks. A file named in `corrupt` is served as a zlib stream of the same
/// length that inflates to other bytes.
fn depot(files: &[(&str, &[&str], &[u8])], corrupt: &[&str]) -> Depot {
    let mut responses = HashMap::new();
    let mut items = Vec::new();
    for (path, flags, bytes) in files {
        let mut chunks = Vec::new();
        if !bytes.is_empty() {
            let packed = if corrupt.contains(path) {
                let mut wrong = bytes.to_vec();
                wrong[0] ^= 1;
                stored(&wrong)
            } else {
                compressed(bytes)
            };
            responses.insert(
                format!("/store/{}", galaxy_path(&md5_hex(&packed)).unwrap()),
                packed.clone(),
            );
            chunks.push(json!({"md5": md5_hex(bytes), "size": bytes.len(),
                "compressedMd5": md5_hex(&packed), "compressedSize": packed.len()}));
        }
        items.push(json!({"type": "DepotFile", "path": path, "chunks": chunks, "flags": flags}));
    }
    let manifest: DepotManifest =
        serde_json::from_value(json!({"depot": {"items": items}})).unwrap();
    let mut installed = Installed::default();
    installed.add("", PRODUCT, manifest.depot.items).unwrap();
    Depot {
        cdn: serving(responses),
        installed: Arc::new(installed),
    }
}

/// A zlib stream that stores `bytes` as they are.
fn stored(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::none());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

impl Depot {
    fn links(&self) -> Arc<SecureLinks> {
        let endpoint = Endpoint {
            url: format!("{}/store", self.cdn.base()),
            url_format: String::new(),
            parameters: Default::default(),
            priority: 10,
            fallback_only: false,
        };
        Arc::new(SecureLinks::new(Arc::new(move |_| {
            let endpoint = endpoint.clone();
            async move { Ok(vec![endpoint]) }.boxed()
        })))
    }

    async fn install(&self, root: &Path) -> Result<DownloadSummary, GogError> {
        self.install_watched(root, |_| {}).await
    }

    async fn install_watched(
        &self,
        root: &Path,
        progress: impl Fn(DownloadProgress) + Send + Sync + 'static,
    ) -> Result<DownloadSummary, GogError> {
        download(
            reaching(&[&self.cdn]),
            self.links(),
            Arc::clone(&self.installed),
            "build".into(),
            root,
            &CancelToken::new(),
            progress,
        )
        .await
    }

    /// The chunk requests the CDN took.
    fn requests(&self) -> usize {
        self.cdn.arrivals().len()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_are_published_with_the_modes_their_flag_gives() {
    let files: [(&str, &[&str], &[u8]); 5] = [
        ("bin/flagged", &["executable"], b"flagged bytes"),
        (
            "bin/flagged-among-others",
            &["other", "executable"],
            b"among bytes",
        ),
        ("data/plain.bin", &[], b"plain bytes"),
        ("data/empty", &[], b""),
        ("bin/empty-flagged", &["executable"], b""),
    ];
    let expected = [0o755, 0o755, 0o644, 0o644, 0o755];
    let depot = depot(&files, &[]);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");

    depot.install(&root).await.unwrap();

    for ((path, _, bytes), expected) in files.iter().zip(expected) {
        assert_eq!(fs::read(root.join(path)).unwrap(), *bytes, "{path}");
        assert_eq!(mode(&root.join(path)), expected, "{path}");
    }
    assert!(partial_files(&root).is_empty(), "no partial file is left");
    assert!(record(&root).is_file(), "a finished Transfer records");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_is_already_whole_is_neither_fetched_nor_rewritten_but_takes_the_mode() {
    let helper_bytes = b"\xcf\xfa\xed\xfe helper";
    let depot = depot(
        &[
            ("bin/tool", &["executable"], b"tool bytes"),
            ("data/table", &[], b"table bytes"),
            ("bin/helper", &[], helper_bytes),
        ],
        &[],
    );
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let tool = place(&root, "bin/tool", b"tool bytes", 0o600);
    let table = place(&root, "data/table", b"table bytes", 0o777);
    let helper = place(&root, "bin/helper", helper_bytes, 0o600);
    let inodes = [&tool, &table, &helper].map(|path| fs::metadata(path).unwrap().ino());

    depot.install(&root).await.unwrap();

    assert_eq!(depot.requests(), 0);
    assert_eq!(
        (mode(&tool), mode(&table), mode(&helper)),
        (0o755, 0o644, 0o755)
    );
    assert_eq!(
        [&tool, &table, &helper].map(|path| fs::metadata(path).unwrap().ino()),
        inodes
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_target_that_is_a_link_is_refused_and_never_written_through() {
    let depot = depot(&[("bin/tool", &["executable"], b"tool bytes")], &[]);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let outside = tempfile::tempdir().unwrap();
    let elsewhere = place(outside.path(), "elsewhere", b"unchanged", 0o644);
    fs::create_dir_all(root.join("bin")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, root.join("bin/tool")).unwrap();

    let result = depot.install(&root).await;

    assert!(matches!(result, Err(GogError::UnsafePath(_))), "{result:?}");
    assert_eq!(fs::read(&elsewhere).unwrap(), b"unchanged");
    assert_eq!(mode(&elsewhere), 0o644);
    assert_eq!(depot.requests(), 0);
    assert!(!record(&root).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chunk_that_fails_its_md5_is_never_published_and_the_files_before_it_stay() {
    // GOG records an MD5 for each chunk and none for the whole file: a chunk
    // is checked as it arrives, and nothing is hashed again at publication.
    let depot = depot(
        &[
            ("bin/good", &["executable"], b"good bytes"),
            ("data/bad", &[], b"bad bytes"),
        ],
        &["data/bad"],
    );
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let bad = place(&root, "data/bad", b"old bad bytes", 0o640);

    let result = depot.install(&root).await;

    assert!(matches!(result, Err(GogError::Integrity)), "{result:?}");
    assert_eq!(fs::read(&bad).unwrap(), b"old bad bytes");
    assert_eq!(mode(&bad), 0o640);
    assert_eq!(fs::read(root.join("bin/good")).unwrap(), b"good bytes");
    assert_eq!(mode(&root.join("bin/good")), 0o755);
    assert!(
        !record(&root).exists(),
        "a Transfer that failed records nothing"
    );
    assert!(
        partial_root(&root).is_dir(),
        "the partial directory is kept for the next attempt"
    );
    let kept = partial_files(&root);
    assert!(
        matches!(&kept[..], [copy] if copy.to_string_lossy().ends_with(".part.next")),
        "only the copy still being written is kept: {kept:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_swapped_for_a_link_before_the_rename_is_refused_and_the_retry_resumes() {
    let depot = depot(&[("Game/data.bin", &[], b"data bytes")], &[]);
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("game");
    let outside = tempfile::tempdir().unwrap();
    // The download makes the folder as it publishes the file; this one is
    // there already, for the swap to replace.
    fs::create_dir_all(root.join("Game")).unwrap();

    let result = depot
        .install_watched(
            &root,
            swap_for_a_link_when_assembled(root.join("Game"), outside.path().into(), 10),
        )
        .await;

    assert!(matches!(result, Err(GogError::UnsafePath(_))), "{result:?}");
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing reached the folder the link leads to"
    );
    assert!(!record(&root).exists());
    let kept = partial_files(&root);
    assert!(
        matches!(&kept[..], [copy] if copy.to_string_lossy().ends_with(".part")),
        "the finished copy is kept: {kept:?}"
    );
    assert_eq!(fs::read(&kept[0]).unwrap(), b"data bytes");
    assert_eq!(depot.requests(), 1);

    // The folder is put back: what the refusal left is enough to publish.
    fs::remove_file(root.join("Game")).unwrap();
    fs::create_dir(root.join("Game")).unwrap();
    depot.install(&root).await.unwrap();

    assert_eq!(fs::read(root.join("Game/data.bin")).unwrap(), b"data bytes");
    assert_eq!(depot.requests(), 1, "the retry fetches nothing again");
    assert!(partial_files(&root).is_empty());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(record(&root).is_file());
}
